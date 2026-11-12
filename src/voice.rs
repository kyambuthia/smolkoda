use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use std::io;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use ureq::Agent;

use crate::config::{self, Config};
use crate::looper::Looper;
use crate::ui;

const OPENROUTER_API: &str = "https://openrouter.ai/api/v1";
const TRANSCRIPTION_MODEL: &str = "openai/whisper-large-v3-turbo";
const SPEECH_MODEL: &str = "fish-audio/drama-3";
const MAX_SPEECH_CHARS: usize = 30_000;

pub const DEFAULT_RECORD_SECONDS: u64 = 8;

pub fn run(looper: &mut Looper, config: &Config, seconds: u64) -> Result<(), String> {
    let api_key = openrouter_api_key(looper, config)?;
    ui::info(&format!("Press Enter to record for {seconds} seconds."));
    let mut ready = String::new();
    if io::stdin()
        .read_line(&mut ready)
        .map_err(|err| format!("could not read recording prompt: {err}"))?
        == 0
    {
        ui::info("Voice recording cancelled.");
        return Ok(());
    }
    ui::info(&format!("Recording for {seconds} seconds. Speak now..."));
    let recording = record_audio(seconds)?;

    ui::info("Transcribing...");
    let timeout = looper.router.primary().timeout;
    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let transcript = transcribe(&agent, &api_key, &recording)?;
    if transcript.trim().is_empty() {
        ui::info("No speech was detected.");
        return Ok(());
    }
    ui::info(&format!("Heard: {}", transcript.trim()));

    let reply = looper.run_turn_with_output(transcript.trim())?;
    if reply.trim().is_empty() {
        return Ok(());
    }
    let speech_text: String = reply.chars().take(MAX_SPEECH_CHARS).collect();
    if speech_text.chars().count() < reply.chars().count() {
        ui::warn("Reply is long; speaking only its first 30,000 characters.");
    }

    ui::info("Generating speech with Fish Audio Drama...");
    let audio = synthesize(&agent, &api_key, &speech_text)?;
    play_audio(&audio)
}

fn openrouter_api_key(looper: &Looper, config: &Config) -> Result<String, String> {
    let current = looper.router.primary();
    if current.base_url.contains("openrouter.ai") && !current.api_key.trim().is_empty() {
        return Ok(current.api_key.clone());
    }

    if let Some(profile) = config.providers.get("openrouter") {
        if let Some(raw_key) = profile.api_key.as_deref() {
            let key = config::resolve_secret(raw_key);
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
    }

    if let Some(key) = config::env_nonempty("OPENROUTER_API_KEY") {
        return Ok(key);
    }

    Err("/voice needs an OpenRouter API key. Set OPENROUTER_API_KEY or configure an openrouter profile.".to_string())
}

fn record_audio(seconds: u64) -> Result<Vec<u8>, String> {
    let ffmpeg = std::env::var("SMOLKODA_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
    let input_override = std::env::var("SMOLKODA_AUDIO_INPUT").ok();
    let inputs = audio_inputs(input_override.as_deref());
    let mut errors = Vec::new();

    for input in inputs {
        let mut command = Command::new(&ffmpeg);
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin"])
            .args(input)
            .arg("-t")
            .arg(seconds.to_string())
            .args(["-ac", "1", "-ar", "16000", "-f", "wav", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = match command.output() {
            Ok(output) => output,
            Err(err) => {
                return Err(format!(
                    "could not start {ffmpeg}: {err}; install ffmpeg and make it available on PATH"
                ));
            }
        };
        if output.status.success() && output.stdout.len() > 44 {
            return Ok(output.stdout);
        }

        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        errors.push(if detail.is_empty() {
            format!("ffmpeg exited with {}", output.status)
        } else {
            detail
        });
    }

    Err(format!(
        "could not record from the microphone: {}. Install ffmpeg and check the default audio input; set SMOLKODA_AUDIO_INPUT to override it.",
        errors.join("; ")
    ))
}

fn audio_inputs(override_input: Option<&str>) -> Vec<Vec<String>> {
    if let Some(input) = override_input.filter(|input| !input.trim().is_empty()) {
        let (format, device) = input
            .split_once(':')
            .unwrap_or((default_input_format(), input));
        return vec![vec![
            "-f".to_string(),
            format.to_string(),
            "-i".to_string(),
            device.to_string(),
        ]];
    }

    #[cfg(target_os = "linux")]
    {
        vec![
            vec!["-f".into(), "pulse".into(), "-i".into(), "default".into()],
            vec!["-f".into(), "alsa".into(), "-i".into(), "default".into()],
        ]
    }
    #[cfg(target_os = "macos")]
    {
        vec![vec![
            "-f".into(),
            "avfoundation".into(),
            "-i".into(),
            ":0".into(),
        ]]
    }
    #[cfg(target_os = "windows")]
    {
        vec![vec![
            "-f".into(),
            "dshow".into(),
            "-i".into(),
            "audio=default".into(),
        ]]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        vec![vec![
            "-f".into(),
            default_input_format().into(),
            "-i".into(),
            "default".into(),
        ]]
    }
}

fn default_input_format() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "avfoundation"
    }
    #[cfg(target_os = "windows")]
    {
        "dshow"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "pulse"
    }
}

fn transcribe(agent: &Agent, api_key: &str, recording: &[u8]) -> Result<String, String> {
    let body = json!({
        "model": TRANSCRIPTION_MODEL,
        "input_audio": {
            "data": STANDARD.encode(recording),
            "format": "wav"
        }
    });
    let response = match agent
        .post(&format!("{OPENROUTER_API}/audio/transcriptions"))
        .set("authorization", &format!("Bearer {api_key}"))
        .set("content-type", "application/json")
        .send_json(body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            return Err(api_error("transcription", code, response));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(format!("transcription request failed: {err}"));
        }
    };

    let data: Value = response
        .into_json()
        .map_err(|err| format!("invalid transcription response: {err}"))?;
    data.get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("transcription response had no text: {}", data))
}

fn synthesize(agent: &Agent, api_key: &str, text: &str) -> Result<Vec<u8>, String> {
    let body = json!({
        "model": SPEECH_MODEL,
        "input": format!("[clear, natural conversational delivery] {text}"),
        "response_format": "mp3"
    });
    let response = match agent
        .post(&format!("{OPENROUTER_API}/audio/speech"))
        .set("authorization", &format!("Bearer {api_key}"))
        .set("content-type", "application/json")
        .send_json(body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            return Err(api_error("speech generation", code, response));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(format!("speech generation request failed: {err}"));
        }
    };

    let mut audio = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut audio)
        .map_err(|err| format!("could not read generated audio: {err}"))?;
    if audio.is_empty() {
        return Err("Fish Audio returned an empty audio response".to_string());
    }
    Ok(audio)
}

fn play_audio(audio: &[u8]) -> Result<(), String> {
    let ffplay = std::env::var("SMOLKODA_FFPLAY").unwrap_or_else(|_| "ffplay".to_string());
    let mut child = Command::new(&ffplay)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nodisp",
            "-autoexit",
            "-i",
            "pipe:0",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            format!("could not start {ffplay}: {err}; install ffplay and make it available on PATH")
        })?;

    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "could not open ffplay input".to_string())?
        .write_all(audio);
    let output = child
        .wait_with_output()
        .map_err(|err| format!("could not wait for ffplay: {err}"))?;
    write_result.map_err(|err| format!("could not send audio to ffplay: {err}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("ffplay exited with {}", output.status)
        } else {
            format!("could not play generated audio: {detail}")
        });
    }
    Ok(())
}

fn api_error(action: &str, code: u16, response: ureq::Response) -> String {
    let body = response.into_string().unwrap_or_default();
    let detail = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|data| {
            data.pointer("/error/message")
                .and_then(Value::as_str)
                .or_else(|| data.get("message").and_then(Value::as_str))
                .or_else(|| data.get("error").and_then(Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(500).collect());
    format!("{action} failed with HTTP {code}: {detail}")
}
