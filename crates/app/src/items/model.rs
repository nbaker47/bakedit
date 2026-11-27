//! In-terminal 3D model viewer tab for `.glb`, `.gltf`, and `.obj` files.
//!
//! Renders meshes with a pure-Rust CPU software rasterizer (z-buffer, 3-point studio
//! lighting, base-color textures, vertex colors, optional wireframe and ground grid) and
//! displays frames via the Kitty Graphics Protocol (HQ native-DPI in Ghostty / Kitty /
//! WezTerm) with automatic true-color half-block (`▀`) fallback for other terminals.
//!
//! Controls: drag or `hjkl`/arrows to orbit, Shift-drag or right-drag to pan, scroll or
//! `+/-` to zoom, `1`-`4` preset angles, `0`/`r` reset, `w` wireframe, `m` shading mode,
//! `g` ground grid, `a`/Space turntable spin, `o` open externally.

use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::Instant;

use bakedit_core::{Action, Key, KeyEvent, Mods, MouseButton, MouseEvent, MouseKind, Rgb};
use image::{DynamicImage, Rgba, RgbaImage};
use ratatui::layout::Rect;

use crate::chrome::Tab;
use crate::icons::file_icon;
use crate::items::image::{encode_fast_png, kitty_graphics_supported, KittyImageKey, KittyImagePlacement};
use crate::ui::{self, Canvas, Style};
use crate::view::{Context, Cursor, Cx, Effect, FsChange, Item, ItemStatus, RenderCx, View};

const MODEL_EXTENSIONS: &[&str] = &["glb", "gltf", "obj"];

/// Cap CPU rasterizer resolution so interactive orbit stays responsive.
const MAX_RENDER_W: u32 = 840;
const MAX_RENDER_H: u32 = 640;

const DEFAULT_YAW: f32 = 0.55;
const DEFAULT_PITCH: f32 = 0.18;
const DEFAULT_DIST_MULT: f32 = 2.55;
const SPIN_RAD_PER_SEC: f32 = 0.75;

/// Shading style for the 3D viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShadingMode {
    Shaded,
    Clay,
    Normals,
    Wireframe,
}

impl ShadingMode {
    fn next(self) -> Self {
        match self {
            ShadingMode::Shaded => ShadingMode::Clay,
            ShadingMode::Clay => ShadingMode::Normals,
            ShadingMode::Normals => ShadingMode::Wireframe,
            ShadingMode::Wireframe => ShadingMode::Shaded,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ShadingMode::Shaded => "Shaded",
            ShadingMode::Clay => "Clay",
            ShadingMode::Normals => "Normals",
            ShadingMode::Wireframe => "Wireframe",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Vertex {
    pos: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 3],
}

#[derive(Debug, Clone)]
struct MeshPart {
    vertices: Vec<Vertex>,
    indices: Vec<[u32; 3]>,
    base_color: [f32; 4],
    texture_idx: Option<usize>,
    double_sided: bool,
}

#[derive(Debug, Clone)]
struct ModelScene {
    parts: Vec<MeshPart>,
    textures: Vec<RgbaImage>,
    bounds_min: [f32; 3],
    center: [f32; 3],
    radius: f32,
    triangle_count: usize,
    vertex_count: usize,
}

impl ModelScene {
    fn from_parts(parts: Vec<MeshPart>, textures: Vec<RgbaImage>) -> Result<Self, String> {
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut vertex_count = 0usize;
        let mut triangle_count = 0usize;

        for part in &parts {
            vertex_count += part.vertices.len();
            triangle_count += part.indices.len();
            for v in &part.vertices {
                for i in 0..3 {
                    if v.pos[i].is_finite() {
                        min[i] = min[i].min(v.pos[i]);
                        max[i] = max[i].max(v.pos[i]);
                    }
                }
            }
        }

        if vertex_count == 0 || !min[0].is_finite() {
            return Err("3D model contains no geometry".to_string());
        }

        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let dx = max[0] - min[0];
        let dy = max[1] - min[1];
        let dz = max[2] - min[2];
        let radius = ((dx * dx + dy * dy + dz * dz).sqrt() * 0.5).max(1e-4);

        Ok(ModelScene {
            parts,
            textures,
            bounds_min: min,
            center,
            radius,
            triangle_count,
            vertex_count,
        })
    }
}

#[derive(Debug, Clone, Copy)]
enum DragMode {
    Orbit { start_col: u16, start_row: u16, start_yaw: f32, start_pitch: f32 },
    Pan { start_col: u16, start_row: u16, start_pan_x: f32, start_pan_y: f32 },
}

/// A tab that displays a 3D model (`.glb`, `.gltf`, `.obj`) with interactive orbit/pan/zoom.
pub struct ModelViewer {
    path: PathBuf,
    file_size: u64,
    revision: u64,
    scene: ModelScene,
    yaw: f32,
    pitch: f32,
    distance: f32,
    pan_x: f32,
    pan_y: f32,
    shading: ShadingMode,
    wireframe_overlay: bool,
    show_grid: bool,
    auto_spin: bool,
    last_spin_tick: Instant,
    drag: Option<DragMode>,
    last_viewport: Rect,
    last_bg: Rgb,
    use_kitty_graphics: bool,
    cached_placement: Option<KittyImagePlacement>,
}

impl ModelViewer {
    /// True if `path` has a supported 3D model file extension.
    pub fn supports(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| MODEL_EXTENSIONS.iter().any(|&s| s.eq_ignore_ascii_case(ext)))
    }

    /// Load a 3D model from disk.
    pub fn open(path: &Path) -> Result<ModelViewer, String> {
        let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let scene = load_model(path)?;
        Ok(Self::from_scene(path.to_path_buf(), file_size, scene, kitty_graphics_supported()))
    }

    fn from_scene(
        path: PathBuf,
        file_size: u64,
        scene: ModelScene,
        use_kitty_graphics: bool,
    ) -> ModelViewer {
        ModelViewer {
            path,
            file_size,
            revision: 1,
            scene,
            yaw: DEFAULT_YAW,
            pitch: DEFAULT_PITCH,
            distance: DEFAULT_DIST_MULT,
            pan_x: 0.0,
            pan_y: 0.0,
            shading: ShadingMode::Shaded,
            wireframe_overlay: false,
            show_grid: true,
            auto_spin: false,
            last_spin_tick: Instant::now(),
            drag: None,
            last_viewport: Rect::default(),
            last_bg: Rgb::new(24, 24, 32),
            use_kitty_graphics,
            cached_placement: None,
        }
    }

    fn reload(&mut self) {
        if let Ok(scene) = load_model(&self.path) {
            self.file_size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
            self.scene = scene;
            self.revision = self.revision.wrapping_add(1);
            self.cached_placement = None;
        }
    }

    fn reset_camera(&mut self) {
        self.yaw = DEFAULT_YAW;
        self.pitch = DEFAULT_PITCH;
        self.distance = DEFAULT_DIST_MULT;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    fn orbit(&mut self, dyaw: f32, dpitch: f32) {
        self.yaw = (self.yaw + dyaw).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + dpitch).clamp(-1.48, 1.48);
    }

    fn zoom_by(&mut self, factor: f32) {
        self.distance = (self.distance * factor).clamp(0.55, 12.0);
    }

    fn state_hash(&self, bg: Rgb) -> u64 {
        let mut h = DefaultHasher::new();
        self.yaw.to_bits().hash(&mut h);
        self.pitch.to_bits().hash(&mut h);
        self.distance.to_bits().hash(&mut h);
        self.pan_x.to_bits().hash(&mut h);
        self.pan_y.to_bits().hash(&mut h);
        self.shading.hash(&mut h);
        self.wireframe_overlay.hash(&mut h);
        self.show_grid.hash(&mut h);
        bg.r.hash(&mut h);
        bg.g.hash(&mut h);
        bg.b.hash(&mut h);
        h.finish()
    }

    /// Build or return the cached Kitty Graphics placement for the last rendered viewport.
    pub fn kitty_placement(&mut self, cell_px: (u16, u16)) -> Option<KittyImagePlacement> {
        if !self.use_kitty_graphics || self.last_viewport.width < 2 || self.last_viewport.height < 2 {
            return None;
        }
        let vp = self.last_viewport;
        let key = KittyImageKey {
            path: self.path.clone(),
            revision: self.revision,
            rect: vp,
            state_hash: self.state_hash(self.last_bg),
            cell_px,
        };
        if let Some(cached) = &self.cached_placement {
            if cached.key == key {
                return Some(cached.clone());
            }
        }

        let raw_w = (vp.width as u32 * cell_px.0.max(4) as u32).max(64);
        let raw_h = (vp.height as u32 * cell_px.1.max(4) as u32).max(64);
        let scale = (MAX_RENDER_W as f32 / raw_w as f32)
            .min(MAX_RENDER_H as f32 / raw_h as f32)
            .min(1.0);
        let render_w = ((raw_w as f32 * scale).round() as u32).max(32);
        let render_h = ((raw_h as f32 * scale).round() as u32).max(32);

        let img = self.render_rgba(render_w, render_h, self.last_bg);
        let png = encode_fast_png(&img)?;
        let placement = KittyImagePlacement { key, rect: vp, png };
        self.cached_placement = Some(placement.clone());
        Some(placement)
    }

    fn render_rgba(&self, width: u32, height: u32, bg: Rgb) -> RgbaImage {
        render_scene(
            &self.scene,
            width,
            height,
            CameraParams {
                yaw: self.yaw,
                pitch: self.pitch,
                distance: self.distance,
                pan_x: self.pan_x,
                pan_y: self.pan_y,
                shading: self.shading,
                wireframe_overlay: self.wireframe_overlay,
                show_grid: self.show_grid,
                bg,
            },
        )
    }

    fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }
}

impl View for ModelViewer {
    fn render(&mut self, canvas: &mut Canvas<'_>, area: Rect, cx: &RenderCx<'_>) -> Option<Cursor> {
        let bg = cx.theme.background;
        self.last_bg = bg;
        canvas.fill(area, bg);
        if area.width == 0 || area.height == 0 {
            return None;
        }

        // Top info bar (1 row).
        let header = Rect::new(area.x, area.y, area.width, 1);
        let header_bg = cx.theme.surface;
        canvas.fill(header, header_bg);
        let quality = if self.use_kitty_graphics { "  ·  HQ" } else { "" };
        let spin = if self.auto_spin { "  ·  Spin" } else { "" };
        let wire = if self.wireframe_overlay && self.shading != ShadingMode::Wireframe { "+Wire" } else { "" };
        let left = format!(
            " {}  ·  {} tris  ·  {} verts  ·  {}  ·  {}{}{}{}",
            self.title(),
            format_count(self.scene.triangle_count),
            format_count(self.scene.vertex_count),
            format_bytes(self.file_size),
            self.shading.label(),
            wire,
            spin,
            quality,
        );
        let hints = "drag/hjkl orbit  +/- zoom  w wire  m mode  a spin  r reset  o open ";
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

        if self.use_kitty_graphics {
            return None;
        }

        // Half-block (`▀`) fallback when Kitty Graphics Protocol is unavailable.
        let rw = viewport.width.max(1) as u32;
        let rh = (viewport.height.max(1) as u32) * 2;
        let img = self.render_rgba(rw, rh, bg);
        for row in 0..viewport.height {
            let py_top = (row as u32) * 2;
            let py_bot = py_top + 1;
            for col in 0..viewport.width {
                let px = col as u32;
                let top = img.get_pixel(px, py_top).0;
                let bot = img.get_pixel(px, py_bot).0;
                canvas.put(
                    viewport.x + col,
                    viewport.y + row,
                    "▀",
                    Style::colors(Rgb::new(top[0], top[1], top[2]), Rgb::new(bot[0], bot[1], bot[2])),
                );
            }
        }
        None
    }

    fn contexts(&self, out: &mut Vec<Context>) {
        out.push(Context::new("ModelViewer"));
    }

    fn tick(&mut self, _cx: &mut Cx<'_>) -> bool {
        if !self.auto_spin {
            return false;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_spin_tick).as_secs_f32();
        if dt < 0.033 {
            return false;
        }
        self.last_spin_tick = now;
        self.orbit(dt.min(0.2) * SPIN_RAD_PER_SEC, 0.0);
        true
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
            Key::Left | Key::Char('h') => {
                self.auto_spin = false;
                self.orbit(-0.18, 0.0);
                true
            }
            Key::Right | Key::Char('l') => {
                self.auto_spin = false;
                self.orbit(0.18, 0.0);
                true
            }
            Key::Up | Key::Char('k') => {
                self.orbit(0.0, 0.14);
                true
            }
            Key::Down | Key::Char('j') => {
                self.orbit(0.0, -0.14);
                true
            }
            Key::Char('+') | Key::Char('=') => {
                self.zoom_by(0.84);
                true
            }
            Key::Char('-') | Key::Char('_') => {
                self.zoom_by(1.2);
                true
            }
            Key::Char('0') | Key::Char('r') => {
                self.auto_spin = false;
                self.reset_camera();
                true
            }
            Key::Char('1') => {
                self.auto_spin = false;
                self.yaw = 0.0;
                self.pitch = 0.08;
                true
            }
            Key::Char('2') => {
                self.auto_spin = false;
                self.yaw = std::f32::consts::FRAC_PI_2;
                self.pitch = 0.08;
                true
            }
            Key::Char('3') => {
                self.auto_spin = false;
                self.yaw = std::f32::consts::PI;
                self.pitch = 0.08;
                true
            }
            Key::Char('4') => {
                self.auto_spin = false;
                self.yaw = 0.0;
                self.pitch = 1.35;
                true
            }
            Key::Char('w') => {
                self.wireframe_overlay = !self.wireframe_overlay;
                true
            }
            Key::Char('m') => {
                self.shading = self.shading.next();
                true
            }
            Key::Char('g') => {
                self.show_grid = !self.show_grid;
                true
            }
            Key::Char('a') | Key::Char(' ') => {
                self.auto_spin = !self.auto_spin;
                self.last_spin_tick = Instant::now();
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
                self.zoom_by(0.88);
                true
            }
            MouseKind::ScrollDown => {
                self.zoom_by(1.14);
                true
            }
            MouseKind::Press(MouseButton::Left) => {
                self.auto_spin = false;
                if event.mods.contains(Mods::SHIFT) {
                    self.drag = Some(DragMode::Pan {
                        start_col: event.col,
                        start_row: event.row,
                        start_pan_x: self.pan_x,
                        start_pan_y: self.pan_y,
                    });
                } else {
                    self.drag = Some(DragMode::Orbit {
                        start_col: event.col,
                        start_row: event.row,
                        start_yaw: self.yaw,
                        start_pitch: self.pitch,
                    });
                }
                true
            }
            MouseKind::Press(MouseButton::Right) => {
                self.auto_spin = false;
                self.drag = Some(DragMode::Pan {
                    start_col: event.col,
                    start_row: event.row,
                    start_pan_x: self.pan_x,
                    start_pan_y: self.pan_y,
                });
                true
            }
            MouseKind::Drag(MouseButton::Left) | MouseKind::Drag(MouseButton::Right) => {
                match self.drag {
                    Some(DragMode::Orbit { start_col, start_row, start_yaw, start_pitch }) => {
                        let dx = event.col as f32 - start_col as f32;
                        let dy = event.row as f32 - start_row as f32;
                        self.yaw = (start_yaw + dx * 0.045).rem_euclid(std::f32::consts::TAU);
                        self.pitch = (start_pitch + dy * 0.065).clamp(-1.48, 1.48);
                        true
                    }
                    Some(DragMode::Pan { start_col, start_row, start_pan_x, start_pan_y }) => {
                        let dx = event.col as f32 - start_col as f32;
                        let dy = event.row as f32 - start_row as f32;
                        let scale = self.distance * 0.015;
                        self.pan_x = (start_pan_x - dx * scale).clamp(-4.0, 4.0);
                        self.pan_y = (start_pan_y + dy * scale * 2.0).clamp(-4.0, 4.0);
                        true
                    }
                    None => false,
                }
            }
            MouseKind::Release(MouseButton::Left) | MouseKind::Release(MouseButton::Right) => {
                self.drag = None;
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

impl Item for ModelViewer {
    fn kind(&self) -> &'static str {
        "model"
    }

    fn status(&self) -> ItemStatus {
        ItemStatus {
            language: Some(format!("3D {} tris", format_count(self.scene.triangle_count))),
            ..ItemStatus::default()
        }
    }

    fn tab(&self, cx: &RenderCx<'_>) -> Tab {
        let title = self.title();
        let icon = Some(file_icon(cx.settings, &title, false, false));
        Tab { title, dirty: false, preview: false, pinned: false, icon }
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

// ---------------------------------------------------------------------------
// Loaders: glTF / GLB and OBJ
// ---------------------------------------------------------------------------

fn load_model(path: &Path) -> Result<ModelScene, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "glb" | "gltf" => load_gltf(path),
        "obj" => load_obj(path),
        _ => Err(format!("unsupported 3D model format: {}", path.display())),
    }
}

fn load_gltf(path: &Path) -> Result<ModelScene, String> {
    let (doc, buffers, images) = gltf::import(path)
        .map_err(|e| format!("could not load glTF {}: {e}", path.display()))?;

    let textures: Vec<RgbaImage> = images
        .iter()
        .map(|img| gltf_image_to_rgba(img))
        .collect();

    let mut parts = Vec::new();
    let identity = mat4_identity();

    if let Some(scene) = doc.default_scene().or_else(|| doc.scenes().next()) {
        for node in scene.nodes() {
            collect_gltf_node(&node, &identity, &buffers, &mut parts);
        }
    } else {
        for mesh in doc.meshes() {
            collect_gltf_mesh(&mesh, &identity, &buffers, &mut parts);
        }
    }

    ModelScene::from_parts(parts, textures)
}

fn gltf_image_to_rgba(img: &gltf::image::Data) -> RgbaImage {
    let (w, h) = (img.width, img.height);
    let mut out = RgbaImage::new(w, h);
    let buf: &mut [u8] = out.as_mut();
    match img.format {
        gltf::image::Format::R8G8B8A8 => {
            for (dst, src) in buf.chunks_exact_mut(4).zip(img.pixels.chunks_exact(4)) {
                dst.copy_from_slice(src);
            }
        }
        gltf::image::Format::R8G8B8 => {
            for (dst, src) in buf.chunks_exact_mut(4).zip(img.pixels.chunks_exact(3)) {
                dst[0] = src[0];
                dst[1] = src[1];
                dst[2] = src[2];
                dst[3] = 255;
            }
        }
        gltf::image::Format::R8 => {
            for (dst, &v) in buf.chunks_exact_mut(4).zip(img.pixels.iter()) {
                dst[0] = v;
                dst[1] = v;
                dst[2] = v;
                dst[3] = 255;
            }
        }
        gltf::image::Format::R8G8 => {
            for (dst, src) in buf.chunks_exact_mut(4).zip(img.pixels.chunks_exact(2)) {
                dst[0] = src[0];
                dst[1] = src[0];
                dst[2] = src[0];
                dst[3] = src[1];
            }
        }
        _ => {
            // 16-bit / float formats: fall back to neutral white so we never panic.
            for p in out.pixels_mut() {
                *p = Rgba([220, 220, 220, 255]);
            }
        }
    }
    out
}

fn collect_gltf_node(
    node: &gltf::Node<'_>,
    parent: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    parts: &mut Vec<MeshPart>,
) {
    let local = node.transform().matrix();
    let world = mat4_mul(parent, &local);
    if let Some(mesh) = node.mesh() {
        collect_gltf_mesh(&mesh, &world, buffers, parts);
    }
    for child in node.children() {
        collect_gltf_node(&child, &world, buffers, parts);
    }
}

fn collect_gltf_mesh(
    mesh: &gltf::Mesh<'_>,
    world: &[[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    parts: &mut Vec<MeshPart>,
) {
    for prim in mesh.primitives() {
        if prim.mode() != gltf::mesh::Mode::Triangles {
            continue;
        }
        let reader = prim.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
        let Some(positions) = reader.read_positions() else {
            continue;
        };
        let raw_positions: Vec<[f32; 3]> = positions.collect();
        if raw_positions.is_empty() {
            continue;
        }
        let raw_normals: Vec<[f32; 3]> = reader
            .read_normals()
            .map(|it| it.collect())
            .unwrap_or_default();
        let raw_uvs: Vec<[f32; 2]> = reader
            .read_tex_coords(0)
            .map(|tc| tc.into_f32().collect())
            .unwrap_or_default();
        let raw_colors: Vec<[f32; 3]> = reader
            .read_colors(0)
            .map(|c| c.into_rgb_f32().collect())
            .unwrap_or_default();

        let flat_indices: Vec<u32> = match reader.read_indices() {
            Some(idx) => idx.into_u32().collect(),
            None => (0..raw_positions.len() as u32).collect(),
        };
        let indices: Vec<[u32; 3]> = flat_indices
            .chunks_exact(3)
            .filter_map(|tri| {
                let n = raw_positions.len() as u32;
                if tri[0] < n && tri[1] < n && tri[2] < n {
                    Some([tri[0], tri[1], tri[2]])
                } else {
                    None
                }
            })
            .collect();
        if indices.is_empty() {
            continue;
        }

        let mut vertices: Vec<Vertex> = Vec::with_capacity(raw_positions.len());
        let has_normals = raw_normals.len() == raw_positions.len();
        for (i, &p) in raw_positions.iter().enumerate() {
            let pos = mat4_transform_point(world, p);
            let normal = if has_normals {
                vec3_normalize(mat4_transform_dir(world, raw_normals[i]))
            } else {
                [0.0, 0.0, 0.0]
            };
            let uv = raw_uvs.get(i).copied().unwrap_or([0.0, 0.0]);
            let color = raw_colors.get(i).copied().unwrap_or([1.0, 1.0, 1.0]);
            vertices.push(Vertex { pos, normal, uv, color });
        }
        if !has_normals {
            compute_smooth_normals(&mut vertices, &indices);
        }

        let mat = prim.material();
        let pbr = mat.pbr_metallic_roughness();
        let base_color = pbr.base_color_factor();
        let texture_idx = pbr.base_color_texture().map(|t| t.texture().source().index());
        let double_sided = mat.double_sided();

        parts.push(MeshPart { vertices, indices, base_color, texture_idx, double_sided });
    }
}

fn load_obj(path: &Path) -> Result<ModelScene, String> {
    let (models, materials) = tobj::load_obj(path, &tobj::GPU_LOAD_OPTIONS)
        .map_err(|e| format!("could not load OBJ {}: {e}", path.display()))?;
    let materials = materials.unwrap_or_default();
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let mut textures: Vec<RgbaImage> = Vec::new();
    let mut mat_tex_idx: Vec<Option<usize>> = Vec::with_capacity(materials.len());
    for mat in &materials {
        let idx = mat.diffuse_texture.as_ref().and_then(|rel| {
            let full = base_dir.join(rel);
            image::open(full).ok().map(|img: DynamicImage| {
                let idx = textures.len();
                textures.push(img.to_rgba8());
                idx
            })
        });
        mat_tex_idx.push(idx);
    }

    let mut parts = Vec::new();
    for model in models {
        let mesh = model.mesh;
        let vcount = mesh.positions.len() / 3;
        if vcount == 0 {
            continue;
        }
        let has_normals = mesh.normals.len() == mesh.positions.len();
        let has_uvs = mesh.texcoords.len() == vcount * 2;
        let has_colors = mesh.vertex_color.len() == mesh.positions.len();

        let mut vertices = Vec::with_capacity(vcount);
        for i in 0..vcount {
            let pos = [mesh.positions[i * 3], mesh.positions[i * 3 + 1], mesh.positions[i * 3 + 2]];
            let normal = if has_normals {
                vec3_normalize([mesh.normals[i * 3], mesh.normals[i * 3 + 1], mesh.normals[i * 3 + 2]])
            } else {
                [0.0, 0.0, 0.0]
            };
            let uv = if has_uvs {
                [mesh.texcoords[i * 2], 1.0 - mesh.texcoords[i * 2 + 1]]
            } else {
                [0.0, 0.0]
            };
            let color = if has_colors {
                [mesh.vertex_color[i * 3], mesh.vertex_color[i * 3 + 1], mesh.vertex_color[i * 3 + 2]]
            } else {
                [1.0, 1.0, 1.0]
            };
            vertices.push(Vertex { pos, normal, uv, color });
        }

        let indices: Vec<[u32; 3]> = mesh
            .indices
            .chunks_exact(3)
            .filter_map(|tri| {
                let n = vcount as u32;
                if tri[0] < n && tri[1] < n && tri[2] < n {
                    Some([tri[0], tri[1], tri[2]])
                } else {
                    None
                }
            })
            .collect();
        if indices.is_empty() {
            continue;
        }
        if !has_normals {
            compute_smooth_normals(&mut vertices, &indices);
        }

        let (base_color, texture_idx) = match mesh.material_id.and_then(|id| materials.get(id).map(|m| (id, m))) {
            Some((id, mat)) => {
                let d = mat.diffuse.unwrap_or([0.82, 0.82, 0.85]);
                ([d[0], d[1], d[2], mat.dissolve.unwrap_or(1.0)], mat_tex_idx.get(id).copied().flatten())
            }
            None => ([0.82, 0.82, 0.85, 1.0], None),
        };

        parts.push(MeshPart { vertices, indices, base_color, texture_idx, double_sided: true });
    }

    ModelScene::from_parts(parts, textures)
}

fn compute_smooth_normals(vertices: &mut [Vertex], indices: &[[u32; 3]]) {
    for v in vertices.iter_mut() {
        v.normal = [0.0, 0.0, 0.0];
    }
    for &[i0, i1, i2] in indices {
        let p0 = vertices[i0 as usize].pos;
        let p1 = vertices[i1 as usize].pos;
        let p2 = vertices[i2 as usize].pos;
        let n = vec3_cross(vec3_sub(p1, p0), vec3_sub(p2, p0));
        for &idx in &[i0, i1, i2] {
            let vn = &mut vertices[idx as usize].normal;
            vn[0] += n[0];
            vn[1] += n[1];
            vn[2] += n[2];
        }
    }
    for v in vertices.iter_mut() {
        let n = vec3_normalize(v.normal);
        v.normal = if vec3_len(n) < 1e-5 { [0.0, 1.0, 0.0] } else { n };
    }
}

// ---------------------------------------------------------------------------
// Software Rasterizer
// ---------------------------------------------------------------------------

struct CameraParams {
    yaw: f32,
    pitch: f32,
    distance: f32,
    pan_x: f32,
    pan_y: f32,
    shading: ShadingMode,
    wireframe_overlay: bool,
    show_grid: bool,
    bg: Rgb,
}

#[derive(Clone, Copy)]
struct ScreenVert {
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
    normal: [f32; 3],
    uv: [f32; 2],
    color: [f32; 3],
}

fn render_scene(scene: &ModelScene, width: u32, height: u32, cam: CameraParams) -> RgbaImage {
    let w = width.max(1) as usize;
    let h = height.max(1) as usize;
    let mut color_buf = vec![0u8; w * h * 4];
    let mut depth_buf = vec![f32::INFINITY; w * h];

    // Subtle vertical studio backdrop matching the theme background.
    let top_bg = [
        (cam.bg.r as f32 * 1.18 + 8.0).min(255.0),
        (cam.bg.g as f32 * 1.18 + 8.0).min(255.0),
        (cam.bg.b as f32 * 1.24 + 12.0).min(255.0),
    ];
    let bot_bg = [
        (cam.bg.r as f32 * 0.80).max(0.0),
        (cam.bg.g as f32 * 0.80).max(0.0),
        (cam.bg.b as f32 * 0.85).max(0.0),
    ];
    for y in 0..h {
        let t = y as f32 / (h.saturating_sub(1).max(1) as f32);
        let r = (top_bg[0] + (bot_bg[0] - top_bg[0]) * t) as u8;
        let g = (top_bg[1] + (bot_bg[1] - top_bg[1]) * t) as u8;
        let b = (top_bg[2] + (bot_bg[2] - top_bg[2]) * t) as u8;
        let row_start = y * w * 4;
        for x in 0..w {
            let idx = row_start + x * 4;
            color_buf[idx] = r;
            color_buf[idx + 1] = g;
            color_buf[idx + 2] = b;
            color_buf[idx + 3] = 255;
        }
    }

    // Orbit camera around scene center.
    let dist = cam.distance * scene.radius;
    let cp = cam.pitch.cos();
    let sp = cam.pitch.sin();
    let cy = cam.yaw.cos();
    let sy = cam.yaw.sin();

    let cam_dir = [sy * cp, sp, cy * cp];
    let right = vec3_normalize(vec3_cross([0.0, 1.0, 0.0], cam_dir));
    let up = vec3_cross(cam_dir, right);

    let target = [
        scene.center[0] + right[0] * cam.pan_x * scene.radius + up[0] * cam.pan_y * scene.radius,
        scene.center[1] + right[1] * cam.pan_x * scene.radius + up[1] * cam.pan_y * scene.radius,
        scene.center[2] + right[2] * cam.pan_x * scene.radius + up[2] * cam.pan_y * scene.radius,
    ];
    let eye = [
        target[0] + cam_dir[0] * dist,
        target[1] + cam_dir[1] * dist,
        target[2] + cam_dir[2] * dist,
    ];

    let view = mat4_look_at(eye, target, [0.0, 1.0, 0.0]);
    let aspect = w as f32 / h as f32;
    let near = (scene.radius * 0.05).max(1e-3);
    let far = (scene.radius * 50.0).max(dist * 4.0);
    let proj = mat4_perspective(40.0_f32.to_radians(), aspect, near, far);
    let vp = mat4_mul(&proj, &view);

    // Light directions in world space (camera-relative key/fill/rim so lighting always reads well).
    let key_dir = vec3_normalize(vec3_add(vec3_add(vec3_scale(right, 0.65), vec3_scale(up, 0.72)), vec3_scale(cam_dir, 0.55)));
    let fill_dir = vec3_normalize(vec3_add(vec3_scale(right, -0.80), vec3_scale(up, 0.25)));
    let rim_dir = vec3_normalize(vec3_add(vec3_scale(up, 0.55), vec3_scale(cam_dir, -0.85)));

    // Draw optional studio ground grid under the model's feet.
    if cam.show_grid {
        draw_ground_grid(scene, &vp, w, h, &mut color_buf, &mut depth_buf, cam.bg);
    }

    let wf = w as f32;
    let hf = h as f32;

    for part in &scene.parts {
        let tex = part.texture_idx.and_then(|i| scene.textures.get(i));
        let proj_verts: Vec<Option<ScreenVert>> = part
            .vertices
            .iter()
            .map(|v| project_vertex(v, &vp, wf, hf))
            .collect();

        for &[i0, i1, i2] in &part.indices {
            let (Some(v0), Some(v1), Some(v2)) = (
                proj_verts[i0 as usize],
                proj_verts[i1 as usize],
                proj_verts[i2 as usize],
            ) else {
                continue;
            };

            let area = edge_fn(v0.x, v0.y, v1.x, v1.y, v2.x, v2.y);
            if area.abs() < 1e-4 {
                continue;
            }
            if !part.double_sided && cam.shading != ShadingMode::Wireframe && area < 0.0 {
                continue;
            }

            if cam.shading == ShadingMode::Wireframe {
                let wire_col = [140u8, 210, 255];
                draw_line_3d(v0, v1, wire_col, w, h, &mut color_buf, &mut depth_buf, 0.0);
                draw_line_3d(v1, v2, wire_col, w, h, &mut color_buf, &mut depth_buf, 0.0);
                draw_line_3d(v2, v0, wire_col, w, h, &mut color_buf, &mut depth_buf, 0.0);
                continue;
            }

            let min_x = v0.x.min(v1.x).min(v2.x).floor().max(0.0) as usize;
            let max_x = v0.x.max(v1.x).max(v2.x).ceil().min(wf - 1.0) as usize;
            let min_y = v0.y.min(v1.y).min(v2.y).floor().max(0.0) as usize;
            let max_y = v0.y.max(v1.y).max(v2.y).ceil().min(hf - 1.0) as usize;
            if min_x > max_x || min_y > max_y {
                continue;
            }

            let inv_area = 1.0 / area;
            let flip_normal = area < 0.0;

            for py in min_y..=max_y {
                let fy = py as f32 + 0.5;
                let row_offset = py * w;
                for px in min_x..=max_x {
                    let fx = px as f32 + 0.5;
                    let w0 = edge_fn(v1.x, v1.y, v2.x, v2.y, fx, fy) * inv_area;
                    if w0 < 0.0 {
                        continue;
                    }
                    let w1 = edge_fn(v2.x, v2.y, v0.x, v0.y, fx, fy) * inv_area;
                    if w1 < 0.0 {
                        continue;
                    }
                    let w2 = 1.0 - w0 - w1;
                    if w2 < 0.0 {
                        continue;
                    }

                    let z = w0 * v0.z + w1 * v1.z + w2 * v2.z;
                    let idx = row_offset + px;
                    if z >= depth_buf[idx] || z < 0.0 || z > 1.0 {
                        continue;
                    }

                    let iw0 = w0 * v0.inv_w;
                    let iw1 = w1 * v1.inv_w;
                    let iw2 = w2 * v2.inv_w;
                    let inv_sum = 1.0 / (iw0 + iw1 + iw2).max(1e-8);
                    let b0 = iw0 * inv_sum;
                    let b1 = iw1 * inv_sum;
                    let b2 = iw2 * inv_sum;

                    let mut n = vec3_normalize([
                        b0 * v0.normal[0] + b1 * v1.normal[0] + b2 * v2.normal[0],
                        b0 * v0.normal[1] + b1 * v1.normal[1] + b2 * v2.normal[1],
                        b0 * v0.normal[2] + b1 * v1.normal[2] + b2 * v2.normal[2],
                    ]);
                    if flip_normal {
                        n = [-n[0], -n[1], -n[2]];
                    }

                    let rgb = match cam.shading {
                        ShadingMode::Normals => [
                            ((n[0] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                            ((n[1] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                            ((n[2] * 0.5 + 0.5).clamp(0.0, 1.0) * 255.0) as u8,
                        ],
                        ShadingMode::Clay => {
                            shade_lit([0.78, 0.74, 0.70], n, key_dir, fill_dir, rim_dir, cam_dir)
                        }
                        ShadingMode::Shaded => {
                            let mut albedo = [
                                part.base_color[0] * (b0 * v0.color[0] + b1 * v1.color[0] + b2 * v2.color[0]),
                                part.base_color[1] * (b0 * v0.color[1] + b1 * v1.color[1] + b2 * v2.color[1]),
                                part.base_color[2] * (b0 * v0.color[2] + b1 * v1.color[2] + b2 * v2.color[2]),
                            ];
                            if let Some(t) = tex {
                                let u = b0 * v0.uv[0] + b1 * v1.uv[0] + b2 * v2.uv[0];
                                let v = b0 * v0.uv[1] + b1 * v1.uv[1] + b2 * v2.uv[1];
                                let [tr, tg, tb, ta] = sample_texture(t, u, v);
                                if ta < 0.25 {
                                    continue;
                                }
                                albedo[0] *= tr;
                                albedo[1] *= tg;
                                albedo[2] *= tb;
                            }
                            shade_lit(albedo, n, key_dir, fill_dir, rim_dir, cam_dir)
                        }
                        ShadingMode::Wireframe => unreachable!(),
                    };

                    depth_buf[idx] = z;
                    let p = idx * 4;
                    color_buf[p] = rgb[0];
                    color_buf[p + 1] = rgb[1];
                    color_buf[p + 2] = rgb[2];
                    color_buf[p + 3] = 255;
                }
            }

            if cam.wireframe_overlay {
                let wire_col = [40u8, 220, 210];
                draw_line_3d(v0, v1, wire_col, w, h, &mut color_buf, &mut depth_buf, -0.0008);
                draw_line_3d(v1, v2, wire_col, w, h, &mut color_buf, &mut depth_buf, -0.0008);
                draw_line_3d(v2, v0, wire_col, w, h, &mut color_buf, &mut depth_buf, -0.0008);
            }
        }
    }

    RgbaImage::from_raw(width.max(1), height.max(1), color_buf)
        .unwrap_or_else(|| RgbaImage::new(width.max(1), height.max(1)))
}

#[inline]
fn shade_lit(
    albedo: [f32; 3],
    n: [f32; 3],
    key_dir: [f32; 3],
    fill_dir: [f32; 3],
    rim_dir: [f32; 3],
    view_dir: [f32; 3],
) -> [u8; 3] {
    // Hemisphere ambient.
    let hemi = (n[1] * 0.5 + 0.5).clamp(0.0, 1.0);
    let amb_r = 0.16 + 0.14 * hemi;
    let amb_g = 0.17 + 0.15 * hemi;
    let amb_b = 0.20 + 0.16 * hemi;

    // 3-point diffuse.
    let k = vec3_dot(n, key_dir).max(0.0);
    let f = vec3_dot(n, fill_dir).max(0.0);
    let r = vec3_dot(n, rim_dir).max(0.0);

    // Subtle Blinn-Phong specular from key light.
    let half_v = vec3_normalize(vec3_add(key_dir, view_dir));
    let spec = vec3_dot(n, half_v).max(0.0).powf(28.0) * 0.22;

    let lr = amb_r + k * 0.88 + f * 0.24 + r * 0.22;
    let lg = amb_g + k * 0.85 + f * 0.28 + r * 0.24;
    let lb = amb_b + k * 0.80 + f * 0.34 + r * 0.30;

    [
        ((albedo[0] * lr + spec).clamp(0.0, 1.0) * 255.0) as u8,
        ((albedo[1] * lg + spec).clamp(0.0, 1.0) * 255.0) as u8,
        ((albedo[2] * lb + spec).clamp(0.0, 1.0) * 255.0) as u8,
    ]
}

#[inline]
fn sample_texture(tex: &RgbaImage, u: f32, v: f32) -> [f32; 4] {
    let w = tex.width();
    let h = tex.height();
    if w == 0 || h == 0 {
        return [1.0, 1.0, 1.0, 1.0];
    }
    let uu = u.rem_euclid(1.0);
    let vv = v.rem_euclid(1.0);
    let x = ((uu * w as f32) as u32).min(w - 1);
    let y = ((vv * h as f32) as u32).min(h - 1);
    let p = tex.get_pixel(x, y).0;
    [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0, p[3] as f32 / 255.0]
}

fn draw_ground_grid(
    scene: &ModelScene,
    vp: &[[f32; 4]; 4],
    w: usize,
    h: usize,
    color_buf: &mut [u8],
    depth_buf: &mut [f32],
    bg: Rgb,
) {
    let y = scene.bounds_min[1];
    let half = scene.radius * 1.35;
    let steps = 8i32;
    let step = (half * 2.0) / steps as f32;
    let cx = scene.center[0];
    let cz = scene.center[2];
    let grid_col = [
        bg.r.saturating_add(34),
        bg.g.saturating_add(38),
        bg.b.saturating_add(48),
    ];
    let axis_x_col = [145u8, 65, 65];
    let axis_z_col = [65u8, 105, 165];
    let wf = w as f32;
    let hf = h as f32;

    for i in -steps / 2..=steps / 2 {
        let offset = i as f32 * step;
        let col_x = if i == 0 { axis_x_col } else { grid_col };
        let col_z = if i == 0 { axis_z_col } else { grid_col };

        if let (Some(a), Some(b)) = (
            project_point([cx - half, y, cz + offset], vp, wf, hf),
            project_point([cx + half, y, cz + offset], vp, wf, hf),
        ) {
            draw_line_3d(a, b, col_x, w, h, color_buf, depth_buf, 0.0);
        }
        if let (Some(a), Some(b)) = (
            project_point([cx + offset, y, cz - half], vp, wf, hf),
            project_point([cx + offset, y, cz + half], vp, wf, hf),
        ) {
            draw_line_3d(a, b, col_z, w, h, color_buf, depth_buf, 0.0);
        }
    }
}

fn project_vertex(v: &Vertex, vp: &[[f32; 4]; 4], wf: f32, hf: f32) -> Option<ScreenVert> {
    let c = mat4_mul_vec4(vp, [v.pos[0], v.pos[1], v.pos[2], 1.0]);
    if c[3] <= 1e-4 {
        return None;
    }
    let inv_w = 1.0 / c[3];
    let ndc_x = c[0] * inv_w;
    let ndc_y = c[1] * inv_w;
    let ndc_z = c[2] * inv_w;
    if ndc_z < -1.0 || ndc_z > 1.0 {
        return None;
    }
    Some(ScreenVert {
        x: (ndc_x * 0.5 + 0.5) * wf,
        y: (0.5 - ndc_y * 0.5) * hf,
        z: ndc_z * 0.5 + 0.5,
        inv_w,
        normal: v.normal,
        uv: v.uv,
        color: v.color,
    })
}

fn project_point(p: [f32; 3], vp: &[[f32; 4]; 4], wf: f32, hf: f32) -> Option<ScreenVert> {
    project_vertex(
        &Vertex { pos: p, normal: [0.0, 1.0, 0.0], uv: [0.0, 0.0], color: [1.0, 1.0, 1.0] },
        vp,
        wf,
        hf,
    )
}

fn draw_line_3d(
    a: ScreenVert,
    b: ScreenVert,
    col: [u8; 3],
    w: usize,
    h: usize,
    color_buf: &mut [u8],
    depth_buf: &mut [f32],
    z_bias: f32,
) {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 || steps > 4096 {
        return;
    }
    let inv = 1.0 / steps as f32;
    for s in 0..=steps {
        let t = s as f32 * inv;
        let x = (a.x + dx * t).round() as i32;
        let y = (a.y + dy * t).round() as i32;
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            continue;
        }
        let z = a.z + (b.z - a.z) * t + z_bias;
        let idx = y as usize * w + x as usize;
        if z < depth_buf[idx] && z >= 0.0 && z <= 1.0 {
            depth_buf[idx] = z;
            let p = idx * 4;
            color_buf[p] = col[0];
            color_buf[p + 1] = col[1];
            color_buf[p + 2] = col[2];
            color_buf[p + 3] = 255;
        }
    }
}

#[inline]
fn edge_fn(ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32) -> f32 {
    (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
}

// ---------------------------------------------------------------------------
// Minimal 3D math (column-major 4x4 matrices matching glTF)
// ---------------------------------------------------------------------------

fn mat4_identity() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat4_mul(a: &[[f32; 4]; 4], b: &[[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = a[0][r] * b[c][0]
                + a[1][r] * b[c][1]
                + a[2][r] * b[c][2]
                + a[3][r] * b[c][3];
        }
    }
    out
}

fn mat4_mul_vec4(m: &[[f32; 4]; 4], v: [f32; 4]) -> [f32; 4] {
    [
        m[0][0] * v[0] + m[1][0] * v[1] + m[2][0] * v[2] + m[3][0] * v[3],
        m[0][1] * v[0] + m[1][1] * v[1] + m[2][1] * v[2] + m[3][1] * v[3],
        m[0][2] * v[0] + m[1][2] * v[1] + m[2][2] * v[2] + m[3][2] * v[3],
        m[0][3] * v[0] + m[1][3] * v[1] + m[2][3] * v[2] + m[3][3] * v[3],
    ]
}

fn mat4_transform_point(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 3] {
    let v = mat4_mul_vec4(m, [p[0], p[1], p[2], 1.0]);
    [v[0], v[1], v[2]]
}

fn mat4_transform_dir(m: &[[f32; 4]; 4], d: [f32; 3]) -> [f32; 3] {
    let v = mat4_mul_vec4(m, [d[0], d[1], d[2], 0.0]);
    [v[0], v[1], v[2]]
}

fn mat4_look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> [[f32; 4]; 4] {
    let f = vec3_normalize(vec3_sub(target, eye));
    let s = vec3_normalize(vec3_cross(f, up));
    let u = vec3_cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [-vec3_dot(s, eye), -vec3_dot(u, eye), vec3_dot(f, eye), 1.0],
    ]
}

fn mat4_perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> [[f32; 4]; 4] {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        [f / aspect.max(1e-4), 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, (far + near) * nf, -1.0],
        [0.0, 0.0, 2.0 * far * near * nf, 0.0],
    ]
}

#[inline]
fn vec3_add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

#[inline]
fn vec3_sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn vec3_scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}

#[inline]
fn vec3_dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn vec3_cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn vec3_len(a: [f32; 3]) -> f32 {
    vec3_dot(a, a).sqrt()
}

#[inline]
fn vec3_normalize(a: [f32; 3]) -> [f32; 3] {
    let l = vec3_len(a);
    if l > 1e-6 {
        [a[0] / l, a[1] / l, a[2] / l]
    } else {
        [0.0, 0.0, 0.0]
    }
}

fn format_count(n: usize) -> String {
    if n < 1_000 {
        format!("{n}")
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{:.2}M", n as f64 / 1_000_000.0)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::image::encode_kitty_graphics;

    fn sample_cube_scene() -> ModelScene {
        let verts = vec![
            Vertex { pos: [-1.0, -1.0, 1.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 0.0], color: [1.0, 0.4, 0.3] },
            Vertex { pos: [1.0, -1.0, 1.0], normal: [0.0, 0.0, 1.0], uv: [1.0, 0.0], color: [0.3, 1.0, 0.4] },
            Vertex { pos: [1.0, 1.0, 1.0], normal: [0.0, 0.0, 1.0], uv: [1.0, 1.0], color: [0.3, 0.4, 1.0] },
            Vertex { pos: [-1.0, 1.0, 1.0], normal: [0.0, 0.0, 1.0], uv: [0.0, 1.0], color: [1.0, 1.0, 0.4] },
        ];
        let part = MeshPart {
            vertices: verts,
            indices: vec![[0, 1, 2], [0, 2, 3]],
            base_color: [1.0, 1.0, 1.0, 1.0],
            texture_idx: None,
            double_sided: true,
        };
        ModelScene::from_parts(vec![part], Vec::new()).unwrap()
    }

    #[test]
    fn recognizes_model_extensions() {
        assert!(ModelViewer::supports(Path::new("human_male.glb")));
        assert!(ModelViewer::supports(Path::new("SCENE.GLTF")));
        assert!(ModelViewer::supports(Path::new("prop.obj")));
        assert!(!ModelViewer::supports(Path::new("photo.png")));
        assert!(!ModelViewer::supports(Path::new("main.rs")));
    }

    #[test]
    fn loads_obj_file_and_renders_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tri.obj");
        std::fs::write(
            &path,
            "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n",
        )
        .unwrap();
        let viewer = ModelViewer::open(&path).expect("loads valid obj");
        assert_eq!(viewer.scene.triangle_count, 1);
        assert_eq!(viewer.scene.vertex_count, 3);
        let img = viewer.render_rgba(120, 90, Rgb::new(20, 20, 28));
        assert_eq!(img.dimensions(), (120, 90));
        // Some pixels in the center should differ from the backdrop.
        let corner = img.get_pixel(0, 0).0;
        assert!(img.pixels().any(|p| p.0 != corner), "rendered triangle should be visible");
    }

    #[test]
    fn builds_kitty_placement_and_orbits_on_key() {
        let mut viewer = ModelViewer::from_scene(
            PathBuf::from("cube.glb"),
            2048,
            sample_cube_scene(),
            true,
        );
        viewer.last_viewport = Rect::new(2, 3, 40, 20);
        let p1 = viewer.kitty_placement((10, 20)).expect("kitty placement");
        assert!(p1.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let seq = String::from_utf8(encode_kitty_graphics(&p1)).unwrap();
        assert!(seq.contains("a=T,f=100,i=42,c=40,r=20"));

        let old_yaw = viewer.yaw;
        viewer.orbit(0.25, 0.0);
        assert!((viewer.yaw - old_yaw).abs() > 0.1);
        let p2 = viewer.kitty_placement((10, 20)).expect("updated placement");
        assert_ne!(p1.key, p2.key);
    }
}
