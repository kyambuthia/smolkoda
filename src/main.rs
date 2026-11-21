mod config;
mod feed;
mod looper;
mod presets;
mod router;
mod setup;
mod tools;
mod ui;
mod voice;

use std::io::{self, IsTerminal};
use std::path::Path;

use config::{Config, Overrides, Profile, Settings};
use feed::{Block, Feed};
use looper::Looper;
use presets::{Auth, PRESETS};
use router::Router;

#[derive(Default)]
struct Args {
    prompt: Vec<String>,
    setup: bool,
    list_providers: bool,
    provider: Option<String>,
    model: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    protocol: Option<String>,
    auth: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f64>,
    test: bool,
    config: Option<String>,
    help: bool,
    version: bool,
}

fn main() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("{err}");
            print_usage();
            std::process::exit(2);
        }
    };
    if args.help {
        print_usage();
        return;
    }
    if args.version {
        println!("smolkoda {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let path = config::config_path(args.config.as_deref());
    let mut config = config::load_config(&path);

    if args.list_providers {
        print_providers(&path, &config);
        return;
    }

    if args.setup {
        if !io::stdin().is_terminal() {
            eprintln!("setup needs an interactive terminal; edit the config instead");
            std::process::exit(1);
        }
        match setup::setup_wizard(&mut config, &path, args.provider.as_deref()) {
            Ok(true) => return,
            Ok(false) => std::process::exit(1),
            Err(err) => {
                ui::error(&err);
                std::process::exit(1);
            }
        }
    }

    let mut name = config::pick_provider(&config, args.provider.as_deref());
    let overrides = Overrides {
        model: args.model.clone(),
        base_url: args.base_url.clone(),
        api_key: args.api_key.clone(),
        protocol: args.protocol.clone(),
        auth: args.auth.clone(),
        max_tokens: args.max_tokens,
        temperature: args.temperature,
    };

    let mut settings = match config::build_settings(&name, &config, &overrides) {
        Ok(settings) => settings,
        Err(err) => {
            ui::error(&err);
            std::process::exit(1);
        }
    };

    if settings.auth != Auth::None && settings.api_key.is_empty() {
        ui::warn(&format!("No API key for '{}'.", settings.name));
        if io::stdin().is_terminal() {
            match setup::setup_wizard(&mut config, &path, Some(&settings.name)) {
                Ok(true) => {}
                Ok(false) => std::process::exit(1),
                Err(err) => {
                    ui::error(&err);
                    std::process::exit(1);
                }
            }
            name = config
                .active
                .clone()
                .unwrap_or_else(|| settings.name.clone());
            settings = match config::build_settings(&name, &config, &overrides) {
                Ok(settings) => settings,
                Err(err) => {
                    ui::error(&err);
                    std::process::exit(1);
                }
            };
            if settings.auth != Auth::None && settings.api_key.is_empty() {
                ui::error("Still no API key; set one with --setup or an env var.");
                std::process::exit(1);
            }
        } else {
            eprintln!("Set an API key env var or run: smolkoda --setup");
            std::process::exit(1);
        }
    }

    let router = build_router(&settings, &config);

    if args.test {
        std::process::exit(if test_connection(&router) { 0 } else { 1 });
    }

    let mut looper = Looper::new(router);
    if !args.prompt.is_empty() {
        let prompt = args.prompt.join(" ");
        if let Err(err) = looper.run_turn(&prompt) {
            ui::error(&err);
            std::process::exit(1);
        }
        return;
    }

    repl(&mut looper, &mut config, &path);
}

fn build_router(settings: &Settings, config: &Config) -> Router {
    let mut routes = vec![settings.clone()];
    for fallback in &settings.fallbacks {
        if fallback == &settings.name {
            continue;
        }
        match config::build_settings(fallback, config, &Overrides::default()) {
            Ok(route) => routes.push(route),
            Err(err) => ui::warn(&format!("fallback '{fallback}' skipped: {err}")),
        }
    }
    Router::new(routes)
}

pub(crate) fn test_connection(router: &Router) -> bool {
    let settings = router.primary();
    ui::info(&format!(
        "testing {} -> {} ...",
        settings.name,
        config::endpoint(settings)
    ));
    let mut feed = Feed::default();
    feed.push_user("Reply with exactly: OK");
    match router.complete_with_tools(&feed, "", Some(32), false) {
        Ok(blocks) => {
            let text = blocks
                .iter()
                .filter_map(|block| match block {
                    Block::Text(text) => Some(text.as_str()),
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
            true
        }
        Err(err) => {
            ui::error(&format!("connection failed: {err}"));
            false
        }
    }
}

fn repl(looper: &mut Looper, config: &mut Config, path: &Path) {
    let mut dock = ui::PromptDock::new();
    {
        let settings = looper.router.primary();
        if dock.is_active() {
            dock.push(ui::FeedEntry::Banner {
                model: settings.model.clone(),
                provider: settings.name.clone(),
            });
        } else {
            ui::banner(&settings.model, &settings.name, &config::endpoint(settings));
        }
        dock.redraw(&settings.model);
    }

    loop {
        let active_model = looper.router.primary().model.clone();
        let Some(raw_line) = dock.read_line(&active_model) else {
            break;
        };
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }
        if line == "/clear" {
            looper.clear();
            if dock.is_active() {
                dock.clear_feed();
                dock.push(ui::FeedEntry::Info {
                    text: "Conversation cleared.".to_string(),
                    is_error: false,
                });
            } else {
                ui::info("Conversation cleared.");
            }
            continue;
        }
        if line == "/help" || line == "/?" {
            if dock.is_active() {
                dock.push(ui::FeedEntry::Info {
                    text: help_text(),
                    is_error: false,
                });
            } else {
                print_help();
            }
            continue;
        }
        if line == "/config" {
            print_config(&looper.router, path);
            continue;
        }

        if line == "/voice" || line.starts_with("/voice ") {
            let argument = line.strip_prefix("/voice").unwrap_or("").trim();
            let seconds = if argument.is_empty() {
                voice::DEFAULT_RECORD_SECONDS
            } else {
                match argument.parse::<u64>() {
                    Ok(seconds @ 1..=60) => seconds,
                    _ => {
                        ui::warn("Usage: /voice [seconds] (choose 1 to 60)");
                        continue;
                    }
                }
            };
            if let Err(err) = voice::run(looper, config, seconds) {
                ui::error(&err);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/provider") {
            let argument = rest.trim();
            let current = looper.router.primary().name.clone();
            let (target, force_setup) = if argument.is_empty() {
                if ui::can_select() {
                    let (items, targets) = switch_provider_items(config, &current);
                    let initial = targets
                        .iter()
                        .position(|target| target == &current)
                        .unwrap_or(0);
                    match ui::select("Switch or reconfigure provider", &items, initial) {
                        Some(index) => {
                            let chosen = targets[index].clone();
                            let reconfig = chosen == current;
                            (chosen, reconfig)
                        }
                        None => continue,
                    }
                } else {
                    print_providers(path, config);
                    match setup::ask("Switch to (blank to cancel)", None) {
                        Some(value) => {
                            let reconfig = value == current;
                            (value, reconfig)
                        }
                        None => continue,
                    }
                }
            } else if argument == "setup" {
                (current.clone(), true)
            } else if let Some(name) = argument.strip_suffix(" setup") {
                (name.trim().to_string(), true)
            } else {
                (argument.to_string(), false)
            };
            if target.is_empty() {
                continue;
            }
            dock.suspend();
            let switched = if force_setup {
                reconfigure_provider(&target, config, path)
            } else {
                switch_provider(&target, config, path)
            };
            match switched {
                Ok(settings) => {
                    looper.router = build_router(&settings, config);
                    dock.resume(&settings.model);
                    let msg = format!("Active provider: {} ({})", settings.name, settings.model);
                    if dock.is_active() {
                        dock.push(ui::FeedEntry::Info {
                            text: msg,
                            is_error: false,
                        });
                    } else {
                        println!("{}{}{}", ui::green(), msg, ui::reset());
                    }
                }
                Err(err) => {
                    dock.resume(&looper.router.primary().model);
                    if dock.is_active() {
                        dock.push(ui::FeedEntry::Info {
                            text: format!("error: {err}"),
                            is_error: true,
                        });
                    } else {
                        ui::error(&err);
                    }
                }
            }
            continue;
        }
        if line == "/model" || line.starts_with("/model ") {
            let argument = line.strip_prefix("/model").unwrap_or("").trim();
            if argument.is_empty() {
                match ui::busy(ui::AnimKind::Fetch, "fetching models", || {
                    looper.router.available_models()
                }) {
                    Ok(models) if models.is_empty() => {
                        ui::info("No models returned by the provider.");
                    }
                    Ok(models) => {
                        let current = looper.router.primary().model.clone();
                        let selected = models
                            .iter()
                            .position(|model| model.id == current)
                            .unwrap_or(0);
                        if ui::can_select() {
                            if let Some(index) =
                                ui::select_model("Choose a model", &models, selected)
                            {
                                set_model(looper, config, path, &models[index].id);
                                dock.redraw(&looper.router.primary().model);
                            }
                        } else {
                            println!(
                                "{}Models available from provider{}",
                                ui::bold(),
                                ui::reset()
                            );
                            for model in &models {
                                let marker = if model.id == current { "*" } else { " " };
                                println!("  {} {}", marker, model.display_label());
                            }
                            println!("Set one with: /model MODEL_ID");
                        }
                    }
                    Err(err) => ui::error(&format!("could not load provider models: {err}")),
                }
                continue;
            }
            set_model(looper, config, path, argument);
            continue;
        }
        if line == "/cost" {
            if dock.is_active() {
                dock.push(ui::FeedEntry::Info {
                    text: cost_text(&looper.router),
                    is_error: false,
                });
            } else {
                print_cost(&looper.router);
            }
            continue;
        }
        if line.starts_with('/') {
            if dock.is_active() {
                dock.push(ui::FeedEntry::Info {
                    text: "Unknown command. Try /help.".to_string(),
                    is_error: true,
                });
            } else {
                ui::warn("Unknown command. Try /help.");
            }
            continue;
        }
        if let Err(err) = looper.run_turn_with_dock(line, dock.is_active().then_some(&mut dock)) {
            if dock.is_active() {
                dock.push(ui::FeedEntry::Info {
                    text: format!("error: {err}"),
                    is_error: true,
                });
            } else {
                ui::error(&err);
            }
        }
    }
}

fn reconfigure_provider(name: &str, config: &mut Config, path: &Path) -> Result<Settings, String> {
    if presets::find(name).is_none() && !config.providers.contains_key(name) {
        return Err(format!("unknown provider '{name}'; try /provider"));
    }
    if !setup::setup_wizard(config, path, Some(name))? {
        return Err("cancelled".to_string());
    }
    let active = config.active.clone().unwrap_or_else(|| name.to_string());
    let settings = config::build_settings(&active, config, &Overrides::default())?;
    config.active = Some(settings.name.clone());
    config::save_config(path, config)?;
    Ok(settings)
}

fn switch_provider(name: &str, config: &mut Config, path: &Path) -> Result<Settings, String> {
    if presets::find(name).is_none() && !config.providers.contains_key(name) {
        return Err(format!("unknown provider '{name}'; try /provider"));
    }
    let mut settings = config::build_settings(name, config, &Overrides::default())?;
    if settings.auth != Auth::None && settings.api_key.is_empty() {
        ui::warn(&format!("No API key configured for '{name}'."));
        if !setup::setup_wizard(config, path, Some(name))? {
            return Err("cancelled".to_string());
        }
        let active = config.active.clone().unwrap_or_else(|| name.to_string());
        settings = config::build_settings(&active, config, &Overrides::default())?;
    }
    config.active = Some(settings.name.clone());
    config::save_config(path, config)?;
    Ok(settings)
}

fn set_model(looper: &mut Looper, config: &mut Config, path: &Path, model: &str) {
    looper.router.primary_mut().model = model.to_string();
    let name = looper.router.primary().name.clone();
    let profile = config.providers.entry(name.clone()).or_insert_with(|| {
        let mut profile = Profile::default();
        if presets::find(&name).is_some() {
            profile.preset = Some(name.clone());
        }
        profile
    });
    profile.model = Some(model.to_string());
    config.active = Some(name);
    if let Err(err) = config::save_config(path, config) {
        ui::error(&err);
    } else {
        println!("{}Model set to {}{}", ui::green(), model, ui::reset());
    }
}

fn switch_provider_items(config: &Config, current: &str) -> (Vec<String>, Vec<String>) {
    let mut items = Vec::new();
    let mut targets = Vec::new();

    for preset in PRESETS {
        if config.providers.contains_key(preset.id) {
            continue;
        }
        let marker = if preset.id == current { "*" } else { " " };
        items.push(format!(
            "{} {:<11} {:<9} {}",
            marker,
            preset.id,
            preset.protocol.as_str(),
            preset.label
        ));
        targets.push(preset.id.to_string());
    }

    for (name, profile) in &config.providers {
        let marker = if name.as_str() == current { "*" } else { " " };
        items.push(format!(
            "{} {:<11} {:<12} {}",
            marker,
            name,
            profile.preset.as_deref().unwrap_or("custom"),
            profile.model.as_deref().unwrap_or("?")
        ));
        targets.push(name.clone());
    }

    (items, targets)
}

fn print_providers(path: &Path, config: &Config) {
    println!("{}Provider presets{}", ui::bold(), ui::reset());
    for preset in PRESETS {
        let marker = if config.active.as_deref() == Some(preset.id) {
            "*"
        } else {
            " "
        };
        let env = if preset.env.is_empty() {
            "-".to_string()
        } else {
            preset.env.join(", ")
        };
        println!(
            " {} {:<11} {:<9} {:<42} {}",
            marker,
            preset.id,
            preset.protocol.as_str(),
            preset.default_model,
            env
        );
    }
    if !config.providers.is_empty() {
        println!("\n{}Saved profiles{}", ui::bold(), ui::reset());
        for (name, profile) in &config.providers {
            let marker = if config.active.as_deref() == Some(name.as_str()) {
                "*"
            } else {
                " "
            };
            println!(
                " {} {:<11} preset: {:<11} model: {}",
                marker,
                name,
                profile.preset.as_deref().unwrap_or("custom"),
                profile.model.as_deref().unwrap_or("?")
            );
        }
    }
    println!("\n{}Config: {}{}", ui::dim(), path.display(), ui::reset());
    println!(
        "{}Add your own with: smolkoda --setup{}",
        ui::dim(),
        ui::reset()
    );
}

fn print_config(router: &Router, path: &Path) {
    let settings = router.primary();
    println!("\n{}Active route{}", ui::bold(), ui::reset());
    println!("  profile:  {}", settings.name);
    println!("  protocol: {}", settings.protocol.as_str());
    println!("  base url: {}", settings.base_url);
    println!("  model:    {}", settings.model);
    println!(
        "  auth:     {} ({})",
        settings.auth.as_str(),
        config::mask_secret(&settings.api_key)
    );
    println!("  max toks: {}", settings.max_tokens);
    println!("  routes:   {}", router.route_names().join(" -> "));
    println!("  config:   {}", path.display());
}

fn cost_text(router: &Router) -> String {
    let route = router.primary();
    let usage = router.usage();
    let mut lines = vec![
        format!("  provider: {}", route.name),
        format!("  model:    {}", route.model),
        format!(
            "  tokens:   {} in / {} out",
            usage.input_tokens, usage.output_tokens
        ),
    ];
    if usage.has_reported_cost {
        lines.push(format!(
            "  cost:     {}",
            router::format_usd(usage.reported_cost_usd)
        ));
        return lines.join("\n");
    }
    if let Ok(models) = router.available_models() {
        if let Some(pricing) = models
            .into_iter()
            .find(|model| model.id == route.model)
            .and_then(|model| model.pricing)
        {
            lines.push(format!("  rate:     {}", pricing.format_label()));
            let estimated = pricing.estimate_cost(usage.input_tokens, usage.output_tokens);
            lines.push(format!("  cost:     ~{}", router::format_usd(estimated)));
            return lines.join("\n");
        }
    }
    lines.push("  cost:     unavailable (provider did not report pricing)".to_string());
    lines.join("\n")
}

fn help_text() -> String {
    [
        "Commands",
        "  /provider [name]  open provider picker or switch",
        "  /model [name]     choose a model or set its ID",
        "  /cost             show session token usage and cost",
        "  /config           show active route",
        "  /voice [seconds]  record a prompt and speak the reply",
        "  /clear            clear conversation",
        "  /quit             quit",
    ]
    .join("\n")
}

fn print_cost(router: &Router) {
    let route = router.primary();
    let usage = router.usage();
    println!("  provider: {}", route.name);
    println!("  model:    {}", route.model);
    println!(
        "  tokens:   {} in / {} out",
        usage.input_tokens, usage.output_tokens
    );
    if usage.has_reported_cost {
        println!(
            "  cost:     {}",
            router::format_usd(usage.reported_cost_usd)
        );
        return;
    }
    if let Ok(models) = router.available_models() {
        if let Some(pricing) = models
            .into_iter()
            .find(|model| model.id == route.model)
            .and_then(|model| model.pricing)
        {
            println!("  rate:     {}", pricing.format_label());
            let estimated = pricing.estimate_cost(usage.input_tokens, usage.output_tokens);
            println!("  cost:     ~{}", router::format_usd(estimated));
            return;
        }
    }
    println!("  cost:     unavailable (provider did not report pricing)");
}

fn print_help() {
    println!("{}Commands{}", ui::bold(), ui::reset());
    println!("  /provider [name]  open provider picker or switch");
    println!("  /model [name]     choose a model or set its ID");
    println!("  /cost             show session token usage and cost");
    println!("  /config           show active route");
    println!("  /voice [seconds]  record a prompt and speak the reply");
    println!("  /clear            clear conversation");
    println!("  /quit             quit");
}

fn print_usage() {
    println!(
        "smolkoda {} - tiny agentic coding harness\n",
        env!("CARGO_PKG_VERSION")
    );
    println!("USAGE:");
    println!("  smolkoda [OPTIONS] [PROMPT...]\n");
    println!("OPTIONS:");
    println!("  --setup                 interactive provider setup");
    println!("  --list-providers        list presets and saved profiles");
    println!("  --provider NAME         preset or saved profile");
    println!("  --model MODEL           override the model");
    println!("  --base-url URL          override the API base URL");
    println!("  --api-key KEY           override the API key");
    println!("  --protocol openai|anthropic");
    println!("  --auth bearer|api-key|x-api-key|none");
    println!("  --max-tokens N          override max output tokens");
    println!("  --temperature N         override temperature");
    println!("  --test                  test the provider and exit");
    println!("  --config PATH           config file path");
    println!("  -h, --help              show this help");
    println!("  -V, --version           show version\n");
    println!("ENV:");
    println!("  SMOLKODA_PROVIDER, SMOLKODA_MODEL, SMOLKODA_BASE_URL");
    println!("  SMOLKODA_API_KEY, SMOLKODA_PROTOCOL, SMOLKODA_CONFIG");
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(mut iter: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut args = Args::default();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--" => {
                args.prompt.extend(iter);
                break;
            }
            "--setup" => args.setup = true,
            "--list-providers" => args.list_providers = true,
            "--test" => args.test = true,
            "--help" | "-h" => args.help = true,
            "--version" | "-V" => args.version = true,
            "--provider" => args.provider = Some(next_value(&mut iter, "--provider")?),
            "--model" => args.model = Some(next_value(&mut iter, "--model")?),
            "--base-url" => args.base_url = Some(next_value(&mut iter, "--base-url")?),
            "--api-key" => args.api_key = Some(next_value(&mut iter, "--api-key")?),
            "--protocol" => args.protocol = Some(next_value(&mut iter, "--protocol")?),
            "--auth" => args.auth = Some(next_value(&mut iter, "--auth")?),
            "--max-tokens" => {
                args.max_tokens = Some(
                    next_value(&mut iter, "--max-tokens")?
                        .parse()
                        .map_err(|_| "--max-tokens must be a number".to_string())?,
                )
            }
            "--temperature" => {
                args.temperature = Some(
                    next_value(&mut iter, "--temperature")?
                        .parse()
                        .map_err(|_| "--temperature must be a number".to_string())?,
                )
            }
            "--config" => args.config = Some(next_value(&mut iter, "--config")?),
            _ => {
                if let Some(value) = arg.strip_prefix("--provider=") {
                    args.provider = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--model=") {
                    args.model = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--base-url=") {
                    args.base_url = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--api-key=") {
                    args.api_key = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--protocol=") {
                    args.protocol = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--auth=") {
                    args.auth = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--max-tokens=") {
                    args.max_tokens = Some(
                        value
                            .parse()
                            .map_err(|_| "--max-tokens must be a number".to_string())?,
                    );
                } else if let Some(value) = arg.strip_prefix("--temperature=") {
                    args.temperature = Some(
                        value
                            .parse()
                            .map_err(|_| "--temperature must be a number".to_string())?,
                    );
                } else if let Some(value) = arg.strip_prefix("--config=") {
                    args.config = Some(value.to_string());
                } else if arg.starts_with('-') {
                    return Err(format!("unknown option '{arg}'"));
                } else {
                    args.prompt.push(arg);
                }
            }
        }
    }
    Ok(args)
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next().ok_or_else(|| format!("{flag} needs a value"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_args_supports_end_of_options_marker() {
        let args = parse_args_from(
            ["--provider", "openai", "--", "--not-a-flag", "hello"]
                .into_iter()
                .map(str::to_string),
        )
        .unwrap();
        assert_eq!(args.provider.as_deref(), Some("openai"));
        assert_eq!(args.prompt, vec!["--not-a-flag", "hello"]);
    }

    #[test]
    fn set_model_preserves_custom_profile_without_preset() {
        let dir = std::env::temp_dir().join(format!("smolkoda-main-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");

        let mut config = Config::default();
        config.providers.insert(
            "openai".to_string(),
            Profile {
                base_url: Some("http://localhost:8080/v1".to_string()),
                model: Some("local-a".to_string()),
                ..Profile::default()
            },
        );
        let settings = Settings {
            name: "openai".to_string(),
            protocol: presets::Protocol::OpenAi,
            base_url: "http://localhost:8080/v1".to_string(),
            model: "local-a".to_string(),
            api_key: String::new(),
            auth: Auth::None,
            headers: Vec::new(),
            max_tokens: 8192,
            max_tokens_param: "max_tokens".to_string(),
            temperature: None,
            timeout: Duration::from_secs(30),
            fallbacks: Vec::new(),
        };
        let mut looper = Looper::new(Router::new(vec![settings]));

        set_model(&mut looper, &mut config, &path, "local-b");
        let saved = config.providers.get("openai").unwrap();
        assert_eq!(saved.preset, None);
        assert_eq!(saved.model.as_deref(), Some("local-b"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
