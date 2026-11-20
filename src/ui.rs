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
}
