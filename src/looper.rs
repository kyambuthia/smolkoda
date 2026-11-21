use std::time::Instant;

use crate::feed::{Block, Feed, ToolResult};
use crate::router::Router;
use crate::tools;
use crate::ui;

pub struct Looper {
    pub router: Router,
    pub feed: Feed,
    pub max_steps: usize,
}

impl Looper {
    pub fn new(router: Router) -> Self {
        let max_steps = std::env::var("SMOLKODA_MAX_STEPS")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(50);
        Self {
            router,
            feed: Feed::default(),
            max_steps,
        }
    }

    pub fn clear(&mut self) {
        self.feed.clear();
    }

    /// Run one user turn. The feed is committed only if the turn succeeds.
    pub fn run_turn(&mut self, input: &str) -> Result<(), String> {
        self.run_turn_with_dock(input, None).map(|_| ())
    }

    /// Run one user turn, pushing entries into `dock` when running in TUI mode.
    pub fn run_turn_with_dock(
        &mut self,
        input: &str,
        dock: Option<&mut ui::PromptDock>,
    ) -> Result<String, String> {
        self.run_turn_inner(input, dock)
    }

    /// Run one user turn and return the final assistant text for voice playback.
    pub fn run_turn_with_output(&mut self, input: &str) -> Result<String, String> {
        self.run_turn_inner(input, None)
    }

    fn run_turn_inner(
        &mut self,
        input: &str,
        mut dock: Option<&mut ui::PromptDock>,
    ) -> Result<String, String> {
        let _sigint = ui::SigintGuard::install();
        let start_usage = self.router.usage();
        let mut working = self.feed.clone();
        working.push_user(input);
        let system = system_prompt();

        for _step in 1..=self.max_steps {
            if ui::is_interrupted() {
                return Err("interrupted".to_string());
            }
            let blocks = ui::busy(ui::AnimKind::Think, "thinking", || {
                complete_interruptible(&self.router, &working, &system)
            })?;
            if ui::is_interrupted() {
                return Err("interrupted".to_string());
            }
            if blocks.is_empty() {
                if let Some(d) = dock.as_deref_mut() {
                    d.push(ui::FeedEntry::Assistant {
                        text: "(empty response)".to_string(),
                    });
                } else {
                    ui::print_text("(empty response)");
                }
                working.push_assistant(vec![Block::Text("(empty response)".to_string())]);
                self.feed = working;
                self.print_usage_delta(start_usage, dock);
                return Ok(String::new());
            }
            working.push_assistant(blocks.clone());

            let mut results = Vec::new();
            let mut step_text = Vec::new();
            for block in &blocks {
                match block {
                    Block::Text(text) => {
                        if let Some(d) = dock.as_deref_mut() {
                            d.push(ui::FeedEntry::Assistant { text: text.clone() });
                        } else {
                            ui::print_text(text);
                        }
                        step_text.push(text.as_str());
                    }
                    Block::ToolUse {
                        id,
                        name,
                        input,
                        parse_error,
                    } => {
                        if dock.is_none() {
                            ui::print_tool(name, input);
                        }
                        let started = Instant::now();
                        let (result, is_error) = match parse_error {
                            Some(err) => (format!("error: {err}"), true),
                            None => {
                                let label = format!("running {name}");
                                match ui::busy(ui::AnimKind::Tool, &label, || {
                                    tools::run(name, input)
                                }) {
                                    Ok(output) => {
                                        let err_flag = output.starts_with("error:");
                                        (output, err_flag)
                                    }
                                    Err(err) => (format!("error: {err}"), true),
                                }
                            }
                        };
                        let elapsed = started.elapsed();
                        if let Some(d) = dock.as_deref_mut() {
                            let preview = ui::tool_preview_arg(name, input)
                                .map(|v| {
                                    v.as_str()
                                        .map(str::to_string)
                                        .unwrap_or_else(|| v.to_string())
                                })
                                .unwrap_or_default();
                            let args_detail =
                                serde_json::to_string_pretty(input).unwrap_or_default();
                            d.push(ui::FeedEntry::Tool {
                                name: name.clone(),
                                preview,
                                args_detail,
                                result: result.clone(),
                                elapsed,
                                is_error,
                                expanded: is_error,
                            });
                        } else {
                            ui::print_result(&result, elapsed);
                        }
                        results.push(ToolResult {
                            tool_use_id: id.clone(),
                            content: result,
                            is_error,
                        });
                    }
                }
            }

            if results.is_empty() {
                self.feed = working;
                self.print_usage_delta(start_usage, dock);
                return Ok(step_text.join("\n"));
            }
            working.push_tool_results(results);
        }

        Err(format!(
            "looper exceeded {} steps; stopping to avoid a runaway loop",
            self.max_steps
        ))
    }
}

impl Looper {
    fn print_usage_delta(
        &self,
        start: crate::router::SessionUsage,
        dock: Option<&mut ui::PromptDock>,
    ) {
        let end = self.router.usage();
        let turn_in = end.input_tokens.saturating_sub(start.input_tokens);
        let turn_out = end.output_tokens.saturating_sub(start.output_tokens);
        let turn_cost = if end.has_reported_cost {
            Some((end.reported_cost_usd - start.reported_cost_usd).max(0.0))
        } else {
            None
        };
        let session_cost = end.has_reported_cost.then_some(end.reported_cost_usd);
        if let Some(d) = dock {
            if turn_in > 0 || turn_out > 0 {
                d.push(ui::FeedEntry::TurnMeta {
                    input_tokens: turn_in,
                    output_tokens: turn_out,
                    turn_cost,
                    session_cost,
                });
            }
        } else {
            ui::print_turn_usage(turn_in, turn_out, turn_cost, session_cost);
        }
    }
}

fn complete_interruptible(
    router: &Router,
    feed: &Feed,
    system: &str,
) -> Result<Vec<Block>, String> {
    let router = router.clone();
    let feed = feed.clone();
    let system = system.to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(router.complete(&feed, &system, None));
    });
    loop {
        if ui::is_interrupted() {
            return Err("interrupted".to_string());
        }
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => return result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err("model worker exited unexpectedly".to_string());
            }
        }
    }
}

fn system_prompt() -> String {
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    format!(
        "You are smolkoda, a concise coding agent. cwd: {cwd}\n\
         Use tools to inspect and change files. Prefer read/glob/grep over bash \
         for exploration. Make minimal, correct changes and briefly report what you did."
    )
}
