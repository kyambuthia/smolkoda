use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use glob::glob;
use regex::Regex;
use serde_json::{json, Value};

use crate::presets::Protocol;

pub const MAX_TOOL_CHARS: usize = 40_000;
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "env",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "dist",
    "build",
    "target",
    ".next",
    ".idea",
];

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [(&'static str, &'static str, bool)],
}

pub static SPECS: &[ToolSpec] = &[
    ToolSpec {
        name: "read",
        description: "Read a text file with line numbers. offset is 1-based.",
        params: &[
            ("path", "string", true),
            ("offset", "integer", false),
            ("limit", "integer", false),
        ],
    },
    ToolSpec {
        name: "write",
        description: "Write content to a file, creating parent directories.",
        params: &[("path", "string", true), ("content", "string", true)],
    },
    ToolSpec {
        name: "edit",
        description: "Replace old with new in a file. old must be unique unless all=true.",
        params: &[
            ("path", "string", true),
            ("old", "string", true),
            ("new", "string", true),
            ("all", "boolean", false),
        ],
    },
    ToolSpec {
        name: "glob",
        description: "Find files by glob pattern, newest first.",
        params: &[("pat", "string", true), ("path", "string", false)],
    },
    ToolSpec {
        name: "grep",
        description: "Search files for a regex pattern (max 100 hits).",
        params: &[("pat", "string", true), ("path", "string", false)],
    },
    ToolSpec {
        name: "bash",
        description: "Run a shell command, streaming output. Default timeout 30s.",
        params: &[("cmd", "string", true), ("timeout", "number", false)],
    },
];

pub fn schemas(protocol: Protocol) -> Vec<Value> {
    SPECS
        .iter()
        .map(|spec| {
            let mut properties = serde_json::Map::new();
            let mut required = Vec::new();
            for (name, kind, is_required) in spec.params {
                properties.insert((*name).to_string(), json!({ "type": kind }));
                if *is_required {
                    required.push((*name).to_string());
                }
            }
            let schema = json!({
                "type": "object",
                "properties": properties,
                "required": required,
            });
            match protocol {
                Protocol::Anthropic => json!({
                    "name": spec.name,
                    "description": spec.description,
                    "input_schema": schema,
                }),
                Protocol::OpenAi => json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": schema,
                    },
                }),
            }
        })
        .collect()
}

pub fn run(name: &str, args: &Value) -> Result<String, String> {
    let result = match name {
        "read" => read(args),
        "write" => write(args),
        "edit" => edit(args),
        "glob" => glob_files(args),
        "grep" => grep(args),
        "bash" => bash(args),
        _ => Err(format!("unknown tool '{name}'")),
    };
    result.map(|text| truncate(&text))
}

fn string_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn path_arg(args: &Value, key: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(string_arg(args, key)?))
}

fn read(args: &Value) -> Result<String, String> {
    let path = path_arg(args, "path")?;
    if path.is_dir() {
        return Err(format!("{} is a directory", path.display()));
    }
    let metadata = fs::metadata(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    if metadata.len() > 5_000_000 {
        return Err(format!("{} is larger than 5 MB", path.display()));
    }
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(2000)
        .max(1) as usize;

    if offset > lines.len() {
        return Ok(format!(
            "(no lines at offset {offset}; file has {} lines)",
            lines.len()
        ));
    }
    let end = (offset - 1 + limit).min(lines.len());
    let width = end.to_string().len();
    let mut output = String::new();
    for (index, line) in lines[offset - 1..end].iter().enumerate() {
        output.push_str(&format!(
            "{:>width$}| {}\n",
            offset + index,
            line,
            width = width
        ));
    }
    if end < lines.len() {
        output.push_str(&format!("... ({} more lines)", lines.len() - end));
    }
    Ok(output.trim_end().to_string())
}

fn write(args: &Value) -> Result<String, String> {
    let path = path_arg(args, "path")?;
    let content = string_arg(args, "content")?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| format!("{}: {err}", parent.display()))?;
        }
    }
    fs::write(&path, &content).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok(format!(
        "wrote {} chars to {}",
        content.chars().count(),
        path.display()
    ))
}

fn edit(args: &Value) -> Result<String, String> {
    let path = path_arg(args, "path")?;
    let old = string_arg(args, "old")?;
    let new = args
        .get("new")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let replace_all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    if old.is_empty() {
        return Err("old string is empty".to_string());
    }
    let text = fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    let count = text.matches(&old).count();
    if count == 0 {
        return Err("old string not found".to_string());
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old string appears {count} times; add context or pass all=true"
        ));
    }
    let replaced = if replace_all {
        text.replace(&old, &new)
    } else {
        text.replacen(&old, &new, 1)
    };
    fs::write(&path, replaced).map_err(|err| format!("{}: {err}", path.display()))?;
    Ok("ok".to_string())
}

fn glob_files(args: &Value) -> Result<String, String> {
    let pattern = string_arg(args, "pat")?;
    let root = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let full = if Path::new(&pattern).is_absolute() {
        pattern
    } else {
        Path::new(root).join(&pattern).to_string_lossy().to_string()
    };
    let mut files = Vec::new();
    for path in glob(&full)
        .map_err(|err| format!("bad glob: {err}"))?
        .flatten()
    {
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());
    files.reverse();
    let list: Vec<String> = files
        .iter()
        .take(200)
        .map(|path| path.display().to_string())
        .collect();
    Ok(if list.is_empty() {
        "none".to_string()
    } else {
        list.join("\n")
    })
}

fn walk_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk_files(&path, files);
            }
        } else if path.is_file() {
            files.push(path);
        }
    }
}

fn grep(args: &Value) -> Result<String, String> {
    let pattern =
        Regex::new(&string_arg(args, "pat")?).map_err(|err| format!("bad regex: {err}"))?;
    let root = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or("."));
    let mut files = Vec::new();
    if root.is_file() {
        files.push(root);
    } else {
        walk_files(&root, &mut files);
    }

    let mut hits = Vec::new();
    'outer: for file in files {
        let Ok(metadata) = fs::metadata(&file) else {
            continue;
        };
        if metadata.len() > 2_000_000 {
            continue;
        }
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if pattern.is_match(line) {
                hits.push(format!("{}:{}:{}", file.display(), index + 1, line));
                if hits.len() >= 100 {
                    break 'outer;
                }
            }
        }
    }
    Ok(if hits.is_empty() {
        "none".to_string()
    } else {
        hits.join("\n")
    })
}

fn bash(args: &Value) -> Result<String, String> {
    let command = string_arg(args, "cmd")?;
    let timeout = args
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .max(1);

    let mut command_builder = if cfg!(windows) {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg(&command);
        cmd
    } else {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
        cmd
    };
    let mut child = command_builder
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("could not start command: {err}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "could not capture stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "could not capture stderr".to_string())?;
    let (sender, receiver) = mpsc::channel::<String>();

    let stdout_sender = sender.clone();
    let stdout_thread = thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = stdout_sender.send(line);
        }
    });
    let stderr_thread = thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = sender.send(line);
        }
    });

    let deadline = Instant::now() + Duration::from_secs(timeout);
    let mut chunks: Vec<String> = Vec::new();
    let mut size = 0usize;
    let mut truncated = false;
    let mut timed_out = false;

    loop {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        match receiver.recv_timeout((deadline - now).min(Duration::from_millis(100))) {
            Ok(line) => {
                if size < MAX_TOOL_CHARS {
                    size += line.len();
                    println!("  | {line}");
                    chunks.push(line);
                } else {
                    truncated = true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(_)) = child.try_wait() {
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    for line in receiver.try_iter() {
                        if size < MAX_TOOL_CHARS {
                            size += line.len();
                            println!("  | {line}");
                            chunks.push(line);
                        } else {
                            truncated = true;
                        }
                    }
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let _ = child.wait();
    let mut output = chunks.join("\n");
    if timed_out {
        output.push_str(&format!("\n(timed out after {timeout}s)"));
    }
    if truncated {
        output.push_str("\n(output truncated)");
    }
    if output.trim().is_empty() {
        output = "(empty)".to_string();
    }
    Ok(output)
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_TOOL_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_TOOL_CHARS).collect();
    format!("{kept}\n... truncated")
}
