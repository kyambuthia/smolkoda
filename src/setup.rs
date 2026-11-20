use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::time::Duration;

use crate::config::{self, Config};
use crate::presets::{self, Auth, Protocol, PRESETS};
use crate::router::{ModelInfo, Router};
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

    let items = provider_items(config);
    let active_index = config.active.as_deref().and_then(|name| {
        let preset = config
            .providers
            .get(name)
            .and_then(|profile| profile.preset.as_deref())
            .unwrap_or(name);
        PRESETS.iter().position(|candidate| candidate.id == preset)
    });
    let default_index = preferred_id
        .and_then(|id| PRESETS.iter().position(|preset| preset.id == id))
        .or_else(|| {
            PRESETS
                .iter()
                .position(|preset| presets::default_key_ref(Some(preset)).is_some())
        })
        .or(active_index)
        .unwrap_or(0);
    let choice = if ui::can_select() {
        match ui::select("Choose a provider", &items, default_index) {
            Some(index) if index == PRESETS.len() => "0".to_string(),
            Some(index) => (index + 1).to_string(),
            None => return Ok(false),
        }
    } else {
        for (index, item) in items.iter().enumerate() {
            if index == PRESETS.len() {
                println!("   0. {item}");
            } else {
                println!("  {:2}. {item}", index + 1);
            }
        }
        let default_choice = (default_index + 1).to_string();
        match ask("Provider", Some(&default_choice)) {
            Some(choice) => choice,
            None => return Ok(false),
        }
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
                    let mut models: Vec<ModelInfo> = preset
                        .models
                        .iter()
                        .map(|model| ModelInfo {
                            id: (*model).to_string(),
                            pricing: None,
                        })
                        .collect();
                    if !crate::router::is_local_url(&base_url) {
                        crate::router::enrich_pricing_from_public_catalog(&mut models);
                    }
                    models
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

fn provider_items(config: &Config) -> Vec<String> {
    let mut items: Vec<String> = PRESETS
        .iter()
        .map(|preset| {
            let active = if config.active.as_deref() == Some(preset.id) {
                " *"
            } else {
                ""
            };
            format!(
                "{:<11} {:<9} {}{}",
                preset.id,
                preset.protocol.as_str(),
                preset.label,
                active
            )
        })
        .collect();
    items.push(format!(
        "{:<11} {:<9} {}",
        "custom", "compatible", "Any OpenAI- or Anthropic-compatible endpoint"
    ));
    items
}

fn discover_models(
    base_url: &str,
    auth: Auth,
    raw_key: &str,
    headers: &[(String, String)],
) -> Option<Vec<ModelInfo>> {
    let api_key = config::resolve_secret(raw_key);
    if auth != Auth::None && api_key.is_empty() {
        ui::warn("No API key available; skipping model discovery.");
        return None;
    }
    match crate::router::fetch_models(base_url, auth, &api_key, headers, Duration::from_secs(30)) {
        Ok(models) => {
            let priced = models
                .iter()
                .filter(|model| model.pricing.is_some())
                .count();
            if priced > 0 {
                println!(
                    "{}Loaded {} models from provider ({} with pricing).{}",
                    ui::green(),
                    models.len(),
                    priced,
                    ui::reset()
                );
            } else {
                println!(
                    "{}Loaded {} models from provider.{}",
                    ui::green(),
                    models.len(),
                    ui::reset()
                );
            }
            Some(models)
        }
        Err(err) => {
            ui::warn(&format!("Could not load provider models: {err}"));
            None
        }
    }
}

fn ask_model(default: Option<&str>, models: Option<&[ModelInfo]>) -> Option<String> {
    let Some(models) = models.filter(|models| !models.is_empty()) else {
        return ask("Model", default);
    };
    let suggested_index = default
        .and_then(|model| models.iter().position(|candidate| candidate.id == model))
        .unwrap_or(0);
    if ui::can_select() {
        return ui::select_model("Choose a model", models, suggested_index)
            .map(|index| models[index].id.clone());
    }
    let suggested = models.get(suggested_index).map(|model| model.id.as_str());
    println!("\n{}Available models{}", ui::bold(), ui::reset());
    for (index, model) in models.iter().enumerate() {
        let marker = if Some(model.id.as_str()) == suggested {
            "*"
        } else {
            " "
        };
        println!("  {:2}. {} {}", index + 1, marker, model.display_label());
    }
    let choice = ask("Model number or ID", suggested)?;
    if let Ok(number) = choice.parse::<usize>() {
        if let Some(model) = number.checked_sub(1).and_then(|index| models.get(index)) {
            return Some(model.id.clone());
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
    let router = Router::new(vec![settings]);
    crate::test_connection(&router);
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
