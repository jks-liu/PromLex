use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use directories::ProjectDirs;
use parking_lot::RwLock;

use crate::models::{
    AiConversationLog, AppData, AppSettings, Glossary, HistoryEntry, PromptTemplate, Provider,
    SystemLogEntry,
};

#[derive(Clone)]
pub struct AppStore {
    inner: Arc<StoreInner>,
    file_log_context: Option<(String, String)>,
}

struct StoreInner {
    path: PathBuf,
    data: RwLock<AppData>,
}

impl AppStore {
    pub fn load() -> Result<Self, io::Error> {
        let configured_directory = std::env::var_os("PROMLEX_DATA_DIR")
            .or_else(|| std::env::var_os("TRANOVA_DATA_DIR"))
            .map(PathBuf::from);
        // Retain the legacy location so renaming the app preserves existing user data.
        let project_dirs = if configured_directory.is_none() {
            Some(
                ProjectDirs::from("com", "Tranova", "Tranova")
                    .ok_or_else(|| io::Error::other("unable to resolve user data directory"))?,
            )
        } else {
            None
        };
        let directory = configured_directory
            .as_deref()
            .or_else(|| project_dirs.as_ref().map(ProjectDirs::data_local_dir))
            .expect("a configured or platform data directory is always present");
        fs::create_dir_all(directory)?;
        let path = directory.join("tranova-data.json");
        let mut data = match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|_| AppData::default()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => AppData::default(),
            Err(error) => return Err(error),
        };
        data.upgrade_default_prompt();
        let store = Self {
            file_log_context: None,
            inner: Arc::new(StoreInner {
                path,
                data: RwLock::new(data),
            }),
        };
        store.persist()?;
        Ok(store)
    }

    pub fn snapshot(&self) -> AppData {
        self.inner.data.read().clone()
    }

    pub fn for_file_job(&self, job_id: &str, filename: &str) -> Self {
        Self {
            inner: self.inner.clone(),
            file_log_context: Some((job_id.to_string(), filename.to_string())),
        }
    }

    pub fn settings(&self) -> AppSettings {
        self.inner.data.read().settings.clone()
    }

    pub fn provider(&self, id: &str) -> Option<Provider> {
        self.inner
            .data
            .read()
            .providers
            .iter()
            .find(|provider| provider.id == id)
            .cloned()
    }

    pub fn logs(&self) -> (Vec<SystemLogEntry>, Vec<AiConversationLog>) {
        let data = self.inner.data.read();
        (data.system_logs.clone(), data.ai_conversations.clone())
    }

    pub fn system_logs_page(&self, page: usize, page_size: usize) -> (Vec<SystemLogEntry>, usize) {
        let data = self.inner.data.read();
        (
            page_slice(&data.system_logs, page, page_size),
            data.system_logs.len(),
        )
    }

    pub fn ai_conversations_page(
        &self,
        kind: &str,
        page: usize,
        page_size: usize,
    ) -> (Vec<AiConversationLog>, usize) {
        let data = self.inner.data.read();
        let entries: Vec<_> = data
            .ai_conversations
            .iter()
            .filter(|entry| log_matches(entry, kind))
            .cloned()
            .collect();
        if kind != "file" {
            return (page_slice(&entries, page, page_size), entries.len());
        }
        // Page by jobs so all retained conversations for a job stay together.
        let mut jobs = Vec::new();
        for entry in &entries {
            let key = entry.job_id.as_deref().unwrap_or("");
            if !jobs.contains(&key) {
                jobs.push(key);
            }
        }
        let selected = page_slice(&jobs, page, page_size);
        let items = entries
            .iter()
            .filter(|entry| selected.contains(&entry.job_id.as_deref().unwrap_or("")))
            .cloned()
            .collect();
        (items, jobs.len())
    }

    pub fn delete_log(&self, kind: &str, id: &str) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        match kind {
            "system" => data.system_logs.retain(|entry| entry.id != id),
            "conversations" | "text" | "file" => data
                .ai_conversations
                .retain(|entry| !log_matches(entry, kind) || entry.id != id),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Unknown log kind",
                ))
            }
        }
        drop(data);
        self.persist()
    }

    pub fn clear_logs(&self, kind: &str, before: Option<u64>) -> Result<(), io::Error> {
        let retain = |timestamp: &str| {
            before.is_some_and(|limit| {
                timestamp
                    .parse::<u64>()
                    .map_or(true, |value| value >= limit)
            })
        };
        let mut data = self.inner.data.write();
        match kind {
            "system" => data.system_logs.retain(|entry| retain(&entry.timestamp)),
            "conversations" | "text" | "file" => data
                .ai_conversations
                .retain(|entry| !log_matches(entry, kind) || retain(&entry.timestamp)),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Unknown log kind",
                ))
            }
        }
        drop(data);
        self.persist()
    }

    pub fn add_system_log(&self, level: &str, scope: &str, message: impl Into<String>) {
        let message = message.into();
        let mut data = self.inner.data.write();
        if !logging_enabled(&data.settings, level) {
            return;
        }
        data.system_logs.insert(
            0,
            SystemLogEntry {
                id: log_id(),
                timestamp: unix_timestamp_millis(),
                level: normalize_log_level(level),
                scope: scope.trim().to_string(),
                message,
            },
        );
        data.system_logs.truncate(500);
        drop(data);
        let _ = self.persist();
    }

    pub fn add_ai_conversation(&self, mut entry: AiConversationLog) {
        if let Some((job_id, filename)) = &self.file_log_context {
            entry.job_id = Some(job_id.clone());
            entry.filename = Some(filename.clone());
        }
        let mut data = self.inner.data.write();
        if !data.settings.logging_enabled {
            return;
        }
        data.ai_conversations.insert(0, entry);
        data.ai_conversations.truncate(200);
        drop(data);
        let _ = self.persist();
    }

    pub fn add_history(&self, mut entry: HistoryEntry) -> Result<HistoryEntry, io::Error> {
        if entry.id.trim().is_empty()
            || entry.source_language.trim().is_empty()
            || entry.target_language.trim().is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "History entry languages and id are required",
            ));
        }
        entry.source_text = entry.source_text.chars().take(20_000).collect();
        entry.translated_text = entry.translated_text.chars().take(20_000).collect();
        let mut data = self.inner.data.write();
        data.history.retain(|current| current.id != entry.id);
        data.history.insert(0, entry.clone());
        data.history.truncate(100);
        drop(data);
        self.persist()?;
        Ok(entry)
    }

    pub fn delete_history(&self, id: &str) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        data.history.retain(|entry| entry.id != id);
        drop(data);
        self.persist()
    }

    pub fn clear_history(&self) -> Result<(), io::Error> {
        self.inner.data.write().history.clear();
        self.persist()
    }

    pub fn save_provider(&self, provider: Provider) -> Result<Provider, io::Error> {
        let provider = validate_provider(provider)?;
        let mut data = self.inner.data.write();
        upsert(&mut data.providers, provider.clone(), |value| &value.id);
        drop(data);
        self.persist()?;
        self.add_system_log(
            "info",
            "provider",
            format!("Saved provider {}", provider.name),
        );
        Ok(provider)
    }

    pub fn delete_provider(&self, id: &str) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        data.providers.retain(|provider| provider.id != id);
        drop(data);
        self.persist()
    }

    pub fn save_glossary(&self, glossary: Glossary) -> Result<Glossary, io::Error> {
        if glossary.id.trim().is_empty() || glossary.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Glossary name and id are required",
            ));
        }
        let mut glossary = glossary;
        glossary
            .entries
            .retain(|entry| !entry.source.trim().is_empty() && !entry.target.trim().is_empty());
        let mut data = self.inner.data.write();
        upsert(&mut data.glossaries, glossary.clone(), |value| &value.id);
        drop(data);
        self.persist()?;
        Ok(glossary)
    }

    pub fn delete_glossary(&self, id: &str) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        data.glossaries.retain(|glossary| glossary.id != id);
        drop(data);
        self.persist()
    }

    pub fn save_prompt(&self, prompt: PromptTemplate) -> Result<PromptTemplate, io::Error> {
        if prompt.id.trim().is_empty()
            || prompt.name.trim().is_empty()
            || prompt.content.trim().is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Prompt id, name and content are required",
            ));
        }
        let mut data = self.inner.data.write();
        upsert(&mut data.prompts, prompt.clone(), |value| &value.id);
        drop(data);
        self.persist()?;
        Ok(prompt)
    }

    pub fn delete_prompt(&self, id: &str) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        data.prompts
            .retain(|prompt| prompt.id != id || id == "faithful-translation");
        drop(data);
        self.persist()
    }

    pub fn save_settings(&self, mut settings: AppSettings) -> Result<AppSettings, io::Error> {
        settings.language = match settings.language.as_str() {
            "zh-CN" => "zh-CN".to_string(),
            _ => "en".to_string(),
        };
        settings.web_host = match settings.web_host.trim() {
            "localhost" => "127.0.0.1".to_string(),
            value => value.to_string(),
        };
        let host = settings.web_host.parse::<std::net::IpAddr>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Web access must use a loopback IP address",
            )
        })?;
        if !host.is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Web access is limited to this device for security; use 127.0.0.1 or ::1",
            ));
        }
        if settings.web_port < 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Web port must be between 1024 and 65535",
            ));
        }
        settings.ai_timeout_seconds = settings.ai_timeout_seconds.clamp(10, 3_600);
        settings.log_level = normalize_log_level(&settings.log_level);
        settings.custom_download_directory = settings.custom_download_directory.trim().to_string();
        settings.last_download_directory = settings.last_download_directory.trim().to_string();
        self.inner.data.write().settings = settings.clone();
        self.persist()?;
        Ok(settings)
    }

    pub fn set_last_download_directory(&self, directory: &Path) -> Result<(), io::Error> {
        let directory = directory.to_string_lossy().trim().to_string();
        if directory.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A download directory is required",
            ));
        }
        self.inner.data.write().settings.last_download_directory = directory;
        self.persist()
    }

    pub fn replace_glossaries(&self, glossaries: Vec<Glossary>) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        for glossary in glossaries {
            if !glossary.id.trim().is_empty() && !glossary.name.trim().is_empty() {
                upsert(&mut data.glossaries, glossary, |value| &value.id);
            }
        }
        drop(data);
        self.persist()
    }

    pub fn replace_prompts(&self, prompts: Vec<PromptTemplate>) -> Result<(), io::Error> {
        let mut data = self.inner.data.write();
        for prompt in prompts {
            if !prompt.id.trim().is_empty()
                && !prompt.name.trim().is_empty()
                && !prompt.content.trim().is_empty()
            {
                upsert(&mut data.prompts, prompt, |value| &value.id);
            }
        }
        drop(data);
        self.persist()
    }

    fn persist(&self) -> Result<(), io::Error> {
        let content = serde_json::to_vec_pretty(&*self.inner.data.read())
            .map_err(|error| io::Error::other(error.to_string()))?;
        fs::write(&self.inner.path, content)
    }
}

fn validate_provider(mut provider: Provider) -> Result<Provider, io::Error> {
    provider.id = provider.id.trim().to_string();
    provider.name = provider.name.trim().to_string();
    provider.base_url = provider.base_url.trim().trim_end_matches('/').to_string();
    provider.model = provider.model.trim().to_string();
    provider.context_size = provider.context_size.clamp(1_024, 2_000_000);
    provider.max_segments = provider.max_segments.clamp(1, 1_024);
    provider.max_concurrent = provider.max_concurrent.clamp(1, 64);
    if provider.id.is_empty()
        || provider.name.is_empty()
        || provider.base_url.is_empty()
        || provider.model.is_empty()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Provider id, name, API URL and model are required",
        ));
    }
    if !(provider.base_url.starts_with("http://") || provider.base_url.starts_with("https://")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Provider URL must begin with http:// or https://",
        ));
    }
    Ok(provider)
}

fn normalize_log_level(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "debug" => "debug".to_string(),
        "warn" => "warn".to_string(),
        "error" => "error".to_string(),
        _ => "info".to_string(),
    }
}

fn logging_enabled(settings: &AppSettings, level: &str) -> bool {
    if !settings.logging_enabled {
        return false;
    }
    let rank = |value: &str| match value {
        "debug" => 0,
        "info" => 1,
        "warn" => 2,
        "error" => 3,
        _ => 1,
    };
    rank(level) >= rank(&settings.log_level)
}

fn log_id() -> String {
    format!("log-{}-{}", unix_timestamp_nanos(), std::process::id())
}

fn unix_timestamp_millis() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn unix_timestamp_nanos() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .to_string()
}

fn upsert<T, F>(items: &mut Vec<T>, item: T, key: F)
where
    F: Fn(&T) -> &String,
{
    if let Some(position) = items.iter().position(|current| key(current) == key(&item)) {
        items[position] = item;
    } else {
        items.push(item);
    }
}

fn page_slice<T: Clone>(items: &[T], page: usize, page_size: usize) -> Vec<T> {
    let start = page.saturating_sub(1).saturating_mul(page_size);
    items.iter().skip(start).take(page_size).cloned().collect()
}

fn log_matches(entry: &AiConversationLog, kind: &str) -> bool {
    match kind {
        "text" => !entry.is_file(),
        "file" => entry.is_file(),
        "conversations" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use parking_lot::RwLock;

    use super::{AppStore, StoreInner};
    use crate::models::{AiConversationLog, AppData, AppSettings, SystemLogEntry};

    #[test]
    fn file_logs_keep_job_context_across_clones_retries_and_pagination() {
        let directory = tempfile::tempdir().unwrap();
        let store = AppStore {
            file_log_context: None,
            inner: Arc::new(StoreInner {
                path: directory.path().join("data.json"),
                data: RwLock::new(AppData {
                    settings: AppSettings {
                        logging_enabled: true,
                        ..AppSettings::default()
                    },
                    ..AppData::default()
                }),
            }),
        };
        let entry = |id: &str, timestamp: &str| AiConversationLog {
            id: id.into(),
            timestamp: timestamp.into(),
            operation: "translation".into(),
            job_id: None,
            filename: None,
            provider: "test".into(),
            model: "test".into(),
            request: "source".into(),
            response: "translated".into(),
            duration_ms: 1,
            success: true,
            error: None,
        };
        let first = store.for_file_job("job-1", "same.md");
        let second = store.for_file_job("job-2", "same.md");
        first.clone().add_ai_conversation(entry("first", "1000"));
        second.add_ai_conversation(entry("second", "2000"));
        store.add_ai_conversation(entry("text", "3000"));
        store
            .for_file_job("job-1", "same.md")
            .add_ai_conversation(entry("retry", "4000"));

        let (page, total) = store.ai_conversations_page("file", 1, 1);
        assert_eq!(total, 2);
        assert_eq!(
            page.iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["retry", "first"]
        );
        assert!(page
            .iter()
            .all(|entry| entry.job_id.as_deref() == Some("job-1")
                && entry.filename.as_deref() == Some("same.md")));
        let (page, _) = store.ai_conversations_page("file", 2, 1);
        assert_eq!(page[0].id, "second");
        assert_eq!(store.ai_conversations_page("text", 1, 20).0[0].id, "text");

        store.delete_log("text", "first").unwrap();
        assert_eq!(store.ai_conversations_page("file", 1, 1).0.len(), 2);
        store.clear_logs("file", Some(3000)).unwrap();
        assert_eq!(store.ai_conversations_page("file", 1, 20).0.len(), 1);
        assert_eq!(store.ai_conversations_page("text", 1, 20).1, 1);
        store.clear_logs("text", None).unwrap();
        assert_eq!(store.ai_conversations_page("text", 1, 20).1, 0);
        assert_eq!(store.ai_conversations_page("file", 1, 20).1, 1);
    }

    #[test]
    fn settings_can_be_saved_repeatedly_on_windows() {
        let directory = std::env::temp_dir().join(format!(
            "tranova-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("data.json");
        let store = AppStore {
            file_log_context: None,
            inner: Arc::new(StoreInner {
                path: path.clone(),
                data: RwLock::new(AppData::default()),
            }),
        };

        let mut settings = AppSettings {
            proxy_url: "http://127.0.0.1:8080".to_string(),
            ..AppSettings::default()
        };
        store.save_settings(settings.clone()).unwrap();
        settings.proxy_url = "socks5h://127.0.0.1:1080".to_string();
        store.save_settings(settings).unwrap();

        let saved: AppData = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.settings.proxy_url, "socks5h://127.0.0.1:1080");
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn logs_can_be_paginated_deleted_and_cleared_before_a_timestamp() {
        let directory = std::env::temp_dir().join(format!(
            "tranova-log-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("data.json");
        let data = AppData {
            system_logs: (1..=5)
                .rev()
                .map(|value| SystemLogEntry {
                    id: format!("log-{value}"),
                    timestamp: (value * 1_000).to_string(),
                    level: "info".to_string(),
                    scope: "test".to_string(),
                    message: value.to_string(),
                })
                .collect(),
            ..AppData::default()
        };
        let store = AppStore {
            file_log_context: None,
            inner: Arc::new(StoreInner {
                path: path.clone(),
                data: RwLock::new(data),
            }),
        };

        let (second_page, total) = store.system_logs_page(2, 2);
        assert_eq!(total, 5);
        assert_eq!(second_page[0].id, "log-3");
        store.delete_log("system", "log-3").unwrap();
        store.clear_logs("system", Some(4_000)).unwrap();
        let (remaining, total) = store.system_logs_page(1, 20);
        assert_eq!(total, 2);
        assert_eq!(remaining[0].id, "log-5");

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
