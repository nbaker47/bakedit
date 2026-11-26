//! In-terminal image viewer tab using true-color Unicode half-blocks (`▀`).
//!
//! Each terminal cell renders two vertical pixels (top = foreground, bottom = background)
//! with alpha composited over a subtle checkerboard so transparent images remain legible.
//! Supports fit-to-pane, zoom (`+`/`-`/`0`), pan (arrows / `hjkl` / drag), and opening
//! externally (`o`).

use std::any::Any;
use std::path::{Path, PathBuf};

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb};
use image::{DynamicImage, GenericImageView, RgbaImage};
use ratatui::layout::Rect;

use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemStatus, RenderCx, View};

const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff",
];

/// A tab that displays an image file using 24-bit half-block rendering.
pub struct ImageViewer {
    path: PathBuf,
    file_size: u64,
    width: u32,
    height: u32,
    pixels: RgbaImage,
    /// `None` = fit to viewport; `Some(scale)` = explicit pixel-to-halfblock scale (1.0 = 100%).
    zoom: Option<f32>,
    /// Pan offset in source image pixels when zoomed in.
    pan_x: f32,
    pan_y: f32,
    dragging_from: Option<(u16, u16, f32, f32)>,
    last_viewport: Rect,
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
        let img = image::load_from_memory(&bytes)
            .map_err(|e| format!("could not decode image {}: {e}", path.display()))?;
        Ok(Self::from_dynamic(path.to_path_buf(), file_size, img))
    }

    fn from_dynamic(path: PathBuf, file_size: u64, img: DynamicImage) -> ImageViewer {
        let (width, height) = img.dimensions();
        let pixels = img.to_rgba8();
        ImageViewer {
            path,
            file_size,
            width,
            height,
            pixels,
            zoom: None,
            pan_x: 0.0,
            pan_y: 0.0,
            dragging_from: None,
            last_viewport: Rect::default(),
        }
    }

    fn reload(&mut self) {
        if let Ok(bytes) = std::fs::read(&self.path) {
            if let Ok(img) = image::load_from_memory(&bytes) {
                let (w, h) = img.dimensions();
                self.file_size = bytes.len() as u64;
                self.width = w;
                self.height = h;
                self.pixels = img.to_rgba8();
                self.clamp_pan();
            }
        }
    }

    /// Compute the effective scale (image pixels per half-block pixel) for `viewport`.
    fn fit_scale(&self, viewport: Rect) -> f32 {
        let vw = viewport.width.max(1) as f32;
        let vh = (viewport.height.max(1) * 2) as f32;
        let sx = vw / self.width.max(1) as f32;
        let sy = vh / self.height.max(1) as f32;
        sx.min(sy).min(1.0).max(0.01)
    }

    fn effective_scale(&self, viewport: Rect) -> f32 {
        self.zoom.unwrap_or_else(|| self.fit_scale(viewport))
    }

    fn zoom_by(&mut self, factor: f32) {
        let cur = self.effective_scale(self.last_viewport);
        let next = (cur * factor).clamp(0.02, 8.0);
        self.zoom = Some(next);
        self.clamp_pan();
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
        let scale = self.effective_scale(self.last_viewport).max(0.01);
        self.pan_x += dx_cells / scale;
        self.pan_y += (dy_cells * 2.0) / scale;
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
        // Subtle checkerboard for transparent pixels.
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
        let left = format!(
            " {}  ·  {} × {}  ·  {}  ·  {}",
            self.title(),
            self.width,
            self.height,
            format_bytes(self.file_size),
            zoom_label
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

        let scale = self.effective_scale(viewport);
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
                        let scale = self.effective_scale(self.last_viewport).max(0.01);
                        let dx = (sx as f32 - event.col as f32) / scale;
                        let dy = ((sy as f32 - event.row as f32) * 2.0) / scale;
                        self.pan_x = px + dx;
                        self.pan_y = py + dy;
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
    fn renders_image_with_header_and_half_blocks() {
        let mut rgba = RgbaImage::new(4, 4);
        for p in rgba.pixels_mut() {
            *p = image::Rgba([255, 80, 40, 255]);
        }
        let mut viewer = ImageViewer::from_dynamic(
            PathBuf::from("/tmp/sample.png"),
            128,
            DynamicImage::ImageRgba8(rgba),
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
}
