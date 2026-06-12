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
        let request = ChatCompletionRequest {
            model: &self.model,
            messages,
            temperature: 0.0,
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
    use super::{ChatEndpoint, ChatMessage, EmbeddingEndpoint};
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
