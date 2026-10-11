use std::{io, path::PathBuf, sync::Arc};

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Semaphore};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tower_http::{
    cors::{Any, CorsLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

use crate::{
    ai, files,
    jobs::FileJobManager,
    models::{
        AppData, AppSettings, FileJobStatus, Glossary, HistoryEntry, LogsData, PromptTemplate,
        Provider, ProviderModel, ProxyMode, TranslateOptions, TranslateRequest, TranslationResult,
    },
    scheduler::{AiScheduler, Priority},
    store::AppStore,
};

#[derive(Clone)]
pub struct ServerState {
    pub store: AppStore,
    pub server_url: String,
    pub jobs: FileJobManager,
}

#[derive(Clone)]
struct ApiState {
    store: AppStore,
    server_url: String,
    scheduler: AiScheduler,
    jobs: FileJobManager,
    file_queue: Arc<Semaphore>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BootstrapResponse {
    version: String,
    providers: Vec<Provider>,
    glossaries: Vec<Glossary>,
    prompts: Vec<PromptTemplate>,
    settings: AppSettings,
    history: Vec<HistoryEntry>,
    logs: LogsData,
    server_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelDiscoveryRequest {
    base_url: String,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    proxy_mode: ProxyMode,
}

#[derive(Serialize)]
struct MessageResponse {
    message: String,
}

pub async fn start_server(
    state: ServerState,
    host: &str,
    port: u16,
    asset_dir: PathBuf,
) -> io::Result<()> {
    let app = api_router(state).fallback_service(
        ServeDir::new(&asset_dir).not_found_service(ServeFile::new(asset_dir.join("index.html"))),
    );
    let listener = tokio::net::TcpListener::bind((host, port)).await?;
    axum::serve(listener, app).await.map_err(io::Error::other)
}

fn api_router(state: ServerState) -> Router {
    let origins = [
        HeaderValue::from_static("http://localhost:1420"),
        HeaderValue::from_static("http://127.0.0.1:1420"),
        HeaderValue::from_static("http://tauri.localhost"),
        HeaderValue::from_static("https://tauri.localhost"),
        HeaderValue::from_static("tauri://localhost"),
    ];
    let store = state.store.clone();
    let api_state = ApiState {
        store: store.clone(),
        server_url: state.server_url,
        scheduler: AiScheduler::new(store),
        jobs: state.jobs,
        file_queue: Arc::new(Semaphore::new(1)),
    };
    Router::new()
        .route("/api/health", get(health))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/translate", post(translate))
        .route("/api/translate-stream", post(translate_stream))
        .route("/api/translate-file", post(translate_file))
        .route("/api/file-jobs", get(file_jobs))
        .route("/api/file-jobs/{id}", get(file_job))
        .route("/api/file-jobs/{id}/cancel", post(cancel_file_job))
        .route("/api/file-jobs/{id}/download", get(download_file_job))
        .route("/api/file-jobs/{id}/retry", post(retry_file_job))
        .route("/api/history", get(history).delete(clear_history))
        .route("/api/history/{id}", delete(delete_history))
        .route("/api/providers", put(save_provider))
        .route("/api/providers/{id}", delete(delete_provider))
        .route("/api/providers/{id}/test", post(test_provider))
        .route("/api/providers/models", post(discover_models))
        .route("/api/glossaries", put(save_glossary))
        .route("/api/glossaries/{id}", delete(delete_glossary))
        .route("/api/prompts", put(save_prompt))
        .route("/api/prompts/{id}", delete(delete_prompt))
        .route("/api/settings", put(save_settings))
        .route("/api/logs", get(logs))
        .route("/api/logs/{kind}", delete(clear_logs))
        .route("/api/logs/{kind}/{id}", delete(delete_log))
        .route("/api/import/{kind}", post(import_data))
        .layer(DefaultBodyLimit::max(75 * 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
                .allow_headers(Any),
        )
        .with_state(api_state)
}

async fn health() -> Json<MessageResponse> {
    Json(MessageResponse {
        message: "ok".to_string(),
    })
}

async fn bootstrap(State(state): State<ApiState>) -> Json<BootstrapResponse> {
    let AppData {
        providers,
        glossaries,
        prompts,
        settings,
        history,
        ..
    } = state.store.snapshot();
    let (system, conversations) = state.store.logs();
    Json(BootstrapResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        providers,
        glossaries,
        prompts,
        settings,
        history,
        logs: LogsData {
            system,
            conversations,
        },
        server_url: state.server_url,
    })
}

async fn translate(
    State(state): State<ApiState>,
    Json(request): Json<TranslateRequest>,
) -> Result<Json<TranslationResult>, ApiError> {
    let result = state
        .scheduler
        .translate(request.clone(), Priority::High)
        .await
        .map_err(ApiError::from_ai)?;
    state
        .store
        .add_history(history_for_text(&request, &result))
        .map_err(ApiError::from_io)?;
    Ok(Json(result))
}

async fn translate_stream(
    State(state): State<ApiState>,
    Json(request): Json<TranslateRequest>,
) -> impl IntoResponse {
    let (sender, receiver) = mpsc::unbounded_channel::<Result<Event, std::convert::Infallible>>();
    let scheduler = state.scheduler.clone();
    let store = state.store.clone();
    tokio::spawn(async move {
        let stream_sender = sender.clone();
        let callback: ai::StreamCallback = Arc::new(move |text| {
            let payload = serde_json::json!({ "text": text });
            if let Ok(data) = serde_json::to_string(&payload) {
                let _ = stream_sender.send(Ok(Event::default().event("delta").data(data)));
            }
        });
        match scheduler
            .translate_with_stream(request.clone(), Priority::High, Some(callback))
            .await
        {
            Ok(result) => {
                if let Err(error) = store.add_history(history_for_text(&request, &result)) {
                    let payload = serde_json::json!({ "error": error.to_string() });
                    let _ = sender.send(Ok(Event::default()
                        .event("error")
                        .data(payload.to_string())));
                } else {
                    let payload = serde_json::json!({ "result": result });
                    let _ =
                        sender.send(Ok(Event::default().event("done").data(payload.to_string())));
                }
            }
            Err(error) => {
                let payload = serde_json::json!({ "error": error.to_string() });
                let _ = sender.send(Ok(Event::default()
                    .event("error")
                    .data(payload.to_string())));
            }
        }
    });
    Sse::new(UnboundedReceiverStream::new(receiver)).keep_alive(KeepAlive::default())
}

async fn translate_file(
    State(state): State<ApiState>,
    mut multipart: Multipart,
) -> Result<Json<FileJobStatus>, ApiError> {
    let mut file: Option<(String, Vec<u8>)> = None;
    let mut options: Option<TranslateOptions> = None;
    let mut source_path: Option<String> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(ApiError::bad_request)?
    {
        match field.name() {
            Some("file") => {
                let name = field
                    .file_name()
                    .unwrap_or("document")
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("document")
                    .to_string();
                let bytes = field.bytes().await.map_err(ApiError::bad_request)?.to_vec();
                file = Some((name, bytes));
            }
            Some("options") => {
                let text = field.text().await.map_err(ApiError::bad_request)?;
                options = Some(serde_json::from_str(&text).map_err(ApiError::bad_request)?);
            }
            Some("sourcePath") => {
                source_path = Some(field.text().await.map_err(ApiError::bad_request)?);
            }
            _ => {}
        }
    }
    let (filename, bytes) = file.ok_or_else(|| ApiError::bad_request("A file is required"))?;
    let options =
        options.ok_or_else(|| ApiError::bad_request("Translation options are required"))?;
    let output_mode = options.output_mode;
    let request = options.into_request();
    let retry_context =
        files::FileRetryContext::new(&filename, bytes.clone(), request.clone(), output_mode);
    let status = state
        .jobs
        .create(&filename, output_mode, source_path, retry_context);
    let job_id = status.id.clone();
    state.store.add_system_log(
        "info",
        "file",
        format!("Queued file job {} for {}", job_id, filename),
    );
    let jobs = state.jobs.clone();
    let store = state.store.clone();
    let scheduler = state.scheduler.clone();
    let file_queue = state.file_queue.clone();
    tokio::spawn(async move {
        let cancel = match jobs.cancellation_token(&job_id) {
            Some(cancel) => cancel,
            None => return,
        };
        let permit = tokio::select! {
            permit = file_queue.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    jobs.fail(&job_id, "File translation queue is unavailable".to_string());
                    store.add_system_log("error", "file", format!("File queue unavailable for job {job_id}"));
                    return;
                }
            },
            _ = cancel.cancelled() => return,
        };
        jobs.mark_processing(&job_id);
        store.add_system_log("debug", "file", format!("Started file job {job_id}"));
        let progress_jobs = jobs.clone();
        let progress_job_id = job_id.clone();
        let progress: files::ProgressCallback = Arc::new(move |value| {
            progress_jobs.update_progress(&progress_job_id, value);
        });
        match files::translate_file_with_cancel(
            &store,
            &scheduler,
            &filename,
            bytes,
            request.clone(),
            output_mode,
            progress,
            cancel.clone(),
        )
        .await
        {
            Ok(result) => {
                let history_result = history_for_file(&request, &result.filename);
                if let Err(error) = store.add_history(history_result) {
                    jobs.fail(&job_id, error.to_string());
                    store.add_system_log(
                        "error",
                        "file",
                        format!("Could not save history for file job {job_id}: {error}"),
                    );
                } else {
                    jobs.complete(&job_id, result);
                    store.add_system_log("info", "file", format!("Completed file job {job_id}"));
                }
            }
            Err(files::FileError::Cancelled) => {
                let _ = jobs.cancel(&job_id);
            }
            Err(error) => {
                let message = error.to_string();
                jobs.fail(&job_id, message.clone());
                store.add_system_log(
                    "error",
                    "file",
                    format!("File job {job_id} failed: {message}"),
                );
            }
        }
        drop(permit);
    });
    Ok(Json(status))
}

async fn file_jobs(State(state): State<ApiState>) -> Json<Vec<FileJobStatus>> {
    Json(state.jobs.list())
}

async fn file_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<FileJobStatus>, ApiError> {
    state
        .jobs
        .get(&id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("File job was not found"))
}

async fn cancel_file_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<FileJobStatus>, ApiError> {
    let status = state.jobs.cancel(&id).map_err(ApiError::bad_request)?;
    state
        .store
        .add_system_log("info", "file", format!("Cancelled file job {id}"));
    Ok(Json(status))
}

async fn retry_file_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<FileJobStatus>, ApiError> {
    let (status, context) = state.jobs.begin_retry(&id).map_err(ApiError::bad_request)?;
    state.store.add_system_log(
        "info",
        "file",
        format!("Retrying failed batches for file job {id}"),
    );
    let jobs = state.jobs.clone();
    let store = state.store.clone();
    let scheduler = state.scheduler.clone();
    let file_queue = state.file_queue.clone();
    let restore_context = context.clone();
    let queue_restore_context = restore_context.clone();
    let job_id = id.clone();
    tokio::spawn(async move {
        let cancel = match jobs.cancellation_token(&job_id) {
            Some(cancel) => cancel,
            None => return,
        };
        let permit = tokio::select! {
            permit = file_queue.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    jobs.restore_retry(
                        &job_id,
                        queue_restore_context,
                        "File translation queue is unavailable".to_string(),
                    );
                    return;
                }
            },
            _ = cancel.cancelled() => return,
        };
        let progress_jobs = jobs.clone();
        let progress_job_id = job_id.clone();
        let progress: files::ProgressCallback = Arc::new(move |value| {
            progress_jobs.update_progress(&progress_job_id, value);
        });
        match files::retry_file_with_cancel(&store, &scheduler, context, progress, cancel).await {
            Ok(result) => jobs.complete(&job_id, result),
            Err(files::FileError::Cancelled) => {
                let _ = jobs.cancel(&job_id);
            }
            Err(error) => {
                let message = error.to_string();
                jobs.restore_retry(&job_id, restore_context, message.clone());
                store.add_system_log(
                    "error",
                    "file",
                    format!("Retry failed for file job {job_id}: {message}"),
                );
            }
        }
        drop(permit);
    });
    Ok(Json(status))
}

async fn download_file_job(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let (filename, media_type, content) = state
        .jobs
        .download(&id)
        .ok_or_else(|| ApiError::bad_request("File translation is not complete"))?;
    let mut response = content.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&media_type).map_err(ApiError::internal)?,
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(&filename)).map_err(ApiError::internal)?,
    );
    Ok(response)
}

fn content_disposition(filename: &str) -> String {
    let fallback = filename
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        if fallback.is_empty() {
            "translation"
        } else {
            &fallback
        },
        percent_encode(filename.as_bytes())
    )
}

fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    encoded
}

async fn delete_history(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.store.delete_history(&id).map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_history(State(state): State<ApiState>) -> Result<StatusCode, ApiError> {
    state.store.clear_history().map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn history(State(state): State<ApiState>) -> Json<Vec<HistoryEntry>> {
    Json(state.store.snapshot().history)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LogsQuery {
    kind: String,
    #[serde(default = "default_page")]
    page: usize,
    #[serde(default = "default_page_size")]
    page_size: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LogPage<T> {
    items: Vec<T>,
    page: usize,
    page_size: usize,
    total: usize,
    total_pages: usize,
}

fn default_page() -> usize {
    1
}
fn default_page_size() -> usize {
    20
}

async fn logs(
    State(state): State<ApiState>,
    Query(query): Query<LogsQuery>,
) -> Result<Response, ApiError> {
    let page = query.page.max(1);
    let page_size = query.page_size.clamp(1, 100);
    let response = match query.kind.as_str() {
        "system" => {
            let (items, total) = state.store.system_logs_page(page, page_size);
            Json(log_page(items, page, page_size, total)).into_response()
        }
        "conversations" => {
            let (items, total) = state.store.ai_conversations_page(page, page_size);
            Json(log_page(items, page, page_size, total)).into_response()
        }
        _ => return Err(ApiError::bad_request("Unknown log kind")),
    };
    Ok(response)
}

fn log_page<T>(items: Vec<T>, page: usize, page_size: usize, total: usize) -> LogPage<T> {
    LogPage {
        items,
        page,
        page_size,
        total,
        total_pages: total.div_ceil(page_size),
    }
}

#[derive(Deserialize)]
struct ClearLogsQuery {
    before: Option<u64>,
}

async fn clear_logs(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Query(query): Query<ClearLogsQuery>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .clear_logs(&kind, query.before)
        .map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_log(
    State(state): State<ApiState>,
    Path((kind, id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .delete_log(&kind, &id)
        .map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

fn history_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", timestamp, std::process::id())
}

fn history_for_text(request: &TranslateRequest, result: &TranslationResult) -> HistoryEntry {
    HistoryEntry {
        id: history_id(),
        kind: "text".to_string(),
        source_language: request.source_language.clone(),
        target_language: request.target_language.clone(),
        source_text: request.text.clone(),
        translated_text: result.translated_text.clone(),
        filename: None,
        provider: result.provider.clone(),
        model: result.model.clone(),
        created_at: unix_timestamp(),
    }
}

fn history_for_file(request: &TranslateRequest, filename: &str) -> HistoryEntry {
    HistoryEntry {
        id: history_id(),
        kind: "file".to_string(),
        source_language: request.source_language.clone(),
        target_language: request.target_language.clone(),
        source_text: String::new(),
        translated_text: String::new(),
        filename: Some(filename.to_string()),
        provider: request.provider_id.clone(),
        model: String::new(),
        created_at: unix_timestamp(),
    }
}

fn unix_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{seconds}")
}

async fn save_provider(
    State(state): State<ApiState>,
    Json(provider): Json<Provider>,
) -> Result<Json<Provider>, ApiError> {
    Ok(Json(
        state
            .store
            .save_provider(provider)
            .map_err(ApiError::from_io)?,
    ))
}

async fn delete_provider(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .delete_provider(&id)
        .map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_provider(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<Json<MessageResponse>, ApiError> {
    let provider = state
        .store
        .snapshot()
        .providers
        .into_iter()
        .find(|provider| provider.id == id)
        .ok_or_else(|| ApiError::bad_request("Provider was not found"))?;
    let request = TranslateRequest {
        text: "Reply with OK only.".to_string(),
        source_language: "English".to_string(),
        target_language: "English".to_string(),
        provider_id: id,
        prompt_id: None,
        additional_prompt: String::new(),
        glossary_ids: Vec::new(),
        reasoning_effort: "none".to_string(),
        summarize: false,
        context_summary: None,
    };
    state
        .scheduler
        .translate(request, Priority::High)
        .await
        .map_err(ApiError::from_ai)?;
    Ok(Json(MessageResponse {
        message: format!("{} responded successfully", provider.name),
    }))
}

async fn discover_models(
    State(state): State<ApiState>,
    Json(request): Json<ModelDiscoveryRequest>,
) -> Result<Json<Vec<ProviderModel>>, ApiError> {
    if request.base_url.trim().is_empty() {
        return Err(ApiError::bad_request(
            "An OpenAI-compatible API base URL is required",
        ));
    }
    let settings = state.store.settings();
    let models = ai::list_models(
        &request.base_url,
        &request.api_key,
        request.proxy_mode,
        &settings.proxy_url,
        settings.ai_timeout_seconds,
    )
    .await
    .map_err(ApiError::from_ai)?;
    state.store.add_system_log(
        "info",
        "provider",
        format!("Discovered {} models", models.len()),
    );
    Ok(Json(models))
}

async fn save_glossary(
    State(state): State<ApiState>,
    Json(glossary): Json<Glossary>,
) -> Result<Json<Glossary>, ApiError> {
    Ok(Json(
        state
            .store
            .save_glossary(glossary)
            .map_err(ApiError::from_io)?,
    ))
}

async fn delete_glossary(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .store
        .delete_glossary(&id)
        .map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_prompt(
    State(state): State<ApiState>,
    Json(prompt): Json<PromptTemplate>,
) -> Result<Json<PromptTemplate>, ApiError> {
    Ok(Json(
        state.store.save_prompt(prompt).map_err(ApiError::from_io)?,
    ))
}

async fn delete_prompt(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.store.delete_prompt(&id).map_err(ApiError::from_io)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_settings(
    State(state): State<ApiState>,
    Json(settings): Json<AppSettings>,
) -> Result<Json<AppSettings>, ApiError> {
    let settings = state
        .store
        .save_settings(settings)
        .map_err(ApiError::from_io)?;
    state
        .store
        .add_system_log("info", "settings", "Settings updated");
    Ok(Json(settings))
}

async fn import_data(
    State(state): State<ApiState>,
    Path(kind): Path<String>,
    Json(body): Json<Value>,
) -> Result<Json<BootstrapResponse>, ApiError> {
    match kind.as_str() {
        "glossaries" => state
            .store
            .replace_glossaries(read_import(body, "glossaries")?)
            .map_err(ApiError::from_io)?,
        "prompts" => state
            .store
            .replace_prompts(read_import(body, "prompts")?)
            .map_err(ApiError::from_io)?,
        _ => return Err(ApiError::bad_request("Unknown import type")),
    }
    let AppData {
        providers,
        glossaries,
        prompts,
        settings,
        history,
        ..
    } = state.store.snapshot();
    Ok(Json(BootstrapResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        providers,
        glossaries,
        prompts,
        settings,
        history,
        logs: {
            let (system, conversations) = state.store.logs();
            LogsData {
                system,
                conversations,
            }
        },
        server_url: state.server_url,
    }))
}

fn read_import<T: serde::de::DeserializeOwned>(
    body: Value,
    field: &str,
) -> Result<Vec<T>, ApiError> {
    let value = match body {
        Value::Array(_) => body,
        Value::Object(mut object) => object
            .remove(field)
            .ok_or_else(|| ApiError::bad_request(format!("Expected a '{field}' array")))?,
        _ => {
            return Err(ApiError::bad_request(
                "Expected a JSON array or export object",
            ))
        }
    };
    serde_json::from_value(value).map_err(ApiError::bad_request)
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }
    fn not_found(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: error.to_string(),
        }
    }
    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
    fn from_io(error: std::io::Error) -> Self {
        let status = if error.kind() == std::io::ErrorKind::InvalidInput {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
    fn from_ai(error: ai::AiError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}
