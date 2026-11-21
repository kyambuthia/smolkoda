use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use serde_json::Value;

use crate::router::{format_usd, CostTier, ModelInfo};

static STATUS_MUTEX: Mutex<Option<String>> = Mutex::new(None);

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        let no_color = std::env::var("NO_COLOR").is_ok_and(|value| !value.is_empty());
        let dumb = std::env::var("TERM").ok().as_deref() == Some("dumb");
        !no_color && !dumb && io::stdout().is_terminal()
    })
}

fn sgr(code: &'static str) -> &'static str {
    if color_enabled() {
        code
    } else {
        ""
    }
}

pub fn reset() -> &'static str {
    sgr("\x1b[0m")
}

pub fn bold() -> &'static str {
    sgr("\x1b[1m")
}

pub fn dim() -> &'static str {
    sgr("\x1b[2m")
}

pub fn cyan() -> &'static str {
    sgr("\x1b[36m")
}

pub fn green() -> &'static str {
    sgr("\x1b[32m")
}

pub fn yellow() -> &'static str {
    sgr("\x1b[33m")
}

pub fn red() -> &'static str {
    sgr("\x1b[31m")
}

pub fn is_utf8_locale() -> bool {
    static UTF8: OnceLock<bool> = OnceLock::new();
    *UTF8.get_or_init(|| {
        if std::env::var("TERM").ok().as_deref() == Some("linux") {
            return false;
        }
        for key in ["LC_ALL", "LC_CTYPE", "LANG"] {
            if let Ok(val) = std::env::var(key) {
                let upper = val.to_ascii_uppercase();
                if upper.contains("UTF-8") || upper.contains("UTF8") {
                    return true;
                }
            }
        }
        cfg!(windows) && std::env::var_os("WT_SESSION").is_some()
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimKind {
    Think,
    Tool,
    Fetch,
    Voice,
}

const BAYER_4X4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Pure time-based Bayer dither frame generator with distinct motion per state.
pub fn dither_frame(t_ms: u64, width: usize, kind: AnimKind, utf8: bool) -> String {
    if width == 0 {
        return String::new();
    }
    let ramp: &[char] = if !utf8 {
        &[' ', '.', ':', '-', '=', '+', '*', '#']
    } else {
        &[' ', '·', '▫', '▪', '■']
    };
    let speed = match kind {
        AnimKind::Think => 70,
        AnimKind::Tool => 50,
        AnimKind::Fetch => 65,
        AnimKind::Voice => 55,
    };
    let phase = (t_ms / speed) as usize;
    let bayer_y = ((t_ms / (speed * 2)) % 4) as usize;
    let center = (width as isize - 1) / 2;

    let mut out = String::with_capacity(width * 3);
    for x in 0..width {
        let level = match kind {
            AnimKind::Tool => {
                // Directional left-to-right sweep
                let period = width.max(8) * 2;
                let dist = (phase + period - (x % period)) % period;
                if dist < 6 {
                    63 - dist * 10
                } else {
                    4
                }
            }
            AnimKind::Voice => {
                // Symmetric center-out pulse
                let d = (x as isize - center).unsigned_abs();
                let period = 16usize;
                let pos = (d * 2 + phase) % period;
                let half = period / 2;
                let tri = if pos <= half { pos } else { period - pos };
                (tri * 63) / half
            }
            AnimKind::Think | AnimKind::Fetch => {
                let period = 20usize;
                let half = period / 2;
                let pos = (x * 2 + phase) % period;
                let tri = if pos <= half { pos } else { period - pos };
                (tri * 63) / half
            }
        };
        let threshold = usize::from(BAYER_4X4[bayer_y][x % 4]) * 4;
        let dithered = (level + threshold).saturating_sub(26).min(63);
        let idx = (dithered * (ramp.len() - 1)) / 63;
        out.push(ramp[idx]);
    }
    out
}

fn anim_enabled() -> bool {
    let dumb = std::env::var("TERM").ok().as_deref() == Some("dumb");
    let ci = std::env::var_os("CI").is_some();
    let no_anim =
        std::env::var("SMOLKODA_NO_ANIM").is_ok_and(|value| !value.is_empty() && value != "0");
    let anim_off = std::env::var("SMOLKODA_ANIM").ok().as_deref() == Some("0");
    io::stderr().is_terminal() && !dumb && !ci && !no_anim && !anim_off
}

/// Run `work` while displaying a dithered status line on stderr.
///
/// Operations completing under 120ms do not flash on screen.
pub fn busy<T>(kind: AnimKind, label: &str, work: impl FnOnce() -> T) -> T {
    if !anim_enabled() {
        return work();
    }
    let clean_label = sanitize(label);
    let stop = Arc::new((Mutex::new(false), Condvar::new()));
    let drawn = Arc::new(AtomicBool::new(false));

    if let Ok(mut active) = STATUS_MUTEX.lock() {
        *active = Some(clean_label.clone());
    }

    let stop_bg = Arc::clone(&stop);
    let drawn_bg = Arc::clone(&drawn);
    let bg_label = clean_label.clone();
    let utf8 = is_utf8_locale();

    let handle = thread::spawn(move || {
        let start = Instant::now();
        let (lock, cvar) = &*stop_bg;
        let mut guard = match lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let initial_wait = cvar.wait_timeout(guard, Duration::from_millis(120));
        guard = match initial_wait {
            Ok((g, _)) => g,
            Err(_) => return,
        };
        if *guard {
            return;
        }
        drop(guard);

        loop {
            let elapsed = start.elapsed();
            let t_ms = elapsed.as_millis() as u64;
            let width = terminal::size()
                .map(|(w, _)| usize::from(w))
                .unwrap_or(80)
                .saturating_sub(1)
                .max(1);
            let suffix = if elapsed.as_secs() >= 1 {
                format!(" {:.1}s", elapsed.as_secs_f64())
            } else {
                String::new()
            };
            let label_full = format!("{bg_label}{suffix}");
            let label_max = (width / 2).max(12).min(width);
            let label_clipped = clip_line(&label_full, label_max);
            let label_trim = label_clipped.trim_end();
            let label_len = label_trim.chars().count();
            let bar_width = width.saturating_sub(label_len + 1);
            let bar = dither_frame(t_ms, bar_width, kind, utf8);

            if let Ok(active) = STATUS_MUTEX.lock() {
                if active.is_some() {
                    let mut err = io::stderr().lock();
                    let _ = write!(
                        err,
                        "\r{}{}{} {}{}{}\x1b[K",
                        dim(),
                        label_trim,
                        reset(),
                        cyan(),
                        bar,
                        reset()
                    );
                    let _ = err.flush();
                    drawn_bg.store(true, Ordering::Relaxed);
                }
            }

            let guard = match lock.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            let (guard, _) = match cvar.wait_timeout(guard, Duration::from_millis(60)) {
                Ok(res) => res,
                Err(_) => break,
            };
            if *guard {
                break;
            }
        }
    });

    struct BusyGuard<'a> {
        stop: &'a (Mutex<bool>, Condvar),
        drawn: &'a AtomicBool,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Drop for BusyGuard<'_> {
        fn drop(&mut self) {
            if let Ok(mut done) = self.stop.0.lock() {
                *done = true;
                self.stop.1.notify_all();
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
            if let Ok(mut active) = STATUS_MUTEX.lock() {
                *active = None;
            }
            if self.drawn.load(Ordering::Relaxed) {
                let mut err = io::stderr().lock();
                let _ = write!(err, "\r\x1b[2K");
                let _ = err.flush();
            }
        }
    }

    let _guard = BusyGuard {
        stop: &stop,
        drawn: &drawn,
        handle: Some(handle),
    };
    work()
}

/// Print a line cleanly while a `busy` status bar may be active.
pub fn emit_line(line: &str) {
    let clean = sanitize(line);
    if let Ok(active) = STATUS_MUTEX.lock() {
        if active.is_some() && io::stderr().is_terminal() {
            let mut err = io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
        println!("{clean}");
    } else {
        println!("{clean}");
    }
}

/// Strip terminal escape sequences and control characters (keeping `\n` and `\t`).
pub fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == '\r' {
            continue;
        }
        if ch.is_control() && ch != '\n' && ch != '\t' {
            continue;
        }
        out.push(ch);
    }
    out
}

pub fn prompt() {
    let glyph = if is_utf8_locale() { "›" } else { ">" };
    print!("{}{}{}{} ", bold(), cyan(), glyph, reset());
    let _ = io::stdout().flush();
}

pub fn banner(model: &str, provider: &str, _endpoint: &str) {
    let utf8 = is_utf8_locale();
    let (top, mid, bot, dot) = if utf8 {
        ("╭─", "│ ", "╰─", "·")
    } else {
        ("+-", "| ", "+-", "-")
    };
    println!(
        "{}{} {}smolkoda{} {}v{}{}",
        dim(),
        top,
        bold(),
        reset(),
        dim(),
        env!("CARGO_PKG_VERSION"),
        reset()
    );
    println!("{}{} {} {} {}{}", dim(), mid, model, dot, provider, reset());
    println!(
        "{}{} /help {} /model {} /cost {} /quit{}\n",
        dim(),
        bot,
        dot,
        dot,
        dot,
        reset()
    );
}

pub fn print_text(text: &str) {
    let clean = sanitize(text);
    let mut lines = clean.lines();
    match lines.next() {
        Some(first) => println!("\n{first}"),
        None => println!(),
    }
    for line in lines {
        println!("{line}");
    }
}

pub fn tool_preview_arg<'a>(name: &str, args: &'a Value) -> Option<&'a Value> {
    let obj = args.as_object()?;
    let preferred = match name {
        "read" | "write" | "edit" => &["path"][..],
        "glob" | "grep" => &["pattern", "path"][..],
        "bash" => &["cmd", "command"][..],
        _ => &[][..],
    };
    for key in preferred {
        if let Some(val) = obj.get(*key) {
            return Some(val);
        }
    }
    obj.values().next()
}

pub fn print_tool(name: &str, args: &Value) {
    let preview = tool_preview_arg(name, args)
        .map(value_preview)
        .unwrap_or_default();
    let glyph = if is_utf8_locale() { "┌▸" } else { "+>" };
    println!(
        "\n{}{} {}{} {}{}{}",
        cyan(),
        glyph,
        reset(),
        bold(),
        sanitize(name),
        reset(),
        if preview.is_empty() {
            String::new()
        } else {
            format!(" {}{}{}", dim(), sanitize(&preview), reset())
        }
    );
}

pub fn print_result(result: &str, elapsed: Duration) {
    let clean = sanitize(result);
    let lines: Vec<&str> = clean.lines().collect();
    let first = lines.first().copied().unwrap_or("");
    let mut preview: String = first.chars().take(72).collect();
    if lines.len() > 1 {
        preview.push_str(&format!(" (+{} lines)", lines.len() - 1));
    } else if first.chars().count() > 72 {
        preview.push_str("...");
    }
    let prefix = if is_utf8_locale() { "└─" } else { "+-" };
    let dot = if is_utf8_locale() { "·" } else { "-" };
    let ms = elapsed.as_millis();
    let timing = if ms >= 1000 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{ms}ms")
    };
    println!(
        "{}{} {} {} {}{}",
        dim(),
        prefix,
        preview,
        dot,
        timing,
        reset()
    );
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
    let dot = if is_utf8_locale() { "·" } else { "-" };
    let mut line = format!("  tokens: {} in / {} out", input_tokens, output_tokens);
    if let Some(cost) = turn_cost {
        line.push_str(&format!(" {} ~{}", dot, format_usd(cost)));
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
pub fn select(prompt: &str, items: &[String], initial: usize) -> Option<usize> {
    if items.is_empty() || !can_select() {
        return None;
    }
    let _guard = TerminalGuard::enter().ok()?;
    let mut state = PickerState::new(items.len(), initial, 1);
    let mut stdout = io::stdout();

    loop {
        let (_, height) = terminal::size().unwrap_or((80, 24));
        let viewport = usize::from(height).saturating_sub(3).max(1);
        state.resize(viewport);
        render_select(&mut stdout, prompt, items, &state).ok()?;

        match event::read().ok()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
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

/// Tabbed interactive model picker with aligned cost-tier columns,
/// type-to-filter search, Tab/Left/Right tier tabs, and Ctrl-S price sorting.
pub fn select_model(prompt: &str, models: &[ModelInfo], initial: usize) -> Option<usize> {
    if models.is_empty() || !can_select() {
        return None;
    }
    let _guard = TerminalGuard::enter().ok()?;
    let mut stdout = io::stdout();
    let mut query = String::new();
    let mut tier_filter: Option<CostTier> = None;
    let mut sort_by_price = false;

    let has_pricing = models.iter().any(|model| model.pricing.is_some());
    let mut visible = filter_and_sort_model_indices(models, &query, tier_filter, sort_by_price);
    let initial_pos = visible.iter().position(|&idx| idx == initial).unwrap_or(0);
    let mut state = PickerState::new(visible.len().max(1), initial_pos, 1);

    loop {
        let (_, height) = terminal::size().unwrap_or((80, 24));
        let header_rows = if has_pricing { 4 } else { 3 };
        let viewport = usize::from(height).saturating_sub(header_rows + 1).max(1);
        state.resize(viewport);
        render_model_select(
            &mut stdout,
            prompt,
            models,
            &visible,
            &state,
            &query,
            tier_filter,
            sort_by_price,
            has_pricing,
        )
        .ok()?;

        match event::read().ok()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                {
                    return None;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('s'))
                {
                    sort_by_price = !sort_by_price;
                    visible =
                        filter_and_sort_model_indices(models, &query, tier_filter, sort_by_price);
                    state = PickerState::new(visible.len().max(1), 0, viewport);
                    continue;
                }
                match key.code {
                    KeyCode::Up => state.up(),
                    KeyCode::Down => state.down(),
                    KeyCode::PageUp => state.page_up(),
                    KeyCode::PageDown => state.page_down(),
                    KeyCode::Home => state.home(),
                    KeyCode::End => state.end(),
                    KeyCode::Tab | KeyCode::Right if has_pricing => {
                        tier_filter = next_tier_filter(tier_filter);
                        visible = filter_and_sort_model_indices(
                            models,
                            &query,
                            tier_filter,
                            sort_by_price,
                        );
                        state = PickerState::new(visible.len().max(1), 0, viewport);
                    }
                    KeyCode::BackTab | KeyCode::Left if has_pricing => {
                        tier_filter = prev_tier_filter(tier_filter);
                        visible = filter_and_sort_model_indices(
                            models,
                            &query,
                            tier_filter,
                            sort_by_price,
                        );
                        state = PickerState::new(visible.len().max(1), 0, viewport);
                    }
                    KeyCode::Backspace => {
                        if query.pop().is_some() {
                            visible = filter_and_sort_model_indices(
                                models,
                                &query,
                                tier_filter,
                                sort_by_price,
                            );
                            state = PickerState::new(visible.len().max(1), 0, viewport);
                        }
                    }
                    KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        query.push(ch);
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
                        if !query.is_empty() {
                            query.clear();
                            visible = filter_and_sort_model_indices(
                                models,
                                &query,
                                tier_filter,
                                sort_by_price,
                            );
                            state = PickerState::new(visible.len().max(1), 0, viewport);
                        } else if tier_filter.is_some() {
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

fn prev_tier_filter(current: Option<CostTier>) -> Option<CostTier> {
    match current {
        None => Some(CostTier::Expensive),
        Some(CostTier::Expensive) => Some(CostTier::Standard),
        Some(CostTier::Standard) => Some(CostTier::Cheap),
        Some(CostTier::Cheap) => Some(CostTier::Free),
        Some(CostTier::Free) => None,
    }
}

pub fn tier_counts(models: &[ModelInfo], query: &str) -> [usize; 5] {
    let needle = query.trim().to_ascii_lowercase();
    let mut counts = [0usize; 5];
    for model in models {
        if !needle.is_empty() && !model.id.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        counts[0] += 1;
        if let Some(pricing) = model.pricing {
            match pricing.tier() {
                CostTier::Free => counts[1] += 1,
                CostTier::Cheap => counts[2] += 1,
                CostTier::Standard => counts[3] += 1,
                CostTier::Expensive => counts[4] += 1,
            }
        }
    }
    counts
}

pub fn tab_bar(width: usize, counts: [usize; 5], active: Option<CostTier>) -> String {
    let tabs: [(Option<CostTier>, &str, usize); 5] = [
        (None, "All", counts[0]),
        (Some(CostTier::Free), "Free", counts[1]),
        (Some(CostTier::Cheap), "Cheap", counts[2]),
        (Some(CostTier::Standard), "Std", counts[3]),
        (Some(CostTier::Expensive), "Exp", counts[4]),
    ];
    let utf8 = is_utf8_locale();
    let (l_arrow, r_arrow) = if utf8 { ("◀", "▶") } else { ("<", ">") };
    if width < 54 {
        let (_, name, count) = tabs
            .iter()
            .find(|(tier, _, _)| *tier == active)
            .copied()
            .unwrap_or(tabs[0]);
        return clip_line(&format!("{l_arrow} [{name} {count}] {r_arrow}"), width);
    }
    let parts: Vec<String> = tabs
        .iter()
        .map(|(tier, name, count)| {
            if *tier == active {
                format!("[{name} {count}]")
            } else {
                format!(" {name} {count} ")
            }
        })
        .collect();
    clip_line(&format!("{l_arrow} {} {r_arrow}", parts.join(" ")), width)
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
    len: usize,
    selected: usize,
    offset: usize,
    height: usize,
}

impl PickerState {
    fn new(len: usize, initial: usize, height: usize) -> Self {
        let mut state = Self {
            len: len.max(1),
            selected: initial.min(len.saturating_sub(1)),
            offset: 0,
            height: height.max(1),
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

pub fn scrollbar_thumb(offset: usize, viewport: usize, total: usize, row: usize) -> bool {
    if total <= viewport || viewport == 0 {
        return false;
    }
    let max_offset = total.saturating_sub(viewport).max(1);
    let thumb_row = (offset * (viewport - 1)) / max_offset;
    row == thumb_row
}

pub fn box_border_line(
    left_cap: &str,
    label: &str,
    fill: &str,
    right_cap: &str,
    width: usize,
) -> String {
    if width < 6 {
        return clip_line(label, width);
    }
    let mut base = if label.is_empty() {
        left_cap.to_string()
    } else {
        format!("{left_cap} {label} ")
    };
    let used = base.chars().count() + right_cap.chars().count();
    if used > width {
        let trimmed: String = base
            .chars()
            .take(width.saturating_sub(right_cap.chars().count()))
            .collect();
        return format!("{trimmed}{right_cap}");
    }
    for _ in 0..(width - used) {
        base.push_str(fill);
    }
    base.push_str(right_cap);
    base
}

fn render_select(
    stdout: &mut io::Stdout,
    prompt: &str,
    items: &[String],
    state: &PickerState,
) -> io::Result<()> {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let width = usize::from(width).max(1);
    let height = usize::from(height).max(1);
    let box_width = width.saturating_sub(1).max(12);
    let inner_width = box_width.saturating_sub(4).max(1);
    let viewport = height.saturating_sub(3).max(1);
    let utf8 = is_utf8_locale();
    let (tl, tr, bl, br, hz, vt, thumb, marker) = if utf8 {
        ("╭─", "─╮", "╰─", "─╯", "─", "│", "┃", "▌ ")
    } else {
        ("+-", "-+", "+-", "-+", "-", "|", "#", "> ")
    };

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::UntilNewLine))?;
    let title = format!("{prompt} ({}/{})", state.selected + 1, items.len());
    queue!(
        stdout,
        SetForegroundColor(Color::Cyan),
        Print(box_border_line(tl, &title, hz, tr, box_width)),
        ResetColor
    )?;

    for row in 0..viewport {
        let y = (row + 1) as u16;
        queue!(stdout, MoveTo(0, y), Clear(ClearType::UntilNewLine))?;
        let rail = if scrollbar_thumb(state.offset, viewport, items.len(), row) {
            thumb
        } else {
            vt
        };
        let Some(item) = items.get(state.offset + row) else {
            let empty = clip_line("", inner_width);
            queue!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("{vt} {empty} {rail}")),
                ResetColor
            )?;
            continue;
        };
        let selected = state.offset + row == state.selected;
        let row_inner = inner_width.saturating_sub(2).max(1);
        let text = clip_line(item, row_inner);
        queue!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{vt} ")),
            ResetColor
        )?;
        if selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                Print(marker),
                ResetColor,
                SetAttribute(Attribute::Bold),
                Print(&text),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print("  "), Print(&text))?;
        }
        queue!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!(" {rail}")),
            ResetColor
        )?;
    }

    let footer_y = height.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
    let hint = if utf8 {
        "↑↓ move · PgUp/Dn page · Enter select · Esc cancel"
    } else {
        "Up/Down move · PgUp/Dn page · Enter select · Esc cancel"
    };
    queue!(
        stdout,
        MoveTo(0, footer_y),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        Print(box_border_line(bl, hint, hz, br, box_width)),
        ResetColor
    )?;
    stdout.flush()
}

#[allow(clippy::too_many_arguments)]
fn render_model_select(
    stdout: &mut io::Stdout,
    prompt: &str,
    models: &[ModelInfo],
    visible: &[usize],
    state: &PickerState,
    query: &str,
    tier_filter: Option<CostTier>,
    sort_by_price: bool,
    has_pricing: bool,
) -> io::Result<()> {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let width = usize::from(width).max(1);
    let height = usize::from(height).max(1);
    let box_width = width.saturating_sub(1).max(20);
    let inner_width = box_width.saturating_sub(4).max(1);
    let header_rows = if has_pricing { 4 } else { 3 };
    let viewport = height.saturating_sub(header_rows + 1).max(1);
    let utf8 = is_utf8_locale();
    let (tl, tr, ml, mr, bl, br, hz, vt, thumb, marker) = if utf8 {
        ("╭─", "─╮", "├─", "─┤", "╰─", "─╯", "─", "│", "┃", "▌ ")
    } else {
        ("+-", "-+", "+-", "-+", "+-", "-+", "-", "|", "#", "> ")
    };

    // Row 0: top container border with title + sort badge
    queue!(stdout, MoveTo(0, 0), Clear(ClearType::UntilNewLine))?;
    let mut title = format!("{prompt} ({}/{})", visible.len(), models.len());
    if sort_by_price {
        title.push_str(" [cheapest]");
    }
    queue!(
        stdout,
        SetForegroundColor(Color::Cyan),
        Print(box_border_line(tl, &title, hz, tr, box_width)),
        ResetColor
    )?;

    let mut cur_y = 1u16;
    // Optional tab row
    if has_pricing {
        let counts = tier_counts(models, query);
        let bar = tab_bar(inner_width, counts, tier_filter);
        queue!(
            stdout,
            MoveTo(0, cur_y),
            Clear(ClearType::UntilNewLine),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{vt} ")),
            SetForegroundColor(Color::Cyan),
            Print(&bar),
            SetForegroundColor(Color::DarkGrey),
            Print(format!(" {vt}")),
            ResetColor
        )?;
        cur_y += 1;
    }

    // Dedicated filter input row
    queue!(stdout, MoveTo(0, cur_y), Clear(ClearType::UntilNewLine))?;
    let filter_text = if query.is_empty() {
        "filter: (type to search)".to_string()
    } else {
        format!("filter: {query}_")
    };
    let filter_clipped = clip_line(&filter_text, inner_width);
    queue!(
        stdout,
        SetForegroundColor(Color::DarkGrey),
        Print(format!("{vt} ")),
        ResetColor
    )?;
    if query.is_empty() {
        queue!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(&filter_clipped),
            ResetColor
        )?;
    } else {
        queue!(
            stdout,
            SetAttribute(Attribute::Bold),
            Print(&filter_clipped),
            SetAttribute(Attribute::Reset)
        )?;
    }
    queue!(
        stdout,
        SetForegroundColor(Color::DarkGrey),
        Print(format!(" {vt}")),
        ResetColor
    )?;
    cur_y += 1;

    // Divider between header/filter and list
    queue!(
        stdout,
        MoveTo(0, cur_y),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        Print(box_border_line(ml, "", hz, mr, box_width)),
        ResetColor
    )?;

    // List rows with right-edge scrollbar
    for row in 0..viewport {
        let y = (row + header_rows) as u16;
        queue!(stdout, MoveTo(0, y), Clear(ClearType::UntilNewLine))?;
        let rail = if scrollbar_thumb(state.offset, viewport, visible.len(), row) {
            thumb
        } else {
            vt
        };
        let Some(&model_idx) = visible.get(state.offset + row) else {
            let empty_msg = if row == 0 && visible.is_empty() {
                clip_line("  (no matching models)", inner_width)
            } else {
                clip_line("", inner_width)
            };
            queue!(
                stdout,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("{vt} {empty_msg} {rail}")),
                ResetColor
            )?;
            continue;
        };
        let selected = state.offset + row == state.selected;
        let row_width = inner_width.saturating_sub(2).max(1);
        let (left, tag, tier) = format_model_columns(&models[model_idx], row_width);

        queue!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{vt} ")),
            ResetColor
        )?;
        if selected {
            queue!(
                stdout,
                SetForegroundColor(Color::Cyan),
                Print(marker),
                ResetColor,
                SetAttribute(Attribute::Bold),
                Print(&left),
                SetAttribute(Attribute::Reset)
            )?;
        } else {
            queue!(stdout, Print("  "), Print(&left))?;
        }
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
        queue!(
            stdout,
            SetForegroundColor(Color::DarkGrey),
            Print(format!(" {rail}")),
            ResetColor
        )?;
    }

    // Bottom container border with explicit arrow directions + dynamic Esc action
    let footer_y = height.saturating_sub(1).min(usize::from(u16::MAX)) as u16;
    let esc_action = if !query.is_empty() || tier_filter.is_some() {
        "Esc clear"
    } else {
        "Esc cancel"
    };
    let pos_tag = if !visible.is_empty() {
        format!(" {}/{} ", state.selected + 1, visible.len())
    } else {
        String::new()
    };
    let hint = if has_pricing {
        if utf8 {
            format!("↑↓ move · ←→ tab · Ctrl-S sort · Enter · {esc_action}{pos_tag}")
        } else {
            format!("Up/Dn move · L/R tab · Ctrl-S sort · Enter · {esc_action}{pos_tag}")
        }
    } else if utf8 {
        format!("↑↓ move · Enter select · {esc_action}{pos_tag}")
    } else {
        format!("Up/Dn move · Enter select · {esc_action}{pos_tag}")
    };
    queue!(
        stdout,
        MoveTo(0, footer_y),
        Clear(ClearType::UntilNewLine),
        SetForegroundColor(Color::DarkGrey),
        Print(box_border_line(bl, &hint, hz, br, box_width)),
        ResetColor
    )?;
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
    eprintln!("{}error: {}{}", red(), sanitize(message), reset());
}

pub fn warn(message: &str) {
    eprintln!("{}warn: {}{}", yellow(), sanitize(message), reset());
}

pub fn info(message: &str) {
    println!("{}{}{}", dim(), sanitize(message), reset());
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
    use super::*;
    use crate::router::ModelPricing;
    use serde_json::json;

    #[test]
    fn picker_scrolls_to_keep_selection_visible() {
        let mut state = PickerState::new(10, 0, 3);
        assert_eq!(state.offset, 0);

        state.down();
        state.down();
        assert_eq!(state.selected, 2);
        assert_eq!(state.offset, 0);

        state.down();
        assert_eq!(state.selected, 3);
        assert_eq!(state.offset, 1);

        state.end();
        assert_eq!(state.selected, 9);
        assert_eq!(state.offset, 7);

        state.home();
        assert_eq!(state.selected, 0);
        assert_eq!(state.offset, 0);
    }

    #[test]
    fn picker_pages_and_handles_resizes() {
        let mut state = PickerState::new(20, 0, 5);
        state.page_down();
        assert_eq!(state.selected, 5);
        assert_eq!(state.offset, 1);

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

        let counts = tier_counts(&models, "");
        assert_eq!(counts, [4, 1, 1, 0, 1]);
        let bar = tab_bar(80, counts, Some(CostTier::Cheap));
        assert!(bar.contains("[Cheap 1]"));
        let narrow = tab_bar(30, counts, Some(CostTier::Cheap));
        assert!(narrow.contains("[Cheap 1]"));
    }

    #[test]
    fn dither_frame_produces_exact_width_and_changes_over_time() {
        let a = dither_frame(0, 12, AnimKind::Tool, true);
        let b = dither_frame(240, 12, AnimKind::Tool, true);
        assert_eq!(a.chars().count(), 12);
        assert_eq!(b.chars().count(), 12);
        assert_ne!(a, b);

        let ascii = dither_frame(120, 10, AnimKind::Think, false);
        assert_eq!(ascii.chars().count(), 10);
        assert!(ascii.is_ascii());
    }

    #[test]
    fn sanitize_strips_ansi_escapes_and_control_chars() {
        let raw = "hello\x1b[31m world\x1b[0m\r\n\t\x07ok";
        assert_eq!(sanitize(raw), "hello world\n\tok");
    }

    #[test]
    fn container_helpers_respect_width_and_scrollbar() {
        let border = box_border_line("╭─", "Title", "─", "─╮", 20);
        assert_eq!(border.chars().count(), 20);
        assert!(border.starts_with("╭─ Title "));
        assert!(border.ends_with("─╮"));
        assert!(scrollbar_thumb(0, 5, 20, 0));
        assert!(scrollbar_thumb(15, 5, 20, 4));
        assert!(!scrollbar_thumb(0, 10, 5, 0));
    }

    #[test]
    fn tool_preview_picks_meaningful_argument() {
        let edit_args = json!({"all": true, "new": "x", "old": "y", "path": "src/ui.rs"});
        assert_eq!(
            tool_preview_arg("edit", &edit_args).and_then(Value::as_str),
            Some("src/ui.rs")
        );
        let bash_args = json!({"cmd": "cargo test"});
        assert_eq!(
            tool_preview_arg("bash", &bash_args).and_then(Value::as_str),
            Some("cargo test")
        );
    }
}
