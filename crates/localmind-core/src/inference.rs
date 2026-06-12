use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InferenceSettings {
    pub chat_base_url: Option<String>,
    pub chat_model: Option<String>,
    pub embedding_base_url: Option<String>,
    pub embedding_model: Option<String>,
    pub api_key_env: Option<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub features: InferenceFeatureSettings,
}

impl InferenceSettings {
    #[must_use]
    pub fn embedding_base_url(&self) -> Option<&str> {
        self.embedding_base_url
            .as_deref()
            .or(self.chat_base_url.as_deref())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InferenceFeatureSettings {
    #[serde(default = "default_feature_enabled")]
    pub extraction: bool,
    #[serde(default = "default_feature_enabled")]
    pub review: bool,
    #[serde(default = "default_feature_enabled")]
    pub embeddings: bool,
    #[serde(default = "default_feature_enabled")]
    pub skills: bool,
    #[serde(default = "default_feature_enabled")]
    pub research: bool,
}

impl Default for InferenceFeatureSettings {
    fn default() -> Self {
        Self {
            extraction: true,
            review: true,
            embeddings: true,
            skills: true,
            research: true,
        }
    }
}

fn default_timeout_secs() -> u64 {
    120
}

fn default_feature_enabled() -> bool {
    true
}
