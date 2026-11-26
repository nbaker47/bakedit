//! In-terminal image viewer tab with Kitty Graphics Protocol (HQ native-DPI rendering
//! in Kitty / Ghostty / WezTerm) and automatic true-color half-block (`▀`) fallback for
//! other terminals.
//!
//! Supports fit-to-pane, zoom (`+`/`-`/`0`/scroll), pan (arrows / `hjkl` / drag), live
//! reload on disk changes, and opening externally (`o`).

use std::any::Any;
use std::io::Cursor as IoCursor;
use std::path::{Path, PathBuf};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb};
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat, RgbaImage};
use ratatui::layout::Rect;

use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemStatus, RenderCx, View};

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff",
];

const KITTY_IMAGE_ID: u32 = 42;
const MAX_RAW_PNG_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENCODED_DIM: u32 = 2400;

/// Cache key identifying the exact image, viewport, and view transform currently shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KittyImageKey {
    pub path: PathBuf,
    pub revision: u64,
    pub rect: Rect,
    pub zoom_bits: Option<u32>,
    pub pan_x_bits: u32,
    pub pan_y_bits: u32,
    pub cell_px: (u16, u16),
}

/// Ready-to-transmit Kitty Graphics Protocol placement for the active image tab.
#[derive(Debug, Clone)]
pub struct KittyImagePlacement {
    pub key: KittyImageKey,
    /// Display rectangle in 0-based terminal cells.
    pub rect: Rect,
    /// PNG bytes to transmit with `f=100`.
    pub png: Vec<u8>,
}

/// True when the host terminal supports the Kitty Graphics Protocol.
pub fn kitty_graphics_supported() -> bool {
    if cfg!(test) {
        return false;
    }
    if std::env::var("TERM_PROGRAM").ok().as_deref() == Some("ptyshot") {
        return false;
    }
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        return true;
    }
    if let Ok(term) = std::env::var("TERM") {
        let lower = term.to_ascii_lowercase();
        if lower.contains("kitty") || lower.contains("ghostty") {
            return true;
        }
    }
    if let Ok(prog) = std::env::var("TERM_PROGRAM") {
        let lower = prog.to_ascii_lowercase();
        if lower.contains("ghostty") || lower.contains("kitty") || lower.contains("wezterm") {
            return true;
        }
    }
    false
}

/// Pixel dimensions of one terminal cell `(width_px, height_px)` via `TIOCGWINSZ`.
pub fn terminal_cell_pixels() -> (u16, u16) {
    #[cfg(unix)]
    {
        let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: TIOCGWINSZ writes terminal dimensions into the valid stack `winsize` struct.
        let rc = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
        if rc == 0 && ws.ws_col > 0 && ws.ws_row > 0 && ws.ws_xpixel > 0 && ws.ws_ypixel > 0 {
            let cw = (ws.ws_xpixel / ws.ws_col).max(4);
            let ch = (ws.ws_ypixel / ws.ws_row).max(4);
            return (cw, ch);
        }
    }
    (10, 20)
}

/// Escape sequence that removes bakedit's active Kitty graphics image.
pub fn delete_kitty_graphics() -> &'static [u8] {
    b"\x1b_Ga=d,d=I,i=42,q=2\x1b\\"
}

/// Escape sequence that transmits and displays `placement` at its exact cell rectangle.
pub fn encode_kitty_graphics(placement: &KittyImagePlacement) -> Vec<u8> {
    let b64 = base64_encode(&placement.png);
    let mut out = Vec::with_capacity(b64.len() + 256);
    out.extend_from_slice(delete_kitty_graphics());
    // Save cursor, move to 1-based (row, col), transmit chunks with C=1 (no cursor move), restore.
    out.extend_from_slice(b"\x1b7");
    let pos = format!("\x1b[{};{}H", placement.rect.y + 1, placement.rect.x + 1);
    out.extend_from_slice(pos.as_bytes());
    const CHUNK: usize = 4096;
    let total = b64.len();
    let mut offset = 0;
    let mut first = true;
    while offset < total {
        let end = (offset + CHUNK).min(total);
        let more = u8::from(end < total);
        if first {
            let header = format!(
                "\x1b_Ga=T,f=100,i={KITTY_IMAGE_ID},c={},r={},C=1,q=2,m={more};",
                placement.rect.width, placement.rect.height
            );
            out.extend_from_slice(header.as_bytes());
            first = false;
        } else {
            let header = format!("\x1b_Gm={more};");
            out.extend_from_slice(header.as_bytes());
        }
        out.extend_from_slice(&b64.as_bytes()[offset..end]);
        out.extend_from_slice(b"\x1b\\");
        offset = end;
    }
    out.extend_from_slice(b"\x1b8");
    out
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((triple >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(triple & 0x3f) as usize] as char } else { '=' });
    }
    out
}

/// A tab that displays an image file using Kitty Graphics Protocol (HQ) or half-blocks.
pub struct ImageViewer {
    path: PathBuf,
    file_size: u64,
    revision: u64,
    width: u32,
    height: u32,
    pixels: RgbaImage,
    /// Original PNG bytes when the file is already a reasonably-sized PNG (fast path for Fit).
    raw_png: Option<Vec<u8>>,
    /// `None` = fit to viewport; `Some(mult)` = zoom multiplier relative to Fit (`1.0` = Fit).
    zoom: Option<f32>,
    /// Pan offset in source image pixels when zoomed in.
    pan_x: f32,
    pan_y: f32,
    dragging_from: Option<(u16, u16, f32, f32)>,
    last_viewport: Rect,
    use_kitty_graphics: bool,
    cached_placement: Option<KittyImagePlacement>,
}

impl ImageViewer {
    /// True if `path` has a supported image file extension.
    pub fn supports(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| IMAGE_EXTENSIONS.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
    }

    /// Load and decode an image from disk.
    pub fn open(path: &Path) -> Result<ImageViewer, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
        let file_size = bytes.len() as u64;
        let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() <= MAX_RAW_PNG_BYTES;
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("could not decode image {}: {e}", path.display()))?;
        let raw_png = if is_png { Some(bytes) } else { None };
        Ok(Self::from_dynamic(path.to_path_buf(), file_size, img, raw_png, kitty_graphics_supported()))
    }

    fn from_dynamic(
        path: PathBuf,
        file_size: u64,
        img: DynamicImage,
        raw_png: Option<Vec<u8>>,
        use_kitty_graphics: bool,
    ) -> ImageViewer {
        let (width, height) = img.dimensions();
        let pixels = img.to_rgba8();
        ImageViewer {
            path,
            file_size,
            revision: 1,
            width,
            height,
            pixels,
            raw_png,
            zoom: None,
            pan_x: 0.0,
            pan_y: 0.0,
            dragging_from: None,
            last_viewport: Rect::default(),
            use_kitty_graphics,
            cached_placement: None,
        }
    }

    fn reload(&mut self) {
        if let Ok(bytes) = std::fs::read(&self.path) {
            let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() <= MAX_RAW_PNG_BYTES;
            if let Ok(img) = image::load_from_memory(&bytes) {
                let (w, h) = img.dimensions();
                self.file_size = bytes.len() as u64;
                self.width = w;
                self.height = h;
                self.pixels = img.to_rgba8();
                self.raw_png = if is_png { Some(bytes) } else { None };
                self.revision = self.revision.wrapping_add(1);
                self.cached_placement = None;
                self.clamp_pan();
            }
        }
    }

    /// Build or return the cached Kitty Graphics placement for the last rendered viewport.
    pub fn kitty_placement(&mut self, cell_px: (u16, u16)) -> Option<KittyImagePlacement> {
        if !self.use_kitty_graphics || self.last_viewport.width < 2 || self.last_viewport.height < 2 {
            return None;
        }
        let vp = self.last_viewport;
        let cw = cell_px.0.max(4) as f32;
        let ch = cell_px.1.max(4) as f32;
        let vp_px_w = vp.width as f32 * cw;
        let vp_px_h = vp.height as f32 * ch;
        let fit_scale = (vp_px_w / self.width.max(1) as f32).min(vp_px_h / self.height.max(1) as f32);

        let (place_rect, crop_rect) = match self.zoom {
            None => {
                let disp_w = (self.width as f32 * fit_scale).max(1.0);
                let disp_h = (self.height as f32 * fit_scale).max(1.0);
                let cols = (disp_w / cw).round().clamp(1.0, vp.width as f32) as u16;
                let rows = (disp_h / ch).round().clamp(1.0, vp.height as f32) as u16;
                let col = vp.x + (vp.width.saturating_sub(cols)) / 2;
                let row = vp.y + (vp.height.saturating_sub(rows)) / 2;
                (Rect::new(col, row, cols, rows), None)
            }
            Some(mult) => {
                let scale = (fit_scale * mult).max(0.01);
                let disp_w = (self.width as f32 * scale).min(vp_px_w).max(1.0);
                let disp_h = (self.height as f32 * scale).min(vp_px_h).max(1.0);
                let cols = (disp_w / cw).round().clamp(1.0, vp.width as f32) as u16;
                let rows = (disp_h / ch).round().clamp(1.0, vp.height as f32) as u16;
                let col = vp.x + (vp.width.saturating_sub(cols)) / 2;
                let row = vp.y + (vp.height.saturating_sub(rows)) / 2;

                let src_w = ((cols as f32 * cw) / scale).round().clamp(1.0, self.width as f32) as u32;
                let src_h = ((rows as f32 * ch) / scale).round().clamp(1.0, self.height as f32) as u32;
                let cx = self.width as f32 * 0.5 + self.pan_x;
                let cy = self.height as f32 * 0.5 + self.pan_y;
                let sx = (cx - src_w as f32 * 0.5).round().clamp(0.0, self.width.saturating_sub(src_w) as f32) as u32;
                let sy = (cy - src_h as f32 * 0.5).round().clamp(0.0, self.height.saturating_sub(src_h) as f32) as u32;
                (Rect::new(col, row, cols, rows), Some((sx, sy, src_w, src_h)))
            }
        };

        let key = KittyImageKey {
            path: self.path.clone(),
            revision: self.revision,
            rect: place_rect,
            zoom_bits: self.zoom.map(f32::to_bits),
            pan_x_bits: self.pan_x.to_bits(),
            pan_y_bits: self.pan_y.to_bits(),
            cell_px,
        };
        if let Some(cached) = &self.cached_placement {
            if cached.key == key {
                return Some(cached.clone());
            }
        }

        let png = if crop_rect.is_none() {
            if let Some(raw) = &self.raw_png {
                raw.clone()
            } else {
                encode_png(&self.pixels, MAX_ENCODED_DIM)?
            }
        } else if let Some((sx, sy, sw, sh)) = crop_rect {
            let sub = image::imageops::crop_imm(&self.pixels, sx, sy, sw, sh).to_image();
            encode_png(&sub, MAX_ENCODED_DIM)?
        } else {
            return None;
        };

        let placement = KittyImagePlacement { key, rect: place_rect, png };
        self.cached_placement = Some(placement.clone());
        Some(placement)
    }

    /// Half-block fallback scale (image pixels per half-block pixel).
    fn halfblock_fit_scale(&self, viewport: Rect) -> f32 {
        let vw = viewport.width.max(1) as f32;
        let vh = (viewport.height.max(1) * 2) as f32;
        let sx = vw / self.width.max(1) as f32;
        let sy = vh / self.height.max(1) as f32;
        sx.min(sy).min(1.0).max(0.01)
    }

    fn zoom_by(&mut self, factor: f32) {
        let cur = self.zoom.unwrap_or(1.0);
        let next = (cur * factor).clamp(0.25, 16.0);
        if (next - 1.0).abs() < 0.03 {
            self.reset_zoom();
        } else {
            self.zoom = Some(next);
            self.clamp_pan();
        }
    }

    fn reset_zoom(&mut self) {
        self.zoom = None;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    fn pan_by(&mut self, dx_cells: f32, dy_cells: f32) {
        if self.zoom.is_none() {
            return;
        }
        let mult = self.zoom.unwrap_or(1.0).max(0.25);
        let step_x = (self.width as f32 / (self.last_viewport.width.max(1) as f32 * mult)).max(1.0);
        let step_y = (self.height as f32 / (self.last_viewport.height.max(1) as f32 * mult)).max(1.0);
        self.pan_x += dx_cells * step_x;
        self.pan_y += dy_cells * step_y;
        self.clamp_pan();
    }

    fn clamp_pan(&mut self) {
        let max_x = (self.width as f32) * 0.5;
        let max_y = (self.height as f32) * 0.5;
        self.pan_x = self.pan_x.clamp(-max_x, max_x);
        self.pan_y = self.pan_y.clamp(-max_y, max_y);
    }

    fn sample_rgb(&self, ix: i32, iy: i32, bg: Rgb) -> Option<Rgb> {
        if ix < 0 || iy < 0 || ix >= self.width as i32 || iy >= self.height as i32 {
            return None;
        }
        let p = self.pixels.get_pixel(ix as u32, iy as u32);
        let [r, g, b, a] = p.0;
        if a == 255 {
            return Some(Rgb::new(r, g, b));
        }
        let checker = if ((ix / 4) + (iy / 4)) % 2 == 0 {
            bg.lerp(Rgb::new(128, 128, 128), 0.14)
        } else {
            bg.lerp(Rgb::new(128, 128, 128), 0.24)
        };
        if a == 0 {
            return Some(checker);
        }
        let alpha = a as f32 / 255.0;
        Some(checker.lerp(Rgb::new(r, g, b), alpha))
    }

    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

fn encode_png(rgba: &RgbaImage, max_dim: u32) -> Option<Vec<u8>> {
    let dyn_img = if rgba.width() > max_dim || rgba.height() > max_dim {
        DynamicImage::ImageRgba8(rgba.clone()).resize(max_dim, max_dim, FilterType::Triangle)
    } else {
        DynamicImage::ImageRgba8(rgba.clone())
    };
    let mut out = Vec::new();
    dyn_img.write_to(&mut IoCursor::new(&mut out), ImageFormat::Png).ok()?;
    Some(out)
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

impl View for ImageViewer {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bg = cx.theme.background;
        canvas.fill(area, bg);
        if area.width == 0 || area.height == 0 {
            return None;
        }

        // Top info bar (1 row).
        let header = Rect::new(area.x, area.y, area.width, 1);
        let header_bg = cx.theme.surface;
        canvas.fill(header, header_bg);
        let zoom_label = match self.zoom {
            None => "Fit".to_string(),
            Some(z) => format!("{:.0}%", z * 100.0),
        };
        let quality = if self.use_kitty_graphics { "  ·  HQ" } else { "" };
        let left = format!(
            " {}  ·  {} × {}  ·  {}  ·  {}{}",
            self.title(),
            self.width,
            self.height,
            format_bytes(self.file_size),
            zoom_label,
            quality
        );
        let hints = "+/- zoom  0 fit  o open ";
        let hint_w = ui::width(hints) as u16;
        let left_max = area.width.saturating_sub(hint_w + 2);
        canvas.text_truncated(header.x, header.y, &left, Style::colors(cx.theme.text, header_bg), left_max);
        if area.width > hint_w + 4 {
            canvas.text_right(header.right(), header.y, hints, Style::colors(cx.theme.text_muted, header_bg));
        }

        if area.height <= 1 {
            return None;
        }
        let viewport = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
        self.last_viewport = viewport;

        // When Kitty Graphics Protocol is active, the host terminal draws the HQ image
        // over the clean background in `App::draw`.
        if self.use_kitty_graphics {
            return None;
        }

        let scale = self.halfblock_fit_scale(viewport) * self.zoom.unwrap_or(1.0);
        let img_cx = self.width as f32 * 0.5 + self.pan_x;
        let img_cy = self.height as f32 * 0.5 + self.pan_y;
        let view_cx = viewport.width as f32 * 0.5;
        let view_cy = (viewport.height as f32 * 2.0) * 0.5;

        for row in 0..viewport.height {
            let py_top = (row as f32 * 2.0) + 0.5;
            let py_bot = py_top + 1.0;
            let iy_top = (img_cy + (py_top - view_cy) / scale).floor() as i32;
            let iy_bot = (img_cy + (py_bot - view_cy) / scale).floor() as i32;

            for col in 0..viewport.width {
                let px = col as f32 + 0.5;
                let ix = (img_cx + (px - view_cx) / scale).floor() as i32;
                let top = self.sample_rgb(ix, iy_top, bg);
                let bot = self.sample_rgb(ix, iy_bot, bg);
                match (top, bot) {
                    (Some(t), Some(b)) => {
                        canvas.put(viewport.x + col, viewport.y + row, "▀", Style::colors(t, b));
                    }
                    (Some(t), None) => {
                        canvas.put(viewport.x + col, viewport.y + row, "▀", Style::colors(t, bg));
                    }
                    (None, Some(b)) => {
                        canvas.put(viewport.x + col, viewport.y + row, "▄", Style::colors(b, bg));
                    }
                    (None, None) => {}
                }
            }
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ImageViewer"));
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
            Key::Char('+') | Key::Char('=') => {
                self.zoom_by(1.25);
                true
            }
            Key::Char('-') | Key::Char('_') => {
                self.zoom_by(0.8);
                true
            }
            Key::Char('0') => {
                self.reset_zoom();
                true
            }
            Key::Left | Key::Char('h') => {
                self.pan_by(-4.0, 0.0);
                true
            }
            Key::Right | Key::Char('l') => {
                self.pan_by(4.0, 0.0);
                true
            }
            Key::Up | Key::Char('k') => {
                self.pan_by(0.0, -2.0);
                true
            }
            Key::Down | Key::Char('j') => {
                self.pan_by(0.0, 2.0);
                true
            }
            Key::Char('o') => {
                cx.push(Effect::OpenExternally(self.path.to_string_lossy().into_owned()));
                true
            }
            _ => false,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent, _area: Rect, _cx: &mut Cx<'_>) -> bool {
        match event.kind {
            MouseKind::ScrollUp => {
                self.zoom_by(1.15);
                true
            }
            MouseKind::ScrollDown => {
                self.zoom_by(1.0 / 1.15);
                true
            }
            MouseKind::Press(MouseButton::Left) => {
                self.dragging_from = Some((event.col, event.row, self.pan_x, self.pan_y));
                true
            }
            MouseKind::Drag(MouseButton::Left) => {
                if let Some((sx, sy, px, py)) = self.dragging_from {
                    if self.zoom.is_some() {
                        let mult = self.zoom.unwrap_or(1.0).max(0.25);
                        let step_x = (self.width as f32 / (self.last_viewport.width.max(1) as f32 * mult)).max(1.0);
                        let step_y = (self.height as f32 / (self.last_viewport.height.max(1) as f32 * mult)).max(1.0);
                        self.pan_x = px + (sx as f32 - event.col as f32) * step_x;
                        self.pan_y = py + (sy as f32 - event.row as f32) * step_y;
                        self.clamp_pan();
                    }
                    return true;
                }
                false
            }
            MouseKind::Release(MouseButton::Left) => {
                self.dragging_from = None;
                true
            }
            _ => false,
        }
    }

    fn paths_changed(&mut self, change: &FsChange, _cx: &mut Cx<'_>) {
        if change.touches(&self.path) {
            self.reload();
        }
    }
}

impl Item for ImageViewer {
    fn kind(&self) -> &'static str {
        "image"
    }

    fn status(&self) -> ItemStatus {
        ItemStatus {
            language: Some(format!("Image {}×{}", self.width, self.height)),
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

    fn moved(&mut self, from: &Path, to: &Path, _cx: &mut Cx<'_>) {
        if let Ok(rel) = self.path.strip_prefix(from) {
            self.path = if rel.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rel) };
            self.cached_placement = None;
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
    fn recognizes_common_image_extensions() {
        assert!(ImageViewer::supports(Path::new("photo.png")));
        assert!(ImageViewer::supports(Path::new("PIC.JPEG")));
        assert!(ImageViewer::supports(Path::new("icon.webp")));
        assert!(!ImageViewer::supports(Path::new("main.rs")));
        assert!(!ImageViewer::supports(Path::new("song.mp3")));
    }

    #[test]
    fn renders_image_with_header_and_half_blocks_fallback() {
        let mut rgba = RgbaImage::new(4, 4);
        for p in rgba.pixels_mut() {
            *p = image::Rgba([255, 80, 40, 255]);
        }
        let mut viewer = ImageViewer::from_dynamic(
            PathBuf::from("/tmp/sample.png"),
            128,
            DynamicImage::ImageRgba8(rgba),
            None,
            false,
        );
        let harness = Harness::new("/tmp");
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        {
            let mut canvas = Canvas::new(&mut buf, area);
            viewer.render(&mut canvas, area, &harness.render_cx(true));
        }
        let header: String = (0..area.width).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        assert!(header.contains("sample.png"), "{header:?}");
        assert!(header.contains("4 × 4"), "{header:?}");
        let has_half_block = (1..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .any(|(x, y)| buf[(x, y)].symbol() == "▀" || buf[(x, y)].symbol() == "▄");
        assert!(has_half_block, "expected half-block image cells");
    }

    #[test]
    fn builds_kitty_graphics_placement_and_sequence() {
        let mut rgba = RgbaImage::new(200, 100);
        for p in rgba.pixels_mut() {
            *p = image::Rgba([40, 160, 255, 255]);
        }
        let mut viewer = ImageViewer::from_dynamic(
            PathBuf::from("/tmp/banner.png"),
            1024,
            DynamicImage::ImageRgba8(rgba),
            None,
            true,
        );
        let harness = Harness::new("/tmp");
        let area = Rect::new(10, 2, 80, 25);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        {
            let mut canvas = Canvas::new(&mut buf, area);
            viewer.render(&mut canvas, area, &harness.render_cx(true));
        }
        let placement = viewer.kitty_placement((10, 20)).expect("kitty placement");
        assert!(placement.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(placement.rect.x >= 10 && placement.rect.y >= 3);
        let seq = String::from_utf8(encode_kitty_graphics(&placement)).unwrap();
        assert!(seq.contains("\x1b_Ga=T,f=100,i=42,"));
        assert!(seq.contains(",C=1,q=2,"));
    }
}
