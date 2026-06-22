//! Opt-in OpenAI-compatible inference clients.

use localmind_core::InferenceSettings;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InferenceCapability {
    chat: Option<ChatEndpoint>,
    embeddings: Option<EmbeddingEndpoint>,
}

impl InferenceCapability {
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            chat: None,
            embeddings: None,
        }
    }

    pub fn from_settings(settings: Option<&InferenceSettings>) -> Result<Self, InferenceError> {
        let Some(settings) = settings else {
            return Ok(Self::disabled());
        };

        let chat = if settings.features.extraction
            || settings.features.review
            || settings.features.skills
            || settings.features.research
        {
            match (&settings.chat_base_url, &settings.chat_model) {
                (Some(base_url), Some(model)) => Some(ChatEndpoint::new(
                    base_url,
                    model,
                    settings.api_key_env.as_deref(),
                    settings.timeout_secs,
                )?),
                _ => None,
            }
        } else {
            None
        };

        let embeddings = if settings.features.embeddings {
            match (settings.embedding_base_url(), &settings.embedding_model) {
                (Some(base_url), Some(model)) => Some(EmbeddingEndpoint::new(
                    base_url,
                    model,
                    settings.api_key_env.as_deref(),
                    settings.timeout_secs,
                )?),
                _ => None,
            }
        } else {
            None
        };

        Ok(Self { chat, embeddings })
    }

    #[must_use]
    pub fn chat(&self) -> Option<&ChatEndpoint> {
        self.chat.as_ref()
    }

    #[must_use]
    pub fn embeddings(&self) -> Option<&EmbeddingEndpoint> {
        self.embeddings.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatEndpoint {
    base_url: String,
    model: String,
    api_key_env: Option<String>,
    timeout_secs: u64,
}

impl ChatEndpoint {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
        timeout_secs: u64,
    ) -> Result<Self, InferenceError> {
        validate_endpoint(base_url)?;
        if model.trim().is_empty() {
            return Err(InferenceError::InvalidModel);
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key_env: api_key_env.map(ToString::to_string),
            timeout_secs,
        })
    }

    pub fn complete(&self, messages: &[ChatMessage]) -> Result<ChatCompletion, InferenceError> {
        self.complete_inner(messages, false)
    }

    /// Like [`ChatEndpoint::complete`], but asks the server for a JSON object via
    /// the OpenAI `response_format` field. Some local servers reject the
    /// constraint (e.g. a turboquant build returns HTTP 400 for structured
    /// formats); on an HTTP error the request is retried once without it so a
    /// caller that only needs best-effort JSON can still proceed and parse the
    /// reply with [`extract_json_payload`].
    pub fn complete_json(
        &self,
        messages: &[ChatMessage],
    ) -> Result<ChatCompletion, InferenceError> {
        match self.complete_inner(messages, true) {
            Err(InferenceError::Http { .. }) => self.complete_inner(messages, false),
            other => other,
        }
    }

    fn complete_inner(
        &self,
        messages: &[ChatMessage],
        json_object: bool,
    ) -> Result<ChatCompletion, InferenceError> {
        let request = ChatCompletionRequest {
            model: &self.model,
            messages,
            temperature: 0.0,
            response_format: json_object.then_some(ResponseFormat {
                kind: "json_object",
            }),
        };
        let response = post_json(
            &format!("{}/v1/chat/completions", self.base_url),
            &self.api_key_env,
            self.timeout_secs,
            &request,
        )?;
        let parsed: ChatCompletionResponse =
            serde_json::from_str(&response).map_err(InferenceError::DecodeResponse)?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .ok_or(InferenceError::MissingContent)?;
        Ok(ChatCompletion {
            content,
            usage: parsed.usage,
        })
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbeddingEndpoint {
    base_url: String,
    model: String,
    api_key_env: Option<String>,
    timeout_secs: u64,
}

impl EmbeddingEndpoint {
    pub fn new(
        base_url: &str,
        model: &str,
        api_key_env: Option<&str>,
        timeout_secs: u64,
    ) -> Result<Self, InferenceError> {
        validate_endpoint(base_url)?;
        if model.trim().is_empty() {
            return Err(InferenceError::InvalidModel);
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key_env: api_key_env.map(ToString::to_string),
            timeout_secs,
        })
    }

    pub fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, InferenceError> {
        let request = EmbeddingRequest {
            model: &self.model,
            input: inputs,
        };
        let response = post_json(
            &format!("{}/v1/embeddings", self.base_url),
            &self.api_key_env,
            self.timeout_secs,
            &request,
        )?;
        let parsed: EmbeddingResponse =
            serde_json::from_str(&response).map_err(InferenceError::DecodeResponse)?;
        Ok(parsed.data.into_iter().map(|item| item.embedding).collect())
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    #[must_use]
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_string(),
            content: content.into(),
        }
    }

    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_string(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatCompletion {
    pub content: String,
    pub usage: Option<TokenUsage>,
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Clone, Copy, Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

#[derive(Serialize)]
struct EmbeddingRequest<'a> {
    model: &'a str,
    input: &'a [String],
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

fn post_json<T: Serialize>(
    url: &str,
    api_key_env: &Option<String>,
    timeout_secs: u64,
    body: &T,
) -> Result<String, InferenceError> {
    let payload = serde_json::to_string(body).map_err(InferenceError::EncodeRequest)?;
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(timeout_secs))
        .build();
    let mut request = agent.post(url).set("Content-Type", "application/json");
    if let Some(env_name) = api_key_env {
        if let Ok(token) = std::env::var(env_name) {
            request = request.set("Authorization", &format!("Bearer {token}"));
        }
    }
    request
        .send_string(&payload)
        .map_err(|source| InferenceError::Http {
            url: url.to_string(),
            source: Box::new(source),
        })?
        .into_string()
        .map_err(InferenceError::ReadResponse)
}

fn validate_endpoint(base_url: &str) -> Result<(), InferenceError> {
    let url = base_url.trim();
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(InferenceError::InvalidEndpoint {
            url: base_url.to_string(),
        })
    }
}

/// Best-effort extraction of a JSON object/array from a chat-model reply.
///
/// Reasoning-capable local models routinely wrap JSON in a `<think>...</think>`
/// block and/or a Markdown code fence (```` ```json ... ``` ````), so a raw
/// `serde_json::from_str` on the whole reply fails at column 1. This strips a
/// leading think block, unwraps a single fenced block, then narrows to the
/// outermost `{...}`/`[...]` span. Returns `None` when no JSON-looking span is
/// present (e.g. prose-only), so the caller can fall back instead of erroring.
#[must_use]
pub fn extract_json_payload(reply: &str) -> Option<&str> {
    let mut text = reply.trim();

    // Drop a leading reasoning block (`<think> ... </think>`), keeping whatever
    // follows. An empty `<think></think>` is common and handled the same way.
    if let Some(end) = text.find("</think>") {
        text = text[end + "</think>".len()..].trim_start();
    }

    // Unwrap a single Markdown code fence if present, skipping an optional
    // language tag on the opening line (e.g. ```json).
    if let Some(start) = text.find("```") {
        let after = &text[start + 3..];
        let after = match after.find('\n') {
            Some(newline)
                if !after[..newline].trim().is_empty()
                    && after[..newline]
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric()) =>
            {
                &after[newline + 1..]
            }
            Some(newline) if after[..newline].trim().is_empty() => &after[newline + 1..],
            _ => after,
        };
        text = match after.find("```") {
            Some(close) => after[..close].trim(),
            None => after.trim(),
        };
    }

    // Narrow to the outermost JSON object/array span.
    let start = text.find(['{', '['])?;
    let end = text.rfind(['}', ']'])?;
    if end < start {
        return None;
    }
    let payload = text[start..=end].trim();
    (!payload.is_empty()).then_some(payload)
}

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("inference endpoint must be http(s): {url}")]
    InvalidEndpoint { url: String },
    #[error("inference model id must not be empty")]
    InvalidModel,
    #[error("failed to encode inference request: {0}")]
    EncodeRequest(serde_json::Error),
    #[error("inference HTTP request failed for {url}: {source}")]
    Http {
        url: String,
        source: Box<ureq::Error>,
    },
    #[error("failed to read inference response: {0}")]
    ReadResponse(std::io::Error),
    #[error("failed to decode inference response: {0}")]
    DecodeResponse(serde_json::Error),
    #[error("inference response did not include content")]
    MissingContent,
}

#[cfg(test)]
mod tests {
    use super::{extract_json_payload, ChatEndpoint, ChatMessage, EmbeddingEndpoint};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn chat_completion_parses_content_and_usage() -> Result<(), Box<dyn std::error::Error>> {
        let base_url = one_response(
            "{\"choices\":[{\"message\":{\"content\":\"ok\"}}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}",
        )?;
        let endpoint = ChatEndpoint::new(&base_url, "chat-model", None, 5)?;

        let response = endpoint.complete(&[ChatMessage::user("hello")])?;

        assert_eq!(response.content, "ok");
        assert_eq!(response.usage.and_then(|usage| usage.total_tokens), Some(3));
        Ok(())
    }

    #[test]
    fn embeddings_parse_vectors() -> Result<(), Box<dyn std::error::Error>> {
        let base_url =
            one_response("{\"data\":[{\"embedding\":[1.0,0.0]},{\"embedding\":[0.0,1.0]}]}")?;
        let endpoint = EmbeddingEndpoint::new(&base_url, "embed-model", None, 5)?;

        let vectors = endpoint.embed(&["a".to_string(), "b".to_string()])?;

        assert_eq!(vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        Ok(())
    }

    #[test]
    fn extract_json_payload_handles_bare_object() {
        assert_eq!(extract_json_payload("{\"ok\":true}"), Some("{\"ok\":true}"));
    }

    #[test]
    fn extract_json_payload_unwraps_fenced_json() {
        assert_eq!(
            extract_json_payload("```json\n{\"ok\":true}\n```"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            extract_json_payload("```\n{\"ok\":true}\n```"),
            Some("{\"ok\":true}")
        );
    }

    #[test]
    fn extract_json_payload_strips_think_block() {
        // The exact shape the live turboquant model returns: an empty think
        // block, then fenced JSON.
        assert_eq!(
            extract_json_payload("<think>\n\n</think>\n\n```json\n{\"ok\": true}\n```"),
            Some("{\"ok\": true}")
        );
        assert_eq!(
            extract_json_payload("<think>reasoning here</think>{\"a\":1}"),
            Some("{\"a\":1}")
        );
    }

    #[test]
    fn extract_json_payload_narrows_to_outermost_span() {
        assert_eq!(
            extract_json_payload("Here is the result: {\"a\":1} -- done"),
            Some("{\"a\":1}")
        );
        assert_eq!(extract_json_payload("[1,2,3]"), Some("[1,2,3]"));
    }

    #[test]
    fn extract_json_payload_returns_none_for_prose() {
        assert_eq!(extract_json_payload("no json here"), None);
        assert_eq!(extract_json_payload("<think>only thinking</think>"), None);
        assert_eq!(extract_json_payload(""), None);
    }

    fn one_response(body: &'static str) -> Result<String, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];
                loop {
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(read) => {
                            request.extend_from_slice(&buffer[..read]);
                            if request_complete(&request) {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _write = stream.write_all(response.as_bytes());
            }
        });
        Ok(format!("http://{address}"))
    }

    fn request_complete(request: &[u8]) -> bool {
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let mut content_length = 0_usize;
        for line in headers.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        request.len() >= header_end + 4 + content_length
    }
}
