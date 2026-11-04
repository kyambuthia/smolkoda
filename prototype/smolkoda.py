#!/usr/bin/env python3
"""smolkoda - a tiny agentic coding harness with pluggable providers.

Single file, standard library only. Works with Anthropic-compatible and
OpenAI-compatible APIs, plus presets for the common hosted and local ones.

    python3 smolkoda.py --setup
    python3 smolkoda.py
"""

from __future__ import annotations

import argparse
import getpass
import glob as globlib
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

VERSION = "0.1.0"
MAX_TOOL_CHARS = 40_000
MAX_STEPS = 50
SKIP_DIRS = {
    ".git", ".hg", ".svn", "node_modules", "__pycache__", ".venv", "venv",
    "env", ".mypy_cache", ".pytest_cache", ".ruff_cache", "dist", "build",
    "target", ".next", ".idea",
}

if os.environ.get("NO_COLOR") or not sys.stdout.isatty():
    RESET = BOLD = DIM = BLUE = CYAN = GREEN = YELLOW = RED = ""
else:
    RESET, BOLD, DIM = "\033[0m", "\033[1m", "\033[2m"
    BLUE, CYAN, GREEN, YELLOW, RED = (
        "\033[34m", "\033[36m", "\033[32m", "\033[33m", "\033[31m",
    )


# --- Provider presets -------------------------------------------------------

@dataclass(frozen=True)
class Preset:
    id: str
    label: str
    protocol: str          # "openai" or "anthropic"
    base_url: str
    default_model: str
    env: tuple[str, ...] = ()
    auth: str = "bearer"   # bearer | x-api-key | api-key | none
    models: tuple[str, ...] = ()
    headers: dict[str, str] = field(default_factory=dict)
    max_tokens_param: str = "max_tokens"
    note: str = ""


PRESETS: dict[str, Preset] = {
    "anthropic": Preset(
        "anthropic", "Anthropic", "anthropic",
        "https://api.anthropic.com/v1", "claude-sonnet-4-5",
        ("ANTHROPIC_API_KEY",), "x-api-key",
        ("claude-opus-4-5", "claude-sonnet-4-5", "claude-haiku-4-5"),
    ),
    "openai": Preset(
        "openai", "OpenAI", "openai",
        "https://api.openai.com/v1", "gpt-5.2",
        ("OPENAI_API_KEY",), "bearer",
        ("gpt-5.2", "gpt-5.1", "gpt-5-mini"),
        max_tokens_param="max_completion_tokens",
    ),
    "openrouter": Preset(
        "openrouter", "OpenRouter", "openai",
        "https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.5",
        ("OPENROUTER_API_KEY",), "bearer",
        ("anthropic/claude-sonnet-4.5", "openai/gpt-5.2",
         "google/gemini-2.5-pro"),
        headers={"X-Title": "smolkoda"},
        note="One key for most hosted models.",
    ),
    "google": Preset(
        "google", "Google Gemini", "openai",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        "gemini-2.5-pro",
        ("GEMINI_API_KEY", "GOOGLE_API_KEY"), "bearer",
        ("gemini-2.5-pro", "gemini-2.5-flash"),
        note="Uses Gemini's OpenAI-compatible endpoint.",
    ),
    "deepseek": Preset(
        "deepseek", "DeepSeek", "openai",
        "https://api.deepseek.com/v1", "deepseek-chat",
        ("DEEPSEEK_API_KEY",), "bearer",
        ("deepseek-chat", "deepseek-reasoner"),
    ),
    "xai": Preset(
        "xai", "xAI", "openai",
        "https://api.x.ai/v1", "grok-4",
        ("XAI_API_KEY",), "bearer",
        ("grok-4", "grok-3"),
    ),
    "groq": Preset(
        "groq", "Groq", "openai",
        "https://api.groq.com/openai/v1", "moonshotai/kimi-k2-instruct",
        ("GROQ_API_KEY",), "bearer",
        ("moonshotai/kimi-k2-instruct", "llama-3.3-70b-versatile"),
    ),
    "mistral": Preset(
        "mistral", "Mistral", "openai",
        "https://api.mistral.ai/v1", "mistral-large-latest",
        ("MISTRAL_API_KEY",), "bearer",
        ("mistral-large-latest", "mistral-medium-latest"),
    ),
    "cohere": Preset(
        "cohere", "Cohere", "openai",
        "https://api.cohere.ai/compatibility/v1", "command-a-03-2025",
        ("COHERE_API_KEY",), "bearer",
        ("command-a-03-2025",),
    ),
    "together": Preset(
        "together", "Together AI", "openai",
        "https://api.together.xyz/v1", "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        ("TOGETHER_API_KEY",), "bearer",
        ("meta-llama/Llama-3.3-70B-Instruct-Turbo",),
    ),
    "fireworks": Preset(
        "fireworks", "Fireworks AI", "openai",
        "https://api.fireworks.ai/inference/v1",
        "accounts/fireworks/models/llama-v3p3-70b-instruct",
        ("FIREWORKS_API_KEY",), "bearer",
        ("accounts/fireworks/models/llama-v3p3-70b-instruct",),
    ),
    "cerebras": Preset(
        "cerebras", "Cerebras", "openai",
        "https://api.cerebras.ai/v1", "llama-3.3-70b",
        ("CEREBRAS_API_KEY",), "bearer",
        ("llama-3.3-70b",),
    ),
    "moonshot": Preset(
        "moonshot", "Moonshot / Kimi", "openai",
        "https://api.moonshot.ai/v1", "kimi-k2-0905-preview",
        ("MOONSHOT_API_KEY",), "bearer",
        ("kimi-k2-0905-preview",),
    ),
    "github": Preset(
        "github", "GitHub Models", "openai",
        "https://models.inference.ai.azure.com", "openai/gpt-4.1",
        ("GITHUB_TOKEN", "GH_TOKEN"), "bearer",
        ("openai/gpt-4.1", "openai/gpt-4o"),
        note="Uses a GitHub token with Models access.",
    ),
    "ollama": Preset(
        "ollama", "Ollama (local)", "openai",
        "http://localhost:11434/v1", "qwen3:8b",
        (), "none",
        ("qwen3:8b", "qwen3:32b", "llama3.3"),
        note="Start Ollama first; tool support depends on the model.",
    ),
    "lmstudio": Preset(
        "lmstudio", "LM Studio (local)", "openai",
        "http://localhost:1234/v1", "qwen3-8b",
        (), "none",
        ("qwen3-8b",),
    ),
    "vllm": Preset(
        "vllm", "vLLM (local)", "openai",
        "http://localhost:8000/v1", "Qwen/Qwen3-32B",
        (), "none",
        ("Qwen/Qwen3-32B",),
    ),
    "llamacpp": Preset(
        "llamacpp", "llama.cpp (local)", "openai",
        "http://localhost:8080/v1", "local-model",
        (), "none",
        ("local-model",),
    ),
    "azure": Preset(
        "azure", "Azure OpenAI", "openai",
        "https://YOUR-RESOURCE.openai.azure.com/openai/deployments/"
        "YOUR-DEPLOYMENT/chat/completions?api-version=2024-10-21",
        "gpt-4o",
        ("AZURE_OPENAI_API_KEY", "AZURE_API_KEY"), "api-key",
        ("gpt-4o", "gpt-4.1"),
        max_tokens_param="max_completion_tokens",
        note="Replace YOUR-RESOURCE and YOUR-DEPLOYMENT in the URL.",
    ),
}

AUTO_ORDER = (
    "anthropic", "openai", "openrouter", "google", "deepseek", "xai",
    "groq", "mistral",
)

# --- Config and settings ----------------------------------------------------

def config_path(override: str | None = None) -> Path:
    if override:
        return Path(override).expanduser()
    if os.environ.get("SMOLKODA_CONFIG"):
        return Path(os.environ["SMOLKODA_CONFIG"]).expanduser()
    base = Path(os.environ.get("XDG_CONFIG_HOME") or (Path.home() / ".config"))
    return base / "smolkoda" / "config.json"


def load_config(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {"active": None, "providers": {}}
    try:
        data = json.loads(path.read_text())
        if not isinstance(data, dict):
            raise ValueError("config root must be an object")
    except Exception as err:
        print(f"{YELLOW}warning: could not read {path}: {err}{RESET}")
        return {"active": None, "providers": {}}
    data.setdefault("active", None)
    data.setdefault("providers", {})
    if not isinstance(data["providers"], dict):
        data["providers"] = {}
    return data


def save_config(path: Path, cfg: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(json.dumps(cfg, indent=2) + "\n")
    try:
        os.chmod(tmp, 0o600)
    except OSError:
        pass
    os.replace(tmp, path)


def resolve_secret(value: str) -> str:
    """Resolve 'env:NAME' and '${NAME}' references; otherwise literal."""
    value = (value or "").strip()
    if value.startswith("env:"):
        return os.environ.get(value[4:].strip(), "")
    match = re.fullmatch(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}", value)
    if match:
        return os.environ.get(match.group(1), "")
    return value


def default_key_ref(preset: Preset | None) -> str:
    if not preset:
        return ""
    for var in preset.env:
        if os.environ.get(var):
            return f"env:{var}"
    return ""


@dataclass
class Settings:
    name: str
    protocol: str
    base_url: str
    model: str
    api_key: str
    auth: str
    headers: dict[str, str]
    max_tokens: int = 8192
    max_tokens_param: str = "max_tokens"
    temperature: float | None = None
    timeout: int = 180


def pick_provider(cfg: dict[str, Any], requested: str | None) -> str:
    if requested:
        return requested
    if os.environ.get("SMOLKODA_PROVIDER"):
        return os.environ["SMOLKODA_PROVIDER"]
    if cfg.get("active"):
        return cfg["active"]
    for pid in AUTO_ORDER:
        preset = PRESETS[pid]
        if any(os.environ.get(var) for var in preset.env):
            return pid
    profiles = cfg.get("providers") or {}
    if profiles:
        return next(iter(profiles))
    return "openrouter"


def build_settings(
    name: str,
    cfg: dict[str, Any],
    overrides: Any = None,
) -> Settings:
    """Layer CLI/env overrides on top of a saved profile and its preset."""
    def opt(attr: str) -> Any:
        return getattr(overrides, attr, None) if overrides is not None else None

    profiles = cfg.get("providers") or {}
    profile = profiles.get(name) or {}
    preset = PRESETS.get(profile.get("preset") or name)
    if preset is None and not profile:
        known = ", ".join(sorted(set(PRESETS) | set(profiles)))
        raise ValueError(f"unknown provider '{name}' (known: {known})")

    protocol = (
        opt("protocol") or os.environ.get("SMOLKODA_PROTOCOL")
        or profile.get("protocol") or (preset.protocol if preset else "openai")
    )
    base_url = (
        opt("base_url") or os.environ.get("SMOLKODA_BASE_URL")
        or profile.get("base_url") or (preset.base_url if preset else "")
    )
    model = (
        opt("model") or os.environ.get("SMOLKODA_MODEL")
        or profile.get("model") or (preset.default_model if preset else "")
    )
    auth = (
        opt("auth") or profile.get("auth")
        or (preset.auth if preset else "bearer")
    )

    # API key precedence: CLI > SMOLKODA_API_KEY > profile (even if empty) >
    # the preset's first populated env var.
    if opt("api_key") is not None:
        raw_key = opt("api_key")
    elif os.environ.get("SMOLKODA_API_KEY"):
        raw_key = os.environ["SMOLKODA_API_KEY"]
    elif "api_key" in profile:
        raw_key = profile.get("api_key") or ""
    else:
        raw_key = default_key_ref(preset)

    headers: dict[str, str] = {}
    if preset:
        headers.update(preset.headers)
    headers.update({str(k): str(v) for k, v in (profile.get("headers") or {}).items()})

    def number(attr: str, env: str, default: Any) -> Any:
        value = opt(attr)
        if value is not None:
            return value
        if os.environ.get(env):
            return os.environ[env]
        if profile.get(attr) is not None:
            return profile[attr]
        return default

    max_tokens = int(number("max_tokens", "SMOLKODA_MAX_TOKENS", 8192))
    timeout = int(number("timeout", "SMOLKODA_TIMEOUT", 180))
    temperature_raw = number("temperature", "SMOLKODA_TEMPERATURE", None)
    temperature = None if temperature_raw in (None, "") else float(temperature_raw)
    max_tokens_param = (
        profile.get("max_tokens_param")
        or (preset.max_tokens_param if preset else "max_tokens")
    )

    if not base_url:
        raise ValueError(f"provider '{name}' has no base URL; run --setup")
    if not model:
        raise ValueError(f"provider '{name}' has no model; run --setup")

    return Settings(
        name=name,
        protocol=protocol,
        base_url=base_url,
        model=model,
        api_key=resolve_secret(raw_key),
        auth=auth,
        headers=headers,
        max_tokens=max_tokens,
        max_tokens_param=max_tokens_param,
        temperature=temperature,
        timeout=timeout,
    )


def endpoint(settings: Settings) -> str:
    url = settings.base_url.rstrip("/")
    if settings.protocol == "anthropic":
        return url if "/messages" in url else url + "/messages"
    return url if "/chat/completions" in url else url + "/chat/completions"


def mask_secret(value: str) -> str:
    if not value:
        return "(none)"
    if value.startswith("env:") or value.startswith("${"):
        return value
    if len(value) <= 8:
        return "***"
    return value[:4] + "..." + value[-4:]

# --- API layer --------------------------------------------------------------

def format_http_error(err: urllib.error.HTTPError) -> str:
    body = err.read().decode("utf-8", "replace").strip()
    message = body
    try:
        data = json.loads(body)
        detail = data.get("error")
        if isinstance(detail, dict):
            message = detail.get("message") or detail.get("type") or body
        elif isinstance(detail, str):
            message = detail
        elif data.get("message"):
            message = data["message"]
    except Exception:
        pass
    return f"HTTP {err.code} {err.reason}: {str(message)[:1000]}"


ANTHROPIC_BLOCK_KEYS = {
    "text": ("type", "text"),
    "tool_use": ("type", "id", "name", "input"),
    "tool_result": ("type", "tool_use_id", "content", "is_error"),
    "thinking": ("type", "thinking", "signature"),
    "redacted_thinking": ("type", "data"),
}


def sanitize_anthropic(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Drop provider-neutral extras (e.g. parse_error) before Anthropic calls."""
    out = []
    for message in messages:
        content = message.get("content")
        if isinstance(content, list):
            blocks = []
            for block in content:
                allowed = ANTHROPIC_BLOCK_KEYS.get(block.get("type"))
                if allowed:
                    blocks.append({key: block[key] for key in allowed if key in block})
            out.append({"role": message.get("role"), "content": blocks})
        else:
            out.append(message)
    return out


def to_openai_messages(messages: list[dict[str, Any]]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for message in messages:
        role = message.get("role")
        content = message.get("content")
        if isinstance(content, str):
            out.append({"role": role, "content": content})
            continue
        if role == "assistant":
            texts: list[str] = []
            calls: list[dict[str, Any]] = []
            for block in content or []:
                if block.get("type") == "text" and block.get("text"):
                    texts.append(block["text"])
                elif block.get("type") == "tool_use":
                    calls.append({
                        "id": block.get("id") or f"call_{uuid.uuid4().hex[:12]}",
                        "type": "function",
                        "function": {
                            "name": block.get("name", ""),
                            "arguments": json.dumps(
                                block.get("input") or {}, ensure_ascii=False
                            ),
                        },
                    })
            converted: dict[str, Any] = {
                "role": "assistant",
                "content": "\n".join(texts) or None,
            }
            if calls:
                converted["tool_calls"] = calls
            out.append(converted)
        else:
            texts = []
            for block in content or []:
                if block.get("type") == "tool_result":
                    out.append({
                        "role": "tool",
                        "tool_call_id": block.get("tool_use_id", ""),
                        "content": str(block.get("content", "")),
                    })
                elif block.get("type") == "text":
                    texts.append(block.get("text", ""))
            if texts:
                out.append({"role": "user", "content": "\n".join(texts)})
    return out


def call_model(
    settings: Settings,
    messages: list[dict[str, Any]],
    system: str,
    tools: list[dict[str, Any]] | None = None,
    max_tokens: int | None = None,
) -> dict[str, Any]:
    url = endpoint(settings)
    headers = {"content-type": "application/json"}

    if settings.protocol == "anthropic":
        body: dict[str, Any] = {
            "model": settings.model,
            "max_tokens": int(max_tokens or settings.max_tokens),
            "messages": sanitize_anthropic(messages),
        }
        if system:
            body["system"] = system
        if tools:
            body["tools"] = tools
        if settings.temperature is not None:
            body["temperature"] = settings.temperature
        headers["anthropic-version"] = "2023-06-01"
    else:
        converted = ([{"role": "system", "content": system}] if system else [])
        body = {
            "model": settings.model,
            "messages": converted + to_openai_messages(messages),
            settings.max_tokens_param: int(max_tokens or settings.max_tokens),
        }
        if tools:
            body["tools"] = tools
        if settings.temperature is not None:
            body["temperature"] = settings.temperature

    headers.update(settings.headers)
    if settings.api_key:
        if settings.auth == "bearer":
            headers["authorization"] = f"Bearer {settings.api_key}"
        elif settings.auth == "x-api-key":
            headers["x-api-key"] = settings.api_key
        elif settings.auth == "api-key":
            headers["api-key"] = settings.api_key

    request = urllib.request.Request(
        url,
        data=json.dumps(body, ensure_ascii=False).encode(),
        headers=headers,
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=settings.timeout) as response:
            raw = response.read()
    except urllib.error.HTTPError as err:
        raise RuntimeError(format_http_error(err)) from None
    except urllib.error.URLError as err:
        raise RuntimeError(f"connection failed: {err.reason}") from None
    except TimeoutError:
        raise RuntimeError(f"request timed out after {settings.timeout}s") from None

    try:
        return json.loads(raw)
    except json.JSONDecodeError as err:
        raise RuntimeError(f"provider returned invalid JSON: {err}") from None


def parse_response(settings: Settings, data: dict[str, Any]) -> list[dict[str, Any]]:
    if settings.protocol == "anthropic":
        blocks = data.get("content")
        if not isinstance(blocks, list):
            raise RuntimeError(f"unexpected response: {json.dumps(data)[:500]}")
        return blocks

    choices = data.get("choices")
    if not choices:
        raise RuntimeError(f"unexpected response: {json.dumps(data)[:500]}")
    message = choices[0].get("message") or {}
    blocks: list[dict[str, Any]] = []

    content = message.get("content")
    if isinstance(content, str) and content.strip():
        blocks.append({"type": "text", "text": content})
    elif isinstance(content, list):
        for part in content:
            if (
                isinstance(part, dict)
                and part.get("type") in ("text", "output_text")
                and part.get("text")
            ):
                blocks.append({"type": "text", "text": part["text"]})

    for call in message.get("tool_calls") or []:
        function = call.get("function") or {}
        name = function.get("name") or ""
        raw_args = function.get("arguments")
        parse_error = None
        if isinstance(raw_args, dict):
            args = raw_args
        else:
            raw_args = raw_args or "{}"
            try:
                args = json.loads(raw_args)
            except json.JSONDecodeError as err:
                args = {}
                parse_error = f"invalid JSON for {name}: {err}; raw={raw_args[:200]}"
        if not isinstance(args, dict):
            parse_error = f"tool arguments must be a JSON object, got {type(args).__name__}"
            args = {}
        block: dict[str, Any] = {
            "type": "tool_use",
            "id": call.get("id") or f"call_{uuid.uuid4().hex[:12]}",
            "name": name,
            "input": args,
        }
        if parse_error:
            block["parse_error"] = parse_error
        blocks.append(block)

    return blocks

# --- Tools ------------------------------------------------------------------

def tool_read(args: dict[str, Any]) -> str:
    path = Path(str(args["path"])).expanduser()
    if path.is_dir():
        return f"error: {path} is a directory"
    if path.stat().st_size > 5_000_000:
        return f"error: {path} is larger than 5 MB"
    lines = path.read_text(errors="replace").splitlines()
    offset = max(int(args.get("offset", 1)), 1)
    limit = max(int(args.get("limit", 2000)), 1)
    selected = lines[offset - 1:offset - 1 + limit]
    if not selected:
        return f"(no lines at offset {offset}; file has {len(lines)} lines)"
    width = len(str(offset + len(selected) - 1))
    body = "\n".join(
        f"{offset + index:>{width}}| {line}"
        for index, line in enumerate(selected)
    )
    remaining = len(lines) - (offset - 1 + len(selected))
    if remaining > 0:
        body += f"\n... ({remaining} more lines)"
    return body


def tool_write(args: dict[str, Any]) -> str:
    path = Path(str(args["path"])).expanduser()
    content = str(args.get("content", ""))
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content)
    return f"wrote {len(content)} chars to {path}"


def tool_edit(args: dict[str, Any]) -> str:
    path = Path(str(args["path"])).expanduser()
    text = path.read_text()
    old = str(args["old"])
    new = str(args.get("new", ""))
    if not old:
        return "error: old string is empty"
    count = text.count(old)
    if count == 0:
        return "error: old string not found"
    if count > 1 and not args.get("all"):
        return (
            f"error: old string appears {count} times; add context "
            "or pass all=true"
        )
    replaced = text.replace(old, new) if args.get("all") else text.replace(old, new, 1)
    path.write_text(replaced)
    return "ok"


def tool_glob(args: dict[str, Any]) -> str:
    pattern = str(args["pat"])
    if not os.path.isabs(pattern):
        pattern = str(Path(str(args.get("path", "."))).expanduser() / pattern)
    files = [name for name in globlib.glob(pattern, recursive=True) if os.path.isfile(name)]
    files.sort(key=os.path.getmtime, reverse=True)
    return "\n".join(files[:200]) or "none"


def tool_grep(args: dict[str, Any]) -> str:
    pattern = re.compile(str(args["pat"]))
    root = Path(str(args.get("path", "."))).expanduser()
    if root.is_file():
        files = [root]
    else:
        files = []
        for dirpath, dirnames, filenames in os.walk(root):
            dirnames[:] = [name for name in dirnames if name not in SKIP_DIRS]
            files.extend(Path(dirpath) / name for name in filenames)

    hits: list[str] = []
    for file in files:
        try:
            if file.stat().st_size > 2_000_000:
                continue
            with file.open(errors="replace") as handle:
                for number, line in enumerate(handle, 1):
                    if pattern.search(line):
                        hits.append(f"{file}:{number}:{line.rstrip()}")
                        if len(hits) >= 100:
                            return "\n".join(hits) + "\n... (limited to 100 hits)"
        except (OSError, ValueError):
            continue
    return "\n".join(hits) or "none"


def tool_bash(args: dict[str, Any]) -> str:
    command = str(args["cmd"])
    timeout = float(args.get("timeout", 30))
    proc = subprocess.Popen(
        command,
        shell=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert proc.stdout is not None
    lines: queue.Queue = queue.Queue()

    def reader() -> None:
        try:
            for line in proc.stdout:
                lines.put(line)
        finally:
            lines.put(None)

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()

    chunks: list[str] = []
    size = 0
    truncated = False
    timed_out = False

    def take(line: str) -> None:
        nonlocal size, truncated
        if size < MAX_TOOL_CHARS:
            chunks.append(line)
            size += len(line)
            print(f"  {DIM}| {line.rstrip()}{RESET}", flush=True)
        else:
            truncated = True

    deadline = time.monotonic() + timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            timed_out = True
            proc.kill()
            break
        try:
            line = lines.get(timeout=min(0.1, remaining))
        except queue.Empty:
            if proc.poll() is not None:
                thread.join(timeout=0.5)
                while True:
                    try:
                        pending = lines.get_nowait()
                    except queue.Empty:
                        break
                    if pending is None:
                        break
                    take(pending)
                break
            continue
        if line is None:
            break
        take(line)

    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()

    output = "".join(chunks).strip()
    if timed_out:
        output += f"\n(timed out after {timeout:g}s)"
    if truncated:
        output += "\n(output truncated)"
    return output or "(empty)"


# --- Tool registry: name -> (description, parameter types, function) --------

TOOLS: dict[str, tuple[str, dict[str, str], Any]] = {
    "read": (
        "Read a text file with line numbers. offset is 1-based.",
        {"path": "string", "offset": "integer?", "limit": "integer?"},
        tool_read,
    ),
    "write": (
        "Write content to a file, creating parent directories.",
        {"path": "string", "content": "string"},
        tool_write,
    ),
    "edit": (
        "Replace old with new in a file. old must be unique unless all=true.",
        {"path": "string", "old": "string", "new": "string", "all": "boolean?"},
        tool_edit,
    ),
    "glob": (
        "Find files by glob pattern, newest first.",
        {"pat": "string", "path": "string?"},
        tool_glob,
    ),
    "grep": (
        "Search files for a regex pattern (max 100 hits).",
        {"pat": "string", "path": "string?"},
        tool_grep,
    ),
    "bash": (
        "Run a shell command, streaming output. Default timeout 30s.",
        {"cmd": "string", "timeout": "number?"},
        tool_bash,
    ),
}


def make_tools(protocol: str) -> list[dict[str, Any]]:
    schemas = []
    for name, (description, params, _function) in TOOLS.items():
        properties: dict[str, Any] = {}
        required: list[str] = []
        for param, kind in params.items():
            optional = kind.endswith("?")
            properties[param] = {"type": kind.rstrip("?")}
            if not optional:
                required.append(param)
        schema = {
            "type": "object",
            "properties": properties,
            "required": required,
        }
        if protocol == "anthropic":
            schemas.append({
                "name": name,
                "description": description,
                "input_schema": schema,
            })
        else:
            schemas.append({
                "type": "function",
                "function": {
                    "name": name,
                    "description": description,
                    "parameters": schema,
                },
            })
    return schemas


def truncate(text: str) -> str:
    if len(text) <= MAX_TOOL_CHARS:
        return text
    return text[:MAX_TOOL_CHARS] + f"\n... truncated {len(text) - MAX_TOOL_CHARS} chars"


def run_tool(name: str, args: dict[str, Any]) -> str:
    tool = TOOLS.get(name)
    if not tool:
        return f"error: unknown tool '{name}'"
    try:
        return truncate(str(tool[2](args)))
    except Exception as err:
        return f"error: {type(err).__name__}: {err}"

# --- Display and agent loop -------------------------------------------------

def render_markdown(text: str) -> str:
    return re.sub(r"\*\*(.+?)\*\*", f"{BOLD}\\1{RESET}", text)


def print_text(text: str) -> None:
    lines = text.splitlines() or [""]
    print(f"\n{CYAN}*{RESET} {render_markdown(lines[0])}")
    for line in lines[1:]:
        print(f"  {render_markdown(line)}")


def preview_args(args: dict[str, Any]) -> str:
    if not args:
        return ""
    value = str(next(iter(args.values()))).replace("\n", " ")
    return value[:50] + ("..." if len(value) > 50 else "")


def print_result_preview(result: str) -> None:
    lines = result.splitlines() or [""]
    preview = lines[0][:80]
    if len(lines) > 1:
        preview += f" ... +{len(lines) - 1} lines"
    elif len(lines[0]) > 80:
        preview += "..."
    print(f"  {DIM}-> {preview}{RESET}")


def system_prompt() -> str:
    return (
        f"You are smolkoda, a concise coding agent. cwd: {os.getcwd()}\n"
        "Use tools to inspect and change files. Prefer read/glob/grep over "
        "bash for exploration. Make minimal, correct changes and briefly "
        "report what you did."
    )


def run_turn(
    settings: Settings,
    messages: list[dict[str, Any]],
    user_input: str,
) -> None:
    """Run one user turn. History is committed only if the turn succeeds."""
    working = list(messages)
    working.append({"role": "user", "content": user_input})
    system = system_prompt()
    tools = make_tools(settings.protocol)
    step = 0

    while True:
        step += 1
        if step > MAX_STEPS:
            raise RuntimeError(f"tool loop exceeded {MAX_STEPS} steps")
        data = call_model(settings, working, system, tools)
        blocks = parse_response(settings, data)
        if not blocks:
            blocks = [{"type": "text", "text": "(empty response)"}]
        working.append({"role": "assistant", "content": blocks})

        results = []
        for block in blocks:
            kind = block.get("type")
            if kind == "text":
                print_text(block.get("text", ""))
            elif kind == "tool_use":
                name = block.get("name", "?")
                args = block.get("input") or {}
                print(
                    f"\n{GREEN}* {name.capitalize()}{RESET}"
                    f"({DIM}{preview_args(args)}{RESET})"
                )
                if block.get("parse_error"):
                    result = f"error: {block['parse_error']}"
                else:
                    result = run_tool(name, args)
                print_result_preview(result)
                results.append({
                    "type": "tool_result",
                    "tool_use_id": block.get("id", ""),
                    "content": result,
                })

        if not results:
            if not any(
                block.get("type") == "text" and block.get("text")
                for block in blocks
            ):
                print(f"{DIM}(no text response){RESET}")
            messages[:] = working
            return
        working.append({"role": "user", "content": results})


def response_text(settings: Settings, data: dict[str, Any]) -> str:
    blocks = parse_response(settings, data)
    return " ".join(
        block.get("text", "") for block in blocks if block.get("type") == "text"
    ).strip()


def test_connection(settings: Settings) -> bool:
    print(f"{DIM}testing {settings.name} -> {endpoint(settings)} ...{RESET}")
    try:
        data = call_model(
            settings,
            [{"role": "user", "content": "Reply with exactly: OK"}],
            system="",
            tools=None,
            max_tokens=32,
        )
        text = response_text(settings, data)
        print(f"{GREEN}connected{RESET}" + (f" - {text[:80]}" if text else ""))
        return True
    except Exception as err:
        print(f"{RED}connection failed: {err}{RESET}")
        return False

# --- Setup wizard -----------------------------------------------------------

def ask(prompt: str, default: str = "") -> str | None:
    suffix = f" [{default}]" if default else ""
    try:
        raw = input(f"{BOLD}{prompt}{RESET}{suffix}: ").strip()
    except (EOFError, KeyboardInterrupt):
        print()
        return None
    return raw or default


def ask_secret(prompt: str, default: str = "") -> str | None:
    suffix = f" [{default}]" if default else ""
    try:
        if sys.stdin.isatty():
            raw = getpass.getpass(f"{prompt}{suffix}: ")
        else:
            raw = input(f"{prompt}{suffix}: ")
    except (EOFError, KeyboardInterrupt):
        print()
        return None
    raw = raw.strip()
    return raw or default


def setup_wizard(cfg: dict[str, Any], path: Path, preferred: str | None = None) -> bool:
    ids = list(PRESETS)
    profiles = cfg.get("providers") or {}
    preferred_profile = preferred if preferred in profiles else None
    preferred_preset = (
        profiles.get(preferred_profile, {}).get("preset") if preferred_profile else None
    )
    if preferred_profile:
        preferred = preferred_preset
    print(f"\n{BOLD}smolkoda provider setup{RESET}")
    print(f"{DIM}Pick a preset or configure any compatible endpoint.{RESET}")
    print(f"{DIM}Config: {path}{RESET}\n")
    for index, pid in enumerate(ids, 1):
        preset = PRESETS[pid]
        active = f" {GREEN}*{RESET}" if pid == cfg.get("active") else ""
        print(f"  {index:2}. {pid:<11} {preset.label}{active}")
    print(f"   0. {'custom':<11} Any OpenAI- or Anthropic-compatible endpoint")

    default_choice = str(ids.index(preferred) + 1) if preferred in ids else ""
    choice = ask("Provider", default_choice)
    if choice is None:
        return False

    extra: dict[str, Any] = {}
    if choice == "0":
        preset = None
        pid = None
        protocol = ask("Protocol (openai/anthropic)", "openai")
        if protocol not in ("openai", "anthropic"):
            print(f"{RED}Invalid protocol.{RESET}")
            return False
        base_url = ask("Base URL", "")
        auth = ask("Auth header (bearer/api-key/x-api-key/none)", "bearer")
        if auth not in ("bearer", "api-key", "x-api-key", "none"):
            print(f"{RED}Invalid auth.{RESET}")
            return False
        key = "" if auth == "none" else ask_secret("API key (or env:VAR)", "")
        model = ask("Model", "")
        if key is None or not base_url or not model:
            print(f"{YELLOW}Cancelled.{RESET}")
            return False
        default_name = preferred_profile or "custom"
        raw_headers = ask("Extra headers as JSON (optional)", "")
        if raw_headers:
            try:
                headers = json.loads(raw_headers)
                if not isinstance(headers, dict):
                    raise ValueError("not an object")
                extra["headers"] = {str(k): str(v) for k, v in headers.items()}
            except Exception:
                print(f"{RED}Invalid headers JSON; skipped.{RESET}")
    else:
        try:
            pid = ids[int(choice) - 1]
        except (ValueError, IndexError):
            print(f"{RED}Invalid choice.{RESET}")
            return False
        preset = PRESETS[pid]
        print(f"\n{DIM}{preset.label} - {preset.protocol} - {preset.base_url}{RESET}")
        if preset.note:
            print(f"{DIM}{preset.note}{RESET}")
        if preset.models:
            print(f"{DIM}models: {', '.join(preset.models)}{RESET}")
        base_url = ask("Base URL", preset.base_url)
        if preset.auth == "none":
            key = ""
            print(f"{DIM}No API key needed.{RESET}")
        else:
            key = ask_secret(
                "API key (blank keeps env reference)",
                default_key_ref(preset),
            )
        model = ask("Model", preset.default_model)
        if key is None or not base_url or not model:
            print(f"{YELLOW}Cancelled.{RESET}")
            return False
        protocol = preset.protocol
        auth = preset.auth
        default_name = preferred_profile if pid == preferred_preset else pid

    name = ask("Profile name", default_name)
    if not name:
        print(f"{YELLOW}Cancelled.{RESET}")
        return False
    if name in (cfg.get("providers") or {}):
        confirm = ask(f"Profile '{name}' exists; overwrite? (y/N)", "n")
        if confirm is None or confirm.lower() not in ("y", "yes"):
            print(f"{YELLOW}Cancelled.{RESET}")
            return False

    profile: dict[str, Any] = {
        "model": model,
        "base_url": base_url,
        "api_key": key,
    }
    if preset:
        profile["preset"] = pid
        if auth != preset.auth:
            profile["auth"] = auth
        if protocol != preset.protocol:
            profile["protocol"] = protocol
    else:
        profile["protocol"] = protocol
        profile["auth"] = auth
    profile.update(extra)

    cfg.setdefault("providers", {})[name] = profile
    cfg["active"] = name
    save_config(path, cfg)
    print(f"\n{GREEN}Saved profile '{name}' to {path}{RESET}")
    missing_key = auth != "none" and not resolve_secret(key)
    if missing_key:
        print(f"{YELLOW}No API key found; set it before chatting.{RESET}")
    elif (ask("Test connection now? (Y/n)", "y") or "").lower() in ("y", "yes"):
        try:
            test_connection(build_settings(name, cfg, None))
        except Exception as err:
            print(f"{RED}connection failed: {err}{RESET}")
    return True


def print_providers(path: Path, cfg: dict[str, Any]) -> None:
    print(f"{BOLD}Provider presets{RESET}")
    for pid, preset in PRESETS.items():
        marker = "*" if pid == cfg.get("active") else " "
        env = ", ".join(preset.env) or "-"
        print(
            f" {marker} {pid:<11} {preset.protocol:<9} "
            f"{preset.default_model:<42} {env}"
        )
    profiles = cfg.get("providers") or {}
    if profiles:
        print(f"\n{BOLD}Saved profiles{RESET}")
        for name, profile in profiles.items():
            print(
                f"   {name} (preset: {profile.get('preset', 'custom')}, "
                f"model: {profile.get('model', '?')})"
            )
    print(f"\n{DIM}Config: {path}{RESET}")
    print(f"{DIM}Add your own with: python3 smolkoda.py --setup{RESET}")


def print_config(settings: Settings, path: Path) -> None:
    print(f"\n{BOLD}Active provider{RESET}")
    print(f"  profile:  {settings.name}")
    print(f"  protocol: {settings.protocol}")
    print(f"  base url: {settings.base_url}")
    print(f"  model:    {settings.model}")
    print(f"  auth:     {settings.auth} ({mask_secret(settings.api_key)})")
    print(f"  max toks: {settings.max_tokens}")
    print(f"  config:   {path}")


def switch_provider(
    name: str,
    cfg: dict[str, Any],
    path: Path,
) -> Settings | None:
    if name not in PRESETS and name not in (cfg.get("providers") or {}):
        print(f"{RED}Unknown provider '{name}'. Try /provider to list them.{RESET}")
        return None
    try:
        settings = build_settings(name, cfg, None)
    except Exception as err:
        print(f"{RED}{err}{RESET}")
        return None
    if settings.auth != "none" and not settings.api_key:
        print(f"{YELLOW}No API key configured for '{name}'.{RESET}")
        if not setup_wizard(cfg, path, name):
            return None
        name = cfg.get("active") or name
        try:
            settings = build_settings(name, cfg, None)
        except Exception as err:
            print(f"{RED}{err}{RESET}")
            return None
    cfg["active"] = name
    save_config(path, cfg)
    print(f"{GREEN}Switched to {name} ({settings.model}){RESET}")
    return settings

# --- REPL and CLI -----------------------------------------------------------

def repl(settings: Settings, cfg: dict[str, Any], path: Path) -> None:
    print(
        f"{BOLD}smolkoda{RESET} {DIM}v{VERSION} - {settings.model} - "
        f"{settings.name}{RESET}"
    )
    print(f"{DIM}{endpoint(settings)}{RESET}")
    print(f"{DIM}/help for commands, /q to quit{RESET}\n")
    messages: list[dict[str, Any]] = []

    while True:
        try:
            line = input(f"{BOLD}{BLUE}>{RESET} ").strip()
        except (EOFError, KeyboardInterrupt):
            print()
            break
        if not line:
            continue
        if line in ("/q", "/quit", "exit", "quit"):
            break
        if line == "/c":
            messages.clear()
            print(f"{GREEN}Conversation cleared.{RESET}")
            continue
        if line.startswith("/provider"):
            _, _, argument = line.partition(" ")
            argument = argument.strip()
            if not argument:
                print_providers(path, cfg)
                argument = (ask("Switch to (blank to cancel)", "") or "").strip()
                if not argument:
                    continue
            new_settings = switch_provider(argument, cfg, path)
            if new_settings:
                settings = new_settings
            continue
        if line.startswith("/model"):
            _, _, argument = line.partition(" ")
            argument = argument.strip()
            if not argument:
                print(f"model: {settings.model}")
                continue
            settings.model = argument
            profile = (cfg.setdefault("providers", {})).setdefault(settings.name, {})
            if settings.name in PRESETS and "preset" not in profile:
                profile["preset"] = settings.name
            profile["model"] = argument
            cfg["active"] = settings.name
            save_config(path, cfg)
            print(f"{GREEN}Model set to {argument}.{RESET}")
            continue
        if line == "/config":
            print_config(settings, path)
            continue
        if line == "/tools":
            for name, (description, _params, _function) in TOOLS.items():
                print(f"  {name:<6} {description}")
            continue
        if line in ("/help", "/?"):
            print(f"{BOLD}Commands{RESET}")
            print("  /provider [name]  list or switch provider")
            print("  /model [name]     show or set model")
            print("  /config           show active provider")
            print("  /tools            list tools")
            print("  /c                clear conversation")
            print("  /q, exit          quit")
            continue
        if line.startswith("/"):
            print(f"{YELLOW}Unknown command. Try /help.{RESET}")
            continue

        try:
            run_turn(settings, messages, line)
        except KeyboardInterrupt:
            print(f"\n{YELLOW}Interrupted.{RESET}")
        except Exception as err:
            print(f"{RED}Error: {err}{RESET}")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="smolkoda",
        description="Tiny agentic coding harness with pluggable providers.",
    )
    parser.add_argument("prompt", nargs="*", help="one-shot prompt; omit for REPL")
    parser.add_argument("--setup", action="store_true", help="run provider setup")
    parser.add_argument(
        "--list-providers", action="store_true", help="list presets and profiles"
    )
    parser.add_argument("--provider", help="preset or saved profile name")
    parser.add_argument("--model", help="override the model")
    parser.add_argument("--base-url", help="override the API base URL")
    parser.add_argument(
        "--api-key", help="override the API key (prefer config or env vars)"
    )
    parser.add_argument(
        "--protocol", choices=("openai", "anthropic"), help="wire protocol"
    )
    parser.add_argument(
        "--auth", choices=("bearer", "api-key", "x-api-key", "none"),
        help="how to send the API key",
    )
    parser.add_argument("--max-tokens", type=int, help="override max output tokens")
    parser.add_argument("--temperature", type=float, help="override temperature")
    parser.add_argument("--test", action="store_true", help="test the provider and exit")
    parser.add_argument("--config", help="config file path")
    parser.add_argument(
        "--version", action="version", version=f"smolkoda {VERSION}"
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    path = config_path(args.config)
    cfg = load_config(path)

    if args.list_providers:
        print_providers(path, cfg)
        return 0

    if args.setup:
        if not sys.stdin.isatty():
            print("setup needs an interactive terminal; edit the config instead")
            return 1
        return 0 if setup_wizard(cfg, path, args.provider) else 1

    name = pick_provider(cfg, args.provider)
    try:
        settings = build_settings(name, cfg, args)
    except Exception as err:
        print(f"{RED}{err}{RESET}")
        return 1

    if settings.auth != "none" and not settings.api_key:
        print(f"{YELLOW}No API key for '{settings.name}'.{RESET}")
        if sys.stdin.isatty():
            if not setup_wizard(cfg, path, settings.name):
                return 1
            name = cfg.get("active") or settings.name
            try:
                settings = build_settings(name, cfg, args)
            except Exception as err:
                print(f"{RED}{err}{RESET}")
                return 1
        else:
            print("Set an API key env var or run: python3 smolkoda.py --setup")
            return 1

    if args.test:
        return 0 if test_connection(settings) else 1

    if args.prompt:
        messages: list[dict[str, Any]] = []
        try:
            run_turn(settings, messages, " ".join(args.prompt))
            return 0
        except KeyboardInterrupt:
            return 130
        except Exception as err:
            print(f"{RED}Error: {err}{RESET}")
            return 1

    repl(settings, cfg, path)
    return 0


if __name__ == "__main__":
    sys.exit(main())






