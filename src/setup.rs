use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use crate::config::{self, Config};
use crate::feed::Feed;
use crate::presets::{self, Auth, Protocol, PRESETS};
use crate::router::Router;
use crate::ui;

pub fn setup_wizard(
    config: &mut Config,
    path: &Path,
    preferred: Option<&str>,
) -> Result<bool, String> {
    let preferred_profile = preferred
        .filter(|name| config.providers.contains_key(*name))
        .map(str::to_string);
    let preferred_preset = preferred_profile
        .as_ref()
        .and_then(|name| config.providers.get(name))
        .and_then(|profile| profile.preset.clone());
    let preferred_id = preferred_preset.as_deref().or(preferred);

    println!("\n{}smolkoda provider setup{}", ui::bold(), ui::reset());
    println!(
        "{}Choose a provider, paste its API key, then select a model.{}",
        ui::dim(),
        ui::reset()
    );
    println!("{}Config: {}{}\n", ui::dim(), path.display(), ui::reset());

    for (index, preset) in PRESETS.iter().enumerate() {
        let active = if config.active.as_deref() == Some(preset.id) {
            format!(" {}*{}", ui::green(), ui::reset())
        } else {
            String::new()
        };
        println!(
            "  {:2}. {:<11} {}{}",
            index + 1,
            preset.id,
            preset.label,
            active
        );
    }
    println!(
        "   0. {:<11} Any OpenAI- or Anthropic-compatible endpoint",
        "custom"
    );

    let default_choice = preferred_id
        .and_then(|id| PRESETS.iter().position(|preset| preset.id == id))
        .or_else(|| {
            PRESETS
                .iter()
                .position(|preset| presets::default_key_ref(Some(preset)).is_some())
        })
        .map(|index| (index + 1).to_string());
    let choice = match ask("Provider", default_choice.as_deref()) {
        Some(choice) => choice,
        None => return Ok(false),
    };

    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let (preset_id, protocol, base_url, auth, key, model, default_name) = if choice == "0" {
        let protocol_text = match ask("Protocol (openai/anthropic)", Some("openai")) {
            Some(value) => value,
            None => return Ok(false),
        };
        let protocol = Protocol::parse(&protocol_text)
            .ok_or_else(|| format!("invalid protocol '{protocol_text}'"))?;
        let base_url = match ask("Base URL", None) {
            Some(value) if !value.is_empty() => value,
            _ => return Ok(false),
        };
        let auth_text = match ask(
            "Auth header (bearer/api-key/x-api-key/none)",
            Some("bearer"),
        ) {
            Some(value) => value,
            None => return Ok(false),
        };
        let auth = Auth::parse(&auth_text).ok_or_else(|| format!("invalid auth '{auth_text}'"))?;
        let key = if auth == Auth::None {
            String::new()
        } else {
            match ask_secret("Paste API key (hidden; or env:VAR)", None) {
                Some(value) => value,
                None => return Ok(false),
            }
        };
        if let Some(raw_headers) = ask("Extra headers as JSON (optional)", None) {
            if !raw_headers.is_empty() {
                match serde_json::from_str::<BTreeMap<String, String>>(&raw_headers) {
                    Ok(parsed) => headers = parsed,
                    Err(_) => ui::warn("Invalid headers JSON; skipped."),
                }
            }
        }
        let model_headers: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
        let available_models = discover_models(&base_url, auth, &key, &model_headers);
        let model = match ask_model(None, available_models.as_deref()) {
            Some(value) if !value.is_empty() => value,
            _ => return Ok(false),
        };
        let default_name = preferred_profile
            .clone()
            .unwrap_or_else(|| "custom".to_string());
        (None, protocol, base_url, auth, key, model, default_name)
    } else {
        let index = choice
            .parse::<usize>()
            .ok()
            .and_then(|value| value.checked_sub(1))
            .filter(|index| *index < PRESETS.len());
        let preset = match index {
            Some(index) => &PRESETS[index],
            None => {
                ui::warn("Invalid choice.");
                return Ok(false);
            }
        };
        println!(
            "\n{}{} - {} - {}{}",
            ui::dim(),
            preset.label,
            preset.protocol.as_str(),
            preset.base_url,
            ui::reset()
        );
        if !preset.note.is_empty() {
            println!("{}{}{}", ui::dim(), preset.note, ui::reset());
        }
        if !preset.models.is_empty() {
            println!(
                "{}models: {}{}",
                ui::dim(),
                preset.models.join(", "),
                ui::reset()
            );
        }
        let base_url = preset.base_url.to_string();
        let key = if preset.auth == Auth::None {
            println!("{}No API key needed.{}", ui::dim(), ui::reset());
            String::new()
        } else if let Some(key_ref) = presets::default_key_ref(Some(preset)) {
            let env_name = key_ref.strip_prefix("env:").unwrap_or("provider key");
            println!(
                "{}Using {env_name} from the environment.{}",
                ui::dim(),
                ui::reset()
            );
            key_ref
        } else {
            match ask_secret("Paste API key (hidden; or env:VAR)", None) {
                Some(value) => value,
                None => return Ok(false),
            }
        };
        let model_headers: Vec<(String, String)> = preset
            .headers
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        let available_models = discover_models(&base_url, preset.auth, &key, &model_headers)
            .filter(|models| !models.is_empty())
            .or_else(|| {
                (!preset.models.is_empty()).then(|| {
                    preset
                        .models
                        .iter()
                        .map(|model| (*model).to_string())
                        .collect()
                })
            });
        let model = match ask_model(Some(preset.default_model), available_models.as_deref()) {
            Some(value) if !value.is_empty() => value,
            _ => return Ok(false),
        };
        let default_name = if preferred_preset.as_deref() == Some(preset.id) {
            preferred_profile
                .clone()
                .unwrap_or_else(|| preset.id.to_string())
        } else {
            preset.id.to_string()
        };
        (
            Some(preset.id),
            preset.protocol,
            base_url,
            preset.auth,
            key,
            model,
            default_name,
        )
    };

    let is_preset = preset_id.is_some();
    let name = if is_preset {
        default_name
    } else {
        match ask("Profile name", Some(&default_name)) {
            Some(value) if !value.is_empty() => value,
            _ => return Ok(false),
        }
    };
    if config.providers.contains_key(&name) {
        let confirm = ask(
            &format!("Profile '{name}' exists; overwrite? (y/N)"),
            Some("n"),
        );
        if !matches!(confirm.as_deref(), Some("y") | Some("Y") | Some("yes")) {
            return Ok(false);
        }
    }

    let mut profile = config.providers.get(&name).cloned().unwrap_or_default();
    profile.model = Some(model);
    profile.base_url = Some(base_url);
    profile.api_key = Some(key.clone());
    profile.max_tokens_param = None;
    if let Some(preset_id) = preset_id {
        profile.preset = Some(preset_id.to_string());
        profile.protocol = None;
        profile.auth = None;
        if let Some(preset) = presets::find(preset_id) {
            if auth != preset.auth {
                profile.auth = Some(auth.as_str().to_string());
            }
            if protocol != preset.protocol {
                profile.protocol = Some(protocol.as_str().to_string());
            }
        }
    } else {
        profile.preset = None;
        profile.protocol = Some(protocol.as_str().to_string());
        profile.auth = Some(auth.as_str().to_string());
    }
    if !headers.is_empty() {
        profile.headers = headers;
    }

    config.providers.insert(name.clone(), profile);
    config.active = Some(name.clone());
    config::save_config(path, config)?;
    println!(
        "\n{}Saved profile '{}' to {}{}",
        ui::green(),
        name,
        path.display(),
        ui::reset()
    );

    let missing_key = auth != Auth::None && config::resolve_secret(&key).is_empty();
    if missing_key {
        ui::warn("No API key found; set it before chatting.");
    } else if is_preset {
        println!("Run `smolkoda --test` to verify the connection.");
    } else if matches!(
        ask("Test connection now? (Y/n)", Some("y")).as_deref(),
        Some("y") | Some("Y") | Some("yes")
    ) {
        test_profile(config, &name);
    }
    Ok(true)
}

fn discover_models(
    base_url: &str,
    auth: Auth,
    raw_key: &str,
    headers: &[(String, String)],
) -> Option<Vec<String>> {
    let api_key = config::resolve_secret(raw_key);
    if auth != Auth::None && api_key.is_empty() {
        ui::warn("No API key available; skipping model discovery.");
        return None;
    }
    match crate::router::fetch_model_ids(base_url, auth, &api_key, headers, Duration::from_secs(30))
    {
        Ok(models) => {
            println!(
                "{}Loaded {} models from provider.{}",
                ui::green(),
                models.len(),
                ui::reset()
            );
            Some(models)
        }
        Err(err) => {
            ui::warn(&format!("Could not load provider models: {err}"));
            None
        }
    }
}

fn ask_model(default: Option<&str>, models: Option<&[String]>) -> Option<String> {
    let Some(models) = models.filter(|models| !models.is_empty()) else {
        return ask("Model", default);
    };
    let suggested = default
        .filter(|model| models.iter().any(|candidate| candidate == model))
        .or_else(|| models.first().map(String::as_str));
    println!("\n{}Available models{}", ui::bold(), ui::reset());
    for (index, model) in models.iter().enumerate() {
        let marker = if Some(model.as_str()) == suggested {
            "*"
        } else {
            " "
        };
        println!("  {:2}. {} {}", index + 1, marker, model);
    }
    let choice = ask("Model number or ID", suggested)?;
    if let Ok(number) = choice.parse::<usize>() {
        if let Some(model) = number.checked_sub(1).and_then(|index| models.get(index)) {
            return Some(model.clone());
        }
        ui::warn("Invalid model number; keeping the suggested model.");
        return suggested.map(str::to_string);
    }
    Some(choice)
}

fn test_profile(config: &Config, name: &str) {
    let settings = match config::build_settings(name, config, &config::Overrides::default()) {
        Ok(settings) => settings,
        Err(err) => {
            ui::error(&err);
            return;
        }
    };
    let route_name = settings.name.clone();
    let endpoint = config::endpoint(&settings);
    ui::info(&format!("testing {route_name} -> {endpoint} ..."));
    let router = Router::new(vec![settings]);
    let mut feed = Feed::default();
    feed.push_user("Reply with exactly: OK");
    match router.complete_with_tools(&feed, "", Some(32), false) {
        Ok(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    crate::feed::Block::Text(text) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            println!(
                "{}connected{}",
                ui::green(),
                if text.is_empty() {
                    String::new()
                } else {
                    format!(" - {}", text.chars().take(80).collect::<String>())
                }
            );
        }
        Err(err) => ui::error(&format!("connection failed: {err}")),
    }
}

pub fn ask(prompt: &str, default: Option<&str>) -> Option<String> {
    let suffix = default
        .map(|value| format!(" [{value}]"))
        .unwrap_or_default();
    print!("{}{}{}{}: ", ui::bold(), prompt, ui::reset(), suffix);
    io::stdout().flush().ok()?;
    let mut line = String::new();
    if io::stdin().read_line(&mut line).ok()? == 0 {
        return None;
    }
    let value = line.trim().to_string();
    if value.is_empty() {
        default.map(str::to_string)
    } else {
        Some(value)
    }
}

fn ask_secret(prompt: &str, default: Option<&str>) -> Option<String> {
    let suffix = default
        .map(|value| format!(" [{value}]"))
        .unwrap_or_default();
    let full = format!("{}{}{}{}: ", ui::bold(), prompt, ui::reset(), suffix);
    let raw = if io::stdin().is_terminal() {
        rpassword::prompt_password(&full).ok()?
    } else {
        print!("{full}");
        io::stdout().flush().ok()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line).ok()? == 0 {
            return None;
        }
        line
    };
    let value = raw.trim().to_string();
    if value.is_empty() {
        Some(default.unwrap_or("").to_string())
    } else {
        Some(value)
    }
}
