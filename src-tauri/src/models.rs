use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_parser: ReasoningParser,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub proxy_mode: ProxyMode,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub supports_images: bool,
    #[serde(default = "default_context_size")]
    pub context_size: usize,
    #[serde(default = "default_max_segments")]
    pub max_segments: usize,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    #[serde(default)]
    pub text_translation_model: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningParser {
    #[default]
    Auto,
    Openai,
    Qwen3,
    DeepseekR1,
    DeepseekV3,
    Glm45,
    Gemma4,
    Granite,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    None,
    #[default]
    Settings,
    System,
}

fn default_true() -> bool {
    true
}

fn default_context_size() -> usize {
    32_768
}

fn default_max_segments() -> usize {
    16
}

fn default_max_concurrent() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryEntry {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Glossary {
    pub id: String,
    pub name: String,
    pub source_language: String,
    pub target_language: String,
    #[serde(default)]
    pub entries: Vec<GlossaryEntry>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub id: String,
    pub name: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub proxy_url: String,
    #[serde(default = "default_web_host")]
    pub web_host: String,
    #[serde(default = "default_web_port")]
    pub web_port: u16,
    #[serde(default = "default_ai_timeout_seconds")]
    pub ai_timeout_seconds: u64,
    #[serde(default)]
    pub logging_enabled: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub download_location: DownloadLocation,
    #[serde(default)]
    pub custom_download_directory: String,
    #[serde(default)]
    pub auto_download_files: bool,
    #[serde(default)]
    pub ask_download_location: bool,
    #[serde(default)]
    pub last_download_directory: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DownloadLocation {
    #[default]
    Source,
    Downloads,
    Custom,
}

fn default_language() -> String {
    "en".to_string()
}
fn default_web_host() -> String {
    "127.0.0.1".to_string()
}
fn default_web_port() -> u16 {
    48731
}
fn default_ai_timeout_seconds() -> u64 {
    180
}
fn default_log_level() -> String {
    "info".to_string()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            language: default_language(),
            proxy_url: String::new(),
            web_host: default_web_host(),
            web_port: default_web_port(),
            ai_timeout_seconds: default_ai_timeout_seconds(),
            logging_enabled: false,
            log_level: default_log_level(),
            download_location: DownloadLocation::default(),
            custom_download_directory: String::new(),
            auto_download_files: false,
            ask_download_location: false,
            last_download_directory: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppData {
    #[serde(default)]
    pub providers: Vec<Provider>,
    #[serde(default = "default_prompts")]
    pub prompts: Vec<PromptTemplate>,
    #[serde(default)]
    pub glossaries: Vec<Glossary>,
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub history: Vec<HistoryEntry>,
    #[serde(default)]
    pub system_logs: Vec<SystemLogEntry>,
    #[serde(default)]
    pub ai_conversations: Vec<AiConversationLog>,
}

fn default_prompts() -> Vec<PromptTemplate> {
    vec![PromptTemplate {
        id: "faithful-translation".to_string(),
        name: "Faithful translation".to_string(),
        content: "Translate faithfully from {{source_language}} to {{target_language}}. Preserve the original meaning, tone, formatting, markup and line breaks. Use the terminology below when relevant. Return only the translated text.\n\n{{glossary}}".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    }]
}

impl Default for AppData {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            prompts: default_prompts(),
            glossaries: Vec::new(),
            settings: AppSettings::default(),
            history: Vec::new(),
            system_logs: Vec::new(),
            ai_conversations: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub kind: String,
    pub source_language: String,
    pub target_language: String,
    #[serde(default)]
    pub source_text: String,
    #[serde(default)]
    pub translated_text: String,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateOptions {
    pub source_language: String,
    pub target_language: String,
    pub provider_id: String,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub additional_prompt: String,
    #[serde(default)]
    pub glossary_ids: Vec<String>,
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    #[serde(default)]
    pub summarize: bool,
    #[serde(default)]
    pub output_mode: FileOutputMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum FileOutputMode {
    #[default]
    Translated,
    Bilingual,
}

impl TranslateOptions {
    pub fn into_request(self) -> TranslateRequest {
        TranslateRequest {
            text: String::new(),
            source_language: self.source_language,
            target_language: self.target_language,
            provider_id: self.provider_id,
            prompt_id: self.prompt_id,
            additional_prompt: self.additional_prompt,
            glossary_ids: self.glossary_ids,
            reasoning_effort: self.reasoning_effort,
            summarize: self.summarize,
            context_summary: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AppSettings, DownloadLocation, Provider, ProxyMode, ReasoningParser, TranslateOptions,
    };

    #[test]
    fn settings_default_timeout_is_180_seconds() {
        assert_eq!(AppSettings::default().ai_timeout_seconds, 180);
        let settings: AppSettings = serde_json::from_str(
            r#"{"language":"en","proxyUrl":"","webHost":"127.0.0.1","webPort":48731}"#,
        )
        .unwrap();
        assert_eq!(settings.ai_timeout_seconds, 180);
        assert_eq!(settings.log_level, "info");
        assert_eq!(settings.download_location, DownloadLocation::Source);
        assert!(settings.custom_download_directory.is_empty());
        assert!(!settings.auto_download_files);
        assert!(!settings.ask_download_location);
    }

    #[test]
    fn file_translation_options_do_not_require_text() {
        let options: TranslateOptions = serde_json::from_str(
            r#"{"sourceLanguage":"en","targetLanguage":"zh","providerId":"provider","glossaryIds":[]}"#,
        )
        .unwrap();
        let request = options.into_request();
        assert!(request.text.is_empty());
        assert_eq!(request.target_language, "zh");
        assert_eq!(request.reasoning_effort, "none");
    }

    #[test]
    fn providers_default_to_the_settings_proxy_mode() {
        let provider: Provider = serde_json::from_str(
            r#"{"id":"provider","name":"Provider","baseUrl":"http://127.0.0.1:1","model":"model"}"#,
        )
        .unwrap();
        assert_eq!(provider.proxy_mode, ProxyMode::Settings);
        assert_eq!(provider.reasoning_parser, ReasoningParser::Auto);
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateRequest {
    pub text: String,
    pub source_language: String,
    pub target_language: String,
    pub provider_id: String,
    #[serde(default)]
    pub prompt_id: Option<String>,
    #[serde(default)]
    pub additional_prompt: String,
    #[serde(default)]
    pub glossary_ids: Vec<String>,
    #[serde(default = "default_reasoning_effort")]
    pub reasoning_effort: String,
    #[serde(default)]
    pub summarize: bool,
    #[serde(default, skip_serializing)]
    pub context_summary: Option<String>,
}

fn default_reasoning_effort() -> String {
    "none".to_string()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationResult {
    pub translated_text: String,
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct FileProgress {
    pub stage: String,
    pub total_segments: usize,
    pub translated_segments: usize,
    pub failed_segments: usize,
    pub skipped_segments: usize,
    pub total_batches: usize,
    pub completed_batches: usize,
    pub streaming_batch: Option<usize>,
    pub streaming_segments: usize,
    pub streaming_batch_segments: usize,
    pub streaming_text: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileBatchFailure {
    pub id: usize,
    pub segment_count: usize,
    pub attempts: usize,
    pub error: String,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileJobState {
    Queued,
    Processing,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileJobStatus {
    pub id: String,
    pub filename: String,
    pub output_mode: FileOutputMode,
    pub state: FileJobState,
    pub stage: String,
    pub total_segments: usize,
    pub translated_segments: usize,
    pub failed_segments: usize,
    pub skipped_segments: usize,
    pub total_batches: usize,
    pub completed_batches: usize,
    pub failed_batches: Vec<FileBatchFailure>,
    pub streaming_batch: Option<usize>,
    pub streaming_segments: usize,
    pub streaming_batch_segments: usize,
    pub streaming_text: Option<String>,
    pub result_filename: String,
    pub media_type: String,
    pub error: Option<String>,
    pub created_at: String,
    pub source_path: Option<String>,
    pub downloaded_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemLogEntry {
    pub id: String,
    pub timestamp: String,
    pub level: String,
    pub scope: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConversationLog {
    pub id: String,
    pub timestamp: String,
    pub operation: String,
    pub provider: String,
    pub model: String,
    pub request: String,
    pub response: String,
    pub duration_ms: u64,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogsData {
    pub system: Vec<SystemLogEntry>,
    pub conversations: Vec<AiConversationLog>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModel {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_size: Option<usize>,
}
