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

use config::{Config, Overrides, Settings};
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

fn test_connection(router: &Router) -> bool {
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
    {
        let settings = looper.router.primary();
        ui::banner(&settings.model, &settings.name, &config::endpoint(settings));
    }

    loop {
        ui::prompt();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line == "/quit" {
            break;
        }
        if line == "/clear" {
            looper.clear();
            ui::info("Conversation cleared.");
            continue;
        }
        if line == "/help" || line == "/?" {
            print_help();
            continue;
        }
        if line == "/config" {
            print_config(&looper.router, path);
            continue;
        }
        if line == "/feed" || line == "/timeline" {
            print_feed(&looper.feed);
            continue;
        }
        if line == "/tools" {
            for spec in tools::SPECS {
                println!("  {:<6} {}", spec.name, spec.description);
            }
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
            let target = if argument.is_empty() {
                print_providers(path, config);
                match setup::ask("Switch to (blank to cancel)", None) {
                    Some(value) => value,
                    None => continue,
                }
            } else {
                argument.to_string()
            };
            if target.is_empty() {
                continue;
            }
            match switch_provider(&target, config, path) {
                Ok(settings) => {
                    looper.router = build_router(&settings, config);
                    println!(
                        "{}Switched to {} ({}){}",
                        ui::green(),
                        settings.name,
                        settings.model,
                        ui::reset()
                    );
                }
                Err(err) => ui::error(&err),
            }
            continue;
        }
        if line == "/models" {
            match looper.router.available_models() {
                Ok(models) => {
                    let current = looper.router.primary().model.as_str();
                    println!(
                        "{}Models available from provider{}",
                        ui::bold(),
                        ui::reset()
                    );
                    for model in models {
                        let marker = if model == current { "*" } else { " " };
                        println!("  {} {}", marker, model);
                    }
                    println!("Set one with: /model MODEL_ID");
                }
                Err(err) => ui::error(&format!("could not load provider models: {err}")),
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("/model") {
            let argument = rest.trim();
            if argument.is_empty() {
                println!("model: {}", looper.router.primary().model);
                continue;
            }
            looper.router.primary_mut().model = argument.to_string();
            let name = looper.router.primary().name.clone();
            let profile = config.providers.entry(name.clone()).or_default();
            if presets::find(&name).is_some() && profile.preset.is_none() {
                profile.preset = Some(name.clone());
            }
            profile.model = Some(argument.to_string());
            config.active = Some(name);
            if let Err(err) = config::save_config(path, config) {
                ui::error(&err);
            } else {
                println!("{}Model set to {}{}", ui::green(), argument, ui::reset());
            }
            continue;
        }
        if line.starts_with('/') {
            ui::warn("Unknown command. Try /help.");
            continue;
        }
        if let Err(err) = looper.run_turn(line) {
            ui::error(&err);
        }
    }
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

fn print_feed(feed: &Feed) {
    if feed.is_empty() {
        ui::info("(empty feed)");
        return;
    }
    let timeline = feed.timeline();
    println!(
        "{}Feed timeline{} ({} events)",
        ui::bold(),
        ui::reset(),
        feed.len()
    );
    for (role, preview, age) in timeline {
        println!("  {:>9}  {:>4}s  {}", role, age, preview);
    }
}

fn print_help() {
    println!("{}Commands{}", ui::bold(), ui::reset());
    println!("  /provider [name]  list or switch provider");
    println!("  /model [name]     show or set model");
    println!("  /models           fetch available models from provider");
    println!("  /config           show active route");
    println!("  /feed, /timeline  show the feed timeline");
    println!("  /tools            list tools");
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
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
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
