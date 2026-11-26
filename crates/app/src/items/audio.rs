//! In-terminal audio player tab with waveform display, seeking, and system playback.
//!
//! Uses `ffprobe` / `ffmpeg` off the UI thread for metadata and waveform peaks when available,
//! and plays audio via `ffplay` / `mpv` / `afplay` / `pw-play` / `paplay` / `aplay`.
//! Playback always stops cleanly when the tab is closed or dropped.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::background::{Task, Waker};
use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemStatus, RenderCx, View};

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "opus", "m4a", "aac", "aiff", "aif", "wma", "weba", "caf", "alac",
];

const WAVEFORM_BARS: usize = 64;

#[derive(Debug, Clone, Default)]
struct AudioMeta {
    duration: Option<f64>,
    sample_rate: Option<u32>,
    channels: Option<u32>,
    codec: Option<String>,
    bit_rate: Option<u64>,
    title: Option<String>,
    artist: Option<String>,
    peaks: Vec<f32>,
}

/// A tab that displays audio metadata/waveform and plays audio through the system player.
pub struct AudioPlayer {
    path: PathBuf,
    file_size: u64,
    meta: AudioMeta,
    meta_task: Option<Task<AudioMeta>>,
    child: Option<Child>,
    playing: bool,
    /// Saved playback offset in seconds when paused, or base offset when playing.
    position_secs: f64,
    /// When the current `child` started playing from `position_secs`.
    started_at: Option<Instant>,
    /// Signals the UI wake thread to stop when playback stops.
    wake_stop: Option<Arc<AtomicBool>>,
    /// Clickable hit rects from the last frame.
    play_btn: Option<Rect>,
    restart_btn: Option<Rect>,
    bar_rect: Option<Rect>,
    error: Option<String>,
}

impl AudioPlayer {
    /// True if `path` has a supported audio file extension.
    pub fn supports(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| AUDIO_EXTENSIONS.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
    }

    /// Open an audio file and kick off background metadata + waveform extraction.
    pub fn open(path: &Path, cx: &Cx<'_>) -> Result<AudioPlayer, String> {
        let meta_fs = std::fs::metadata(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
        let path_buf = path.to_path_buf();
        let probe_path = path_buf.clone();
        let meta_task = cx.project.spawn(move || probe_audio(&probe_path));
        Ok(AudioPlayer {
            path: path_buf,
            file_size: meta_fs.len(),
            meta: AudioMeta::default(),
            meta_task: Some(meta_task),
            child: None,
            playing: false,
            position_secs: 0.0,
            started_at: None,
            wake_stop: None,
            play_btn: None,
            restart_btn: None,
            bar_rect: None,
            error: None,
        })
    }

    fn current_position(&self) -> f64 {
        let mut pos = self.position_secs;
        if self.playing {
            if let Some(started) = self.started_at {
                pos += started.elapsed().as_secs_f64();
            }
        }
        if let Some(dur) = self.meta.duration {
            pos.clamp(0.0, dur)
        } else {
            pos.max(0.0)
        }
    }

    fn stop_process(&mut self) {
        if let Some(stop) = self.wake_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.playing = false;
        self.started_at = None;
    }

    fn start_playback(&mut self, waker: Waker) {
        self.stop_process();
        if let Some(dur) = self.meta.duration {
            if self.position_secs >= (dur - 0.1).max(0.0) {
                self.position_secs = 0.0;
            }
        }
        match spawn_player(&self.path, self.position_secs) {
            Ok(child) => {
                self.child = Some(child);
                self.playing = true;
                self.started_at = Some(Instant::now());
                self.error = None;
                let stop = Arc::new(AtomicBool::new(false));
                let stop_thread = stop.clone();
                let _ = std::thread::Builder::new().name("bakedit-audio-tick".into()).spawn(move || {
                    while !stop_thread.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(200));
                        if stop_thread.load(Ordering::Relaxed) {
                            break;
                        }
                        waker.wake();
                    }
                });
                self.wake_stop = Some(stop);
            }
            Err(msg) => {
                self.error = Some(msg);
            }
        }
    }

    fn toggle_play(&mut self, waker: Waker) {
        if self.playing {
            self.position_secs = self.current_position();
            self.stop_process();
        } else {
            self.start_playback(waker);
        }
    }

    fn seek_to(&mut self, target: f64, waker: Waker) {
        let clamped = match self.meta.duration {
            Some(dur) => target.clamp(0.0, dur),
            None => target.max(0.0),
        };
        let was_playing = self.playing;
        self.position_secs = clamped;
        if was_playing {
            self.start_playback(waker);
        }
    }

    fn seek_by(&mut self, delta: f64, waker: Waker) {
        let cur = self.current_position();
        self.seek_to(cur + delta, waker);
    }

    fn restart(&mut self, waker: Waker) {
        self.position_secs = 0.0;
        self.start_playback(waker);
    }

    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.stop_process();
    }
}

fn spawn_player(path: &Path, start_secs: f64) -> Result<Child, String> {
    let start_str = format!("{start_secs:.2}");
    if let Some(ffplay) = crate::format::which("ffplay") {
        let mut cmd = Command::new(ffplay);
        cmd.args(["-nodisp", "-autoexit", "-loglevel", "quiet"]);
        if start_secs > 0.05 {
            cmd.args(["-ss", &start_str]);
        }
        return cmd
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffplay failed: {e}"));
    }
    if let Some(mpv) = crate::format::which("mpv") {
        let mut cmd = Command::new(mpv);
        cmd.args(["--no-video", "--really-quiet"]);
        if start_secs > 0.05 {
            cmd.arg(format!("--start={start_str}"));
        }
        return cmd
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("mpv failed: {e}"));
    }
    for bin in ["afplay", "pw-play", "paplay", "aplay"] {
        if let Some(prog) = crate::format::which(bin) {
            return Command::new(prog)
                .arg(path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("{bin} failed: {e}"));
        }
    }
    Err("no audio player found (install ffplay or mpv)".into())
}

fn probe_audio(path: &Path) -> AudioMeta {
    let mut meta = AudioMeta::default();
    if let Some(ffprobe) = crate::format::which("ffprobe") {
        if let Ok(out) = Command::new(ffprobe)
            .args(["-v", "quiet", "-print_format", "json", "-show_format", "-show_streams"])
            .arg(path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                if let Ok(text) = std::str::from_utf8(&out.stdout) {
                    parse_ffprobe_json(text, &mut meta);
                }
            }
        }
    }
    if let Some(ffmpeg) = crate::format::which("ffmpeg") {
        if let Ok(out) = Command::new(ffmpeg)
            .args([
                "-v", "quiet", "-i",
            ])
            .arg(path)
            .args(["-ac", "1", "-ar", "4000", "-f", "s16le", "-"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() && out.stdout.len() >= 2 {
                meta.peaks = compute_peaks(&out.stdout, WAVEFORM_BARS);
            }
        }
    }
    meta
}

fn parse_ffprobe_json(text: &str, meta: &mut AudioMeta) {
    let Ok(doc) = serde_json::from_str::<Value>(text) else { return };
    if let Some(fmt) = doc.get("format") {
        meta.duration = fmt.get("duration").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
        meta.bit_rate = fmt.get("bit_rate").and_then(Value::as_str).and_then(|s| s.parse::<u64>().ok());
        if let Some(tags) = fmt.get("tags").and_then(Value::as_object) {
            meta.title = tags.get("title").or_else(|| tags.get("TITLE")).and_then(Value::as_str).map(str::to_string);
            meta.artist = tags.get("artist").or_else(|| tags.get("ARTIST")).and_then(Value::as_str).map(str::to_string);
        }
    }
    if let Some(streams) = doc.get("streams").and_then(Value::as_array) {
        if let Some(audio) = streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio")) {
            meta.codec = audio.get("codec_name").and_then(Value::as_str).map(|s| s.to_uppercase());
            meta.sample_rate = audio.get("sample_rate").and_then(Value::as_str).and_then(|s| s.parse::<u32>().ok());
            meta.channels = audio.get("channels").and_then(Value::as_u64).map(|c| c as u32);
            if meta.duration.is_none() {
                meta.duration = audio.get("duration").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
            }
        }
    }
}

fn compute_peaks(pcm_s16le: &[u8], bars: usize) -> Vec<f32> {
    let sample_count = pcm_s16le.len() / 2;
    if sample_count == 0 || bars == 0 {
        return Vec::new();
    }
    let chunk = (sample_count / bars).max(1);
    let mut peaks = Vec::with_capacity(bars);
    let mut global_max: f32 = 1.0;
    for i in 0..bars {
        let start = i * chunk;
        if start >= sample_count {
            break;
        }
        let end = ((i + 1) * chunk).min(sample_count);
        let mut max_amp: i32 = 0;
        for s in start..end {
            let lo = pcm_s16le[s * 2];
            let hi = pcm_s16le[s * 2 + 1];
            let sample = i16::from_le_bytes([lo, hi]) as i32;
            max_amp = max_amp.max(sample.abs());
        }
        let val = max_amp as f32;
        global_max = global_max.max(val);
        peaks.push(val);
    }
    for p in &mut peaks {
        *p = (*p / global_max).clamp(0.05, 1.0);
    }
    peaks
}

fn format_time(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

impl View for AudioPlayer {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bg = cx.theme.background;
        canvas.fill(area, bg);
        self.play_btn = None;
        self.restart_btn = None;
        self.bar_rect = None;

        if area.width < 16 || area.height < 6 {
            return None;
        }

        let card_w = area.width.saturating_sub(4).min(76);
        let card_h = 12u16.min(area.height.saturating_sub(2));
        let card_x = area.x + (area.width.saturating_sub(card_w)) / 2;
        let card_y = area.y + (area.height.saturating_sub(card_h)) / 2;
        let card = Rect::new(card_x, card_y, card_w, card_h);
        let card_bg = cx.theme.surface;
        canvas.fill(card, card_bg);
        canvas.box_border(card, Style::colors(cx.theme.border, card_bg));

        let inner_x = card.x + 2;
        let inner_w = card.width.saturating_sub(4);
        let mut y = card.y + 1;

        // Title line
        let heading = match (&self.meta.artist, &self.meta.title) {
            (Some(artist), Some(title)) => format!("♪  {title} — {artist}"),
            (_, Some(title)) => format!("♪  {title}"),
            _ => format!("♪  {}", self.title()),
        };
        canvas.text_truncated(inner_x, y, &heading, Style::colors(cx.theme.text, card_bg).bold(), inner_w);
        y += 1;

        // Metadata line
        let mut parts = Vec::new();
        if let Some(codec) = &self.meta.codec {
            parts.push(codec.clone());
        } else if let Some(ext) = self.path.extension().and_then(|e| e.to_str()) {
            parts.push(ext.to_uppercase());
        }
        if let Some(sr) = self.meta.sample_rate {
            parts.push(format!("{:.1} kHz", sr as f64 / 1000.0));
        }
        if let Some(ch) = self.meta.channels {
            parts.push(match ch {
                1 => "Mono".into(),
                2 => "Stereo".into(),
                n => format!("{n} ch"),
            });
        }
        if let Some(br) = self.meta.bit_rate {
            parts.push(format!("{} kbps", br / 1000));
        }
        parts.push(format_bytes(self.file_size));
        let meta_line = parts.join("  ·  ");
        canvas.text_truncated(inner_x, y, &meta_line, Style::colors(cx.theme.text_muted, card_bg), inner_w);
        y += 2;

        // Waveform / progress bar
        let pos = self.current_position();
        let ratio = match self.meta.duration {
            Some(dur) if dur > 0.0 => (pos / dur).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let bar_w = inner_w as usize;
        let played_cols = (ratio * bar_w as f64).round() as usize;
        let blocks = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
        if y < card.bottom().saturating_sub(4) {
            self.bar_rect = Some(Rect::new(inner_x, y, inner_w, 2));
            for col in 0..bar_w {
                let peak = if !self.meta.peaks.is_empty() {
                    let idx = (col * self.meta.peaks.len()) / bar_w.max(1);
                    self.meta.peaks[idx.min(self.meta.peaks.len() - 1)]
                } else {
                    0.35
                };
                let level = ((peak * (blocks.len() - 1) as f32).round() as usize).min(blocks.len() - 1);
                let color = if col < played_cols { cx.theme.text_accent } else { cx.theme.text_muted };
                canvas.put(inner_x + col as u16, y, blocks[level], Style::colors(color, card_bg));
            }
            y += 1;
            // Thin progress rail beneath waveform
            for col in 0..bar_w {
                let (sym, color) = if col < played_cols {
                    ("━", cx.theme.text_accent)
                } else {
                    ("─", cx.theme.border)
                };
                canvas.put(inner_x + col as u16, y, sym, Style::colors(color, card_bg));
            }
            y += 1;
        }

        // Timestamp line
        if y < card.bottom().saturating_sub(2) {
            let cur_str = format_time(pos);
            let dur_str = self.meta.duration.map(format_time).unwrap_or_else(|| "--:--".into());
            canvas.text(inner_x, y, &cur_str, Style::colors(cx.theme.text, card_bg), inner_w);
            canvas.text_right(inner_x + inner_w, y, &dur_str, Style::colors(cx.theme.text_muted, card_bg));
            y += 2;
        }

        // Controls row (clickable buttons with clean spacing)
        if y < card.bottom().saturating_sub(1) {
            let play_label = if self.playing { " ⏸ Pause " } else { " ▶ Play " };
            let play_style = if self.playing {
                Style::colors(cx.theme.background, cx.theme.text_accent).bold()
            } else {
                Style::colors(cx.theme.background, cx.theme.success).bold()
            };
            let pw = canvas.text(inner_x, y, play_label, play_style, inner_w);
            self.play_btn = Some(Rect::new(inner_x, y, pw, 1));

            let rx = inner_x + pw + 2;
            let restart_style = Style::colors(cx.theme.text, cx.theme.elevated_surface);
            let rw = canvas.text(rx, y, " ↺ Restart ", restart_style, inner_w.saturating_sub(pw + 2));
            self.restart_btn = Some(Rect::new(rx, y, rw, 1));

            let hints = "Space play  ←/→ 5s  o open";
            let hw = ui::width(hints) as u16;
            if rx + rw + 2 + hw <= inner_x + inner_w {
                canvas.text_right(inner_x + inner_w, y, hints, Style::colors(cx.theme.text_muted, card_bg));
            }
            y += 1;
        }

        if let Some(err) = &self.error {
            if y < card.bottom() {
                canvas.text_truncated(inner_x, y, err, Style::colors(cx.theme.error, card_bg), inner_w);
            }
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("AudioPlayer"));
    }

    fn handle_action(&mut self, action: &Action, cx: &mut Cx<'_>) -> bool {
        match action.name.as_str() {
            "pane::RevealInProjectPanel" => {
                cx.push(Effect::RevealInProjectPanel(self.path.clone()));
                true
            }
            "editor::CopyPath" => {
                cx.clipboard.set(&self.path.to_string_lossy(), false);
                cx.status("Copied path");
                true
            }
            "editor::RevealInFileManager" => {
                cx.push(Effect::OpenExternally(self.path.to_string_lossy().into_owned()));
                true
            }
            _ => false,
        }
    }

    fn handle_key(&mut self, key: &KeyEvent, cx: &mut Cx<'_>) -> bool {
        if !key.is_down() || key.mods.intersects(Mods::CTRL | Mods::CMD | Mods::ALT) {
            return false;
        }
        match key.key {
            Key::Char(' ') | Key::Enter => {
                self.toggle_play(cx.project.waker().clone());
                true
            }
            Key::Left | Key::Char(',') => {
                self.seek_by(-5.0, cx.project.waker().clone());
                true
            }
            Key::Right | Key::Char('.') => {
                self.seek_by(5.0, cx.project.waker().clone());
                true
            }
            Key::Char('0') | Key::Home | Key::Char('r') => {
                self.restart(cx.project.waker().clone());
                true
            }
            Key::Char('o') => {
                cx.push(Effect::OpenExternally(self.path.to_string_lossy().into_owned()));
                true
            }
            _ => false,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, cx: &mut Cx<'_>) -> bool {
        if event.kind != MouseKind::Press(MouseButton::Left) {
            return false;
        }
        let inside = |r: Rect| {
            event.col >= r.x && event.col < r.right() && event.row >= r.y && event.row < r.bottom()
        };
        if self.play_btn.is_some_and(inside) {
            self.toggle_play(cx.project.waker().clone());
            return true;
        }
        if self.restart_btn.is_some_and(inside) {
            self.restart(cx.project.waker().clone());
            return true;
        }
        if let Some(bar) = self.bar_rect.filter(|&r| inside(r)) {
            if let Some(dur) = self.meta.duration {
                let frac = (event.col.saturating_sub(bar.x) as f64) / (bar.width.max(1) as f64);
                self.seek_to(frac * dur, cx.project.waker().clone());
                return true;
            }
        }
        false
    }

    fn tick(&mut self, _cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        if let Some(task) = &mut self.meta_task {
            if let Some(meta) = task.poll() {
                self.meta = meta;
                self.meta_task = None;
                dirty = true;
            }
        }
        if self.playing {
            dirty = true;
            if let Some(child) = &mut self.child {
                if let Ok(Some(_)) = child.try_wait() {
                    self.position_secs = 0.0;
                    self.stop_process();
                }
            }
        }
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        if change.touches(&self.path) {
            if let Ok(meta_fs) = std::fs::metadata(&self.path) {
                self.file_size = meta_fs.len();
            }
            let probe_path = self.path.clone();
            self.meta_task = Some(cx.project.spawn(move || probe_audio(&probe_path)));
        }
    }
}

impl Item for AudioPlayer {
    fn kind(&self) -> &'static str {
        "audio"
    }

    fn status(&self) -> ItemStatus {
        let state = if self.playing { "Playing" } else { "Audio" };
        ItemStatus {
            language: Some(state.into()),
            ..ItemStatus::default()
        }
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let title = self.title();
        let icon = Some(file_icon(cx.settings, &title, false, false));
        Tab { title, dirty: false, preview: false, icon }
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn closed(&mut self, _cx: &mut Cx<'_>) {
        self.stop_process();
    }

    fn moved(&mut self, from: &Path, to: &Path, _cx: &mut Cx<'_>) {
        if let Ok(rel) = self.path.strip_prefix(from) {
            self.path = if rel.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rel) };
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::test_support::Harness;
    use ratatui::buffer::Buffer;

    #[test]
    fn recognizes_common_audio_extensions() {
        assert!(AudioPlayer::supports(Path::new("track.mp3")));
        assert!(AudioPlayer::supports(Path::new("VOICE.WAV")));
        assert!(AudioPlayer::supports(Path::new("song.flac")));
        assert!(AudioPlayer::supports(Path::new("clip.m4a")));
        assert!(!AudioPlayer::supports(Path::new("photo.png")));
        assert!(!AudioPlayer::supports(Path::new("main.rs")));
    }

    #[test]
    fn parses_ffprobe_metadata_and_formats_time() {
        let json = r#"{
            "format": {"duration": "125.4", "bit_rate": "320000", "tags": {"title": "Demo", "artist": "Synth"}},
            "streams": [{"codec_type": "audio", "codec_name": "mp3", "sample_rate": "44100", "channels": 2}]
        }"#;
        let mut meta = AudioMeta::default();
        parse_ffprobe_json(json, &mut meta);
        assert_eq!(meta.duration, Some(125.4));
        assert_eq!(meta.codec.as_deref(), Some("MP3"));
        assert_eq!(meta.sample_rate, Some(44100));
        assert_eq!(meta.channels, Some(2));
        assert_eq!(meta.title.as_deref(), Some("Demo"));
        assert_eq!(meta.artist.as_deref(), Some("Synth"));
        assert_eq!(format_time(125.4), "02:05");
        assert_eq!(format_time(3661.0), "1:01:01");
    }

    #[test]
    fn renders_audio_card_with_controls() {
        let mut player = AudioPlayer {
            path: PathBuf::from("/tmp/theme.mp3"),
            file_size: 2_500_000,
            meta: AudioMeta {
                duration: Some(180.0),
                sample_rate: Some(48000),
                channels: Some(2),
                codec: Some("MP3".into()),
                bit_rate: Some(192000),
                title: Some("Theme".into()),
                artist: None,
                peaks: vec![0.2, 0.8, 0.5, 1.0],
            },
            meta_task: None,
            child: None,
            playing: false,
            position_secs: 45.0,
            started_at: None,
            wake_stop: None,
            play_btn: None,
            restart_btn: None,
            bar_rect: None,
            error: None,
        };
        let harness = Harness::new("/tmp");
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        {
            let mut canvas = Canvas::new(&mut buf, area);
            player.render(&mut canvas, area, &harness.render_cx(true));
        }
        let text: String = (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Theme"), "{text}");
        assert!(text.contains("MP3"), "{text}");
        assert!(text.contains("00:45"), "{text}");
        assert!(text.contains("03:00"), "{text}");
        assert!(text.contains("Play"), "{text}");
        assert!(player.play_btn.is_some());
        assert!(player.bar_rect.is_some());
    }
}
