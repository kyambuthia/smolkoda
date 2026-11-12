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
        Self {
            router,
            feed: Feed::default(),
            max_steps: 50,
        }
    }

    pub fn clear(&mut self) {
        self.feed.clear();
    }

    /// Run one user turn. The feed is committed only if the turn succeeds.
    pub fn run_turn(&mut self, input: &str) -> Result<(), String> {
        self.run_turn_with_output(input).map(|_| ())
    }

    /// Run one user turn and return the final assistant text for voice playback.
    pub fn run_turn_with_output(&mut self, input: &str) -> Result<String, String> {
        let mut working = self.feed.clone();
        working.push_user(input);
        let system = system_prompt();

        for _step in 1..=self.max_steps {
            let blocks = self.router.complete(&working, &system, None)?;
            if blocks.is_empty() {
                ui::print_text("(empty response)");
                working.push_assistant(vec![Block::Text("(empty response)".to_string())]);
                self.feed = working;
                return Ok(String::new());
            }
            working.push_assistant(blocks.clone());

            let mut results = Vec::new();
            let mut step_text = Vec::new();
            for block in &blocks {
                match block {
                    Block::Text(text) => {
                        ui::print_text(text);
                        step_text.push(text.as_str());
                    }
                    Block::ToolUse {
                        id,
                        name,
                        input,
                        parse_error,
                    } => {
                        ui::print_tool(name, input);
                        let result = match parse_error {
                            Some(err) => format!("error: {err}"),
                            None => match tools::run(name, input) {
                                Ok(output) => output,
                                Err(err) => format!("error: {err}"),
                            },
                        };
                        ui::print_result(&result);
                        results.push(ToolResult {
                            tool_use_id: id.clone(),
                            content: result,
                        });
                    }
                }
            }

            if results.is_empty() {
                self.feed = working;
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
