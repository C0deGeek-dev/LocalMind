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
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub sync: SyncConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct LearningConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_local_only")]
    pub local_only: bool,
    #[serde(default = "default_memory_root")]
    pub memory_root: PathBuf,
    /// Override the machine-wide global memory root (an absolute directory). When
    /// unset, the global store lives under the per-OS user home
    /// (`~/.localmind/memory`). Global-scope memory is shared across every project
    /// on the machine, so it is resolved separately from the project store; it is
    /// only used when `allowed_scopes` opts in to `GlobalUser`.
    #[serde(default)]
    pub global_memory_root: Option<PathBuf>,
    #[serde(default = "default_allowed_scopes")]
    pub allowed_scopes: Vec<MemoryScope>,
    #[serde(default)]
    pub excluded_paths: Vec<String>,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            local_only: true,
            memory_root: default_memory_root(),
            global_memory_root: None,
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
    /// Opt in to embedding-based dedup on top of the deterministic lexical rung.
    /// Only takes effect when an inference embedding endpoint is also configured;
    /// otherwise the queue degrades to the lexical contract (see
    /// [`ProjectConfig::semantic_dedup_active`]).
    #[serde(default)]
    pub semantic_dedup: bool,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            mode: ReviewModeConfig::Manual,
            trusted_threshold: default_trusted_threshold(),
            semantic_dedup: false,
        }
    }
}

/// Retrieval-ranking knobs. All default to the deterministic blend; the rerank
/// stage is opt-in and only takes effect when an embedding endpoint is also
/// configured (see [`ProjectConfig::rerank_active`]).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct RetrievalConfig {
    /// Opt in to the embedding rerank stage on top of the deterministic blend.
    /// Off by default; without an inference embedding endpoint it is inert and
    /// the blend order is the whole story.
    #[serde(default)]
    pub rerank: bool,
    /// How many top blended hits the rerank stage may reorder.
    #[serde(default = "default_rerank_window")]
    pub rerank_window: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            rerank: false,
            rerank_window: default_rerank_window(),
        }
    }
}

/// Cross-device sync settings (`[sync]`). All optional; a project with no
/// `[sync]` section is unchanged. `project_key` fixes the path-independent
/// project identity (otherwise derived from the git remote, else the directory
/// name); `device_label` names this machine on synced knowledge (otherwise a
/// best-effort hostname is used). The sync folder and enrollment settings are
/// added by later work.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct SyncConfig {
    #[serde(default)]
    pub project_key: Option<String>,
    #[serde(default)]
    pub device_label: Option<String>,
    /// The folder this project exchanges encrypted sync bundles through (carried
    /// by Syncthing/OneDrive/a share/a private git repo — LocalMind opens no
    /// sockets). Absolute. When unset, `localmind sync` needs `--folder`.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// Injection down-weight factor in `(0, 1]` for a synced lesson whose origin
    /// machine differs from this one. `1.0` (or unset) disables the down-weight;
    /// a smaller value ranks foreign-machine lessons lower **without dropping
    /// them**. Conservative default.
    #[serde(default = "default_foreign_env_weight")]
    pub foreign_env_weight: f32,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            project_key: None,
            device_label: None,
            folder: None,
            foreign_env_weight: default_foreign_env_weight(),
        }
    }
}

fn default_foreign_env_weight() -> f32 {
    // Conservative: a foreign-machine lesson keeps most of its rank; it drops
    // only enough to prefer a same-machine lesson when both match, and is never
    // filtered out.
    0.85
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

fn default_enabled() -> bool {
    // Learning is on by default: a project accumulates reviewed, machine-wide
    // memory (good *and* anti-pattern lessons about languages, tooling, shell,
    // and builds) out of the box. It stays `local_only` (same-machine, never
    // remote) and review-gated — sessions yield lesson *candidates*, never
    // auto-active memory — and a project opts out with `[learning] enabled = false`.
    true
}

fn default_local_only() -> bool {
    true
}

fn default_memory_root() -> PathBuf {
    PathBuf::from(".localmind/memory")
}

fn default_allowed_scopes() -> Vec<MemoryScope> {
    // Global-scope memory is on by default: cross-project knowledge (tool-use
    // patterns, debugging recipes, durable preferences) accumulates in the
    // machine-wide store out of the box. It stays `local_only` (same-machine), and
    // a project that wants project-only memory narrows this to `["project"]`.
    vec![MemoryScope::Project, MemoryScope::GlobalUser]
}

fn default_trusted_threshold() -> f32 {
    0.82
}

fn default_rerank_window() -> usize {
    20
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

        // A configured global memory root is a machine-wide location, so it must
        // be absolute (the opposite of `memory_root`, which is project-relative)
        // and free of `..` traversal.
        if let Some(global_root) = &learning.global_memory_root {
            if !global_root.is_absolute()
                || global_root
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(StoreConfigError::UnsafeGlobalMemoryRoot {
                    path: self.config_path.clone(),
                    global_memory_root: global_root.clone(),
                });
            }
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

    /// The machine-wide global memory root, resolved separately from the project
    /// store: the configured `global_memory_root` (absolute) when set, otherwise
    /// `~/.localmind/memory` from the per-OS user home. `None` only when no home
    /// directory is resolvable and no override is configured.
    #[must_use]
    pub fn global_memory_root(&self) -> Option<PathBuf> {
        if let Some(root) = &self.config.learning.global_memory_root {
            return Some(root.clone());
        }
        // `LOCALMIND_GLOBAL_ROOT` redirects the machine-wide store away from the
        // home directory. The special value `@project` roots it under *this*
        // project (`<project>/.localmind-global`) — per-project isolation that
        // keeps tests/CI hermetic now that global scope is on by default (each
        // test's global store is its own and is cleaned with its workspace);
        // production leaves it unset and uses the home default. Any other value is
        // an explicit absolute override.
        if let Some(env_root) = std::env::var_os("LOCALMIND_GLOBAL_ROOT") {
            if !env_root.is_empty() {
                if env_root == "@project" {
                    return Some(self.project_root.join(".localmind-global"));
                }
                return Some(PathBuf::from(env_root));
            }
        }
        home_dir().map(|home| home.join(".localmind").join("memory"))
    }

    /// Whether the project allows machine-wide global-scope memory (its
    /// `allowed_scopes` lists `GlobalUser`). Global memory is **on by default**
    /// (D-LM-0017/0019): cross-project knowledge accumulates in the same-machine
    /// (`local_only`) global store out of the box; narrow `allowed_scopes` to
    /// `["project"]` for project-only memory.
    #[must_use]
    pub fn allows_global(&self) -> bool {
        self.allows_scope(&MemoryScope::GlobalUser)
    }

    #[must_use]
    pub fn allows_scope(&self, scope: &MemoryScope) -> bool {
        self.config.learning.allowed_scopes.contains(scope)
    }

    /// Whether embedding-based review-queue dedup should run: the opt-in flag is
    /// set **and** an inference embedding endpoint is configured. When this is
    /// false the queue uses the deterministic lexical contract alone — the
    /// fallback that always holds when no endpoint is present.
    #[must_use]
    pub fn semantic_dedup_active(&self) -> bool {
        self.config.review.semantic_dedup
            && self
                .config
                .inference
                .as_ref()
                .and_then(|inference| inference.embedding_base_url())
                .is_some()
    }

    /// Whether the embedding rerank stage should run: the opt-in flag is set
    /// **and** an inference embedding endpoint is configured. When this is false
    /// the ranked search path uses the deterministic blend alone — the floor
    /// that always holds when no endpoint is present, keeping ranking
    /// reproducible and offline.
    #[must_use]
    pub fn rerank_active(&self) -> bool {
        self.config.retrieval.rerank
            && self
                .config
                .inference
                .as_ref()
                .and_then(|inference| inference.embedding_base_url())
                .is_some()
    }

    /// The number of top blended hits the rerank stage may reorder.
    #[must_use]
    pub fn rerank_window(&self) -> usize {
        self.config.retrieval.rerank_window
    }

    /// The explicit `[sync] project_key`, if the project pinned one.
    #[must_use]
    pub fn sync_project_key(&self) -> Option<&str> {
        self.config.sync.project_key.as_deref()
    }

    /// The label this machine stamps on synced knowledge: the configured
    /// `[sync] device_label` when set, otherwise a best-effort hostname (empty
    /// when even that is unavailable). Never fails.
    #[must_use]
    pub fn sync_device_label(&self) -> String {
        self.config
            .sync
            .device_label
            .clone()
            .unwrap_or_else(localmind_core::host_device_label)
    }

    /// The configured `[sync] folder`, if any.
    #[must_use]
    pub fn sync_folder(&self) -> Option<&Path> {
        self.config.sync.folder.as_deref()
    }

    /// The injection down-weight factor for a synced lesson from another machine,
    /// clamped to `(0, 1]` (a non-positive or `> 1` config value is treated as
    /// disabled, `1.0`).
    #[must_use]
    pub fn foreign_env_weight(&self) -> f32 {
        let weight = self.config.sync.foreign_env_weight;
        if weight > 0.0 && weight <= 1.0 {
            weight
        } else {
            1.0
        }
    }
}

/// The per-OS user home directory, used to root the machine-wide global memory
/// store. Resolved cross-platform (Windows `USERPROFILE`, Unix `HOME`); `None`
/// when unset so the caller can fall back rather than panic.
#[cfg(windows)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

#[cfg(not(windows))]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
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
    #[error("global memory root {global_memory_root:?} in {path:?} must be an absolute path with no `..`")]
    UnsafeGlobalMemoryRoot {
        path: PathBuf,
        global_memory_root: PathBuf,
    },
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn discover(toml: &str) -> ProjectConfig {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE_NAME), toml).unwrap();
        ProjectConfig::discover(dir.path()).unwrap()
    }

    #[test]
    fn semantic_dedup_is_off_without_the_flag_and_endpoint() {
        // Default: no flag, no endpoint → lexical only.
        let plain = discover("[learning]\nenabled = true\n");
        assert!(!plain.semantic_dedup_active());

        // Flag on but no embedding endpoint → still lexical (the fallback holds).
        let flag_only = discover("[learning]\nenabled = true\n\n[review]\nsemantic_dedup = true\n");
        assert!(!flag_only.semantic_dedup_active());

        // Endpoint configured but flag off → opt-in, so still lexical.
        let endpoint_only = discover(
            "[learning]\nenabled = true\n\n[inference]\nembedding_base_url = \"http://127.0.0.1:1\"\n",
        );
        assert!(!endpoint_only.semantic_dedup_active());
    }

    #[test]
    fn semantic_dedup_activates_only_with_both_flag_and_endpoint() {
        let active = discover(
            "[learning]\nenabled = true\n\n[review]\nsemantic_dedup = true\n\n[inference]\nembedding_base_url = \"http://127.0.0.1:1\"\n",
        );
        assert!(active.semantic_dedup_active());
    }

    #[test]
    fn rerank_is_off_by_default_and_needs_both_flag_and_endpoint() {
        // Default: deterministic blend only.
        let plain = discover("[learning]\nenabled = true\n");
        assert!(!plain.rerank_active());
        assert_eq!(plain.rerank_window(), 20);

        // Flag on but no embedding endpoint → still the blend floor.
        let flag_only = discover("[learning]\nenabled = true\n\n[retrieval]\nrerank = true\n");
        assert!(!flag_only.rerank_active());

        // Endpoint but flag off → opt-in, so still the blend floor.
        let endpoint_only = discover(
            "[learning]\nenabled = true\n\n[inference]\nembedding_base_url = \"http://127.0.0.1:1\"\n",
        );
        assert!(!endpoint_only.rerank_active());
    }

    #[test]
    fn rerank_activates_only_with_both_flag_and_endpoint() {
        let active = discover(
            "[learning]\nenabled = true\n\n[retrieval]\nrerank = true\nrerank_window = 8\n\n[inference]\nembedding_base_url = \"http://127.0.0.1:1\"\n",
        );
        assert!(active.rerank_active());
        assert_eq!(active.rerank_window(), 8);
    }
}
