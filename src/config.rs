use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::presets::{self, Auth, Preset, Protocol, PRESETS};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Profile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub name: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub auth: Auth,
    pub headers: Vec<(String, String)>,
    pub max_tokens: u32,
    pub max_tokens_param: String,
    pub temperature: Option<f64>,
    pub timeout: Duration,
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub protocol: Option<String>,
    pub auth: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
}

pub fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

pub fn config_path(override_path: Option<&str>) -> PathBuf {
    if let Some(path) = override_path {
        return PathBuf::from(path);
    }
    if let Some(path) = env_nonempty("SMOLKODA_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(xdg) = env_nonempty("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("smolkoda").join("config.json");
    }
    let home = env_nonempty("HOME")
        .or_else(|| env_nonempty("USERPROFILE"))
        .unwrap_or_else(|| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("smolkoda")
        .join("config.json")
}

pub fn load_config(path: &Path) -> Config {
    if !path.exists() {
        return Config::default();
    }
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<Config>(&text) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("warning: could not parse {}: {err}", path.display());
                Config::default()
            }
        },
        Err(err) => {
            eprintln!("warning: could not read {}: {err}", path.display());
            Config::default()
        }
    }
}

pub fn save_config(path: &Path, config: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
        }
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|err| format!("could not serialize config: {err}"))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let temp = path.with_file_name(format!("{file_name}.tmp"));
    std::fs::write(&temp, format!("{text}\n"))
        .map_err(|err| format!("could not write {}: {err}", temp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&temp, path)
        .map_err(|err| format!("could not replace {}: {err}", path.display()))?;
    Ok(())
}

pub fn resolve_secret(value: &str) -> String {
    let value = value.trim();
    if let Some(name) = value.strip_prefix("env:") {
        return std::env::var(name.trim()).unwrap_or_default();
    }
    if value.starts_with("${") && value.ends_with('}') {
        return std::env::var(&value[2..value.len() - 1]).unwrap_or_default();
    }
    value.to_string()
}

pub fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return "(none)".to_string();
    }
    if value.starts_with("env:") || value.starts_with("${") {
        return value.to_string();
    }
    if value.chars().count() <= 8 {
        return "***".to_string();
    }
    let prefix: String = value.chars().take(4).collect();
    let suffix: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}...{suffix}")
}

pub fn pick_provider(config: &Config, requested: Option<&str>) -> String {
    if let Some(name) = requested {
        return name.to_string();
    }
    if let Some(name) = env_nonempty("SMOLKODA_PROVIDER") {
        return name;
    }
    if let Some(name) = &config.active {
        return name.clone();
    }
    for id in presets::AUTO_ORDER {
        if let Some(preset) = presets::find(id) {
            if preset
                .env
                .iter()
                .any(|name| std::env::var_os(name).is_some())
            {
                return (*id).to_string();
            }
        }
    }
    if let Some((name, _)) = config.providers.iter().next() {
        return name.clone();
    }
    "openrouter".to_string()
}

fn profile_number_u32(
    override_value: Option<u32>,
    env_name: &str,
    profile_value: Option<u32>,
    default: u32,
) -> Result<u32, String> {
    if let Some(value) = override_value {
        return Ok(value);
    }
    if let Some(value) = env_nonempty(env_name) {
        return value
            .parse()
            .map_err(|_| format!("{env_name} must be a number"));
    }
    Ok(profile_value.unwrap_or(default))
}

fn profile_number_u64(
    env_name: &str,
    profile_value: Option<u64>,
    default: u64,
) -> Result<u64, String> {
    if let Some(value) = env_nonempty(env_name) {
        return value
            .parse()
            .map_err(|_| format!("{env_name} must be a number"));
    }
    Ok(profile_value.unwrap_or(default))
}

pub fn build_settings(
    name: &str,
    config: &Config,
    overrides: &Overrides,
) -> Result<Settings, String> {
    let profile = config.providers.get(name);
    // A saved profile with no `preset` is custom, even if its name matches a preset.
    let preset: Option<&Preset> = match profile {
        Some(profile) => profile.preset.as_deref().and_then(presets::find),
        None => presets::find(name),
    };

    if preset.is_none() && profile.is_none() {
        let mut known: Vec<String> = PRESETS.iter().map(|preset| preset.id.to_string()).collect();
        known.extend(config.providers.keys().cloned());
        known.sort();
        known.dedup();
        return Err(format!(
            "unknown provider '{name}' (known: {})",
            known.join(", ")
        ));
    }

    let protocol_value = overrides
        .protocol
        .clone()
        .or_else(|| env_nonempty("SMOLKODA_PROTOCOL"))
        .or_else(|| profile.and_then(|profile| profile.protocol.clone()))
        .or_else(|| preset.map(|preset| preset.protocol.as_str().to_string()))
        .unwrap_or_else(|| "openai".to_string());
    let protocol = Protocol::parse(&protocol_value)
        .ok_or_else(|| format!("invalid protocol '{protocol_value}' (use openai or anthropic)"))?;

    let auth_value = overrides
        .auth
        .clone()
        .or_else(|| profile.and_then(|profile| profile.auth.clone()))
        .or_else(|| preset.map(|preset| preset.auth.as_str().to_string()))
        .unwrap_or_else(|| "bearer".to_string());
    let auth = Auth::parse(&auth_value).ok_or_else(|| {
        format!("invalid auth '{auth_value}' (use bearer, api-key, x-api-key, none)")
    })?;

    let base_url = overrides
        .base_url
        .clone()
        .or_else(|| env_nonempty("SMOLKODA_BASE_URL"))
        .or_else(|| profile.and_then(|profile| profile.base_url.clone()))
        .or_else(|| preset.map(|preset| preset.base_url.to_string()))
        .unwrap_or_default();

    let model = overrides
        .model
        .clone()
        .or_else(|| env_nonempty("SMOLKODA_MODEL"))
        .or_else(|| profile.and_then(|profile| profile.model.clone()))
        .or_else(|| preset.map(|preset| preset.default_model.to_string()))
        .unwrap_or_default();

    let raw_key = if let Some(key) = &overrides.api_key {
        key.clone()
    } else if let Some(key) = env_nonempty("SMOLKODA_API_KEY") {
        key
    } else if let Some(key) = profile.and_then(|profile| profile.api_key.clone()) {
        key
    } else {
        presets::default_key_ref(preset).unwrap_or_default()
    };
    let api_key = resolve_secret(&raw_key);

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    if let Some(preset) = preset {
        for (key, value) in preset.headers {
            headers.insert((*key).to_string(), (*value).to_string());
        }
    }
    if let Some(profile) = profile {
        for (key, value) in &profile.headers {
            headers.insert(key.clone(), value.clone());
        }
    }

    let max_tokens = profile_number_u32(
        overrides.max_tokens,
        "SMOLKODA_MAX_TOKENS",
        profile.and_then(|profile| profile.max_tokens),
        8192,
    )?;
    let timeout_secs = profile_number_u64(
        "SMOLKODA_TIMEOUT",
        profile.and_then(|profile| profile.timeout),
        180,
    )?;
    let temperature = if let Some(value) = overrides.temperature {
        Some(value)
    } else if let Some(value) = env_nonempty("SMOLKODA_TEMPERATURE") {
        Some(
            value
                .parse()
                .map_err(|_| "SMOLKODA_TEMPERATURE must be a number".to_string())?,
        )
    } else {
        profile.and_then(|profile| profile.temperature)
    };
    let max_tokens_param = profile
        .and_then(|profile| profile.max_tokens_param.clone())
        .or_else(|| preset.map(|preset| preset.max_tokens_param.to_string()))
        .unwrap_or_else(|| "max_tokens".to_string());
    let fallbacks = profile
        .map(|profile| profile.fallbacks.clone())
        .unwrap_or_default();

    if base_url.is_empty() {
        return Err(format!("provider '{name}' has no base URL; run --setup"));
    }
    if model.is_empty() {
        return Err(format!("provider '{name}' has no model; run --setup"));
    }

    Ok(Settings {
        name: name.to_string(),
        protocol,
        base_url,
        model,
        api_key,
        auth,
        headers: headers.into_iter().collect(),
        max_tokens,
        max_tokens_param,
        temperature,
        timeout: Duration::from_secs(timeout_secs),
        fallbacks,
    })
}

pub fn endpoint(settings: &Settings) -> String {
    let url = settings.base_url.trim_end_matches('/');
    match settings.protocol {
        Protocol::Anthropic => {
            if url.contains("/messages") {
                url.to_string()
            } else {
                format!("{url}/messages")
            }
        }
        Protocol::OpenAi => {
            if url.contains("/chat/completions") {
                url.to_string()
            } else {
                format!("{url}/chat/completions")
            }
        }
    }
}
