//! Cross-platform background text-to-speech owned by the running `bakedit` process.
//!
//! Because `bakedit` owns the audio playback child process, speech continues smoothly after
//! an agent's short-lived shell command exits, never overlaps across steps, and stops cleanly
//! on `--stop-speaking` or when `bakedit` exits.
//!
//! Prefers neural voices via `edge-tts` (or `uvx edge-tts`) played through the first available
//! system audio player (`afplay`, `ffplay`, `mpv`, `mpg123`), falling back to OS-native TTS
//! (`say`, `spd-say`, `espeak-ng`, `espeak`, `powershell.exe`) when offline.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::format::which;

const DEFAULT_VOICE: &str = "en-US-AndrewNeural";
const DEFAULT_RATE: &str = "+10%";

#[derive(Default)]
struct State {
    generation: u64,
    active: Option<Child>,
}

/// Manages at most one active spoken utterance at a time.
#[derive(Clone, Default)]
pub struct Speaker {
    state: Arc<Mutex<State>>,
}

impl Speaker {
    pub fn new() -> Speaker {
        Speaker::default()
    }

    /// Stop any in-flight speech and speak `text` in the background.
    pub fn speak(&self, text: String) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            self.stop();
            return;
        }
        let text = trimmed.to_string();
        let generation = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.generation = state.generation.wrapping_add(1);
            kill_child(&mut state.active);
            state.generation
        };
        let state = Arc::clone(&self.state);
        let _ = std::thread::Builder::new().name("bakedit-tts".into()).spawn(move || {
            run_utterance(&state, generation, &text);
        });
    }

    /// Stop any currently playing or synthesizing speech.
    pub fn stop(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        kill_child(&mut state.active);
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn kill_child(slot: &mut Option<Child>) {
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn is_current(state: &Mutex<State>, generation: u64) -> bool {
    state.lock().unwrap_or_else(|e| e.into_inner()).generation == generation
}

fn run_utterance(state: &Mutex<State>, generation: u64, text: &str) {
    let mp3 = std::env::temp_dir().join(format!("bakedit-tts-{}-{generation}.mp3", std::process::id()));
    let synthesized = synthesize_edge(text, &mp3);
    if !is_current(state, generation) {
        let _ = fs::remove_file(&mp3);
        return;
    }
    if synthesized {
        if let Some(child) = spawn_audio_player(&mp3) {
            track_and_wait(state, generation, child);
            let _ = fs::remove_file(&mp3);
            return;
        }
        let _ = fs::remove_file(&mp3);
    }
    if !is_current(state, generation) {
        return;
    }
    if let Some(child) = spawn_native_tts(text) {
        track_and_wait(state, generation, child);
    }
}

fn track_and_wait(state: &Mutex<State>, generation: u64, mut child: Child) {
    {
        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
        if guard.generation != generation {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        kill_child(&mut guard.active);
        guard.active = Some(child);
    }
    // Poll briefly so `stop()` can lock and kill the child immediately without waiting on `wait()`.
    loop {
        let done = {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.generation != generation {
                return;
            }
            match guard.active.as_mut() {
                Some(child) => match child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        guard.active = None;
                        true
                    }
                    Ok(None) => false,
                },
                None => true,
            }
        };
        if done {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn voice_and_rate() -> (String, String) {
    let voice = std::env::var("WALKTHROUGH_VOICE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VOICE.to_string());
    let rate = std::env::var("WALKTHROUGH_RATE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_RATE.to_string());
    (voice, rate)
}

fn synthesize_edge(text: &str, out_path: &Path) -> bool {
    let (voice, rate) = voice_and_rate();
    let rate_arg = format!("--rate={rate}");
    let out_str = out_path.to_string_lossy().into_owned();
    let mut cmd = if let Some(bin) = which("edge-tts") {
        Command::new(bin)
    } else if let Some(uvx) = which("uvx") {
        let mut c = Command::new(uvx);
        c.arg("edge-tts");
        c
    } else {
        return false;
    };
    let ok = cmd
        .args(["--voice", &voice, &rate_arg, "--text", text, "--write-media", &out_str])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    ok && fs::metadata(out_path).is_ok_and(|meta| meta.len() > 0)
}

fn spawn_audio_player(mp3: &Path) -> Option<Child> {
    let candidates: &[(&str, &[&str])] = &[
        ("afplay", &[]),
        ("ffplay", &["-nodisp", "-autoexit", "-loglevel", "quiet"]),
        ("mpv", &["--no-video", "--really-quiet"]),
        ("mpg123", &["-q"]),
    ];
    for &(name, args) in candidates {
        if let Some(bin) = which(name) {
            if let Ok(child) = Command::new(bin)
                .args(args)
                .arg(mp3)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                return Some(child);
            }
        }
    }
    None
}

fn spawn_native_tts(text: &str) -> Option<Child> {
    if let Some(bin) = which("say") {
        return Command::new(bin)
            .args(["-v", "Samantha", "-r", "200", "--", text])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }
    if let Some(bin) = which("spd-say") {
        return Command::new(bin)
            .args(["-w", "--", text])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }
    for name in ["espeak-ng", "espeak"] {
        if let Some(bin) = which(name) {
            return Command::new(bin)
                .args(["--", text])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok();
        }
    }
    if let Some(bin) = which("powershell.exe") {
        return Command::new(bin)
            .args([
                "-NoProfile",
                "-Command",
                "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak($args[0])",
                text,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }
    None
}

/// Speak `text` synchronously in-process when no `bakedit` window is running.
pub fn speak_blocking(text: &str) {
    let mp3: PathBuf = std::env::temp_dir().join(format!("bakedit-tts-cli-{}.mp3", std::process::id()));
    if synthesize_edge(text, &mp3) {
        if let Some(mut child) = spawn_audio_player(&mp3) {
            let _ = child.wait();
            let _ = fs::remove_file(&mp3);
            return;
        }
        let _ = fs::remove_file(&mp3);
    }
    if let Some(mut child) = spawn_native_tts(text) {
        let _ = child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_stop_advances_generation_cleanly() {
        let speaker = Speaker::new();
        speaker.stop();
        assert_eq!(speaker.state.lock().unwrap().generation, 1);
        speaker.speak("   ".into());
        assert_eq!(speaker.state.lock().unwrap().generation, 2);
    }
}
