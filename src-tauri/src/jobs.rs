use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{
    files::{FileRetryContext, FileTranslationResult},
    models::{FileJobState, FileJobStatus, FileOutputMode, FileProgress},
};

#[derive(Clone, Default)]
pub struct FileJobManager {
    inner: Arc<RwLock<HashMap<String, JobRecord>>>,
}

struct JobRecord {
    status: FileJobStatus,
    content: Option<Vec<u8>>,
    retry_context: Option<FileRetryContext>,
    cancel: CancellationToken,
    completed_contents: Vec<Vec<u8>>,
    output_directory: Option<PathBuf>,
}

impl FileJobManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create(
        &self,
        filename: &str,
        output_mode: FileOutputMode,
        source_path: Option<String>,
        retry_context: FileRetryContext,
    ) -> FileJobStatus {
        let id = job_id();
        let status = FileJobStatus {
            id: id.clone(),
            filename: filename.to_string(),
            output_mode,
            state: FileJobState::Queued,
            stage: "queued".to_string(),
            total_segments: 0,
            translated_segments: 0,
            failed_segments: 0,
            skipped_segments: 0,
            total_batches: 0,
            completed_batches: 0,
            preserved_pdf_pages: Vec::new(),
            archive_pdf_warnings: Vec::new(),
            completed_files: Vec::new(),
            output_directory: None,
            output_error: None,
            failed_batches: Vec::new(),
            streaming_batch: None,
            streaming_segments: 0,
            streaming_batch_segments: 0,
            streaming_text: None,
            result_filename: if crate::archives::is_archive(filename) {
                crate::archives::result_filename(filename)
            } else {
                translated_filename(filename)
            },
            media_type: mime_guess::from_path(filename)
                .first_or_octet_stream()
                .to_string(),
            error: None,
            created_at: unix_timestamp(),
            source_path,
            downloaded_path: None,
        };
        self.inner.write().insert(
            id,
            JobRecord {
                status: status.clone(),
                content: None,
                retry_context: Some(retry_context),
                cancel: CancellationToken::new(),
                completed_contents: Vec::new(),
                output_directory: None,
            },
        );
        status
    }

    pub fn list(&self) -> Vec<FileJobStatus> {
        let mut jobs = self
            .inner
            .read()
            .values()
            .map(|record| record.status.clone())
            .collect::<Vec<_>>();
        jobs.sort_by(|left, right| right.id.cmp(&left.id));
        jobs
    }

    pub fn get(&self, id: &str) -> Option<FileJobStatus> {
        self.inner
            .read()
            .get(id)
            .map(|record| record.status.clone())
    }

    pub fn mark_processing(&self, id: &str) {
        if let Some(record) = self.inner.write().get_mut(id) {
            if record.cancel.is_cancelled() {
                return;
            }
            record.status.state = FileJobState::Processing;
            record.status.stage = "preparing".to_string();
        }
    }

    pub fn update_progress(&self, id: &str, progress: FileProgress) {
        if let Some(record) = self.inner.write().get_mut(id) {
            if record.cancel.is_cancelled() {
                return;
            }
            record.status.state = FileJobState::Processing;
            record.status.stage = progress.stage;
            record.status.total_segments = progress.total_segments;
            record.status.translated_segments = progress.translated_segments;
            record.status.failed_segments = progress.failed_segments;
            record.status.skipped_segments = progress.skipped_segments;
            record.status.total_batches = progress.total_batches;
            record.status.completed_batches = progress.completed_batches;
            record.status.streaming_batch = progress.streaming_batch;
            record.status.streaming_segments = progress.streaming_segments;
            record.status.streaming_batch_segments = progress.streaming_batch_segments;
            record.status.streaming_text = progress.streaming_text;
            if let Some(file) = progress.completed_file {
                let index = record
                    .status
                    .completed_files
                    .iter()
                    .position(|item| item.path == file.path);
                let index = match index {
                    Some(index) => {
                        record.completed_contents[index] = file.bytes;
                        record.status.completed_files[index].revision += 1;
                        record.status.completed_files[index].resumed = file.resumed;
                        index
                    }
                    None => {
                        let id = record.completed_contents.len();
                        record.completed_contents.push(file.bytes);
                        record
                            .status
                            .completed_files
                            .push(crate::models::CompletedFileStatus {
                                id,
                                path: file.path,
                                revision: 1,
                                resumed: file.resumed,
                                saved_path: None,
                            });
                        id
                    }
                };
                if record.output_directory.is_some()
                    && !record.status.completed_files[index].resumed
                {
                    if let Err(error) = write_completed_file(record, index) {
                        record.status.output_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    pub fn complete(&self, id: &str, result: FileTranslationResult) {
        if let Some(record) = self.inner.write().get_mut(id) {
            if record.cancel.is_cancelled() {
                return;
            }
            record.status.state = FileJobState::Completed;
            record.status.stage = "completed".to_string();
            record.status.total_segments = result.total_segments;
            record.status.translated_segments = result.translated_segments;
            record.status.failed_segments = result.failed_segments;
            record.status.skipped_segments = result.skipped_segments;
            record.status.total_batches = result.total_batches;
            record.status.completed_batches = result.total_batches;
            record.status.failed_batches = result.failed_batches;
            record.status.preserved_pdf_pages = result.preserved_pdf_pages;
            record.status.archive_pdf_warnings = result.archive_pdf_warnings;
            record.status.streaming_batch = None;
            record.status.streaming_segments = 0;
            record.status.streaming_batch_segments = 0;
            record.status.streaming_text = None;
            record.status.result_filename = result.filename;
            record.status.media_type = result.media_type;
            record.status.error = None;
            record.content = Some(result.content);
            record.retry_context = if record.status.failed_batches.is_empty() {
                None
            } else {
                result.retry_context
            };
        }
    }

    pub fn begin_retry(&self, id: &str) -> Result<(FileJobStatus, FileRetryContext), String> {
        let mut jobs = self.inner.write();
        let record = jobs
            .get_mut(id)
            .ok_or_else(|| "File job was not found".to_string())?;
        let retryable = match record.status.state {
            FileJobState::Completed => !record.status.failed_batches.is_empty(),
            FileJobState::Failed => record.retry_context.is_some(),
            _ => false,
        };
        if !retryable {
            return Err("This file job has no failed batches to retry".to_string());
        }
        let context = record
            .retry_context
            .take()
            .ok_or_else(|| "Retry data for this file job is unavailable".to_string())?;
        record.status.state = FileJobState::Processing;
        record.status.stage = "translating".to_string();
        record.status.error = None;
        record.status.preserved_pdf_pages.clear();
        record.status.streaming_batch = None;
        record.status.streaming_segments = 0;
        record.status.streaming_batch_segments = 0;
        record.status.streaming_text = None;
        record.cancel = CancellationToken::new();
        Ok((record.status.clone(), context))
    }

    pub fn cancellation_token(&self, id: &str) -> Option<CancellationToken> {
        self.inner
            .read()
            .get(id)
            .map(|record| record.cancel.clone())
    }

    pub fn cancel(&self, id: &str) -> Result<FileJobStatus, String> {
        let mut jobs = self.inner.write();
        let record = jobs
            .get_mut(id)
            .ok_or_else(|| "File job was not found".to_string())?;
        if matches!(
            record.status.state,
            FileJobState::Completed | FileJobState::Failed | FileJobState::Cancelled
        ) {
            return Err("This file job cannot be cancelled".to_string());
        }
        record.cancel.cancel();
        record.status.state = FileJobState::Cancelled;
        record.status.stage = "cancelled".to_string();
        record.status.error = Some("File translation was cancelled".to_string());
        record.status.streaming_batch = None;
        record.status.streaming_segments = 0;
        record.status.streaming_batch_segments = 0;
        record.status.streaming_text = None;
        Ok(record.status.clone())
    }

    pub fn restore_retry(&self, id: &str, context: FileRetryContext, error: String) {
        if let Some(record) = self.inner.write().get_mut(id) {
            if record.cancel.is_cancelled() {
                return;
            }
            let has_result = record.content.is_some();
            record.status.state = if has_result {
                FileJobState::Completed
            } else {
                FileJobState::Failed
            };
            record.status.stage = if has_result {
                "completed".to_string()
            } else {
                "failed".to_string()
            };
            record.status.error = Some(error);
            record.retry_context = Some(context);
        }
    }

    pub fn fail(&self, id: &str, error: String) {
        if let Some(record) = self.inner.write().get_mut(id) {
            if record.cancel.is_cancelled() {
                return;
            }
            record.status.state = FileJobState::Failed;
            record.status.stage = "failed".to_string();
            record.status.error = Some(error);
        }
    }

    pub fn download(&self, id: &str) -> Option<(String, String, Vec<u8>)> {
        let jobs = self.inner.read();
        let record = jobs.get(id)?;
        if !matches!(record.status.state, FileJobState::Completed) {
            return None;
        }
        Some((
            record.status.result_filename.clone(),
            record.status.media_type.clone(),
            record.content.clone()?,
        ))
    }

    pub fn completed_file(&self, id: &str, index: usize) -> Option<(String, Vec<u8>)> {
        let jobs = self.inner.read();
        let record = jobs.get(id)?;
        Some((
            record.status.completed_files.get(index)?.path.clone(),
            record.completed_contents.get(index)?.clone(),
        ))
    }

    pub fn set_output_directory(&self, id: &str, root: &Path) -> Result<FileJobStatus, io::Error> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::other("Output location is not a directory"));
        }
        let mut jobs = self.inner.write();
        let record = jobs
            .get_mut(id)
            .ok_or_else(|| io::Error::other("File job was not found"))?;
        if !crate::archives::is_archive(&record.status.filename) {
            return Err(io::Error::other(
                "Output folders are available for folder and archive jobs",
            ));
        }
        if record.output_directory.is_none() {
            record.status.output_directory = Some(root.to_string_lossy().into_owned());
            record.output_directory = Some(root);
        }
        record.status.output_error = None;
        for index in 0..record.completed_contents.len() {
            if record.status.completed_files[index].resumed {
                continue;
            }
            if let Err(error) = write_completed_file(record, index) {
                record.status.output_error = Some(error.to_string());
                break;
            }
        }
        Ok(record.status.clone())
    }

    pub fn save_to_path(&self, id: &str, destination: &Path) -> Result<FileJobStatus, io::Error> {
        if destination.as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A destination path is required",
            ));
        }
        let content = {
            let jobs = self.inner.read();
            let record = jobs
                .get(id)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "File job was not found"))?;
            if !matches!(record.status.state, FileJobState::Completed) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "File translation is not complete",
                ));
            }
            record.content.clone().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "File content is unavailable")
            })?
        };

        if let Some(parent) = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(destination, content)?;

        let mut jobs = self.inner.write();
        let record = jobs
            .get_mut(id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "File job was not found"))?;
        record.status.downloaded_path = Some(destination.to_string_lossy().into_owned());
        Ok(record.status.clone())
    }
}

fn write_completed_file(record: &mut JobRecord, index: usize) -> Result<(), io::Error> {
    let root = record
        .output_directory
        .as_ref()
        .ok_or_else(|| io::Error::other("No output directory"))?;
    let entry = &mut record.status.completed_files[index];
    let parts = entry.path.split('/').collect::<Vec<_>>();
    if parts.iter().any(|part| {
        part.is_empty() || matches!(*part, "." | "..") || part.contains(['\\', ':', '\0'])
    }) {
        return Err(io::Error::other("Unsafe output path"));
    }
    let mut destination = root.clone();
    for part in &parts[..parts.len() - 1] {
        destination.push(part);
        match fs::create_dir(&destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&destination)?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(io::Error::other("Output path contains a link or file"));
                }
            }
            Err(error) => return Err(error),
        }
    }
    destination.push(parts[parts.len() - 1]);
    if entry.saved_path.is_some() && fs::symlink_metadata(&destination)?.file_type().is_symlink() {
        return Err(io::Error::other("Output file is a link"));
    }
    use std::io::Write;
    let mut temporary = tempfile::NamedTempFile::new_in(destination.parent().unwrap_or(root))?;
    temporary.write_all(&record.completed_contents[index])?;
    let incomplete = fs::symlink_metadata(&destination).is_ok_and(|metadata| {
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() == 0
    });
    if entry.saved_path.is_some() || incomplete {
        temporary
            .persist(&destination)
            .map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(&destination)
            .map_err(|error| error.error)?;
    }
    entry.saved_path = Some(destination.to_string_lossy().into_owned());
    Ok(())
}

fn job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("file-{nanos}-{}", std::process::id())
}

fn unix_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn translated_filename(filename: &str) -> String {
    match filename.rfind('.') {
        Some(index) if index > 0 => {
            format!("{}-translated{}", &filename[..index], &filename[index..])
        }
        _ => format!("{filename}-translated"),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::FileJobManager;
    use crate::{
        files::{FileRetryContext, FileTranslationResult},
        models::FileOutputMode,
    };

    #[test]
    fn completed_members_are_written_before_the_job_finishes_and_existing_files_are_safe() {
        let jobs = FileJobManager::new();
        let context = FileRetryContext::new(
            "folder.zip",
            vec![],
            crate::models::TranslateRequest {
                text: String::new(),
                source_language: "en".into(),
                target_language: "zh-CN".into(),
                provider_id: "test".into(),
                prompt_id: None,
                additional_prompt: String::new(),
                glossary_ids: vec![],
                reasoning_effort: "none".into(),
                summarize: false,
                context_summary: None,
            },
            FileOutputMode::Translated,
        );
        let job = jobs.create("folder.zip", FileOutputMode::Translated, None, context);
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(
            directory.path().join("nested/existing.txt"),
            b"existing translation",
        )
        .unwrap();
        jobs.set_output_directory(&job.id, directory.path())
            .unwrap();
        let progress = |path: &str, bytes: &[u8], resumed| crate::models::FileProgress {
            stage: "translating".into(),
            total_segments: 2,
            translated_segments: 1,
            failed_segments: 0,
            skipped_segments: 0,
            total_batches: 2,
            completed_batches: 1,
            streaming_batch: None,
            streaming_segments: 0,
            streaming_batch_segments: 0,
            streaming_text: None,
            completed_file: Some(crate::models::CompletedFile {
                path: path.into(),
                bytes: bytes.to_vec(),
                resumed,
            }),
        };
        jobs.update_progress(&job.id, progress("nested/new.txt", b"translation", false));
        assert!(matches!(
            jobs.get(&job.id).unwrap().state,
            crate::models::FileJobState::Processing
        ));
        assert_eq!(
            fs::read(directory.path().join("nested/new.txt")).unwrap(),
            b"translation"
        );
        assert_eq!(jobs.completed_file(&job.id, 0).unwrap().1, b"translation");
        jobs.update_progress(
            &job.id,
            progress("nested/new.txt", b"updated translation", false),
        );
        assert_eq!(
            fs::read(directory.path().join("nested/new.txt")).unwrap(),
            b"updated translation"
        );
        jobs.update_progress(
            &job.id,
            progress("nested/existing.txt", b"resumed content", true),
        );
        assert_eq!(
            fs::read(directory.path().join("nested/existing.txt")).unwrap(),
            b"existing translation"
        );
        jobs.update_progress(
            &job.id,
            progress("nested/existing.txt", b"must not overwrite", false),
        );
        assert!(jobs.get(&job.id).unwrap().output_error.is_some());
        assert_eq!(
            fs::read(directory.path().join("nested/existing.txt")).unwrap(),
            b"existing translation"
        );
        jobs.update_progress(&job.id, progress("../outside.txt", b"unsafe", false));
        assert!(jobs.get(&job.id).unwrap().output_error.is_some());
    }

    #[test]
    fn completed_file_can_be_saved_and_reports_its_path() {
        let jobs = FileJobManager::new();
        let queued = jobs.create(
            "notes.txt",
            FileOutputMode::Translated,
            Some("C:\\source\\notes.txt".to_string()),
            FileRetryContext::new(
                "notes.txt",
                b"notes".to_vec(),
                crate::models::TranslateRequest {
                    text: String::new(),
                    source_language: "en".to_string(),
                    target_language: "zh".to_string(),
                    provider_id: "provider".to_string(),
                    prompt_id: None,
                    additional_prompt: String::new(),
                    glossary_ids: Vec::new(),
                    reasoning_effort: "none".to_string(),
                    summarize: false,
                    context_summary: None,
                },
                FileOutputMode::Translated,
            ),
        );
        jobs.complete(
            &queued.id,
            FileTranslationResult {
                filename: "notes-translated.txt".to_string(),
                media_type: "text/plain".to_string(),
                content: b"translated".to_vec(),
                translated_segments: 1,
                skipped_segments: 0,
                total_segments: 1,
                total_batches: 1,
                failed_segments: 0,
                preserved_pdf_pages: Vec::new(),
                archive_pdf_warnings: Vec::new(),
                failed_batches: Vec::new(),
                retry_context: None,
            },
        );

        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("tranova-job-{suffix}"));
        let destination = root.join("nested").join("notes-translated.txt");
        let saved = jobs.save_to_path(&queued.id, &destination).unwrap();
        let destination_text = destination.to_string_lossy().into_owned();

        assert_eq!(fs::read(&destination).unwrap(), b"translated");
        assert_eq!(
            saved.downloaded_path.as_deref(),
            Some(destination_text.as_str())
        );
        assert_eq!(saved.source_path.as_deref(), Some("C:\\source\\notes.txt"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_file_can_be_retried_with_its_original_context() {
        let jobs = FileJobManager::new();
        let queued = jobs.create(
            "notes.txt",
            FileOutputMode::Translated,
            None,
            FileRetryContext::new(
                "notes.txt",
                b"notes".to_vec(),
                crate::models::TranslateRequest {
                    text: String::new(),
                    source_language: "en".to_string(),
                    target_language: "zh".to_string(),
                    provider_id: "provider".to_string(),
                    prompt_id: None,
                    additional_prompt: String::new(),
                    glossary_ids: Vec::new(),
                    reasoning_effort: "none".to_string(),
                    summarize: false,
                    context_summary: None,
                },
                FileOutputMode::Translated,
            ),
        );
        jobs.fail(&queued.id, "provider unavailable".to_string());

        let (status, _context) = jobs.begin_retry(&queued.id).unwrap();
        assert!(matches!(
            status.state,
            crate::models::FileJobState::Processing
        ));
        assert!(jobs.begin_retry(&queued.id).is_err());
    }
}
