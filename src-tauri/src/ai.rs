use std::{
    error::Error as StdError,
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use reqwest::{
    header::{HeaderMap, CONTENT_TYPE},
    Client, Proxy, StatusCode,
};
use serde_json::{json, Value};
use thiserror::Error;

use crate::{
    models::{
        AiConversationLog, AppData, Provider, ProviderModel, ProxyMode, ReasoningParser,
        TranslateRequest, TranslationResult,
    },
    store::AppStore,
};

#[derive(Debug, Error)]
pub enum AiError {
    #[error("{0}")]
    Message(String),
    #[error("Network request failed: {0}")]
    Request(String),
    #[error("AI response format error: {0}")]
    BatchFormat(String),
}

pub type StreamCallback = Arc<dyn Fn(String) + Send + Sync>;

pub const SEGMENT_SEPARATOR: &str = "\n---TRANOVA-SEGMENT-7F3A---\n";
const MAX_DOCUMENT_SUMMARY_CHARS: usize = 512;
const MARTIAN_LANGUAGE_DESCRIPTION: &str = r#"**火星文**是一种非标准中文网络文字，通过替换、拆分、混合字符等方式改变文字外观，通常仍可根据读音、字形和上下文理解原意。

**主要特征：**
1. **繁体与异体字**：使用繁体字、生僻字、形近字替代原字。
2. **谐音与错别字**：使用同音、近音字替换，如“偶”代替“我”。
3. **拆字与混合字符**：混用汉字、拼音、数字、字母、注音、日文、韩文等。
4. **特殊符号**：插入数学符号、箭头、颜文字及特殊字符，如“の”“★”“囧”。

**示例：**
- 我喜欢你 → 莪囍歡伱
- 你在干什么 → 伱恠幹什庅
- 不知道 → 卟知噵

**识别原则：** 结合谐音、字形和上下文还原原意。火星文无统一规则，单独使用繁体字或网络用语不一定属于火星文。"#;

pub fn contains_segment_separator(text: &str) -> bool {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .contains(SEGMENT_SEPARATOR)
}
const AI_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REQUEST_ATTEMPTS: usize = 3;

struct ProviderResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

pub async fn translate(
    store: &AppStore,
    request: &TranslateRequest,
    stream: Option<StreamCallback>,
) -> Result<TranslationResult, AiError> {
    if request.text.trim().is_empty() {
        return Err(AiError::Message("Text to translate is empty".to_string()));
    }
    validate_languages(request)?;
    let data = store.snapshot();
    let provider = enabled_provider(&data, &request.provider_id)?;
    let translated_text = translate_with_data_instruction(
        store,
        &data,
        &provider,
        request,
        None,
        stream,
        "translation",
    )
    .await?;
    Ok(TranslationResult {
        translated_text,
        provider: provider.name,
        model: provider.model,
    })
}

pub async fn translate_batch(
    store: &AppStore,
    request: &TranslateRequest,
    texts: &[String],
    stream: Option<StreamCallback>,
) -> Result<Vec<String>, AiError> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    validate_languages(request)?;
    if texts.iter().any(|text| contains_segment_separator(text)) {
        return Err(AiError::BatchFormat(
            "A source segment contains the batch separator".to_string(),
        ));
    }
    let data = store.snapshot();
    let provider = enabled_provider(&data, &request.provider_id)?;
    let mut batch_request = request.clone();
    batch_request.text = texts.join(SEGMENT_SEPARATOR);
    let instruction = format!(
        "The user input contains {} independent segments separated by the exact delimiter below. Translate every segment in order. Return only the translated segments in the same order, separated by the exact delimiter. Do not add labels, numbering, markdown or commentary.\n\nDELIMITER:\n{}",
        texts.len(), SEGMENT_SEPARATOR
    );
    let response = translate_with_data_instruction(
        store,
        &data,
        &provider,
        &batch_request,
        Some(&instruction),
        stream,
        "batch translation",
    )
    .await?;
    parse_batch_response(&response, texts.len())
}

pub async fn summarize(
    store: &AppStore,
    request: &TranslateRequest,
    text: &str,
) -> Result<String, AiError> {
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    validate_languages(request)?;
    let data = store.snapshot();
    let provider = enabled_provider(&data, &request.provider_id)?;
    let summary_request = document_summary_request(request, text);
    let instruction = "Identify the source document's main topic and domain for a translator. Mention only the most important proper nouns, specialized terms, dates, numbers or references needed to understand the text. Keep it very short: this is a topic note, not a full summary. Do not translate the document, add a preamble, or include general commentary; return only the note.";
    let summary = translate_with_data_instruction(
        store,
        &data,
        &provider,
        &summary_request,
        Some(instruction),
        None,
        "document summary",
    )
    .await?;
    Ok(truncate_document_summary(&summary))
}

fn document_summary_request(request: &TranslateRequest, text: &str) -> TranslateRequest {
    let mut summary_request = request.clone();
    summary_request.text = text.to_string();
    summary_request.context_summary = None;
    summary_request
}

fn truncate_document_summary(summary: &str) -> String {
    let summary = summary.trim();
    if summary.chars().count() <= MAX_DOCUMENT_SUMMARY_CHARS {
        return summary.to_string();
    }

    let mut truncated = summary
        .chars()
        .take(MAX_DOCUMENT_SUMMARY_CHARS)
        .collect::<String>();
    if let Some(index) = truncated.rfind(|character: char| character.is_whitespace()) {
        truncated.truncate(index);
    }
    truncated.push_str("...");
    truncated
}

pub async fn translate_image(
    store: &AppStore,
    request: &TranslateRequest,
    filename: &str,
    image: Vec<u8>,
) -> Result<Vec<u8>, AiError> {
    let data = store.snapshot();
    let provider = enabled_provider(&data, &request.provider_id)?;
    if !provider.supports_images {
        return Err(AiError::Message(
            "Selected provider does not support image input/output".to_string(),
        ));
    }
    let prompt = if request.prompt_id.as_deref().is_none_or(str::is_empty) {
        request.additional_prompt.trim().to_string()
    } else {
        let mut instruction = format!(
        "Translate every readable text element in this image from {} to {}. Preserve all non-text pixels, composition, layout, visual style, font scale and colors. Replace the original text with the translation and return the edited image only.",
        request.source_language, request.target_language
        );
        instruction.push_str("\n\n");
        instruction.push_str(&build_messages(&data, request).0);
        instruction
    };
    let mime = mime_guess::from_path(filename)
        .first_or_octet_stream()
        .to_string();
    let output_format = image_output_format(filename)?;
    let image_data_url = format!("data:{mime};base64,{}", BASE64.encode(&image));
    let body = json!({
        "model": provider.model,
        "input": [{
            "role": "user",
            "content": [
                { "type": "input_text", "text": prompt },
                { "type": "input_image", "image_url": image_data_url }
            ]
        }],
        "tools": [{ "type": "image_generation", "output_format": output_format }],
        "stream": false
    });
    let request_for_log = format!(
        "IMAGE: {filename} ({mime}, {} bytes)\n\nINSTRUCTION:\n{prompt}",
        image.len()
    );
    let started = Instant::now();
    let result = async {
        let client = build_client(
            provider.proxy_mode,
            &data.settings.proxy_url,
            data.settings.ai_timeout_seconds,
        )?;
        let response = send_responses_json(
            &client,
            &endpoint(&provider.base_url, "/responses"),
            &body,
            (!provider.api_key.trim().is_empty()).then_some(provider.api_key.trim()),
        )
        .await?;
        let decoded = decode_responses_image(&response)?;
        if !image_format_matches(filename, &decoded) {
            return Err(AiError::Message(format!(
                "Image provider did not return the requested {output_format} format"
            )));
        }
        Ok(decoded)
    }
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(decoded) => {
            let response_summary = format!("image_generation_call: {} bytes", decoded.len());
            record_ai_log(
                store,
                &provider,
                "image translation",
                request_for_log,
                &response_summary,
                duration_ms,
                &"",
            );
            Ok(decoded)
        }
        Err(error) => {
            record_ai_log(
                store,
                &provider,
                "image translation",
                request_for_log,
                "",
                duration_ms,
                &error,
            );
            Err(error)
        }
    }
}

pub async fn list_models(
    base_url: &str,
    api_key: &str,
    proxy_mode: ProxyMode,
    proxy_url: &str,
    timeout_seconds: u64,
) -> Result<Vec<ProviderModel>, AiError> {
    let client = build_client(proxy_mode, proxy_url, timeout_seconds)?;
    let mut call = client.get(endpoint(base_url, "/models"));
    if !api_key.trim().is_empty() {
        call = call.bearer_auth(api_key.trim());
    }
    let response = call
        .send()
        .await
        .map_err(|error| request_error("model discovery request", error))?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .text()
        .await
        .map_err(|error| request_error("reading model discovery response", error))?;
    let value = parse_provider_json(
        ProviderResponse {
            status,
            headers,
            body,
        },
        "OpenAI-compatible provider",
    )?;
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            AiError::Message("Model discovery response did not contain data".to_string())
        })?
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(Value::as_str)?.trim();
            if id.is_empty() {
                return None;
            }
            let context_size = ["context_length", "context_window", "max_model_len"]
                .iter()
                .find_map(|key| {
                    item.get(*key)
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                })
                .or_else(|| {
                    item.get("metadata")
                        .and_then(|meta| meta.get("context_length"))
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                });
            Some(ProviderModel {
                id: id.to_string(),
                context_size,
            })
        })
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err(AiError::Message(
            "The provider returned no usable models".to_string(),
        ));
    }
    Ok(models)
}

fn validate_languages(request: &TranslateRequest) -> Result<(), AiError> {
    if request.source_language.trim().is_empty() || request.target_language.trim().is_empty() {
        return Err(AiError::Message(
            "Source and target languages are required".to_string(),
        ));
    }
    Ok(())
}

fn enabled_provider(data: &AppData, id: &str) -> Result<Provider, AiError> {
    data.providers
        .iter()
        .find(|provider| provider.id == id)
        .filter(|provider| provider.enabled)
        .cloned()
        .ok_or_else(|| {
            AiError::Message("Selected AI provider does not exist or is disabled".to_string())
        })
}

#[allow(clippy::too_many_arguments)]
async fn translate_with_data_instruction(
    store: &AppStore,
    data: &AppData,
    provider: &Provider,
    request: &TranslateRequest,
    extra_system_instruction: Option<&str>,
    stream: Option<StreamCallback>,
    operation: &str,
) -> Result<String, AiError> {
    let (mut system, user) = build_messages(data, request);
    if let Some(instruction) = extra_system_instruction {
        system.push_str("\n\n");
        system.push_str(instruction);
    }
    let started = Instant::now();
    let request_for_log = format!("SYSTEM:\n{system}\n\nUSER:\n{user}");
    let response = call_text_model(
        data,
        provider,
        &system,
        &user,
        request.reasoning_effort.as_str(),
        stream,
    )
    .await;
    let duration_ms = started.elapsed().as_millis() as u64;
    match response {
        Ok(response) => {
            let response = response.trim().to_string();
            if response.is_empty() {
                let error =
                    AiError::Message("The AI provider returned an empty response".to_string());
                record_ai_log(
                    store,
                    provider,
                    operation,
                    request_for_log,
                    "",
                    duration_ms,
                    &error,
                );
                return Err(error);
            }
            record_ai_log(
                store,
                provider,
                operation,
                request_for_log,
                &response,
                duration_ms,
                &"",
            );
            Ok(response)
        }
        Err(error) => {
            record_ai_log(
                store,
                provider,
                operation,
                request_for_log,
                "",
                duration_ms,
                &error,
            );
            Err(error)
        }
    }
}

fn record_ai_log<E: std::fmt::Display>(
    store: &AppStore,
    provider: &Provider,
    operation: &str,
    request: String,
    response: &str,
    duration_ms: u64,
    result: &E,
) {
    let error_text = result.to_string();
    let success = error_text.is_empty() || error_text == "()";
    store.add_ai_conversation(AiConversationLog {
        id: format!("ai-{}-{}", unix_timestamp_nanos(), std::process::id()),
        timestamp: unix_timestamp_millis(),
        operation: operation.to_string(),
        provider: provider.name.clone(),
        model: provider.model.clone(),
        request,
        response: response.to_string(),
        duration_ms,
        success,
        error: success.then_some(None).flatten().or(Some(error_text)),
    });
}

fn append_language_description(prompt: &mut String, request: &TranslateRequest) {
    if request.source_language.trim() == "火星文" || request.target_language.trim() == "火星文"
    {
        prompt.push_str("\n\n");
        prompt.push_str(MARTIAN_LANGUAGE_DESCRIPTION);
    }
}

fn build_messages(data: &AppData, request: &TranslateRequest) -> (String, String) {
    if request.prompt_id.as_deref().is_none_or(str::is_empty) {
        return (
            request.additional_prompt.trim().to_string(),
            request.text.clone(),
        );
    }
    let glossary = data
        .glossaries
        .iter()
        .filter(|glossary| request.glossary_ids.contains(&glossary.id))
        .flat_map(|glossary| glossary.entries.iter())
        .map(
            |entry| match entry.note.as_deref().filter(|note| !note.trim().is_empty()) {
                Some(note) => format!("- {} => {} ({note})", entry.source, entry.target),
                None => format!("- {} => {}", entry.source, entry.target),
            },
        )
        .collect::<Vec<_>>()
        .join("\n");
    let template = request
        .prompt_id
        .as_ref()
        .and_then(|id| data.prompts.iter().find(|prompt| &prompt.id == id))
        .map(|prompt| prompt.content.as_str())
        .unwrap_or("Translate from {{source_language}} to {{target_language}}. Preserve the original meaning, formatting and markup. Return only the translation.\n\n{{glossary}}");
    let render = |input: &str| {
        input
            .replace("{{source_language}}", &request.source_language)
            .replace("{{target_language}}", &request.target_language)
            .replace(
                "{{glossary}}",
                if glossary.is_empty() {
                    "No special terminology."
                } else {
                    &glossary
                },
            )
    };
    let rendered = render(template);
    let has_text_slot = rendered.contains("{{text}}");
    let mut system = if has_text_slot {
        rendered.replace("{{text}}", "")
    } else {
        rendered
    };
    append_language_description(&mut system, request);
    if !request.additional_prompt.trim().is_empty() {
        system.push_str("\n\nAdditional instructions:\n");
        system.push_str(request.additional_prompt.trim());
    }
    if let Some(summary) = request
        .context_summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        system.push_str("\n\nTranslation context summary:\n");
        system.push_str(summary);
    }
    let user = if has_text_slot {
        render(template).replace("{{text}}", &request.text)
    } else {
        format!("Text to translate:\n{}", request.text)
    };
    (system, user)
}

async fn call_text_model(
    data: &AppData,
    provider: &Provider,
    system: &str,
    user: &str,
    reasoning_effort: &str,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let parser = resolve_reasoning_parser(provider);
    if parser == ReasoningParser::Openai {
        call_responses(data, provider, system, user, reasoning_effort, stream).await
    } else {
        call_chat_completions(
            data,
            provider,
            parser,
            system,
            user,
            reasoning_effort,
            stream,
        )
        .await
    }
}

fn resolve_reasoning_parser(provider: &Provider) -> ReasoningParser {
    if provider.reasoning_parser != ReasoningParser::Auto {
        return provider.reasoning_parser;
    }
    infer_reasoning_parser(&provider.model)
}

fn infer_reasoning_parser(model: &str) -> ReasoningParser {
    let model = model.to_ascii_lowercase();
    if model.contains("qwq")
        || model.contains("deepseek-r1")
        || model.contains("deepseek_r1")
        || model.contains("deepseek-reasoner")
    {
        ReasoningParser::DeepseekR1
    } else if model.contains("qwen3")
        || model.contains("qwen-3.")
        || model.contains("qwen-3-")
        || model.ends_with("qwen-3")
    {
        ReasoningParser::Qwen3
    } else if model.contains("deepseek-v3") || model.contains("deepseek_v3") {
        ReasoningParser::DeepseekV3
    } else if model.contains("glm-4.5") || model.contains("glm4.5") {
        ReasoningParser::Glm45
    } else if model.contains("gemma-4") || model.contains("gemma4") {
        ReasoningParser::Gemma4
    } else if model.contains("granite-3.2") || model.contains("granite_3.2") {
        ReasoningParser::Granite
    } else {
        ReasoningParser::Openai
    }
}

fn normalized_reasoning_effort(reasoning_effort: &str) -> &str {
    match reasoning_effort {
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        _ => "none",
    }
}

fn thinking_token_budget(reasoning_effort: &str, context_size: usize) -> Option<usize> {
    let desired = match reasoning_effort {
        "low" => 1_024,
        "medium" => 4_096,
        "high" => 16_384,
        _ => return None,
    };
    Some(desired.min((context_size / 2).max(256)))
}

fn build_chat_completions_body(
    provider: &Provider,
    parser: ReasoningParser,
    system: &str,
    user: &str,
    reasoning_effort: &str,
) -> Value {
    let effort = normalized_reasoning_effort(reasoning_effort);
    let thinking = effort != "none";
    let mut body = json!({
        "model": provider.model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user }
        ],
        "reasoning_effort": effort,
        "stream": true,
    });
    match parser {
        ReasoningParser::Qwen3 | ReasoningParser::Gemma4 => {
            body["chat_template_kwargs"] = json!({ "enable_thinking": thinking });
        }
        ReasoningParser::DeepseekV3 | ReasoningParser::Granite => {
            body["chat_template_kwargs"] = json!({ "thinking": thinking });
        }
        _ => {}
    }
    if matches!(
        parser,
        ReasoningParser::Qwen3 | ReasoningParser::DeepseekR1 | ReasoningParser::DeepseekV3
    ) {
        if let Some(budget) = thinking_token_budget(effort, provider.context_size) {
            body["thinking_token_budget"] = json!(budget);
        }
    }
    body
}

async fn call_chat_completions(
    data: &AppData,
    provider: &Provider,
    parser: ReasoningParser,
    system: &str,
    user: &str,
    reasoning_effort: &str,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let body = build_chat_completions_body(provider, parser, system, user, reasoning_effort);
    let client = build_streaming_client(
        provider.proxy_mode,
        &data.settings.proxy_url,
        data.settings.ai_timeout_seconds,
    )?;
    let stream = stream.map(|callback| {
        Arc::new(move |content: String| callback(strip_reasoning_markup(&content)))
            as StreamCallback
    });
    let content = send_chat_completions_request(
        &client,
        &endpoint(&provider.base_url, "/chat/completions"),
        &body,
        (!provider.api_key.trim().is_empty()).then_some(provider.api_key.trim()),
        stream,
    )
    .await?;
    Ok(strip_reasoning_markup(&content))
}

fn strip_reasoning_markup(content: &str) -> String {
    if let Some((_, answer)) = content.rsplit_once("</think>") {
        answer.trim_start().to_string()
    } else if content.contains("<think>") {
        String::new()
    } else {
        content.to_string()
    }
}

async fn call_responses(
    data: &AppData,
    provider: &Provider,
    system: &str,
    user: &str,
    reasoning_effort: &str,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let mut body = json!({
        "model": provider.model,
        "instructions": system,
        "input": user,
        "stream": true,
    });
    if !matches!(reasoning_effort, "" | "none") {
        body["reasoning"] = json!({ "effort": reasoning_effort });
    }
    let client = build_streaming_client(
        provider.proxy_mode,
        &data.settings.proxy_url,
        data.settings.ai_timeout_seconds,
    )?;
    send_responses_request(
        &client,
        &endpoint(&provider.base_url, "/responses"),
        &body,
        (!provider.api_key.trim().is_empty()).then_some(provider.api_key.trim()),
        stream,
    )
    .await
}

async fn send_chat_completions_request(
    client: &Client,
    endpoint: &str,
    body: &Value,
    api_key: Option<&str>,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let mut last_error = None;
    for attempt in 1..=MAX_REQUEST_ATTEMPTS {
        match send_attempt(client, endpoint, body, api_key).await {
            Ok(response) if response.status().is_success() => {
                return read_chat_completions_stream(response, stream).await;
            }
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let body = response
                    .text()
                    .await
                    .map_err(|error| request_error("reading provider error response", error))?;
                return Err(parse_provider_json(
                    ProviderResponse {
                        status,
                        headers,
                        body,
                    },
                    "OpenAI-compatible provider",
                )
                .unwrap_err_or_http());
            }
            Err(error) if retryable_request_error(&error) && attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            Err(error) => {
                return Err(request_error("Chat Completions request", error));
            }
        }
    }
    Err(request_error(
        "Chat Completions request",
        last_error.expect("a request attempt must fail before retry exhaustion"),
    ))
}

async fn send_responses_request(
    client: &Client,
    endpoint: &str,
    body: &Value,
    api_key: Option<&str>,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let mut last_error = None;
    for attempt in 1..=MAX_REQUEST_ATTEMPTS {
        match send_attempt(client, endpoint, body, api_key).await {
            Ok(response) if response.status().is_success() => {
                return read_responses_stream(response, stream).await;
            }
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let body = response
                    .text()
                    .await
                    .map_err(|error| request_error("reading provider error response", error))?;
                return Err(parse_provider_json(
                    ProviderResponse {
                        status,
                        headers,
                        body,
                    },
                    "OpenAI-compatible provider",
                )
                .unwrap_err_or_http());
            }
            Err(error) if retryable_request_error(&error) && attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            Err(error) => {
                return Err(request_error("OpenAI Responses request", error));
            }
        }
    }
    Err(request_error(
        "OpenAI Responses request",
        last_error.expect("a request attempt must fail before retry exhaustion"),
    ))
}

async fn send_responses_json(
    client: &Client,
    endpoint: &str,
    body: &Value,
    api_key: Option<&str>,
) -> Result<Value, AiError> {
    let mut last_error = None;
    for attempt in 1..=MAX_REQUEST_ATTEMPTS {
        match send_attempt(client, endpoint, body, api_key).await {
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let body = response
                    .text()
                    .await
                    .map_err(|error| request_error("reading Responses image response", error))?;
                return parse_provider_json(
                    ProviderResponse {
                        status,
                        headers,
                        body,
                    },
                    "OpenAI-compatible provider",
                );
            }
            Err(error) if retryable_request_error(&error) && attempt < MAX_REQUEST_ATTEMPTS => {
                last_error = Some(error);
                tokio::time::sleep(Duration::from_millis(250 * attempt as u64)).await;
            }
            Err(error) => {
                return Err(request_error("OpenAI Responses image request", error));
            }
        }
    }
    Err(request_error(
        "OpenAI Responses image request",
        last_error.expect("a request attempt must fail before retry exhaustion"),
    ))
}

async fn send_attempt(
    client: &Client,
    endpoint: &str,
    body: &Value,
    api_key: Option<&str>,
) -> Result<reqwest::Response, reqwest::Error> {
    let mut call = client.post(endpoint).json(body);
    if let Some(api_key) = api_key {
        call = call.bearer_auth(api_key);
    }
    call.send().await
}

async fn read_responses_stream(
    mut response: reqwest::Response,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let mut pending = Vec::new();
    let mut raw = Vec::new();
    let mut content = String::new();
    let mut parse_error = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error("reading streaming provider response", error))?
    {
        raw.extend_from_slice(&chunk);
        pending.extend_from_slice(&chunk);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            if let Err(error) = append_response_stream_line(&line, &mut content, stream.as_ref()) {
                parse_error.get_or_insert(error);
            }
        }
    }
    if !pending.is_empty() {
        if let Err(error) = append_response_stream_line(&pending, &mut content, stream.as_ref()) {
            parse_error.get_or_insert(error);
        }
    }
    if content.is_empty() {
        if let Ok(body) = std::str::from_utf8(&raw) {
            if let Ok(value) = serde_json::from_str::<Value>(body) {
                append_response_value(&value, &mut content, stream.as_ref());
            }
        }
    }
    if let Some(error) = parse_error {
        return Err(error);
    }
    if content.is_empty() {
        return Err(AiError::Message(
            "Responses API returned no output text".to_string(),
        ));
    }
    Ok(content)
}

async fn read_chat_completions_stream(
    mut response: reqwest::Response,
    stream: Option<StreamCallback>,
) -> Result<String, AiError> {
    let mut pending = Vec::new();
    let mut raw = Vec::new();
    let mut content = String::new();
    let mut parse_error = None;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error("reading streaming provider response", error))?
    {
        raw.extend_from_slice(&chunk);
        pending.extend_from_slice(&chunk);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let line = pending.drain(..=newline).collect::<Vec<_>>();
            if let Err(error) =
                append_chat_completion_stream_line(&line, &mut content, stream.as_ref())
            {
                parse_error.get_or_insert(error);
            }
        }
    }
    if !pending.is_empty() {
        if let Err(error) =
            append_chat_completion_stream_line(&pending, &mut content, stream.as_ref())
        {
            parse_error.get_or_insert(error);
        }
    }
    if content.is_empty() {
        if let Ok(body) = std::str::from_utf8(&raw) {
            if let Ok(value) = serde_json::from_str::<Value>(body) {
                append_chat_completion_value(&value, &mut content, stream.as_ref());
            }
        }
    }
    if let Some(error) = parse_error {
        return Err(error);
    }
    if content.is_empty() {
        return Err(AiError::Message(
            "Chat Completions API returned no output text".to_string(),
        ));
    }
    Ok(content)
}

fn append_chat_completion_stream_line(
    line: &[u8],
    content: &mut String,
    stream: Option<&StreamCallback>,
) -> Result<(), AiError> {
    let line = std::str::from_utf8(line)
        .map_err(|error| {
            AiError::Message(format!(
                "Chat Completions API returned invalid UTF-8: {error}"
            ))
        })?
        .trim();
    if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
        return Ok(());
    }
    let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
    if payload == "[DONE]" || payload.is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|error| {
        AiError::Message(format!(
            "Chat Completions API returned invalid JSON: {error}; response: {payload}"
        ))
    })?;
    if value.get("type").and_then(Value::as_str) == Some("error") || value.get("error").is_some() {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .unwrap_or("Chat Completions API returned an error event");
        return Err(AiError::Message(message.to_string()));
    }
    append_chat_completion_value(&value, content, stream);
    Ok(())
}

fn append_chat_completion_value(
    value: &Value,
    content: &mut String,
    stream: Option<&StreamCallback>,
) {
    if let Some(delta) = value.pointer("/choices/0/delta/content") {
        append_chat_content(delta, content, stream);
        return;
    }
    if content.is_empty() {
        if let Some(message) = value.pointer("/choices/0/message/content") {
            append_chat_content(message, content, stream);
        }
    }
}

fn append_chat_content(value: &Value, content: &mut String, stream: Option<&StreamCallback>) {
    let mut append = |text: &str| {
        content.push_str(text);
        if let Some(stream) = stream {
            stream(content.clone());
        }
    };
    if let Some(text) = value.as_str() {
        append(text);
    } else if let Some(parts) = value.as_array() {
        for part in parts {
            if let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| part.pointer("/text/value").and_then(Value::as_str))
            {
                append(text);
            }
        }
    }
}

fn append_response_stream_line(
    line: &[u8],
    content: &mut String,
    stream: Option<&StreamCallback>,
) -> Result<(), AiError> {
    let line = std::str::from_utf8(line)
        .map_err(|error| {
            AiError::Message(format!("Responses API returned invalid UTF-8: {error}"))
        })?
        .trim();
    if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
        return Ok(());
    }
    let payload = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
    if payload == "[DONE]" || payload.is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|error| {
        AiError::Message(format!(
            "Responses API returned invalid JSON: {error}; response: {payload}"
        ))
    })?;
    if value.get("type").and_then(Value::as_str) == Some("error") {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str))
            .unwrap_or("Responses API returned an error event");
        return Err(AiError::Message(message.to_string()));
    }
    append_response_value(&value, content, stream);
    Ok(())
}

fn append_response_value(value: &Value, content: &mut String, stream: Option<&StreamCallback>) {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let delta = if event_type.is_empty() || event_type == "response.output_text.delta" {
        value.get("delta").and_then(Value::as_str)
    } else {
        None
    };
    if let Some(delta) = delta {
        content.push_str(delta);
        if let Some(stream) = stream {
            stream(content.clone());
        }
        return;
    }
    if !content.is_empty() {
        return;
    }
    let output_text = value
        .get("output_text")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/response/output_text")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            (event_type == "response.output_text.done")
                .then(|| value.get("text"))
                .flatten()
                .and_then(Value::as_str)
        });
    if let Some(text) = output_text {
        content.push_str(text);
        if let Some(stream) = stream {
            stream(content.clone());
        }
        return;
    }
    if let Some(output) = value
        .get("output")
        .or_else(|| value.pointer("/response/output"))
    {
        append_response_output(output, content, stream);
    }
}

fn append_response_output(value: &Value, content: &mut String, stream: Option<&StreamCallback>) {
    let Some(items) = value.as_array() else {
        return;
    };
    for item in items {
        let Some(content_items) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for content_item in content_items {
            if content_item.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(text) = content_item.get("text").and_then(Value::as_str) {
                    content.push_str(text);
                    if let Some(stream) = stream {
                        stream(content.clone());
                    }
                }
            }
        }
    }
}

fn decode_responses_image(value: &Value) -> Result<Vec<u8>, AiError> {
    let encoded = value
        .get("output")
        .or_else(|| value.pointer("/response/output"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("image_generation_call"))
                    .then(|| item.get("result"))
                    .flatten()
                    .and_then(Value::as_str)
            })
        })
        .ok_or_else(|| {
            AiError::Message(
                "Responses API response did not contain an image_generation_call result"
                    .to_string(),
            )
        })?;
    let encoded = encoded
        .strip_prefix("data:")
        .and_then(|value| value.split_once(',').map(|(_, payload)| payload))
        .unwrap_or(encoded)
        .trim();
    BASE64.decode(encoded).map_err(|error| {
        AiError::Message(format!(
            "Responses API returned invalid image base64 data: {error}"
        ))
    })
}

fn parse_batch_response(response: &str, expected: usize) -> Result<Vec<String>, AiError> {
    let normalized = response.replace("\r\n", "\n").replace('\r', "\n");
    let mut parts = normalized
        .split(SEGMENT_SEPARATOR)
        .map(str::trim)
        .collect::<Vec<_>>();
    if parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    if parts.len() != expected || parts.iter().any(|part| part.is_empty()) {
        return Err(AiError::BatchFormat(format!(
            "AI returned {} separated segments for {} input segments",
            parts.len(),
            expected
        )));
    }
    Ok(parts.into_iter().map(str::to_string).collect())
}

fn build_client(
    proxy_mode: ProxyMode,
    proxy_url: &str,
    timeout_seconds: u64,
) -> Result<Client, AiError> {
    build_client_with_timeout(proxy_mode, proxy_url, timeout_seconds, false)
}

fn build_streaming_client(
    proxy_mode: ProxyMode,
    proxy_url: &str,
    timeout_seconds: u64,
) -> Result<Client, AiError> {
    build_client_with_timeout(proxy_mode, proxy_url, timeout_seconds, true)
}

fn build_client_with_timeout(
    proxy_mode: ProxyMode,
    proxy_url: &str,
    timeout_seconds: u64,
    streaming: bool,
) -> Result<Client, AiError> {
    let mut builder = Client::builder();
    match proxy_mode {
        ProxyMode::None => {
            builder = builder.no_proxy();
        }
        ProxyMode::Settings => {
            builder = builder.no_proxy();
            if !proxy_url.trim().is_empty() {
                let proxy = Proxy::all(proxy_url.trim())
                    .map_err(|error| AiError::Message(format!("Invalid proxy URL: {error}")))?;
                builder = builder.proxy(proxy);
            }
        }
        ProxyMode::System => {}
    }
    builder = builder.connect_timeout(AI_CONNECT_TIMEOUT);
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    builder = if streaming {
        builder.read_timeout(timeout)
    } else {
        builder.timeout(timeout)
    };
    builder
        .build()
        .map_err(|error| request_error("building HTTP client", error))
}

fn endpoint(base_url: &str, suffix: &str) -> String {
    let base = base_url.trim().trim_end_matches('/');
    if base.ends_with(suffix) {
        base.to_string()
    } else {
        format!("{base}{suffix}")
    }
}

fn image_output_format(filename: &str) -> Result<&'static str, AiError> {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => Ok("png"),
        "jpg" | "jpeg" => Ok("jpeg"),
        "webp" => Ok("webp"),
        _ => Err(AiError::Message("Unsupported image format".to_string())),
    }
}

fn image_format_matches(filename: &str, bytes: &[u8]) -> bool {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" | "jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    }
}

fn retryable_request_error(error: &reqwest::Error) -> bool {
    !error.is_timeout()
        && (error.is_connect() || error.is_request() || error.is_body() || error.is_decode())
}

fn request_error(operation: &str, error: reqwest::Error) -> AiError {
    let category = if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_body() || error.is_decode() {
        "response body"
    } else if error.is_request() {
        "request"
    } else {
        "unknown"
    };
    let mut details = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        details.push_str("; cause: ");
        details.push_str(&cause.to_string());
        source = cause.source();
    }
    AiError::Request(format!("{operation} [{category}]: {details}"))
}

fn parse_provider_json(response: ProviderResponse, provider_name: &str) -> Result<Value, AiError> {
    let status = response.status;
    let content_type = response
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    if !status.is_success() {
        if let Ok(body) = serde_json::from_str::<Value>(&response.body) {
            return Err(AiError::Message(provider_error(&body, status.as_u16())));
        }
        return Err(AiError::Message(format!(
            "{provider_name} returned HTTP {status} (Content-Type: {content_type}): {}",
            response_excerpt(&response.body)
        )));
    }
    serde_json::from_str(&response.body).map_err(|error| AiError::Message(format!(
        "{provider_name} returned invalid JSON (HTTP {status}, Content-Type: {content_type}): {error}; response body: {}",
        response_excerpt(&response.body)
    )))
}

trait ErrorResultExt<T> {
    fn unwrap_err_or_http(self) -> AiError;
}

impl<T> ErrorResultExt<T> for Result<T, AiError> {
    fn unwrap_err_or_http(self) -> AiError {
        match self {
            Ok(_) => {
                AiError::Message("Provider returned an unsuccessful HTTP response".to_string())
            }
            Err(error) => error,
        }
    }
}

fn provider_error(body: &Value, status: u16) -> String {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| body.get("error").and_then(Value::as_str))
        .or_else(|| body.get("message").and_then(Value::as_str))
        .map(|message| format!("Provider returned HTTP {status}: {message}"))
        .unwrap_or_else(|| format!("Provider returned HTTP {status}"))
}

fn response_excerpt(body: &str) -> String {
    const MAX_RESPONSE_EXCERPT_CHARS: usize = 2_000;
    let excerpt = body
        .chars()
        .take(MAX_RESPONSE_EXCERPT_CHARS)
        .collect::<String>();
    if body.chars().count() > MAX_RESPONSE_EXCERPT_CHARS {
        format!("{excerpt}...")
    } else {
        excerpt
    }
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

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, time::Duration};

    use axum::{body::Bytes, routing::post, Router};
    use serde_json::json;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::{
        append_chat_completion_stream_line, append_response_stream_line,
        build_chat_completions_body, build_messages, build_streaming_client,
        decode_responses_image, document_summary_request, infer_reasoning_parser,
        parse_batch_response, send_responses_request, strip_reasoning_markup,
        truncate_document_summary, AppData, Provider, ProxyMode, ReasoningParser, TranslateRequest,
        MAX_DOCUMENT_SUMMARY_CHARS, SEGMENT_SEPARATOR,
    };

    fn provider(model: &str) -> Provider {
        Provider {
            id: "provider".to_string(),
            name: "Provider".to_string(),
            base_url: "http://127.0.0.1:8000/v1".to_string(),
            model: model.to_string(),
            reasoning_parser: ReasoningParser::Auto,
            api_key: String::new(),
            proxy_mode: ProxyMode::None,
            enabled: true,
            supports_images: false,
            context_size: 32_768,
            max_segments: 16,
            max_concurrent: 2,
            text_translation_model: false,
        }
    }

    #[test]
    fn batch_response_uses_a_plain_delimiter() {
        let response = format!("one{SEGMENT_SEPARATOR}two{SEGMENT_SEPARATOR}");
        assert_eq!(parse_batch_response(&response, 2).unwrap(), ["one", "two"]);
        assert_eq!(
            parse_batch_response(&format!(" {response} \n"), 2).unwrap(),
            ["one", "two"]
        );
        assert_eq!(
            parse_batch_response(&response.replace('\n', "\r\n"), 2).unwrap(),
            ["one", "two"]
        );
        assert!(parse_batch_response("one\ntwo", 2).is_err());
    }

    #[test]
    fn document_summary_preserves_reasoning_setting() {
        let request = TranslateRequest {
            text: "original".to_string(),
            source_language: "English".to_string(),
            target_language: "Chinese".to_string(),
            provider_id: "provider".to_string(),
            prompt_id: None,
            additional_prompt: String::new(),
            glossary_ids: Vec::new(),
            reasoning_effort: "high".to_string(),
            summarize: true,
            context_summary: Some("previous note".to_string()),
        };

        let summary = document_summary_request(&request, "document sample");

        assert_eq!(summary.text, "document sample");
        assert_eq!(summary.reasoning_effort, "high");
        assert!(summary.context_summary.is_none());
    }

    #[test]
    fn additional_prompt_is_appended_to_the_selected_prompt() {
        let request = TranslateRequest {
            text: "Hello".to_string(),
            source_language: "English".to_string(),
            target_language: "Chinese".to_string(),
            provider_id: "provider".to_string(),
            prompt_id: Some("faithful-translation".to_string()),
            additional_prompt: "Use a friendly tone.".to_string(),
            glossary_ids: Vec::new(),
            reasoning_effort: "none".to_string(),
            summarize: false,
            context_summary: None,
        };

        let (system, user) = build_messages(&AppData::default(), &request);

        assert!(system.contains("Translate faithfully"));
        assert!(system.ends_with("Additional instructions:\nUse a friendly tone."));
        assert_eq!(user, "Text to translate:\nHello");
    }

    #[test]
    fn no_template_uses_only_user_instructions_and_raw_input() {
        let mut request = TranslateRequest {
            text: "Hello".into(),
            source_language: "English".into(),
            target_language: "Martian".into(),
            provider_id: "provider".into(),
            prompt_id: None,
            additional_prompt: "My instructions".into(),
            glossary_ids: vec![],
            reasoning_effort: "none".into(),
            summarize: true,
            context_summary: Some("Generated context".into()),
        };
        assert_eq!(
            build_messages(&AppData::default(), &request),
            ("My instructions".into(), "Hello".into())
        );
        request.prompt_id = Some(String::new());
        request.additional_prompt.clear();
        assert_eq!(
            build_messages(&AppData::default(), &request),
            (String::new(), "Hello".into())
        );
    }

    #[test]
    fn document_summary_is_trimmed_locally_when_too_long() {
        let long_summary = format!("{} final detail", "topic ".repeat(100));
        let summary = truncate_document_summary(&long_summary);

        assert!(summary.ends_with("..."));
        assert!(summary.chars().count() <= MAX_DOCUMENT_SUMMARY_CHARS + 3);
    }

    #[test]
    fn reasoning_parser_is_inferred_from_mainstream_model_names() {
        assert_eq!(
            infer_reasoning_parser("Qwen/Qwen3.8-27B"),
            ReasoningParser::Qwen3
        );
        assert_eq!(
            infer_reasoning_parser("deepseek-ai/DeepSeek-R1-Distill-Qwen-32B"),
            ReasoningParser::DeepseekR1
        );
        assert_eq!(
            infer_reasoning_parser("deepseek-ai/DeepSeek-V3.2"),
            ReasoningParser::DeepseekV3
        );
        assert_eq!(
            infer_reasoning_parser("openai/gpt-5.2"),
            ReasoningParser::Openai
        );
    }

    #[test]
    fn qwen3_chat_body_controls_thinking_and_budget() {
        let provider = provider("Qwen/Qwen3.8-27B");
        let disabled = build_chat_completions_body(
            &provider,
            ReasoningParser::Qwen3,
            "system",
            "user",
            "none",
        );
        assert_eq!(disabled["reasoning_effort"], "none");
        assert_eq!(
            disabled.pointer("/chat_template_kwargs/enable_thinking"),
            Some(&json!(false))
        );
        assert!(disabled.get("thinking_token_budget").is_none());

        let medium = build_chat_completions_body(
            &provider,
            ReasoningParser::Qwen3,
            "system",
            "user",
            "medium",
        );
        assert_eq!(medium["reasoning_effort"], "medium");
        assert_eq!(
            medium.pointer("/chat_template_kwargs/enable_thinking"),
            Some(&json!(true))
        );
        assert_eq!(medium["thinking_token_budget"], 4_096);
    }

    #[test]
    fn chat_stream_ignores_reasoning_and_accumulates_final_content() {
        let mut content = String::new();
        append_chat_completion_stream_line(
            br#"data: {"choices":[{"delta":{"reasoning":"internal"}}]}"#,
            &mut content,
            None,
        )
        .unwrap();
        append_chat_completion_stream_line(
            br#"data: {"choices":[{"delta":{"content":"final"}}]}"#,
            &mut content,
            None,
        )
        .unwrap();
        assert_eq!(content, "final");
    }

    #[test]
    fn raw_thinking_markup_is_removed_as_a_fallback() {
        assert_eq!(
            strip_reasoning_markup("<think>private</think>translated"),
            "translated"
        );
        assert!(strip_reasoning_markup("<think>still thinking").is_empty());
    }

    #[test]
    fn responses_stream_accumulates_output_text_deltas() {
        let mut content = String::new();
        append_response_stream_line(
            br#"data: {"type":"response.output_text.delta","delta":"Hel"}"#,
            &mut content,
            None,
        )
        .unwrap();
        append_response_stream_line(
            br#"data: {"type":"response.output_text.delta","delta":"lo"}"#,
            &mut content,
            None,
        )
        .unwrap();
        assert_eq!(content, "Hello");
    }

    #[tokio::test]
    async fn streaming_timeout_resets_after_each_received_chunk() {
        async fn delayed_stream() -> axum::body::Body {
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                let chunks = [
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Hel\"}\n\n"
                        .as_slice(),
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"l\"}\n\n"
                        .as_slice(),
                    b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"o\"}\n\n"
                        .as_slice(),
                ];
                for (index, chunk) in chunks.into_iter().enumerate() {
                    if index > 0 {
                        tokio::time::sleep(Duration::from_millis(600)).await;
                    }
                    if sender
                        .send(Ok::<_, Infallible>(Bytes::copy_from_slice(chunk)))
                        .is_err()
                    {
                        break;
                    }
                }
            });
            axum::body::Body::from_stream(UnboundedReceiverStream::new(receiver))
        }

        let app = Router::new().route("/responses", post(delayed_stream));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = build_streaming_client(ProxyMode::None, "", 1).unwrap();
        let started = std::time::Instant::now();

        let result = send_responses_request(
            &client,
            &format!("http://{address}/responses"),
            &json!({}),
            None,
            None,
        )
        .await
        .unwrap();

        server.abort();
        assert_eq!(result, "Hello");
        assert!(started.elapsed() > Duration::from_secs(1));
    }

    #[test]
    fn responses_stream_ignores_reasoning_deltas() {
        let mut content = String::new();
        append_response_stream_line(
            br#"data: {"type":"response.reasoning_summary_text.delta","delta":"internal"}"#,
            &mut content,
            None,
        )
        .unwrap();
        append_response_stream_line(
            br#"data: {"type":"response.output_text.delta","delta":"visible"}"#,
            &mut content,
            None,
        )
        .unwrap();
        assert_eq!(content, "visible");
    }

    #[test]
    fn responses_completion_output_is_supported() {
        let mut content = String::new();
        append_response_stream_line(br#"data: {"type":"response.completed","response":{"output":[{"content":[{"type":"output_text","text":"done"}]}]}}"#, &mut content, None).unwrap();
        assert_eq!(content, "done");
    }

    #[test]
    fn responses_error_events_are_reported() {
        let error = append_response_stream_line(
            br#"data: {"type":"error","error":{"message":"provider stopped"}}"#,
            &mut String::new(),
            None,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "provider stopped");
    }

    #[test]
    fn responses_image_generation_result_is_decoded() {
        let response = json!({
            "output": [{
                "type": "image_generation_call",
                "result": "aGVsbG8="
            }]
        });
        assert_eq!(decode_responses_image(&response).unwrap(), b"hello");
    }
}
