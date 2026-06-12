use localmind_core::{InferenceSettings, MemoryScope};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CONFIG_FILE_NAME: &str = ".localmind.toml";

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LocalMindConfig {
    #[serde(default)]
    pub learning: LearningConfig,
    #[serde(default)]
    pub inference: Option<InferenceSettings>,
    #[serde(default)]
    pub review: ReviewConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct LearningConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_local_only")]
    pub local_only: bool,
    #[serde(default = "default_memory_root")]
    pub memory_root: PathBuf,
    #[serde(default = "default_allowed_scopes")]
    pub allowed_scopes: Vec<MemoryScope>,
    #[serde(default)]
    pub excluded_paths: Vec<String>,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            local_only: true,
            memory_root: default_memory_root(),
            allowed_scopes: default_allowed_scopes(),
            excluded_paths: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ReviewConfig {
    #[serde(default)]
    pub mode: ReviewModeConfig,
    #[serde(default = "default_trusted_threshold")]
    pub trusted_threshold: f32,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            mode: ReviewModeConfig::Manual,
            trusted_threshold: default_trusted_threshold(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewModeConfig {
    #[default]
    Manual,
    Assisted,
    Trusted,
    Automatic,
}

fn default_local_only() -> bool {
    true
}

fn default_memory_root() -> PathBuf {
    PathBuf::from(".localmind/memory")
}

fn default_allowed_scopes() -> Vec<MemoryScope> {
    vec![MemoryScope::Project]
}

fn default_trusted_threshold() -> f32 {
    0.82
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectConfig {
    pub project_root: PathBuf,
    pub config_path: PathBuf,
    pub config: LocalMindConfig,
}

impl ProjectConfig {
    pub fn discover(project_root: impl AsRef<Path>) -> Result<Self, StoreConfigError> {
        let project_root = project_root.as_ref().to_path_buf();
        let config_path = project_root.join(CONFIG_FILE_NAME);
        let content = fs::read_to_string(&config_path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                StoreConfigError::MissingConfig {
                    path: config_path.clone(),
                }
            } else {
                StoreConfigError::ReadConfig {
                    path: config_path.clone(),
                    source,
                }
            }
        })?;

        let config = toml::from_str::<LocalMindConfig>(&content).map_err(|source| {
            StoreConfigError::MalformedConfig {
                path: config_path.clone(),
                message: source.to_string(),
            }
        })?;

        let project_config = Self {
            project_root,
            config_path,
            config,
        };
        project_config.validate()?;
        Ok(project_config)
    }

    pub fn validate(&self) -> Result<(), StoreConfigError> {
        let learning = &self.config.learning;

        if !learning.enabled {
            return Err(StoreConfigError::LearningDisabled {
                path: self.config_path.clone(),
            });
        }

        if !learning.local_only {
            return Err(StoreConfigError::RemoteLearningUnsupported {
                path: self.config_path.clone(),
            });
        }

        if learning.memory_root.is_absolute()
            || learning
                .memory_root
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(StoreConfigError::UnsafeMemoryRoot {
                path: self.config_path.clone(),
                memory_root: learning.memory_root.clone(),
            });
        }

        if learning.allowed_scopes.is_empty() {
            return Err(StoreConfigError::NoAllowedScopes {
                path: self.config_path.clone(),
            });
        }

        if let Some(inference) = &self.config.inference {
            validate_inference_endpoint(
                &self.config_path,
                inference.chat_base_url.as_deref(),
                inference.chat_model.as_deref(),
                "chat_model",
            )?;
            validate_inference_endpoint(
                &self.config_path,
                inference.embedding_base_url(),
                inference.embedding_model.as_deref(),
                "embedding_model",
            )?;
            if inference.timeout_secs == 0 {
                return Err(StoreConfigError::InvalidInferenceTimeout {
                    path: self.config_path.clone(),
                });
            }
        }

        if !(0.0..=1.0).contains(&self.config.review.trusted_threshold) {
            return Err(StoreConfigError::InvalidReviewThreshold {
                path: self.config_path.clone(),
                value: self.config.review.trusted_threshold,
            });
        }

        Ok(())
    }

    #[must_use]
    pub fn memory_root(&self) -> PathBuf {
        self.project_root.join(&self.config.learning.memory_root)
    }

    #[must_use]
    pub fn allows_scope(&self, scope: &MemoryScope) -> bool {
        self.config.learning.allowed_scopes.contains(scope)
    }
}

fn validate_inference_endpoint(
    path: &Path,
    endpoint: Option<&str>,
    model: Option<&str>,
    model_field: &'static str,
) -> Result<(), StoreConfigError> {
    if let Some(endpoint) = endpoint {
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            return Err(StoreConfigError::InvalidInferenceEndpoint {
                path: path.to_path_buf(),
                endpoint: endpoint.to_string(),
            });
        }
    }
    if model.is_some() && endpoint.is_none() {
        return Err(StoreConfigError::InferenceModelWithoutEndpoint {
            path: path.to_path_buf(),
            model_field,
        });
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum StoreConfigError {
    #[error("LocalMind project config is missing: {path:?}")]
    MissingConfig { path: PathBuf },
    #[error("failed to read LocalMind project config {path:?}: {source}")]
    ReadConfig {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("LocalMind project config is malformed at {path:?}: {message}")]
    MalformedConfig { path: PathBuf, message: String },
    #[error("LocalMind learning is disabled in {path:?}")]
    LearningDisabled { path: PathBuf },
    #[error("remote learning is not supported by the local-first MVP config: {path:?}")]
    RemoteLearningUnsupported { path: PathBuf },
    #[error("unsafe LocalMind memory root {memory_root:?} in {path:?}")]
    UnsafeMemoryRoot { path: PathBuf, memory_root: PathBuf },
    #[error("LocalMind config must allow at least one memory scope: {path:?}")]
    NoAllowedScopes { path: PathBuf },
    #[error("invalid inference endpoint {endpoint:?} in {path:?}; endpoint must be http(s)")]
    InvalidInferenceEndpoint { path: PathBuf, endpoint: String },
    #[error("{model_field} is configured without an inference endpoint in {path:?}")]
    InferenceModelWithoutEndpoint {
        path: PathBuf,
        model_field: &'static str,
    },
    #[error("inference timeout must be greater than zero in {path:?}")]
    InvalidInferenceTimeout { path: PathBuf },
    #[error("review trusted_threshold must be between 0.0 and 1.0 in {path:?}, got {value}")]
    InvalidReviewThreshold { path: PathBuf, value: f32 },
}
