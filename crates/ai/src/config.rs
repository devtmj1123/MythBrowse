// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing

//! `ai.toml` configuration: providers plus which provider serves each role.
//!
//! ```toml
//! default_provider = "local"
//!
//! [providers.local]
//! kind = "ollama"
//! model = "qwen2.5:7b"
//!
//! [providers.openai]
//! kind = "openai_compatible"
//! base_url = "https://api.openai.com/v1"
//! api_key_env = "OPENAI_API_KEY"
//! model = "gpt-4o-mini"
//!
//! [roles]
//! chat = "openai"
//! fast = "local"
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const DEFAULT_OLLAMA_URL: &str = "http://localhost:11434";
pub const DEFAULT_OLLAMA_KEEP_ALIVE: &str = "5m";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not parse AI config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("no providers configured")]
    NoProviders,
    #[error("default provider `{0}` is not defined in [providers]")]
    UnknownDefault(String),
    #[error("role `{role}` points at undefined provider `{provider}`")]
    UnknownRoleProvider {
        role: &'static str,
        provider: String,
    },
    #[error("provider `{name}`: base_url `{url}` must start with http:// or https://")]
    BadBaseUrl { name: String, url: String },
    #[error("provider `{name}`: model must not be empty")]
    EmptyModel { name: String },
    #[error("provider `{name}`: set only one of api_key_env / api_key_ref")]
    AmbiguousKey { name: String },
    #[error("could not read or write the AI settings: {0}")]
    Io(String),
}

/// One configured model backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    /// Any server speaking the OpenAI Chat Completions API (OpenAI, OpenRouter,
    /// Groq, LM Studio, vLLM, ...).
    OpenaiCompatible {
        /// Base URL including the version segment, e.g. `https://api.openai.com/v1`.
        base_url: String,
        model: String,
        /// Name of an environment variable holding the key.
        #[serde(default)]
        api_key_env: Option<String>,
        /// Name of an OS credential-store entry (Windows Credential Manager / Secret Service).
        #[serde(default)]
        api_key_ref: Option<String>,
    },
    /// Local Ollama server.
    Ollama {
        #[serde(default = "default_ollama_url")]
        base_url: String,
        model: String,
        /// How long Ollama keeps the model loaded after a request (unload when idle).
        #[serde(default = "default_keep_alive")]
        keep_alive: String,
    },
}

fn default_ollama_url() -> String {
    DEFAULT_OLLAMA_URL.to_owned()
}

fn default_keep_alive() -> String {
    DEFAULT_OLLAMA_KEEP_ALIVE.to_owned()
}

impl ProviderConfig {
    pub fn base_url(&self) -> &str {
        match self {
            Self::OpenaiCompatible { base_url, .. } | Self::Ollama { base_url, .. } => base_url,
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Self::OpenaiCompatible { model, .. } | Self::Ollama { model, .. } => model,
        }
    }

    /// True when the provider runs on this machine (nothing leaves the PC).
    pub fn is_local(&self) -> bool {
        let authority = self
            .base_url()
            .split_once("://")
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .split('/')
            .next()
            .unwrap_or("");
        let host = if authority.starts_with('[') {
            authority.split(']').next().map(|h| &h[1..]).unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        matches!(host, "localhost" | "127.0.0.1" | "::1")
    }

    /// The URL a chat request will be sent to.
    pub fn chat_endpoint(&self) -> String {
        let base = self.base_url().trim_end_matches('/');
        match self {
            Self::OpenaiCompatible { .. } => format!("{base}/chat/completions"),
            Self::Ollama { .. } => format!("{base}/api/chat"),
        }
    }
}

/// Model roles from the design: chat, fast/cheap, vision, embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Chat,
    Fast,
    Vision,
    Embeddings,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Chat, Role::Fast, Role::Vision, Role::Embeddings];

    pub fn name(self) -> &'static str {
        match self {
            Role::Chat => "chat",
            Role::Fast => "fast",
            Role::Vision => "vision",
            Role::Embeddings => "embeddings",
        }
    }
}

/// Provider name per role; unset roles fall back to `default_provider`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roles {
    pub chat: Option<String>,
    pub fast: Option<String>,
    pub vision: Option<String>,
    pub embeddings: Option<String>,
}

impl Roles {
    fn get(&self, role: Role) -> Option<&String> {
        match role {
            Role::Chat => self.chat.as_ref(),
            Role::Fast => self.fast.as_ref(),
            Role::Vision => self.vision.as_ref(),
            Role::Embeddings => self.embeddings.as_ref(),
        }
    }
}

/// Whether page screenshots are sent to the model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VisionMode {
    /// Use images when the model is known (or probed) to accept them.
    #[default]
    Auto,
    /// Always send images (the user says the model can see).
    On,
    /// Never send images.
    Off,
}

impl VisionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            VisionMode::Auto => "auto",
            VisionMode::On => "on",
            VisionMode::Off => "off",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(VisionMode::Auto),
            "on" | "true" => Some(VisionMode::On),
            "off" | "false" => Some(VisionMode::Off),
            _ => None,
        }
    }
}

/// `[vision]` in `ai.toml`.
///
/// ```toml
/// [vision]
/// mode = "auto"            # auto | on | off
/// send_screenshots = true  # privacy switch: false = never send a screenshot anywhere
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisionSettings {
    #[serde(default)]
    pub mode: VisionMode,
    /// Privacy: when false no screenshot ever leaves the browser (no `look` tool, no
    /// automatic screenshots, no "Ask about this page" image), whatever `mode` says.
    #[serde(default = "yes")]
    pub send_screenshots: bool,
}

fn yes() -> bool {
    true
}

impl Default for VisionSettings {
    fn default() -> Self {
        Self {
            mode: VisionMode::Auto,
            send_screenshots: true,
        }
    }
}

impl VisionSettings {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Screenshots may be sent at all (privacy switch on and mode not off).
    pub fn allowed(&self) -> bool {
        self.send_screenshots && self.mode != VisionMode::Off
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiConfig {
    pub default_provider: String,
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub roles: Roles,
    #[serde(default, skip_serializing_if = "VisionSettings::is_default")]
    pub vision: VisionSettings,
}

impl Default for AiConfig {
    /// Privacy-first default: local Ollama only.
    fn default() -> Self {
        let mut providers = BTreeMap::new();
        providers.insert(
            "local".to_owned(),
            ProviderConfig::Ollama {
                base_url: default_ollama_url(),
                model: "qwen2.5:7b".to_owned(),
                keep_alive: default_keep_alive(),
            },
        );
        Self {
            default_provider: "local".to_owned(),
            providers,
            roles: Roles::default(),
            vision: VisionSettings::default(),
        }
    }
}

impl AiConfig {
    /// Parse and validate `ai.toml` contents.
    pub fn from_toml_str(text: &str) -> Result<Self, ConfigError> {
        let cfg: AiConfig = toml::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.providers.is_empty() {
            return Err(ConfigError::NoProviders);
        }
        if !self.providers.contains_key(&self.default_provider) {
            return Err(ConfigError::UnknownDefault(self.default_provider.clone()));
        }
        for role in Role::ALL {
            if let Some(name) = self.roles.get(role) {
                if !self.providers.contains_key(name) {
                    return Err(ConfigError::UnknownRoleProvider {
                        role: role.name(),
                        provider: name.clone(),
                    });
                }
            }
        }
        for (name, p) in &self.providers {
            let url = p.base_url();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(ConfigError::BadBaseUrl {
                    name: name.clone(),
                    url: url.to_owned(),
                });
            }
            if p.model().trim().is_empty() {
                return Err(ConfigError::EmptyModel { name: name.clone() });
            }
            if let ProviderConfig::OpenaiCompatible {
                api_key_env: Some(_),
                api_key_ref: Some(_),
                ..
            } = p
            {
                return Err(ConfigError::AmbiguousKey { name: name.clone() });
            }
        }
        Ok(())
    }

    /// Name and config of the provider serving `role`.
    pub fn provider_for(&self, role: Role) -> (&str, &ProviderConfig) {
        let name = self.roles.get(role).unwrap_or(&self.default_provider);
        // validate() guarantees presence; fall back to the default defensively.
        match self.providers.get_key_value(name) {
            Some((n, p)) => (n.as_str(), p),
            None => {
                let (n, p) = self
                    .providers
                    .get_key_value(&self.default_provider)
                    .expect("validated config has its default provider");
                (n.as_str(), p)
            }
        }
    }

    /// True if every role resolves to a local provider ("Local-only" privacy mode).
    pub fn is_local_only(&self) -> bool {
        Role::ALL.iter().all(|r| self.provider_for(*r).1.is_local())
    }
}

/// File name of the AI settings inside the profile directory.
pub const CONFIG_FILE: &str = "ai.toml";

/// Name of the provider the settings page edits.
pub const MAIN_PROVIDER: &str = "main";

/// Secret-store entry holding the API key of provider `name`.
pub fn secret_name(name: &str) -> String {
    let clean: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("provider.{clean}")
}

impl AiConfig {
    /// A config with one provider, used for every role.
    pub fn single(name: &str, provider: ProviderConfig) -> Self {
        let mut providers = BTreeMap::new();
        providers.insert(name.to_owned(), provider);
        Self {
            default_provider: name.to_owned(),
            providers,
            roles: Roles::default(),
            vision: VisionSettings::default(),
        }
    }

    /// Read `<dir>/ai.toml`. `Ok(None)` when the file does not exist (not set up yet).
    pub fn load(dir: &std::path::Path) -> Result<Option<Self>, ConfigError> {
        match std::fs::read_to_string(dir.join(CONFIG_FILE)) {
            Ok(text) => Self::from_toml_str(&text).map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ConfigError::Io(e.to_string())),
        }
    }

    /// Validate and write `<dir>/ai.toml`.
    pub fn save(&self, dir: &std::path::Path) -> Result<(), ConfigError> {
        self.validate()?;
        let text = toml::to_string(self).map_err(|e| ConfigError::Io(e.to_string()))?;
        std::fs::create_dir_all(dir).map_err(|e| ConfigError::Io(e.to_string()))?;
        let header = "# MythBrowse AI settings. API keys are not stored here (see secrets/).\n";
        std::fs::write(dir.join(CONFIG_FILE), format!("{header}{text}"))
            .map_err(|e| ConfigError::Io(e.to_string()))
    }
}

impl ProviderConfig {
    /// The API key for this provider: from its environment variable or the secret store.
    pub fn resolve_key(
        &self,
        store: &dyn crate::secrets::SecretStore,
    ) -> Option<crate::secrets::Secret> {
        let (env, reference) = match self {
            ProviderConfig::OpenaiCompatible {
                api_key_env,
                api_key_ref,
                ..
            } => (api_key_env.as_deref(), api_key_ref.as_deref()),
            ProviderConfig::Ollama { .. } => (None, None),
        };
        if let Some(var) = env {
            if let Some(v) = std::env::var_os(var).and_then(|v| v.into_string().ok()) {
                let s = crate::secrets::Secret::new(v.trim());
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        reference
            .and_then(|r| store.get(r).ok().flatten())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
default_provider = "local"

[providers.local]
kind = "ollama"
model = "qwen2.5:7b"

[providers.openai]
kind = "openai_compatible"
base_url = "https://api.openai.com/v1/"
api_key_env = "OPENAI_API_KEY"
model = "gpt-4o-mini"

[roles]
chat = "openai"
fast = "local"
"#;

    #[test]
    fn parses_sample_with_defaults() {
        let cfg = AiConfig::from_toml_str(SAMPLE).unwrap();
        assert_eq!(cfg.providers.len(), 2);
        match &cfg.providers["local"] {
            ProviderConfig::Ollama {
                base_url,
                model,
                keep_alive,
            } => {
                assert_eq!(base_url, DEFAULT_OLLAMA_URL);
                assert_eq!(model, "qwen2.5:7b");
                assert_eq!(keep_alive, "5m");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn role_resolution_and_fallback() {
        let cfg = AiConfig::from_toml_str(SAMPLE).unwrap();
        assert_eq!(cfg.provider_for(Role::Chat).0, "openai");
        assert_eq!(cfg.provider_for(Role::Fast).0, "local");
        // Unset roles fall back to the default provider.
        assert_eq!(cfg.provider_for(Role::Vision).0, "local");
        assert_eq!(cfg.provider_for(Role::Embeddings).0, "local");
        assert!(!cfg.is_local_only());
    }

    #[test]
    fn endpoints() {
        let cfg = AiConfig::from_toml_str(SAMPLE).unwrap();
        assert_eq!(
            cfg.providers["openai"].chat_endpoint(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            cfg.providers["local"].chat_endpoint(),
            "http://localhost:11434/api/chat"
        );
        assert!(cfg.providers["local"].is_local());
        assert!(!cfg.providers["openai"].is_local());
    }

    #[test]
    fn locality_detection() {
        let mk = |url: &str| ProviderConfig::OpenaiCompatible {
            base_url: url.into(),
            model: "m".into(),
            api_key_env: None,
            api_key_ref: None,
        };
        assert!(mk("http://127.0.0.1:1234/v1").is_local());
        assert!(mk("http://[::1]:8080/v1").is_local());
        assert!(mk("http://localhost/v1").is_local());
        assert!(!mk("https://localhost.evil.com/v1").is_local());
        assert!(!mk("https://api.groq.com/openai/v1").is_local());
    }

    #[test]
    fn default_is_local_only() {
        let cfg = AiConfig::default();
        cfg.validate().unwrap();
        assert!(cfg.is_local_only());
    }

    #[test]
    fn rejects_unknown_role_provider() {
        let text = SAMPLE.replace("fast = \"local\"", "fast = \"groq\"");
        match AiConfig::from_toml_str(&text) {
            Err(ConfigError::UnknownRoleProvider { role, provider }) => {
                assert_eq!(role, "fast");
                assert_eq!(provider, "groq");
            }
            other => panic!("expected UnknownRoleProvider, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_default() {
        let text = SAMPLE.replace("default_provider = \"local\"", "default_provider = \"x\"");
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::UnknownDefault(_))
        ));
    }

    #[test]
    fn rejects_bad_url_and_empty_model() {
        let text = SAMPLE.replace("https://api.openai.com/v1/", "api.openai.com");
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::BadBaseUrl { .. })
        ));
        let text = SAMPLE.replace("gpt-4o-mini", " ");
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::EmptyModel { .. })
        ));
    }

    #[test]
    fn rejects_inline_api_key_and_unknown_kind() {
        // Keys must never live in the config file itself.
        let text = SAMPLE.replace("api_key_env = \"OPENAI_API_KEY\"", "api_key = \"sk-123\"");
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::Parse(_))
        ));
        let text = SAMPLE.replace("kind = \"ollama\"", "kind = \"llamafile\"");
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn rejects_both_key_sources() {
        let text = SAMPLE.replace(
            "api_key_env = \"OPENAI_API_KEY\"",
            "api_key_env = \"OPENAI_API_KEY\"\napi_key_ref = \"mythbrowse/openai\"",
        );
        assert!(matches!(
            AiConfig::from_toml_str(&text),
            Err(ConfigError::AmbiguousKey { .. })
        ));
    }

    #[test]
    fn rejects_empty_providers() {
        let text = "default_provider = \"a\"\n[providers]\n";
        assert!(matches!(
            AiConfig::from_toml_str(text),
            Err(ConfigError::NoProviders)
        ));
    }

    #[test]
    fn round_trips_through_toml() {
        let cfg = AiConfig::from_toml_str(SAMPLE).unwrap();
        let text = toml::to_string(&cfg).unwrap();
        assert_eq!(AiConfig::from_toml_str(&text).unwrap(), cfg);
    }
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use crate::secrets::{MemoryStore, Secret, SecretStore};

    #[test]
    fn save_load_and_key_resolution() {
        let dir = std::env::temp_dir().join(format!("mb-aicfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(AiConfig::load(&dir).unwrap().is_none());
        let name = MAIN_PROVIDER;
        let cfg = AiConfig::single(
            name,
            ProviderConfig::OpenaiCompatible {
                base_url: "https://api.example.com/v1".into(),
                model: "m".into(),
                api_key_env: None,
                api_key_ref: Some(secret_name(name)),
            },
        );
        cfg.save(&dir).unwrap();
        let text = std::fs::read_to_string(dir.join(CONFIG_FILE)).unwrap();
        assert!(!text.contains("sk-"));
        let back = AiConfig::load(&dir).unwrap().unwrap();
        assert_eq!(back, cfg);
        let store = MemoryStore::default();
        assert!(back.providers[name].resolve_key(&store).is_none());
        store
            .set(&secret_name(name), &Secret::new("sk-abc-123"))
            .unwrap();
        assert_eq!(
            back.providers[name].resolve_key(&store).unwrap().expose(),
            "sk-abc-123"
        );
        assert_eq!(secret_name("My Provider"), "provider.my-provider");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vision_settings_round_trip_and_default_is_omitted() {
        let mut c = AiConfig::single(
            MAIN_PROVIDER,
            ProviderConfig::Ollama {
                base_url: "http://localhost:11434".into(),
                model: "llava".into(),
                keep_alive: "5m".into(),
            },
        );
        let text = toml::to_string(&c).unwrap();
        assert!(!text.contains("[vision]"), "{text}");
        c.vision = VisionSettings {
            mode: VisionMode::Off,
            send_screenshots: false,
        };
        let text = toml::to_string(&c).unwrap();
        assert!(
            text.contains("[vision]") && text.contains("mode = \"off\""),
            "{text}"
        );
        let back: AiConfig = toml::from_str(&text).unwrap();
        assert_eq!(back.vision, c.vision);
        assert!(!back.vision.allowed());
        let partial: AiConfig =
            toml::from_str(&text.replace("send_screenshots = false\n", "")).unwrap();
        assert!(partial.vision.send_screenshots, "defaults to on");
        assert_eq!(VisionMode::parse("ON"), Some(VisionMode::On));
        assert_eq!(VisionMode::parse("maybe"), None);
    }
}
