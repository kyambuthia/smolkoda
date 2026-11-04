use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use serde_json::Value;

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && io::stdout().is_terminal())
}

pub fn reset() -> &'static str {
    if color_enabled() {
        "\x1b[0m"
    } else {
        ""
    }
}

pub fn bold() -> &'static str {
    if color_enabled() {
        "\x1b[1m"
    } else {
        ""
    }
}

pub fn dim() -> &'static str {
    if color_enabled() {
        "\x1b[2m"
    } else {
        ""
    }
}

pub fn blue() -> &'static str {
    if color_enabled() {
        "\x1b[34m"
    } else {
        ""
    }
}

pub fn cyan() -> &'static str {
    if color_enabled() {
        "\x1b[36m"
    } else {
        ""
    }
}

pub fn green() -> &'static str {
    if color_enabled() {
        "\x1b[32m"
    } else {
        ""
    }
}

pub fn yellow() -> &'static str {
    if color_enabled() {
        "\x1b[33m"
    } else {
        ""
    }
}

pub fn red() -> &'static str {
    if color_enabled() {
        "\x1b[31m"
    } else {
        ""
    }
}

pub fn prompt() {
    print!("{}{}>{} ", bold(), blue(), reset());
    let _ = io::stdout().flush();
}

pub fn banner(model: &str, provider: &str, endpoint: &str) {
    println!(
        "{}smolkoda{} {}v{} - {} - {}{}",
        bold(),
        reset(),
        dim(),
        env!("CARGO_PKG_VERSION"),
        model,
        provider,
        reset()
    );
    println!("{}{}{}", dim(), endpoint, reset());
    println!("{}/help for commands, /q to quit{}\n", dim(), reset());
}

pub fn print_text(text: &str) {
    let mut lines = text.lines();
    match lines.next() {
        Some(first) => println!("\n{}*{} {}", cyan(), reset(), first),
        None => println!(),
    }
    for line in lines {
        println!("  {line}");
    }
}

pub fn print_tool(name: &str, args: &Value) {
    let preview = args
        .as_object()
        .and_then(|object| object.iter().next())
        .map(|(_, value)| value_preview(value))
        .unwrap_or_default();
    println!(
        "\n{}* {}{}({}{}{})",
        green(),
        capitalize(name),
        reset(),
        dim(),
        preview,
        reset()
    );
}

pub fn print_result(result: &str) {
    let lines: Vec<&str> = result.lines().collect();
    let first = lines.first().copied().unwrap_or("");
    let mut preview: String = first.chars().take(80).collect();
    if lines.len() > 1 {
        preview.push_str(&format!(" ... +{} lines", lines.len() - 1));
    } else if first.chars().count() > 80 {
        preview.push_str("...");
    }
    println!("  {}-> {}{}", dim(), preview, reset());
}

pub fn error(message: &str) {
    println!("{}Error: {}{}", red(), message, reset());
}

pub fn warn(message: &str) {
    println!("{}{}{}", yellow(), message, reset());
}

pub fn info(message: &str) {
    println!("{}{}{}", dim(), message, reset());
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn value_preview(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.replace('\n', " "),
        other => other.to_string(),
    };
    let mut preview: String = text.chars().take(50).collect();
    if text.chars().count() > 50 {
        preview.push_str("...");
    }
    preview
}
