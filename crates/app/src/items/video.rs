//! In-terminal video player tab with Kitty Graphics Protocol (HQ native-DPI frames in Kitty /
//! Ghostty / WezTerm) and automatic true-color half-block (`▀`) fallback for other terminals.
//!
//! `ffprobe` reads the metadata off the UI thread. A decoder thread runs `ffmpeg`, which scales
//! and pads every frame to the exact pixel canvas of the pane, paces the frames by the wall
//! clock (dropping late ones), and PNG-encodes them for Kitty, so drawing never waits on video.
//! Sound plays through the audio player's system player (`ffplay` / `mpv` / `afplay` ...).
//!
//! Controls: Space/Enter play-pause, `←`/`→` 5 s, `↑`/`↓` 30 s, `0`/Home restart, `m` mute,
//! `o` open externally; click the picture to play or pause and the progress rail to seek.
//! Playback, decoding and sound always stop when the tab is closed or dropped.

use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb};
use image::RgbaImage;
use ratatui::layout::Rect;
use serde_json::Value;

use crate::background::{Task, Waker};
use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::items::audio::spawn_player;
use crate::items::image::{cell_px_key, encode_fast_png, kitty_graphics_supported, KittyImageKey, KittyImagePlacement};
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemStatus, RenderCx, View};

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "mkv", "webm", "avi", "mpeg", "mpg", "wmv", "flv", "ogv"];

/// Highest frame rate the decoder delivers; terminals cannot show more without stalling.
const MAX_FPS: f64 = 24.0;
/// Largest canvas side (pixels) decoded for Kitty; bigger panes are scaled up by the terminal.
const MAX_CANVAS_SIDE: u32 = 1920;
/// Rows the transport (progress rail and buttons) takes below the picture.
const CONTROLS_ROWS: u16 = 2;

/// What `ffprobe` says about the file.
#[derive(Debug, Clone, Default, PartialEq)]
struct VideoMeta {
    duration: Option<f64>,
    width: u32,
    height: u32,
    fps: Option<f64>,
    codec: Option<String>,
    has_audio: bool,
    /// Set when probing failed, so the tab can say why instead of loading forever.
    error: Option<String>,
}

/// Exact output geometry of the decoder: the scaled picture centred on a padded canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Geometry {
    /// Cells the picture is placed in (Kitty) or drawn into (half-blocks).
    rect: Rect,
    target_w: u32,
    target_h: u32,
    canvas_w: u32,
    canvas_h: u32,
    /// True for Kitty (encode PNG), false for half-blocks (keep only RGBA).
    kitty: bool,
}

/// One decoded frame, ready to draw.
struct Frame {
    seq: u64,
    geometry: Geometry,
    rgba: RgbaImage,
    png: Option<Vec<u8>>,
}

/// What the decoder thread shares with the tab.
#[derive(Default)]
struct Shared {
    frame: Option<Arc<Frame>>,
    /// The decoder reached the end of the stream.
    finished: bool,
    error: Option<String>,
}

/// A running `ffmpeg` decode, killed when dropped.
struct Decoder {
    child: Child,
    stop: Arc<AtomicBool>,
    geometry: Geometry,
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A tab that plays a video file.
pub struct VideoPlayer {
    path: PathBuf,
    file_size: u64,
    meta: Option<VideoMeta>,
    meta_task: Option<Task<VideoMeta>>,
    waker: Waker,
    use_kitty_graphics: bool,
    shared: Arc<Mutex<Shared>>,
    decoder: Option<Decoder>,
    /// Bumped on every decoder start so frames of an old decode are never mistaken for new ones.
    generation: u64,
    audio: Option<Child>,
    playing: bool,
    muted: bool,
    /// Offset in seconds when paused, or the offset playback started from when playing.
    position_secs: f64,
    started_at: Option<Instant>,
    /// Picture area from the last frame drawn.
    last_viewport: Rect,
    /// Cell size in pixels from the last Kitty placement.
    cell_px: (f32, f32),
    /// Sequence number of the frame shown last, to know when to redraw.
    shown_seq: u64,
    play_btn: Option<Rect>,
    restart_btn: Option<Rect>,
    mute_btn: Option<Rect>,
    rail: Option<Rect>,
    error: Option<String>,
}

impl VideoPlayer {
    /// True if `path` has a supported video file extension (case-insensitive).
    pub fn supports(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| VIDEO_EXTENSIONS.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
    }

    /// Open a video file, paused at the start, and probe its metadata in the background.
    /// Fails only when the file cannot be read; a file `ffprobe` cannot parse opens with an error.
    pub fn open(path: &Path, cx: &Cx<'_>) -> Result<VideoPlayer, String> {
        let fs_meta = std::fs::metadata(path).map_err(|e| format!("could not open {}: {e}", path.display()))?;
        let probe_path = path.to_path_buf();
        let meta_task = cx.project.spawn(move || probe_video(&probe_path));
        Ok(VideoPlayer::new(path.to_path_buf(), fs_meta.len(), Some(meta_task), cx.project.waker().clone(), kitty_graphics_supported()))
    }

    fn new(path: PathBuf, file_size: u64, meta_task: Option<Task<VideoMeta>>, waker: Waker, use_kitty_graphics: bool) -> VideoPlayer {
        VideoPlayer {
            path,
            file_size,
            meta: None,
            meta_task,
            waker,
            use_kitty_graphics,
            shared: Arc::new(Mutex::new(Shared::default())),
            decoder: None,
            generation: 0,
            audio: None,
            playing: false,
            muted: false,
            position_secs: 0.0,
            started_at: None,
            last_viewport: Rect::default(),
            cell_px: (10.0, 20.0),
            shown_seq: 0,
            play_btn: None,
            restart_btn: None,
            mute_btn: None,
            rail: None,
            error: None,
        }
    }

    fn duration(&self) -> Option<f64> {
        self.meta.as_ref().and_then(|m| m.duration)
    }

    /// Playback position in seconds, clamped to the duration when it is known.
    fn current_position(&self) -> f64 {
        let mut pos = self.position_secs;
        if self.playing {
            if let Some(started) = self.started_at {
                pos += started.elapsed().as_secs_f64();
            }
        }
        match self.duration() {
            Some(d) => pos.clamp(0.0, d),
            None => pos.max(0.0),
        }
    }

    /// The decoder geometry the current pane wants, or `None` before metadata or a viewport exist.
    fn wanted_geometry(&self) -> Option<Geometry> {
        let meta = self.meta.as_ref()?;
        if meta.width == 0 || meta.height == 0 {
            return None;
        }
        let cell = if self.use_kitty_graphics { Some(self.cell_px) } else { None };
        fit_geometry(meta.width, meta.height, self.last_viewport, cell)
    }

    /// Start a decode from `position_secs` at `geometry`: one frame when paused, the stream when playing.
    fn start_decoder(&mut self, geometry: Geometry) {
        self.decoder = None;
        self.generation += 1;
        if let Ok(mut s) = self.shared.lock() {
            s.finished = false;
            s.error = None;
        }
        let fps = self.meta.as_ref().and_then(|m| m.fps).unwrap_or(MAX_FPS).clamp(1.0, MAX_FPS);
        let single = !self.playing;
        match spawn_decoder(&self.path, self.position_secs, fps, geometry, single) {
            Ok((child, stdout)) => {
                let stop = Arc::new(AtomicBool::new(false));
                let job = DecodeJob {
                    stdout,
                    geometry,
                    fps,
                    single,
                    generation: self.generation,
                    stop: stop.clone(),
                    shared: self.shared.clone(),
                    waker: self.waker.clone(),
                };
                let _ = std::thread::Builder::new().name("bakedit-video-decode".into()).spawn(move || job.run());
                self.decoder = Some(Decoder { child, stop, geometry });
            }
            Err(msg) => self.error = Some(msg),
        }
    }

    /// Restart decoding when the pane size changed, or when nothing has been decoded yet.
    fn sync_decoder(&mut self) {
        let Some(geometry) = self.wanted_geometry() else { return };
        if self.decoder.as_ref().is_some_and(|d| d.geometry == geometry) {
            return;
        }
        if self.playing {
            self.position_secs = self.current_position();
            self.started_at = Some(Instant::now());
            self.restart_audio();
        }
        self.start_decoder(geometry);
    }

    fn stop_audio(&mut self) {
        if let Some(mut child) = self.audio.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// (Re)start the sound from `position_secs`, unless muted or the file has no audio track.
    fn restart_audio(&mut self) {
        self.stop_audio();
        if self.muted || !self.meta.as_ref().is_some_and(|m| m.has_audio) {
            return;
        }
        match spawn_player(&self.path, self.position_secs) {
            Ok(child) => self.audio = Some(child),
            Err(msg) => self.error = Some(msg),
        }
    }

    fn play(&mut self) {
        if self.meta.is_none() {
            return;
        }
        if let Some(d) = self.duration() {
            if self.position_secs >= (d - 0.1).max(0.0) {
                self.position_secs = 0.0;
            }
        }
        self.playing = true;
        self.started_at = Some(Instant::now());
        self.error = None;
        self.restart_audio();
        if let Some(geometry) = self.wanted_geometry() {
            self.start_decoder(geometry);
        }
    }

    fn pause(&mut self) {
        self.position_secs = self.current_position();
        self.playing = false;
        self.started_at = None;
        self.stop_audio();
        // Keep the frame on screen; a fresh single-frame decode is not needed.
        self.decoder = None;
    }

    fn toggle_play(&mut self) {
        if self.playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Jump to `target` seconds (clamped), keeping play or pause.
    fn seek_to(&mut self, target: f64) {
        let target = match self.duration() {
            Some(d) => target.clamp(0.0, d),
            None => target.max(0.0),
        };
        self.position_secs = target;
        if self.playing {
            self.started_at = Some(Instant::now());
            self.restart_audio();
        }
        if let Some(geometry) = self.wanted_geometry() {
            self.start_decoder(geometry);
        }
    }

    fn seek_by(&mut self, delta: f64) {
        let target = self.current_position() + delta;
        self.seek_to(target);
    }

    fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if self.playing {
            self.position_secs = self.current_position();
            self.started_at = Some(Instant::now());
            self.restart_audio();
            // Keep the picture in step with the restarted clock.
            if let Some(geometry) = self.wanted_geometry() {
                self.start_decoder(geometry);
            }
        }
    }

    fn stop_all(&mut self) {
        self.decoder = None;
        self.stop_audio();
        self.playing = false;
        self.started_at = None;
    }

    fn latest_frame(&self) -> Option<Arc<Frame>> {
        self.shared.lock().ok().and_then(|s| s.frame.clone())
    }

    /// Kitty Graphics placement of the latest frame, or `None` when not in Kitty mode or no
    /// frame fits the current pane yet. Also restarts decoding when the cell size changed.
    pub fn kitty_placement(&mut self, cell_px: (f32, f32)) -> Option<KittyImagePlacement> {
        if !self.use_kitty_graphics {
            return None;
        }
        if cell_px_key(cell_px) != cell_px_key(self.cell_px) {
            self.cell_px = cell_px;
            self.sync_decoder();
        }
        let frame = self.latest_frame()?;
        let png = frame.png.clone()?;
        let rect = frame.geometry.rect;
        if rect.width == 0 || rect.height == 0 || !rect_inside(rect, self.last_viewport) {
            return None;
        }
        let mut h = DefaultHasher::new();
        frame.geometry.hash(&mut h);
        let key = KittyImageKey { path: self.path.clone(), revision: frame.seq, rect, state_hash: h.finish(), cell_px: cell_px_key(cell_px) };
        Some(KittyImagePlacement { key, rect, png })
    }

    fn title(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.path.display().to_string())
    }

    fn header_text(&self) -> String {
        let mut parts = vec![self.title()];
        if let Some(m) = &self.meta {
            if m.width > 0 {
                parts.push(format!("{} × {}", m.width, m.height));
            }
            if let Some(fps) = m.fps {
                parts.push(format!("{} fps", trim_float(fps)));
            }
            if let Some(codec) = &m.codec {
                parts.push(codec.clone());
            }
        }
        parts.push(format_bytes(self.file_size));
        if self.use_kitty_graphics {
            parts.push("HQ".into());
        }
        format!(" {}", parts.join("  ·  "))
    }

    /// Draw the progress rail and the buttons into the two rows of `area`.
    fn render_controls(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) {
        let bg = cx.theme.surface;
        canvas.fill(area, bg);
        let inner_x = area.x + 1;
        let inner_w = area.width.saturating_sub(2);
        let pos = self.current_position();
        let ratio = match self.duration() {
            Some(d) if d > 0.0 => (pos / d).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let played = (ratio * inner_w as f64).round() as u16;
        for col in 0..inner_w {
            let (sym, color) = if col < played { ("━", cx.theme.text_accent) } else { ("─", cx.theme.border) };
            canvas.put(inner_x + col, area.y, sym, Style::colors(color, bg));
        }
        if inner_w > 0 {
            let knob = inner_x + played.min(inner_w - 1);
            canvas.put(knob, area.y, "●", Style::colors(cx.theme.text_accent, bg));
        }
        self.rail = Some(Rect::new(inner_x, area.y, inner_w, 1));

        let y = area.y + 1;
        let mut x = inner_x;
        let right = inner_x + inner_w;
        let play_label = if self.playing { " ⏸ Pause " } else { " ▶ Play " };
        let play_style = if self.playing {
            Style::colors(cx.theme.background, cx.theme.text_accent).bold()
        } else {
            Style::colors(cx.theme.background, cx.theme.success).bold()
        };
        let w = canvas.text(x, y, play_label, play_style, right.saturating_sub(x));
        self.play_btn = Some(Rect::new(x, y, w, 1));
        x += w + 1;
        let button = Style::colors(cx.theme.text, cx.theme.elevated_surface);
        let w = canvas.text(x, y, " ↺ Restart ", button, right.saturating_sub(x));
        self.restart_btn = Some(Rect::new(x, y, w, 1));
        x += w + 1;
        let mute_label = if self.muted { " 🔇 Muted " } else { " 🔊 Sound " };
        let w = canvas.text(x, y, mute_label, button, right.saturating_sub(x));
        self.mute_btn = Some(Rect::new(x, y, w, 1));
        x += w + 2;
        let time = format!("{} / {}", format_time(pos), self.duration().map(format_time).unwrap_or_else(|| "--:--".into()));
        let w = canvas.text(x, y, &time, Style::colors(cx.theme.text, bg), right.saturating_sub(x));
        x += w + 2;
        let hints = "Space play  ←/→ 5s  ↑/↓ 30s  m mute  o open";
        let hw = ui::width(hints) as u16;
        if x + hw <= right {
            canvas.text_right(right, y, hints, Style::colors(cx.theme.text_muted, bg));
        }
    }

    /// Draw `frame` with half-blocks: each cell shows two stacked pixels.
    fn render_halfblocks(canvas: &mut Canvas<'_>, frame: &Frame) {
        let g = frame.geometry;
        let (fw, fh) = (frame.rgba.width(), frame.rgba.height());
        for row in 0..g.rect.height {
            for col in 0..g.rect.width {
                let (px, top_y, bot_y) = (col as u32, row as u32 * 2, row as u32 * 2 + 1);
                if px >= fw || bot_y >= fh {
                    continue;
                }
                let top = frame.rgba.get_pixel(px, top_y).0;
                let bot = frame.rgba.get_pixel(px, bot_y).0;
                canvas.put(g.rect.x + col, g.rect.y + row, "▀", Style::colors(Rgb::new(top[0], top[1], top[2]), Rgb::new(bot[0], bot[1], bot[2])));
            }
        }
    }
}

impl Drop for VideoPlayer {
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// True when `inner` lies entirely within `outer`.
fn rect_inside(inner: Rect, outer: Rect) -> bool {
    inner.x >= outer.x && inner.y >= outer.y && inner.right() <= outer.right() && inner.bottom() <= outer.bottom()
}

/// Where and how big to decode a `src_w`×`src_h` video for `viewport`, keeping its aspect ratio.
///
/// With `cell_px` (Kitty), the picture is fitted to the viewport's pixel size, placed on the
/// smallest cell rectangle that holds it, and the canvas is that rectangle in pixels (capped
/// at [`MAX_CANVAS_SIDE`], the terminal scaling it up). Without it (half-blocks), each cell is
/// one pixel wide and two tall. `None` when the viewport or the video has no area.
fn fit_geometry(src_w: u32, src_h: u32, viewport: Rect, cell_px: Option<(f32, f32)>) -> Option<Geometry> {
    if src_w == 0 || src_h == 0 || viewport.width == 0 || viewport.height == 0 {
        return None;
    }
    let (cw, ch) = cell_px.map(|(w, h)| (w.max(4.0), h.max(4.0))).unwrap_or((1.0, 2.0));
    let vp_w = viewport.width as f32 * cw;
    let vp_h = viewport.height as f32 * ch;
    let scale = (vp_w / src_w as f32).min(vp_h / src_h as f32);
    let disp_w = (src_w as f32 * scale).max(1.0);
    let disp_h = (src_h as f32 * scale).max(1.0);
    let cols = (disp_w / cw).ceil().clamp(1.0, viewport.width as f32) as u16;
    let rows = (disp_h / ch).ceil().clamp(1.0, viewport.height as f32) as u16;
    let rect = Rect::new(viewport.x + (viewport.width - cols) / 2, viewport.y + (viewport.height - rows) / 2, cols, rows);
    let mut canvas_w = ((cols as f32 * cw).round() as u32).max(1);
    let mut canvas_h = ((rows as f32 * ch).round() as u32).max(1);
    let mut target_w = (disp_w.round() as u32).clamp(1, canvas_w);
    let mut target_h = (disp_h.round() as u32).clamp(1, canvas_h);
    let longest = canvas_w.max(canvas_h);
    if cell_px.is_some() && longest > MAX_CANVAS_SIDE {
        let k = MAX_CANVAS_SIDE as f32 / longest as f32;
        let shrink = |v: u32| ((v as f32 * k).round() as u32).max(1);
        canvas_w = shrink(canvas_w);
        canvas_h = shrink(canvas_h);
        target_w = shrink(target_w).min(canvas_w);
        target_h = shrink(target_h).min(canvas_h);
    }
    Some(Geometry { rect, target_w, target_h, canvas_w, canvas_h, kitty: cell_px.is_some() })
}

/// The `ffmpeg` arguments that decode `path` from `start_secs` as raw RGBA frames of exactly
/// `canvas_w`×`canvas_h`, at `fps`, without sound; one frame only when `single`.
fn decoder_args(path: &Path, start_secs: f64, fps: f64, g: Geometry, single: bool) -> Vec<String> {
    let mut args: Vec<String> = vec!["-v".into(), "error".into(), "-nostdin".into()];
    if start_secs > 0.01 {
        args.extend(["-ss".into(), format!("{start_secs:.3}")]);
    }
    args.extend(["-i".into(), path.to_string_lossy().into_owned(), "-an".into(), "-sn".into()]);
    let pad_x = (g.canvas_w - g.target_w) / 2;
    let pad_y = (g.canvas_h - g.target_h) / 2;
    let filter = format!(
        "fps={},scale={}:{}:flags=bicubic,format=rgba,pad={}:{}:{}:{}:color=black@0",
        trim_float(fps),
        g.target_w,
        g.target_h,
        g.canvas_w,
        g.canvas_h,
        pad_x,
        pad_y
    );
    args.extend(["-vf".into(), filter]);
    if single {
        args.extend(["-frames:v".into(), "1".into()]);
    }
    args.extend(["-f".into(), "rawvideo".into(), "-pix_fmt".into(), "rgba".into(), "-".into()]);
    args
}

fn spawn_decoder(path: &Path, start_secs: f64, fps: f64, g: Geometry, single: bool) -> Result<(Child, ChildStdout), String> {
    let ffmpeg = crate::format::which("ffmpeg").ok_or("ffmpeg not found (install ffmpeg to play video)")?;
    let mut child = Command::new(ffmpeg)
        .args(decoder_args(path, start_secs, fps, g, single))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg failed: {e}"))?;
    let stdout = child.stdout.take().ok_or("ffmpeg gave no output")?;
    Ok((child, stdout))
}

/// The work of one decoder thread.
struct DecodeJob {
    stdout: ChildStdout,
    geometry: Geometry,
    fps: f64,
    single: bool,
    generation: u64,
    stop: Arc<AtomicBool>,
    shared: Arc<Mutex<Shared>>,
    waker: Waker,
}

impl DecodeJob {
    /// Read frames until the stream ends or the decoder is stopped, publishing each one when
    /// it is due by the wall clock. Frames more than one frame late are dropped, not drawn.
    fn run(mut self) {
        let g = self.geometry;
        let size = g.canvas_w as usize * g.canvas_h as usize * 4;
        let interval = Duration::from_secs_f64(1.0 / self.fps);
        let mut buf = vec![0u8; size];
        let start = Instant::now();
        let mut index: u32 = 0;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            if self.stdout.read_exact(&mut buf).is_err() {
                break;
            }
            let due = start + interval * index;
            index += 1;
            let now = Instant::now();
            if !self.single {
                if now > due + interval {
                    continue;
                }
                if due > now {
                    std::thread::sleep(due - now);
                }
            }
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            let Some(rgba) = RgbaImage::from_raw(g.canvas_w, g.canvas_h, buf.clone()) else { break };
            let png = if g.kitty { encode_fast_png(&rgba) } else { None };
            let seq = (self.generation << 32) | index as u64;
            if let Ok(mut s) = self.shared.lock() {
                s.frame = Some(Arc::new(Frame { seq, geometry: g, rgba, png }));
            }
            self.waker.wake();
        }
        if !self.stop.load(Ordering::Relaxed) {
            if let Ok(mut s) = self.shared.lock() {
                s.finished = true;
                if index == 0 && !self.single {
                    s.error = Some("ffmpeg could not decode this video".into());
                }
            }
            self.waker.wake();
        }
    }
}

fn probe_video(path: &Path) -> VideoMeta {
    let Some(ffprobe) = crate::format::which("ffprobe") else {
        return VideoMeta { error: Some("ffprobe not found (install ffmpeg to play video)".into()), ..VideoMeta::default() };
    };
    let out = Command::new(ffprobe)
        .args(["-v", "quiet", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(out) if out.status.success() => parse_ffprobe_json(&String::from_utf8_lossy(&out.stdout)),
        _ => VideoMeta { error: Some("ffprobe could not read this file".into()), ..VideoMeta::default() },
    }
}

/// Parse `ffprobe -print_format json -show_format -show_streams` output. A file without a
/// video stream comes back with an error set.
fn parse_ffprobe_json(text: &str) -> VideoMeta {
    let mut meta = VideoMeta::default();
    let Ok(doc) = serde_json::from_str::<Value>(text) else {
        meta.error = Some("ffprobe gave unreadable output".into());
        return meta;
    };
    meta.duration = doc.pointer("/format/duration").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
    let streams = doc.get("streams").and_then(Value::as_array).cloned().unwrap_or_default();
    meta.has_audio = streams.iter().any(|s| s.get("codec_type").and_then(Value::as_str) == Some("audio"));
    let Some(video) = streams.iter().find(|s| s.get("codec_type").and_then(Value::as_str) == Some("video")) else {
        meta.error = Some("no video stream in this file".into());
        return meta;
    };
    meta.width = video.get("width").and_then(Value::as_u64).unwrap_or(0) as u32;
    meta.height = video.get("height").and_then(Value::as_u64).unwrap_or(0) as u32;
    meta.codec = video.get("codec_name").and_then(Value::as_str).map(|s| s.to_uppercase());
    meta.fps = video.get("avg_frame_rate").and_then(Value::as_str).and_then(parse_rate).or_else(|| {
        video.get("r_frame_rate").and_then(Value::as_str).and_then(parse_rate)
    });
    if meta.duration.is_none() {
        meta.duration = video.get("duration").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
    }
    meta
}

/// Parse an ffprobe rate such as `30000/1001` or `25`; `None` for `0/0` or garbage.
fn parse_rate(s: &str) -> Option<f64> {
    let value = match s.split_once('/') {
        Some((n, d)) => {
            let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
            if d == 0.0 {
                return None;
            }
            n / d
        }
        None => s.parse::<f64>().ok()?,
    };
    (value.is_finite() && value > 0.0).then_some(value)
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn format_time(secs: f64) -> String {
    let total = secs.max(0.0).floor() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
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

impl View for VideoPlayer {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bg = cx.theme.background;
        canvas.fill(area, bg);
        self.play_btn = None;
        self.restart_btn = None;
        self.mute_btn = None;
        self.rail = None;
        if area.width < 20 || area.height < CONTROLS_ROWS + 3 {
            return None;
        }

        let header = Rect::new(area.x, area.y, area.width, 1);
        canvas.fill(header, cx.theme.surface);
        canvas.text_truncated(header.x, header.y, &self.header_text(), Style::colors(cx.theme.text, cx.theme.surface), header.width.saturating_sub(1));

        let controls = Rect::new(area.x, area.bottom() - CONTROLS_ROWS, area.width, CONTROLS_ROWS);
        self.render_controls(canvas, controls, cx);

        let viewport = Rect::new(area.x, area.y + 1, area.width, area.height - 1 - CONTROLS_ROWS);
        let resized = viewport != self.last_viewport;
        self.last_viewport = viewport;
        if resized {
            self.sync_decoder();
        }

        let message = match (&self.meta, &self.error) {
            (_, Some(err)) => Some((err.clone(), cx.theme.error)),
            (None, _) => Some(("Reading video…".to_string(), cx.theme.text_muted)),
            (Some(m), _) => m.error.clone().map(|e| (e, cx.theme.error)),
        };
        if let Some((text, color)) = message {
            let w = (ui::width(&text) as u16).min(viewport.width);
            let x = viewport.x + (viewport.width - w) / 2;
            canvas.text_truncated(x, viewport.y + viewport.height / 2, &text, Style::colors(color, bg), w);
            return None;
        }

        // In Kitty mode the terminal draws the frame over this background in `App::draw`.
        if !self.use_kitty_graphics {
            if let Some(frame) = self.latest_frame() {
                if rect_inside(frame.geometry.rect, viewport) && !frame.geometry.kitty {
                    Self::render_halfblocks(canvas, &frame);
                }
            }
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("VideoPlayer"));
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
            Key::Char(' ') | Key::Enter => self.toggle_play(),
            Key::Left | Key::Char(',') => self.seek_by(-5.0),
            Key::Right | Key::Char('.') => self.seek_by(5.0),
            Key::Up => self.seek_by(30.0),
            Key::Down => self.seek_by(-30.0),
            Key::Char('0') | Key::Home | Key::Char('r') => self.seek_to(0.0),
            Key::Char('m') => self.toggle_mute(),
            Key::Char('o') => cx.push(Effect::OpenExternally(self.path.to_string_lossy().into_owned())),
            _ => return false,
        }
        true
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, _cx: &mut Cx<'_>) -> bool {
        if event.kind != MouseKind::Press(MouseButton::Left) {
            return false;
        }
        let inside = |r: Rect| event.col >= r.x && event.col < r.right() && event.row >= r.y && event.row < r.bottom();
        if self.play_btn.is_some_and(inside) || inside(self.last_viewport) {
            self.toggle_play();
            return true;
        }
        if self.restart_btn.is_some_and(inside) {
            self.seek_to(0.0);
            return true;
        }
        if self.mute_btn.is_some_and(inside) {
            self.toggle_mute();
            return true;
        }
        if let Some(rail) = self.rail.filter(|&r| inside(r)) {
            if let Some(d) = self.duration() {
                let frac = event.col.saturating_sub(rail.x) as f64 / rail.width.max(1) as f64;
                self.seek_to(frac * d);
                return true;
            }
        }
        false
    }

    fn tick(&mut self, _cx: &mut Cx<'_>) -> bool {
        let mut dirty = false;
        if let Some(task) = &mut self.meta_task {
            if let Some(meta) = task.poll() {
                self.meta = Some(meta);
                self.meta_task = None;
                self.sync_decoder();
                dirty = true;
            }
        }
        let (seq, finished, error) = match self.shared.lock() {
            Ok(s) => (s.frame.as_ref().map(|f| f.seq).unwrap_or(0), s.finished, s.error.clone()),
            Err(_) => (0, false, None),
        };
        if seq != self.shown_seq {
            self.shown_seq = seq;
            dirty = true;
        }
        if error.is_some() && self.error.is_none() {
            self.error = error;
            dirty = true;
        }
        if self.playing {
            // The clock moves even between frames, so keep the time display current.
            dirty = true;
            let at_end = self.duration().is_some_and(|d| self.current_position() >= d - 0.05);
            if finished || at_end {
                self.position_secs = self.duration().unwrap_or_else(|| self.current_position());
                self.stop_all();
            } else {
                self.waker.wake_after(Duration::from_millis(250));
            }
        }
        dirty
    }

    fn paths_changed(&mut self, change: &FsChange, cx: &mut Cx<'_>) {
        if change.touches(&self.path) {
            self.stop_all();
            if let Ok(fs_meta) = std::fs::metadata(&self.path) {
                self.file_size = fs_meta.len();
            }
            let probe_path = self.path.clone();
            self.meta_task = Some(cx.project.spawn(move || probe_video(&probe_path)));
        }
    }
}

impl Item for VideoPlayer {
    fn kind(&self) -> &'static str {
        "video"
    }

    fn status(&self) -> ItemStatus {
        let state = if self.playing { "Playing" } else { "Video" };
        ItemStatus { language: Some(state.into()), ..ItemStatus::default() }
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let title = self.title();
        let icon = Some(file_icon(cx.settings, &title, false, false));
        Tab { title, dirty: false, preview: false, pinned: false, icon }
    }

    fn path(&self) -> Option<&Path> {
        Some(&self.path)
    }

    fn closed(&mut self, _cx: &mut Cx<'_>) {
        self.stop_all();
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

    fn meta(w: u32, h: u32) -> VideoMeta {
        VideoMeta { duration: Some(204.0), width: w, height: h, fps: Some(30.0), codec: Some("H264".into()), has_audio: true, error: None }
    }

    fn render_text(player: &mut VideoPlayer, area: Rect) -> String {
        let harness = Harness::new("/tmp");
        let mut buf = Buffer::empty(area);
        {
            let mut canvas = Canvas::new(&mut buf, area);
            player.render(&mut canvas, area, &harness.render_cx(true));
        }
        (0..area.height)
            .map(|y| (0..area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn recognizes_common_video_extensions() {
        assert!(VideoPlayer::supports(Path::new("demo.mp4")));
        assert!(VideoPlayer::supports(Path::new("CLIP.MOV")));
        assert!(VideoPlayer::supports(Path::new("talk.webm")));
        assert!(VideoPlayer::supports(Path::new("x.mkv")));
        assert!(!VideoPlayer::supports(Path::new("song.mp3")));
        assert!(!VideoPlayer::supports(Path::new("photo.png")));
        assert!(!VideoPlayer::supports(Path::new("mp4")));
    }

    #[test]
    fn parses_ffprobe_video_metadata() {
        let json = r#"{
            "format": {"duration": "203.83"},
            "streams": [
                {"codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001"},
                {"codec_type": "audio", "codec_name": "aac"}
            ]
        }"#;
        let m = parse_ffprobe_json(json);
        assert_eq!(m.duration, Some(203.83));
        assert_eq!((m.width, m.height), (1920, 1080));
        assert_eq!(m.codec.as_deref(), Some("H264"));
        assert!(m.has_audio);
        assert!((m.fps.unwrap() - 29.97).abs() < 0.01);
        assert!(m.error.is_none());
    }

    #[test]
    fn ffprobe_without_video_stream_is_an_error() {
        let m = parse_ffprobe_json(r#"{"format": {"duration": "3"}, "streams": [{"codec_type": "audio"}]}"#);
        assert!(m.error.is_some());
        assert!(parse_ffprobe_json("not json").error.is_some());
    }

    #[test]
    fn parses_frame_rates() {
        assert_eq!(parse_rate("25"), Some(25.0));
        assert_eq!(parse_rate("30/1"), Some(30.0));
        assert_eq!(parse_rate("0/0"), None);
        assert_eq!(parse_rate("abc"), None);
        assert_eq!(trim_float(29.97), "29.97");
        assert_eq!(trim_float(30.0), "30");
    }

    #[test]
    fn fits_kitty_geometry_inside_the_viewport_keeping_aspect() {
        let vp = Rect::new(10, 2, 100, 30);
        let g = fit_geometry(1920, 1080, vp, Some((10.0, 20.0))).unwrap();
        // 1000×600 px viewport: a 16:9 picture is width-bound at 1000×563.
        assert_eq!((g.target_w, g.target_h), (1000, 563));
        assert_eq!(g.rect.width, 100);
        assert_eq!(g.rect.height, 29);
        assert!(rect_inside(g.rect, vp));
        assert_eq!((g.canvas_w, g.canvas_h), (1000, 580));
        assert!(g.kitty);
    }

    #[test]
    fn caps_huge_kitty_canvases() {
        let g = fit_geometry(3840, 2160, Rect::new(0, 0, 400, 100), Some((10.0, 20.0))).unwrap();
        assert!(g.canvas_w.max(g.canvas_h) <= MAX_CANVAS_SIDE);
        assert!(g.target_w <= g.canvas_w && g.target_h <= g.canvas_h);
    }

    #[test]
    fn halfblock_geometry_uses_two_pixels_per_cell() {
        let g = fit_geometry(1920, 1080, Rect::new(0, 1, 80, 20), None).unwrap();
        assert_eq!(g.canvas_w, g.rect.width as u32);
        assert_eq!(g.canvas_h, g.rect.height as u32 * 2);
        assert!(!g.kitty);
        assert!(fit_geometry(0, 10, Rect::new(0, 0, 10, 10), None).is_none());
        assert!(fit_geometry(10, 10, Rect::new(0, 0, 0, 10), None).is_none());
    }

    #[test]
    fn decoder_args_scale_pad_and_seek() {
        let g = Geometry { rect: Rect::new(0, 0, 10, 5), target_w: 100, target_h: 56, canvas_w: 100, canvas_h: 100, kitty: true };
        let args = decoder_args(Path::new("/v/a b.mp4"), 12.5, 24.0, g, true).join(" ");
        assert!(args.contains("-ss 12.500 -i /v/a b.mp4"), "{args}");
        assert!(args.contains("fps=24,scale=100:56"), "{args}");
        assert!(args.contains("pad=100:100:0:22"), "{args}");
        assert!(args.contains("-frames:v 1"), "{args}");
        assert!(args.ends_with("-f rawvideo -pix_fmt rgba -"), "{args}");
        let from_start = decoder_args(Path::new("a.mp4"), 0.0, 24.0, g, false).join(" ");
        assert!(!from_start.contains("-ss") && !from_start.contains("-frames"), "{from_start}");
    }

    #[test]
    fn renders_header_controls_and_loading_state() {
        let mut player = VideoPlayer::new(PathBuf::from("/tmp/demo.mp4"), 9_000_000, None, Waker::detached(), false);
        let area = Rect::new(0, 0, 100, 24);
        let text = render_text(&mut player, area);
        assert!(text.contains("demo.mp4"), "{text}");
        assert!(text.contains("Reading video"), "{text}");
        assert!(text.contains("Play"), "{text}");

        player.meta = Some(meta(1920, 1080));
        player.position_secs = 65.0;
        let text = render_text(&mut player, area);
        assert!(text.contains("1920 × 1080"), "{text}");
        assert!(text.contains("H264"), "{text}");
        assert!(text.contains("01:05 / 03:24"), "{text}");
        assert!(text.contains("Restart") && text.contains("Sound"), "{text}");
        assert!(player.play_btn.is_some() && player.rail.is_some());
    }

    #[test]
    fn shows_probe_errors_instead_of_loading() {
        let mut player = VideoPlayer::new(PathBuf::from("/tmp/bad.mp4"), 10, None, Waker::detached(), false);
        player.meta = Some(VideoMeta { error: Some("no video stream in this file".into()), ..VideoMeta::default() });
        let text = render_text(&mut player, Rect::new(0, 0, 80, 20));
        assert!(text.contains("no video stream"), "{text}");
    }

    #[test]
    fn seeking_clamps_to_the_duration() {
        let mut player = VideoPlayer::new(PathBuf::from("/nonexistent/x.mp4"), 1, None, Waker::detached(), false);
        player.meta = Some(meta(16, 9));
        player.seek_to(500.0);
        assert_eq!(player.current_position(), 204.0);
        player.seek_by(-1000.0);
        assert_eq!(player.current_position(), 0.0);
    }

    /// Decodes a real (generated) clip end to end: probe, a paused poster frame in half-block
    /// mode, then playback to the end. Skipped when ffmpeg is not installed.
    #[test]
    fn decodes_a_generated_clip() {
        if crate::format::which("ffmpeg").is_none() || crate::format::which("ffprobe").is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.mp4");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1", "-pix_fmt", "yuv420p"])
            .arg(&clip)
            .status()
            .unwrap();
        assert!(status.success());

        let m = probe_video(&clip);
        assert_eq!((m.width, m.height), (64, 48), "{m:?}");
        assert!(!m.has_audio);
        assert!((m.duration.unwrap() - 1.0).abs() < 0.2);

        let mut player = VideoPlayer::new(clip.clone(), 1, None, Waker::detached(), false);
        player.meta = Some(m);
        let area = Rect::new(0, 0, 40, 16);
        render_text(&mut player, area);
        let deadline = Instant::now() + Duration::from_secs(10);
        while player.latest_frame().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let frame = player.latest_frame().expect("poster frame");
        assert_eq!(frame.rgba.width(), frame.geometry.canvas_w);
        assert!(frame.png.is_none());
        let text = render_text(&mut player, area);
        assert!(text.contains('▀'), "{text}");

        player.play();
        assert!(player.playing);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut harness = Harness::new("/tmp");
        while player.playing && Instant::now() < deadline {
            player.tick(&mut harness.cx());
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!player.playing, "playback should stop at the end");
        assert!((player.current_position() - 1.0).abs() < 0.2);
    }
}
