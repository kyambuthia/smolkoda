use std::io::{self, IsTerminal, Write};
use std::sync::OnceLock;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::Value;

use crate::router::{format_usd, CostTier, ModelInfo};

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
    println!("{}/help for commands, /quit to quit{}\n", dim(), reset());
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

pub fn print_turn_usage(
    input_tokens: u64,
    output_tokens: u64,
    turn_cost: Option<f64>,
    session_cost: Option<f64>,
) {
    if input_tokens == 0 && output_tokens == 0 {
        return;
    }
    let mut line = format!("  tokens: {} in / {} out", input_tokens, output_tokens);
    if let Some(cost) = turn_cost {
        line.push_str(&format!(" · ~{}", format_usd(cost)));
    }
    if let Some(total) = session_cost {
        line.push_str(&format!(" (session: {})", format_usd(total)));
    }
    println!("{}{}{}", dim(), line, reset());
}

pub fn can_select() -> bool {
    if std::env::var("TERM").ok().as_deref() == Some("dumb") {
        return false;
    }
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Show an arrow-key list in an alternate screen and return the selected index.
///
/// Up/Down move one row, PageUp/PageDown move by a page, Home/End jump to the
/// edges, Enter selects, and Esc cancels. Callers should check `can_select`
/// first and keep their non-interactive prompt as a fallback.
pub fn select(prompt: &str, items: &[String], initial: usize) -> Option<usize> {
    if items.is_empty() || !can_select() {
        return None;
    }
    let _guard = TerminalGuard::enter().ok()?;
    let mut state = PickerState::new(items.len(), initial, 1);
    let mut stdout = io::stdout();

    loop {
        let (_, height) = terminal::size().ok()?;
        let viewport = usize::from(height).saturating_sub(2).max(1);
        state.resize(viewport);
        render_select(&mut stdout, prompt, items, &state).ok()?;

        match event::read().ok()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    return None;
                }
                match key.code {
                    KeyCode::Up => state.up(),
                    KeyCode::Char('k') if key.modifiers.is_empty() => state.up(),
                    KeyCode::Down => state.down(),
                    KeyCode::Char('j') if key.modifiers.is_empty() => state.down(),
                    KeyCode::PageUp => state.page_up(),
                    KeyCode::PageDown => state.page_down(),
                    KeyCode::Home => state.home(),
                    KeyCode::End => state.end(),
                    KeyCode::Enter => return Some(state.selected),
                    KeyCode::Esc => return None,
                    KeyCode::Char('q') if key.modifiers.is_empty() => return None,
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

/// Interactive model picker with aligned cost-tier columns, `/` search,
/// `Tab` tier filtering, and `s` price sorting.
pub fn select_model(prompt: &str, models: &[ModelInfo], initial: usize) -> Option<usize> {
    if models.is_empty() || !can_select() {
        return None;
    }
    let _guard = TerminalGuard::enter().ok()?;
    let mut stdout = io::stdout();
    let mut query = String::new();
    let mut filtering = false;
    let mut tier_filter: Option<CostTier> = None;
    let mut sort_by_price = false;

    let mut visible = filter_and_sort_model_indices(models, &query, tier_filter, sort_by_price);
    let initial_pos = visible.iter().position(|&idx| idx == initial).unwrap_or(0);
    let mut state = PickerState::new(visible.len().max(1), initial_pos, 1);

    loop {
        let (_, height) = terminal::size().ok()?;
        let viewport = usize::from(height).saturating_sub(2).max(1);
        state.resize(viewport);
        render_model_select(
            &mut stdout,
            prompt,
            models,
            &visible,
            &state,
            &query,
            filtering,
            tier_filter,
            sort_by_price,
        )
        .ok()?;

        match event::read().ok()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c'))
                {
                    return None;
                }
                if filtering {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            filtering = false;
                            continue;
                        }
                        KeyCode::Backspace => {
                            query.pop();
                        }
                        KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            query.push(ch);
                        }
                        _ => continue,
                    }
                    visible =
                        filter_and_sort_model_indices(models, &query, tier_filter, sort_by_price);
                    state = PickerState::new(visible.len().max(1), 0, viewport);
                    continue;
                }

                match key.code {
                    KeyCode::Up => state.up(),
                    KeyCode::Char('k') if key.modifiers.is_empty() => state.up(),
                    KeyCode::Down => state.down(),
                    KeyCode::Char('j') if key.modifiers.is_empty() => state.down(),
                    KeyCode::PageUp => state.page_up(),
                    KeyCode::PageDown => state.page_down(),
                    KeyCode::Home => state.home(),
                    KeyCode::End => state.end(),
                    KeyCode::Char('/') if key.modifiers.is_empty() => {
                        filtering = true;
                    }
                    KeyCode::Tab => {
                        tier_filter = next_tier_filter(tier_filter);
                        visible = filter_and_sort_model_indices(
                            models,
                            &query,
                            tier_filter,
                            sort_by_price,
                        );
                        state = PickerState::new(visible.len().max(1), 0, viewport);
                    }
                    KeyCode::Char('s') if key.modifiers.is_empty() => {
                        sort_by_price = !sort_by_price;
                        visible = filter_and_sort_model_indices(
                            models,
                            &query,
                            tier_filter,
                            sort_by_price,
                        );
                        state = PickerState::new(visible.len().max(1), 0, viewport);
                    }
                    KeyCode::Enter => {
                        if let Some(&model_idx) = visible.get(state.selected) {
                            return Some(model_idx);
                        }
                    }
                    KeyCode::Esc => {
                        if !query.is_empty() || tier_filter.is_some() {
                            query.clear();
                            tier_filter = None;
                            visible = filter_and_sort_model_indices(
                                models,
                                &query,
                                tier_filter,
                                sort_by_price,
                            );
                            state = PickerState::new(visible.len().max(1), 0, viewport);
                        } else {
                            return None;
                        }
                    }
                    KeyCode::Char('q') if key.modifiers.is_empty() => return None,
                    _ => {}
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn next_tier_filter(current: Option<CostTier>) -> Option<CostTier> {
    match current {
        None => Some(CostTier::Free),
        Some(CostTier::Free) => Some(CostTier::Cheap),
        Some(CostTier::Cheap) => Some(CostTier::Standard),
        Some(CostTier::Standard) => Some(CostTier::Expensive),
        Some(CostTier::Expensive) => None,
    }
}

pub fn filter_and_sort_model_indices(
    models: &[ModelInfo],
    query: &str,
    tier_filter: Option<CostTier>,
    sort_by_price: bool,
) -> Vec<usize> {
    let needle = query.trim().to_ascii_lowercase();
    let mut indices: Vec<usize> = models
        .iter()
        .enumerate()
        .filter(|(_, model)| {
            if !needle.is_empty() && !model.id.to_ascii_lowercase().contains(&needle) {
                return false;
            }
            if let Some(target_tier) = tier_filter {
                match model.pricing {
                    Some(pricing) => pricing.tier() == target_tier,
                    None => false,
                }
            } else {
                true
            }
        })
        .map(|(index, _)| index)
        .collect();

    if sort_by_price {
        indices.sort_by(|&a, &b| {
            let pa = models[a].pricing.map(|p| p.sort_key());
            let pb = models[b].pricing.map(|p| p.sort_key());
            match (pa, pb) {
                (Some(ka), Some(kb)) => ka
                    .partial_cmp(&kb)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| models[a].id.cmp(&models[b].id)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => models[a].id.cmp(&models[b].id),
            }
        });
    }
    indices
}

pub fn format_model_columns(model: &ModelInfo, width: usize) -> (String, String, Option<CostTier>) {
    let (tag, tier) = match model.pricing {
        Some(pricing) => (pricing.compact_label(), Some(pricing.tier())),
        None => (String::new(), None),
    };
    if tag.is_empty() || width < 20 {
        return (clip_line(&model.id, width), String::new(), tier);
    }
    let tag_len = tag.chars().count();
    let left_width = width.saturating_sub(tag_len + 2).max(1);
    let left = clip_line(&model.id, left_width);
    (left, tag, tier)
}

fn tier_color(tier: CostTier) -> Color {
    match tier {
        CostTier::Free => Color::Green,
        CostTier::Cheap => Color::Cyan,
        CostTier::Standard => Color::Yellow,
        CostTier::Expensive => Color::Red,
    }
}

#[allow(clippy::too_many_arguments)]
fn render_model_select(
    stdout: &mut io::Stdout,
    prompt: &str,
    models: &[ModelInfo],
    visible: &[usize],
    state: &PickerState,
    query: &str,
    filtering: bool,
    tier_filter: Option<CostTier>,
    sort_by_price: bool,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let width = usize::from(width).max(1);
    let height = usize::from(height).max(1);
    let content_width = width.saturating_sub(1).max(1);
    let viewport = height.saturating_sub(2).max(1);

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    let mut title = format!("{prompt} ({}/{})", visible.len(), models.len());
    if let Some(tier) = tier_filter {
        title.push_str(&format!(" [tier: {}]", tier.label()));
    }
    if sort_by_price {
        title.push_str(" [sorted: price]");
    }
    if filtering || !query.is_empty() {
        title.push_str(&format!(" /{}", query));
        if filtering {
            title.push('_');
        }
    }
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print(clip_line(&title, content_width)),
        SetAttribute(Attribute::Reset)
    )?;

    for row in 0..viewport {
        let y = (row + 1) as u16;
        queue!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine))?;
        let Some(&model_idx) = visible.get(state.offset + row) else {
            if row == 0 && visible.is_empty() {
                queue!(
                    stdout,
                    SetForegroundColor(Color::DarkGrey),
                    Print(clip_line("  (no matching models)", content_width)),
                    ResetColor
                )?;
            }
            continue;
        };
        let selected = state.offset + row == state.selected;
        let prefix = if selected { "> " } else { "  " };
        let row_width = content_width.saturating_sub(2).max(1);
        let (left, tag, tier) = format_model_columns(&models[model_idx], row_width);

        if selected {
            let combined = if tag.is_empty() {
                format!("{prefix}{left}")
            } else {
                format!("{prefix}{left}  {tag}")
            };
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(clip_line(&combined, content_width)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(prefix), Print(&left))?;
            if !tag.is_empty() {
                queue!(stdout, Print("  "))?;
                if let Some(t) = tier {
                    queue!(
                        stdout,
                        SetForegroundColor(tier_color(t)),
                        Print(&tag),
                        ResetColor
                    )?;
                } else {
                    queue!(stdout, Print(&tag))?;
                }
            }
        }
    }

    let footer_y = height.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
    let footer_hint = if filtering {
        "Type to filter  Backspace delete  Enter/Esc done"
    } else {
        "/ filter  Tab tier  s price-sort  Enter select  Esc cancel"
    };
    queue!(
        stdout,
        MoveTo(0, footer_y),
        Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::DarkGrey),
        Print(clip_line(footer_hint, content_width)),
        ResetColor
    )?;
    if !visible.is_empty() && visible.len() > viewport {
        let position = format!("{}/{}", state.selected + 1, visible.len());
        let x = width
            .saturating_sub(position.chars().count())
            .max(1)
            .min(usize::from(u16::MAX)) as u16;
        queue!(
            stdout,
            MoveTo(x, footer_y),
            SetForegroundColor(Color::DarkGrey),
            Print(position),
            ResetColor
        )?;
    }
    stdout.flush()
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = terminal::disable_raw_mode();
            return Err(err);
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

#[derive(Debug)]
struct PickerState {
    selected: usize,
    offset: usize,
    len: usize,
    height: usize,
}

impl PickerState {
    fn new(len: usize, selected: usize, height: usize) -> Self {
        let len = len.max(1);
        let mut state = Self {
            selected: selected.min(len - 1),
            offset: 0,
            len,
            height: height.clamp(1, len),
        };
        state.ensure_visible();
        state
    }

    fn resize(&mut self, height: usize) {
        self.height = height.clamp(1, self.len);
        self.ensure_visible();
    }

    fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.ensure_visible();
    }

    fn down(&mut self) {
        self.selected = (self.selected + 1).min(self.len - 1);
        self.ensure_visible();
    }

    fn page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.height);
        self.ensure_visible();
    }

    fn page_down(&mut self) {
        self.selected = (self.selected + self.height).min(self.len - 1);
        self.ensure_visible();
    }

    fn home(&mut self) {
        self.selected = 0;
        self.ensure_visible();
    }

    fn end(&mut self) {
        self.selected = self.len - 1;
        self.ensure_visible();
    }

    fn ensure_visible(&mut self) {
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + self.height {
            self.offset = self.selected + 1 - self.height;
        }
        self.offset = self.offset.min(self.len.saturating_sub(self.height));
    }
}

fn render_select(
    stdout: &mut io::Stdout,
    prompt: &str,
    items: &[String],
    state: &PickerState,
) -> io::Result<()> {
    let (width, height) = terminal::size()?;
    let width = usize::from(width).max(1);
    let height = usize::from(height).max(1);
    let content_width = width.saturating_sub(1).max(1);
    let viewport = height.saturating_sub(2).max(1);

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
    let title = format!("{prompt} ({} items)", items.len());
    queue!(
        stdout,
        SetAttribute(Attribute::Bold),
        Print(clip_line(&title, content_width)),
        SetAttribute(Attribute::Reset)
    )?;

    for row in 0..viewport {
        let y = (row + 1) as u16;
        queue!(stdout, MoveTo(0, y), Clear(ClearType::CurrentLine))?;
        let Some(item) = items.get(state.offset + row) else {
            continue;
        };
        let selected = state.offset + row == state.selected;
        let line = if selected {
            format!("> {item}")
        } else {
            format!("  {item}")
        };
        if selected {
            queue!(
                stdout,
                SetAttribute(Attribute::Reverse),
                Print(clip_line(&line, content_width)),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print(clip_line(&line, content_width)))?;
        }
    }

    let footer_y = height.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
    queue!(
        stdout,
        MoveTo(0, footer_y),
        Clear(ClearType::CurrentLine),
        SetForegroundColor(Color::DarkGrey),
        Print(clip_line(
            "Up/Down scroll  PgUp/PgDn page  Enter select  Esc cancel",
            content_width
        )),
        ResetColor
    )?;
    if items.len() > viewport {
        let position = format!("{}/{}", state.selected + 1, items.len());
        let x = width
            .saturating_sub(position.chars().count())
            .max(1)
            .min(usize::from(u16::MAX)) as u16;
        queue!(
            stdout,
            MoveTo(x, footer_y),
            SetForegroundColor(Color::DarkGrey),
            Print(position),
            ResetColor
        )?;
    }
    stdout.flush()
}

fn clip_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let length = text.chars().count();
    if length <= width {
        let mut line = text.to_string();
        for _ in length..width {
            line.push(' ');
        }
        line
    } else if width <= 3 {
        text.chars().take(width).collect()
    } else {
        let mut line: String = text.chars().take(width - 3).collect();
        line.push_str("...");
        line
    }
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

#[cfg(test)]
mod tests {
    use super::PickerState;

    #[test]
    fn picker_scrolls_to_keep_selection_visible() {
        let mut state = PickerState::new(10, 0, 3);
        state.down();
        state.down();
        state.down();
        assert_eq!(state.selected, 3);
        assert_eq!(state.offset, 1);

        state.end();
        assert_eq!(state.selected, 9);
        assert_eq!(state.offset, 7);

        state.home();
        assert_eq!(state.selected, 0);
        assert_eq!(state.offset, 0);

        state.up();
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn picker_pages_and_handles_resizes() {
        let mut state = PickerState::new(20, 0, 5);
        state.page_down();
        assert_eq!(state.selected, 5);
        state.page_up();
        assert_eq!(state.selected, 0);

        let mut state = PickerState::new(20, 15, 10);
        state.resize(3);
        assert_eq!(state.height, 3);
        assert_eq!(state.selected, 15);
        assert_eq!(state.offset, 13);
    }

    #[test]
    fn model_picker_filters_and_sorts_by_price() {
        use super::*;
        use crate::router::ModelPricing;
        let models = vec![
            ModelInfo {
                id: "opus".to_string(),
                pricing: Some(ModelPricing {
                    input_per_million: 15.0,
                    output_per_million: 75.0,
                }),
            },
            ModelInfo {
                id: "llama-free".to_string(),
                pricing: Some(ModelPricing {
                    input_per_million: 0.0,
                    output_per_million: 0.0,
                }),
            },
            ModelInfo {
                id: "haiku-cheap".to_string(),
                pricing: Some(ModelPricing {
                    input_per_million: 0.25,
                    output_per_million: 1.25,
                }),
            },
            ModelInfo {
                id: "unpriced".to_string(),
                pricing: None,
            },
        ];

        let sorted = filter_and_sort_model_indices(&models, "", None, true);
        assert_eq!(sorted, vec![1, 2, 0, 3]);

        let cheap_only = filter_and_sort_model_indices(&models, "", Some(CostTier::Cheap), false);
        assert_eq!(cheap_only, vec![2]);

        let filtered = filter_and_sort_model_indices(&models, "free", None, false);
        assert_eq!(filtered, vec![1]);

        let (left, tag, tier) = format_model_columns(&models[0], 50);
        assert_eq!(tier, Some(CostTier::Expensive));
        assert_eq!(tag, "[exp] $15.00/$75.00");
        assert!(left.starts_with("opus"));
        assert_eq!(left.chars().count() + 2 + tag.chars().count(), 50);
    }
}
