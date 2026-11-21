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
static FEED_QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_feed_quiet(quiet: bool) {
    FEED_QUIET.store(quiet, Ordering::Relaxed);
}

pub fn is_feed_quiet() -> bool {
    FEED_QUIET.load(Ordering::Relaxed)
}

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
pub fn dither_frame(t_ms: u64, width: usize, _kind: AnimKind, utf8: bool) -> String {
    if width == 0 {
        return String::new();
    }
    let ramp: &[char] = if !utf8 {
        &[' ', '.', ':', '-', '=', '+', '*', '#']
    } else {
        &[' ', '·', '▫', '▪', '■']
    };
    let phase = (t_ms / 50) as usize;
    let bayer_y = ((t_ms / 100) % 4) as usize;
    let period = width.max(12);

    let mut out = String::with_capacity(width * 3);
    for x in 0..width {
        let dist = (phase + period - (x % period)) % period;
        let level = if dist < 8 { 63 - dist * 7 } else { 4 };
        let threshold = usize::from(BAYER_4X4[bayer_y][x % 4]) * 4;
        let dithered = (level + threshold).saturating_sub(24).min(63);
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

/// Slice `input` around `cursor` (char index) so it fits inside `max_width` columns,
/// returning `(visible_text, cursor_column_offset)`.
pub fn format_prompt_input(input: &str, cursor: usize, max_width: usize) -> (String, usize) {
    if max_width == 0 {
        return (String::new(), 0);
    }
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());
    let start = if cursor >= max_width {
        cursor + 1 - max_width
    } else {
        0
    };
    let end = (start + max_width).min(chars.len());
    let visible: String = chars[start..end].iter().collect();
    (visible, cursor - start)
}

#[derive(Debug, Clone)]
pub enum FeedEntry {
    Banner {
        model: String,
        provider: String,
    },
    User {
        text: String,
        expanded: bool,
    },
    Assistant {
        text: String,
    },
    Tool {
        name: String,
        preview: String,
        args_detail: String,
        result: String,
        elapsed: Duration,
        is_error: bool,
        expanded: bool,
    },
    TurnMeta {
        input_tokens: u64,
        output_tokens: u64,
        turn_cost: Option<f64>,
        session_cost: Option<f64>,
    },
    Info {
        text: String,
        is_error: bool,
    },
}

impl FeedEntry {
    pub fn toggle(&mut self) -> bool {
        match self {
            Self::Tool { expanded, .. } => {
                *expanded = !*expanded;
                true
            }
            Self::User { text, expanded } if text.lines().count() > 3 => {
                *expanded = !*expanded;
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineStyle {
    Normal,
    Dim,
    Bold,
    ToolCollapsed,
    ToolError,
    UserHeader,
    Error,
}

#[derive(Debug, Clone)]
struct RenderedLine {
    text: String,
    style: LineStyle,
    entry_idx: Option<usize>,
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    for raw_line in text.lines() {
        if raw_line.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut count = 0usize;
        for ch in raw_line.chars() {
            if count >= width {
                out.push(current);
                current = String::new();
                count = 0;
            }
            current.push(ch);
            count += 1;
        }
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn format_duration_short(elapsed: Duration) -> String {
    let ms = elapsed.as_millis();
    if ms >= 1000 {
        format!("{:.1}s", elapsed.as_secs_f64())
    } else {
        format!("{ms}ms")
    }
}

#[derive(Debug, Default)]
pub struct FeedView {
    pub entries: Vec<FeedEntry>,
    pub scroll: usize,
    pub follow_tail: bool,
    pub hit_boxes: Vec<(u16, u16, usize)>,
}

impl FeedView {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            scroll: 0,
            follow_tail: true,
            hit_boxes: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: FeedEntry) {
        self.entries.push(entry);
        self.follow_tail = true;
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.scroll = 0;
        self.follow_tail = true;
        self.hit_boxes.clear();
    }

    pub fn toggle_last_tool(&mut self) -> bool {
        for entry in self.entries.iter_mut().rev() {
            if matches!(entry, FeedEntry::Tool { .. }) {
                return entry.toggle();
            }
        }
        false
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.follow_tail = false;
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_down(&mut self, lines: usize, total_lines: usize, viewport: usize) {
        let max_scroll = total_lines.saturating_sub(viewport);
        self.scroll = (self.scroll + lines).min(max_scroll);
        if self.scroll >= max_scroll {
            self.follow_tail = true;
        }
    }

    fn build_lines(&self, width: usize, utf8: bool) -> Vec<RenderedLine> {
        let mut lines = Vec::new();
        let dot = if utf8 { "·" } else { "-" };
        let (tl, vt, bl, c_arrow, e_arrow) = if utf8 {
            ("╭─", "│ ", "╰─", "▸", "▾")
        } else {
            ("+-", "| ", "+-", ">", "v")
        };

        for (idx, entry) in self.entries.iter().enumerate() {
            match entry {
                FeedEntry::Banner { model, provider } => {
                    lines.push(RenderedLine {
                        text: format!("{tl} smolkoda v{}", env!("CARGO_PKG_VERSION")),
                        style: LineStyle::Bold,
                        entry_idx: None,
                    });
                    lines.push(RenderedLine {
                        text: format!("{vt} {model} {dot} {provider}"),
                        style: LineStyle::Dim,
                        entry_idx: None,
                    });
                    lines.push(RenderedLine {
                        text: format!("{bl} /help {dot} /model {dot} /cost {dot} /quit"),
                        style: LineStyle::Dim,
                        entry_idx: None,
                    });
                }
                FeedEntry::User { text, expanded } => {
                    lines.push(RenderedLine {
                        text: String::new(),
                        style: LineStyle::Normal,
                        entry_idx: None,
                    });
                    let clean = sanitize(text);
                    let raw_lines: Vec<&str> = clean.lines().collect();
                    let collapsible = raw_lines.len() > 3;
                    let show_lines: Vec<&str> = if collapsible && !*expanded {
                        raw_lines.iter().take(2).copied().collect()
                    } else {
                        raw_lines.clone()
                    };
                    let mut first = true;
                    for raw in show_lines {
                        for wrapped in wrap_text(raw, width.saturating_sub(6).max(10)) {
                            if first {
                                lines.push(RenderedLine {
                                    text: wrapped,
                                    style: LineStyle::UserHeader,
                                    entry_idx: collapsible.then_some(idx),
                                });
                                first = false;
                            } else {
                                lines.push(RenderedLine {
                                    text: format!("      {wrapped}"),
                                    style: LineStyle::Bold,
                                    entry_idx: collapsible.then_some(idx),
                                });
                            }
                        }
                    }
                    if collapsible {
                        let hint = if *expanded {
                            "      [Ctrl-O to collapse]".to_string()
                        } else {
                            format!(
                                "      [+{} lines {dot} Ctrl-O to expand]",
                                raw_lines.len() - 2
                            )
                        };
                        lines.push(RenderedLine {
                            text: hint,
                            style: LineStyle::Dim,
                            entry_idx: Some(idx),
                        });
                    }
                }
                FeedEntry::Assistant { text } => {
                    lines.push(RenderedLine {
                        text: String::new(),
                        style: LineStyle::Normal,
                        entry_idx: None,
                    });
                    let clean = sanitize(text);
                    for wrapped in wrap_text(&clean, width.max(10)) {
                        lines.push(RenderedLine {
                            text: wrapped,
                            style: LineStyle::Normal,
                            entry_idx: None,
                        });
                    }
                }
                FeedEntry::Tool {
                    name,
                    preview,
                    args_detail,
                    result,
                    elapsed,
                    is_error,
                    expanded,
                } => {
                    let clean_res = sanitize(result);
                    let res_lines: Vec<&str> = clean_res.lines().collect();
                    let summary = if *is_error {
                        res_lines.first().copied().unwrap_or("error").to_string()
                    } else if res_lines.len() > 1 {
                        format!("{} lines", res_lines.len())
                    } else {
                        let one = res_lines.first().copied().unwrap_or("ok");
                        let clipped: String = one.chars().take(36).collect();
                        if one.chars().count() > 36 {
                            format!("{clipped}...")
                        } else {
                            clipped
                        }
                    };
                    let timing = format_duration_short(*elapsed);
                    let arrow = if *expanded { e_arrow } else { c_arrow };
                    let prev_part = if preview.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", sanitize(preview))
                    };
                    let header =
                        format!("{arrow} {name}{prev_part} {dot} {summary} {dot} {timing}");
                    let header_style = if *is_error {
                        LineStyle::ToolError
                    } else {
                        LineStyle::ToolCollapsed
                    };
                    for wrapped in wrap_text(&header, width.max(10)) {
                        lines.push(RenderedLine {
                            text: wrapped,
                            style: header_style,
                            entry_idx: Some(idx),
                        });
                    }
                    if *expanded {
                        if !args_detail.is_empty() {
                            for wrapped in
                                wrap_text(&sanitize(args_detail), width.saturating_sub(2).max(10))
                            {
                                lines.push(RenderedLine {
                                    text: format!("{vt}{wrapped}"),
                                    style: LineStyle::Dim,
                                    entry_idx: Some(idx),
                                });
                            }
                        }
                        let cap = 40usize;
                        for raw in res_lines.iter().take(cap) {
                            for wrapped in wrap_text(raw, width.saturating_sub(2).max(10)) {
                                lines.push(RenderedLine {
                                    text: format!("{vt}{wrapped}"),
                                    style: if *is_error {
                                        LineStyle::Error
                                    } else {
                                        LineStyle::Normal
                                    },
                                    entry_idx: Some(idx),
                                });
                            }
                        }
                        if res_lines.len() > cap {
                            lines.push(RenderedLine {
                                text: format!("{vt}... (+{} more lines)", res_lines.len() - cap),
                                style: LineStyle::Dim,
                                entry_idx: Some(idx),
                            });
                        }
                        lines.push(RenderedLine {
                            text: format!("{bl} [Ctrl-O to collapse]"),
                            style: LineStyle::Dim,
                            entry_idx: Some(idx),
                        });
                    }
                }
                FeedEntry::TurnMeta {
                    input_tokens,
                    output_tokens,
                    turn_cost,
                    session_cost,
                } => {
                    let mut meta = format!("  tokens: {input_tokens} in / {output_tokens} out");
                    if let Some(c) = turn_cost {
                        meta.push_str(&format!(" {dot} ~{}", format_usd(*c)));
                    }
                    if let Some(s) = session_cost {
                        meta.push_str(&format!(" (session: {})", format_usd(*s)));
                    }
                    lines.push(RenderedLine {
                        text: meta,
                        style: LineStyle::Dim,
                        entry_idx: None,
                    });
                }
                FeedEntry::Info { text, is_error } => {
                    let clean = sanitize(text);
                    for wrapped in wrap_text(&clean, width.max(10)) {
                        lines.push(RenderedLine {
                            text: wrapped,
                            style: if *is_error {
                                LineStyle::Error
                            } else {
                                LineStyle::Dim
                            },
                            entry_idx: None,
                        });
                    }
                }
            }
        }
        lines
    }

    pub fn render_into(&mut self, width: u16, viewport: u16) -> io::Result<usize> {
        let utf8 = is_utf8_locale();
        let glyph = if utf8 { "›" } else { ">" };
        let content_w = usize::from(width).saturating_sub(1).max(10);
        let all_lines = self.build_lines(content_w, utf8);
        let total = all_lines.len();
        let vp = usize::from(viewport);

        if self.follow_tail {
            self.scroll = total.saturating_sub(vp);
        } else {
            self.scroll = self.scroll.min(total.saturating_sub(vp));
        }

        self.hit_boxes.clear();
        let mut out = io::stdout().lock();
        for row in 0..vp {
            let y = row as u16;
            queue!(out, MoveTo(0, y), Clear(ClearType::UntilNewLine))?;
            let Some(line) = all_lines.get(self.scroll + row) else {
                continue;
            };
            if let Some(idx) = line.entry_idx {
                if let Some(last) = self.hit_boxes.last_mut() {
                    if last.2 == idx && last.1 == y {
                        last.1 = y + 1;
                    } else {
                        self.hit_boxes.push((y, y + 1, idx));
                    }
                } else {
                    self.hit_boxes.push((y, y + 1, idx));
                }
            }
            match line.style {
                LineStyle::Normal => {
                    queue!(out, Print(&line.text))?;
                }
                LineStyle::Dim => {
                    queue!(
                        out,
                        SetForegroundColor(Color::DarkGrey),
                        Print(&line.text),
                        ResetColor
                    )?;
                }
                LineStyle::Bold => {
                    queue!(
                        out,
                        SetAttribute(Attribute::Bold),
                        Print(&line.text),
                        SetAttribute(Attribute::Reset)
                    )?;
                }
                LineStyle::ToolCollapsed => {
                    queue!(
                        out,
                        SetForegroundColor(Color::Cyan),
                        Print(&line.text),
                        ResetColor
                    )?;
                }
                LineStyle::ToolError | LineStyle::Error => {
                    queue!(
                        out,
                        SetForegroundColor(Color::Red),
                        Print(&line.text),
                        ResetColor
                    )?;
                }
                LineStyle::UserHeader => {
                    queue!(
                        out,
                        SetForegroundColor(Color::Cyan),
                        Print(format!("you {glyph} ")),
                        ResetColor,
                        SetAttribute(Attribute::Bold),
                        Print(&line.text),
                        SetAttribute(Attribute::Reset)
                    )?;
                }
            }
        }
        out.flush()?;
        Ok(total)
    }
}

/// Pinned bottom prompt region with an interactive scrollable/clickable `FeedView`
/// above it. Falls back to standard line input when not attached to an interactive TTY.
pub struct PromptDock {
    active: bool,
    history: Vec<String>,
    pub feed: FeedView,
    model: String,
}

impl PromptDock {
    pub fn new() -> Self {
        let (_, height) = terminal::size().unwrap_or((80, 24));
        let active = can_select() && height >= 8;
        let mut dock = Self {
            active,
            history: Vec::new(),
            feed: FeedView::new(),
            model: String::new(),
        };
        if dock.active {
            set_feed_quiet(true);
            let mut out = io::stdout().lock();
            let _ = execute!(out, EnterAlternateScreen);
            dock.render("", 0);
        }
        dock
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn push(&mut self, entry: FeedEntry) {
        self.feed.push(entry);
        if self.active {
            self.render("", 0);
        }
    }

    pub fn clear_feed(&mut self) {
        self.feed.clear();
        if self.active {
            self.render("", 0);
        }
    }

    pub fn redraw(&mut self, model: &str) {
        self.model = model.to_string();
        if self.active {
            self.render("", 0);
        }
    }

    fn render(&mut self, input: &str, cursor: usize) {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        let viewport = height.saturating_sub(3).max(1);
        let _ = self.feed.render_into(width, viewport);
        self.draw_box(input, cursor, width, height);
    }

    fn draw_box(&self, input: &str, cursor: usize, width: u16, height: u16) {
        let box_width = usize::from(width).saturating_sub(1).max(20);
        let inner_width = box_width.saturating_sub(4).max(1);
        let utf8 = is_utf8_locale();
        let (tl, tr, bl, br, hz, vt, glyph) = if utf8 {
            ("╭─", "─╮", "╰─", "─╯", "─", "│", "›")
        } else {
            ("+-", "-+", "+-", "-+", "-", "|", ">")
        };

        let top_label = if self.model.is_empty() {
            "prompt".to_string()
        } else {
            format!("prompt · {}", self.model)
        };
        let hint = if utf8 {
            "Enter send · Ctrl-O toggle tool · ↑↓ hist · PgUp/Dn scroll · Ctrl-D quit"
        } else {
            "Enter send · Ctrl-O toggle tool · Up/Dn hist · PgUp/Dn scroll · Ctrl-D quit"
        };

        let edit_width = inner_width.saturating_sub(2).max(1);
        let (visible, cursor_col) = format_prompt_input(input, cursor, edit_width);
        let padded = clip_line(&visible, edit_width);

        let top_y = height.saturating_sub(3);
        let mid_y = height.saturating_sub(2);
        let bot_y = height.saturating_sub(1);

        let mut out = io::stdout().lock();
        let _ = queue!(
            out,
            MoveTo(0, top_y),
            Clear(ClearType::UntilNewLine),
            SetForegroundColor(Color::DarkGrey),
            Print(box_border_line(tl, &top_label, hz, tr, box_width)),
            ResetColor,
            MoveTo(0, mid_y),
            Clear(ClearType::UntilNewLine),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{vt} ")),
            SetForegroundColor(Color::Cyan),
            SetAttribute(Attribute::Bold),
            Print(format!("{glyph} ")),
            ResetColor,
            Print(&padded),
            SetForegroundColor(Color::DarkGrey),
            Print(format!(" {vt}")),
            ResetColor,
            MoveTo(0, bot_y),
            Clear(ClearType::UntilNewLine),
            SetForegroundColor(Color::DarkGrey),
            Print(box_border_line(bl, hint, hz, br, box_width)),
            ResetColor,
            MoveTo((4 + cursor_col).min(usize::from(u16::MAX)) as u16, mid_y),
        );
        let _ = out.flush();
    }

    pub fn read_line(&mut self, model: &str) -> Option<String> {
        self.model = model.to_string();
        if !self.active {
            prompt();
            let mut line = String::new();
            if io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
                println!();
                return None;
            }
            return Some(line);
        }

        struct InputGuard;
        impl Drop for InputGuard {
            fn drop(&mut self) {
                let _ = terminal::disable_raw_mode();
            }
        }

        if terminal::enable_raw_mode().is_err() {
            return None;
        }
        let _guard = InputGuard;

        let mut chars: Vec<char> = Vec::new();
        let mut cursor = 0usize;
        let mut hist_idx: Option<usize> = None;
        let mut draft = String::new();

        loop {
            let input: String = chars.iter().collect();
            self.render(&input, cursor);

            let Ok(ev) = event::read() else {
                return None;
            };
            match ev {
                Event::Resize(_, _) => {}
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if key.modifiers.contains(KeyModifiers::CONTROL) {
                        match key.code {
                            KeyCode::Char('c') => return None,
                            KeyCode::Char('d') if chars.is_empty() => return None,
                            KeyCode::Char('a') => cursor = 0,
                            KeyCode::Char('e') => cursor = chars.len(),
                            KeyCode::Char('u') => {
                                chars.clear();
                                cursor = 0;
                            }
                            KeyCode::Char('o') => {
                                self.feed.toggle_last_tool();
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Enter => {
                            let submitted: String = chars.iter().collect();
                            let trimmed = submitted.trim();
                            if !trimmed.is_empty() {
                                if !trimmed.starts_with('/') {
                                    self.feed.push(FeedEntry::User {
                                        text: trimmed.to_string(),
                                        expanded: false,
                                    });
                                }
                                if self.history.last().map(String::as_str) != Some(trimmed) {
                                    self.history.push(trimmed.to_string());
                                }
                            }
                            drop(_guard);
                            self.render("", 0);
                            let (_, height) = terminal::size().unwrap_or((80, 24));
                            let mut out = io::stdout().lock();
                            let _ = execute!(out, MoveTo(0, height.saturating_sub(3)));
                            return Some(submitted);
                        }
                        KeyCode::PageUp => {
                            let (_, height) = terminal::size().unwrap_or((80, 24));
                            let vp = usize::from(height.saturating_sub(3).max(1));
                            self.feed.scroll_up(vp.max(1));
                        }
                        KeyCode::PageDown => {
                            let (w, height) = terminal::size().unwrap_or((80, 24));
                            let vp = usize::from(height.saturating_sub(3).max(1));
                            let total = self
                                .feed
                                .build_lines(
                                    usize::from(w).saturating_sub(1).max(10),
                                    is_utf8_locale(),
                                )
                                .len();
                            self.feed.scroll_down(vp.max(1), total, vp);
                        }
                        KeyCode::Backspace => {
                            if cursor > 0 {
                                cursor -= 1;
                                chars.remove(cursor);
                            }
                        }
                        KeyCode::Delete => {
                            if cursor < chars.len() {
                                chars.remove(cursor);
                            }
                        }
                        KeyCode::Left => cursor = cursor.saturating_sub(1),
                        KeyCode::Right => cursor = (cursor + 1).min(chars.len()),
                        KeyCode::Home => cursor = 0,
                        KeyCode::End => cursor = chars.len(),
                        KeyCode::Up => {
                            if !self.history.is_empty() {
                                let next = match hist_idx {
                                    None => {
                                        draft = chars.iter().collect();
                                        self.history.len() - 1
                                    }
                                    Some(idx) => idx.saturating_sub(1),
                                };
                                hist_idx = Some(next);
                                chars = self.history[next].chars().collect();
                                cursor = chars.len();
                            }
                        }
                        KeyCode::Down => {
                            if let Some(idx) = hist_idx {
                                if idx + 1 < self.history.len() {
                                    let next = idx + 1;
                                    hist_idx = Some(next);
                                    chars = self.history[next].chars().collect();
                                } else {
                                    hist_idx = None;
                                    chars = draft.chars().collect();
                                }
                                cursor = chars.len();
                            }
                        }
                        KeyCode::Char(ch) => {
                            chars.insert(cursor, ch);
                            cursor += 1;
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

impl Default for PromptDock {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PromptDock {
    fn drop(&mut self) {
        if self.active {
            set_feed_quiet(false);
            let mut out = io::stdout().lock();
            let _ = execute!(out, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
        }
    }
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
    fn prompt_input_scrolls_horizontally_around_cursor() {
        let (vis, col) = format_prompt_input("abcdefghij", 10, 5);
        assert_eq!(vis, "ghij");
        assert_eq!(col, 4);
        let (vis2, col2) = format_prompt_input("hello", 2, 10);
        assert_eq!(vis2, "hello");
        assert_eq!(col2, 2);
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

    #[test]
    fn feed_view_toggles_tool_and_scrolls() {
        let mut view = FeedView::new();
        view.push(FeedEntry::Tool {
            name: "read".to_string(),
            preview: "src/ui.rs".to_string(),
            args_detail: "path: src/ui.rs".to_string(),
            result: "line1\nline2\nline3".to_string(),
            elapsed: Duration::from_millis(5),
            is_error: false,
            expanded: false,
        });
        let collapsed = view.build_lines(80, true);
        assert_eq!(collapsed.len(), 1);
        assert!(collapsed[0].text.contains("read src/ui.rs"));

        assert!(view.entries[0].toggle());
        let expanded = view.build_lines(80, true);
        assert!(expanded.len() > 3);

        view.scroll_up(2);
        assert!(!view.follow_tail);
        view.scroll_down(10, expanded.len(), 3);
        assert!(view.follow_tail);
    }
}
