//! Lightweight Unicode box-drawing Mermaid renderer for NAV mode in Markdown files.
//!
//! Parses common Mermaid diagram types (`flowchart`/`graph` TD/TB/LR/RL/BT,
//! `sequenceDiagram`, `stateDiagram`/`stateDiagram-v2`, `classDiagram`, `erDiagram`)
//! and renders them into styled Unicode box-drawing lines using the active theme.
//! Unsupported diagram types return `None` so the editor falls back to normal
//! syntax-highlighted source inside the code fence.

use std::collections::{HashMap, HashSet, VecDeque};

use bakedit_core::{Rgb, Theme};

use crate::markdown::Span;
use crate::ui::{self, Canvas, Style};

/// A rendered Mermaid diagram: one `Vec<Span>` per output row.
#[derive(Debug, Clone)]
pub struct RenderedDiagram {
    pub lines: Vec<Vec<Span>>,
}

/// Draw one row of a rendered Mermaid diagram into `canvas`, respecting horizontal scroll `left`.
pub fn draw_diagram_row(
    canvas: &mut Canvas,
    x: u16,
    y: u16,
    width: u16,
    left: usize,
    row: &[Span],
) {
    let mut col = 0usize;
    let max_col = left + width as usize;
    for span in row {
        for ch in span.text.chars() {
            let cw = ui::width(ch.encode_utf8(&mut [0; 4])).max(1);
            if col + cw > left && col < max_col {
                let sx = x + (col.saturating_sub(left)) as u16;
                let mut buf = [0u8; 4];
                canvas.put(sx, y, ch.encode_utf8(&mut buf), span.style);
            }
            col += cw;
            if col >= max_col {
                return;
            }
        }
    }
}

/// Attempt to render a Mermaid fenced block's interior lines into styled Unicode art.
/// Returns `None` if the diagram type is unsupported or empty.
pub fn render(source_lines: &[String], max_width: usize, bg: Rgb, theme: &Theme) -> Option<RenderedDiagram> {
    let header = source_lines
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !l.starts_with("%%"))?;

    let lower = header.to_ascii_lowercase();
    if lower.starts_with("graph ") || lower == "graph" || lower.starts_with("flowchart ") || lower == "flowchart" {
        render_flowchart(source_lines, max_width, bg, theme)
    } else if lower.starts_with("sequencediagram") {
        render_sequence(source_lines, max_width, bg, theme)
    } else if lower.starts_with("statediagram") {
        render_state(source_lines, max_width, bg, theme)
    } else if lower.starts_with("classdiagram") || lower.starts_with("erdiagram") {
        render_relation_graph(source_lines, max_width, bg, theme)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Shared grid buffer for box-drawing art
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    fg: Rgb,
    bold: bool,
}

struct Grid {
    rows: Vec<Vec<Cell>>,
    default_fg: Rgb,
}

impl Grid {
    fn new(width: usize, height: usize, default_fg: Rgb) -> Self {
        let blank = Cell { ch: ' ', fg: default_fg, bold: false };
        Self {
            rows: vec![vec![blank; width]; height],
            default_fg,
        }
    }

    fn ensure(&mut self, x: usize, y: usize) {
        let blank = Cell { ch: ' ', fg: self.default_fg, bold: false };
        if y >= self.rows.len() {
            self.rows.resize(y + 1, Vec::new());
        }
        if x >= self.rows[y].len() {
            self.rows[y].resize(x + 1, blank);
        }
    }

    fn set(&mut self, x: usize, y: usize, ch: char, fg: Rgb, bold: bool) {
        self.ensure(x, y);
        self.rows[y][x] = Cell { ch, fg, bold };
    }

    fn put_str(&mut self, x: usize, y: usize, text: &str, fg: Rgb, bold: bool) {
        let mut cx = x;
        for ch in text.chars() {
            self.set(cx, y, ch, fg, bold);
            cx += ui::width(ch.encode_utf8(&mut [0; 4])).max(1);
        }
    }

    fn draw_box(&mut self, x: usize, y: usize, w: usize, h: usize, shape: NodeShape, label: &str, border_fg: Rgb, text_fg: Rgb) {
        if w < 2 || h < 3 {
            return;
        }
        let (tl, tr, bl, br, hz, vt) = match shape {
            NodeShape::Round | NodeShape::Stadium => ('╭', '╮', '╰', '╯', '─', '│'),
            NodeShape::Diamond => ('╭', '╮', '╰', '╯', '─', '│'),
            NodeShape::Cylinder => ('╭', '╮', '╰', '╯', '─', '│'),
            NodeShape::Circle => ('╭', '╮', '╰', '╯', '─', '│'),
            NodeShape::Rect => ('┌', '┐', '└', '┘', '─', '│'),
        };
        let bottom_hz = if shape == NodeShape::Cylinder { '═' } else { hz };
        self.set(x, y, tl, border_fg, false);
        self.set(x + w - 1, y, tr, border_fg, false);
        self.set(x, y + h - 1, bl, border_fg, false);
        self.set(x + w - 1, y + h - 1, br, border_fg, false);
        for dx in 1..w - 1 {
            self.set(x + dx, y, hz, border_fg, false);
            self.set(x + dx, y + h - 1, bottom_hz, border_fg, false);
        }
        for dy in 1..h - 1 {
            self.set(x, y + dy, vt, border_fg, false);
            self.set(x + w - 1, y + dy, vt, border_fg, false);
        }
        let inner_w = w.saturating_sub(2);
        let disp = if shape == NodeShape::Diamond {
            format!("◇ {label} ◇")
        } else {
            label.to_string()
        };
        let trunc = truncate_display(&disp, inner_w);
        let tw = ui::width(&trunc);
        let pad = inner_w.saturating_sub(tw) / 2;
        self.put_str(x + 1 + pad, y + h / 2, &trunc, text_fg, true);
    }

    fn into_spans(self, bg: Rgb) -> RenderedDiagram {
        let mut lines = Vec::with_capacity(self.rows.len());
        for row in self.rows {
            // Trim trailing spaces.
            let end = row.iter().rposition(|c| c.ch != ' ').map(|i| i + 1).unwrap_or(0);
            let mut spans: Vec<Span> = Vec::new();
            for cell in &row[..end] {
                let mut style = Style::colors(cell.fg, bg);
                if cell.bold {
                    style = style.bold();
                }
                if let Some(last) = spans.last_mut() {
                    if last.style == style {
                        last.text.push(cell.ch);
                        continue;
                    }
                }
                spans.push(Span { text: cell.ch.to_string(), style });
            }
            lines.push(spans);
        }
        // Trim trailing empty rows.
        while lines.last().is_some_and(|r| r.is_empty()) {
            lines.pop();
        }
        RenderedDiagram { lines }
    }
}

fn truncate_display(s: &str, max_w: usize) -> String {
    if ui::width(s) <= max_w {
        return s.to_string();
    }
    if max_w <= 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ui::width(ch.encode_utf8(&mut [0; 4])).max(1);
        if w + cw + 1 > max_w {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Flowchart / Graph / State / Relation graph model & layout
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    TopDown,
    BottomTop,
    LeftRight,
    RightLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodeShape {
    Rect,
    Round,
    Stadium,
    Diamond,
    Circle,
    Cylinder,
}

#[derive(Debug, Clone)]
struct Node {
    label: String,
    shape: NodeShape,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
    dashed: bool,
}

struct Graph {
    direction: Direction,
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    edges: Vec<Edge>,
}

impl Graph {
    fn new(direction: Direction) -> Self {
        Self { direction, nodes: Vec::new(), index: HashMap::new(), edges: Vec::new() }
    }

    fn upsert(&mut self, id: &str, label: Option<String>, shape: Option<NodeShape>) -> usize {
        if let Some(&idx) = self.index.get(id) {
            if let Some(l) = label {
                self.nodes[idx].label = l;
            }
            if let Some(s) = shape {
                self.nodes[idx].shape = s;
            }
            idx
        } else {
            let idx = self.nodes.len();
            let lbl = label.unwrap_or_else(|| id.to_string());
            let shp = shape.unwrap_or(NodeShape::Rect);
            self.nodes.push(Node { label: lbl, shape: shp });
            self.index.insert(id.to_string(), idx);
            idx
        }
    }

    fn add_edge(&mut self, from: usize, to: usize, label: Option<String>, dashed: bool) {
        self.edges.push(Edge { from, to, label, dashed });
    }
}

fn parse_direction(header: &str) -> Direction {
    let tokens: Vec<&str> = header.split_whitespace().collect();
    match tokens.get(1).map(|s| s.to_ascii_uppercase()).as_deref() {
        Some("LR") => Direction::LeftRight,
        Some("RL") => Direction::RightLeft,
        Some("BT") => Direction::BottomTop,
        _ => Direction::TopDown,
    }
}

/// Parse a node token like `A[Start]`, `B("Hello World")`, `C{Is it?}`, `D((End))`, or bare `A`.
fn parse_node_token(raw: &str) -> Option<(String, Option<String>, Option<NodeShape>)> {
    let s = raw.trim().trim_end_matches(';').trim();
    if s.is_empty() {
        return None;
    }
    // Find first shape delimiter.
    let delim_pos = s.find(|c: char| matches!(c, '[' | '(' | '{'));
    let Some(pos) = delim_pos else {
        // Bare identifier (strip any :::class suffix).
        let id = s.split(":::").next().unwrap_or(s).trim();
        if id.is_empty() {
            return None;
        }
        return Some((id.to_string(), None, None));
    };
    let id = s[..pos].trim();
    if id.is_empty() {
        return None;
    }
    let rest = s[pos..].split(":::").next().unwrap_or(&s[pos..]).trim();
    let (inner, shape) = if let Some(inner) = rest.strip_prefix("([").and_then(|r| r.strip_suffix("])")) {
        (inner, NodeShape::Stadium)
    } else if let Some(inner) = rest.strip_prefix("((").and_then(|r| r.strip_suffix("))")) {
        (inner, NodeShape::Circle)
    } else if let Some(inner) = rest.strip_prefix("[(").and_then(|r| r.strip_suffix(")]")) {
        (inner, NodeShape::Cylinder)
    } else if let Some(inner) = rest.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
        (inner, NodeShape::Rect)
    } else if let Some(inner) = rest.strip_prefix("{{").and_then(|r| r.strip_suffix("}}")) {
        (inner, NodeShape::Diamond)
    } else if let Some(inner) = rest.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        (inner, NodeShape::Rect)
    } else if let Some(inner) = rest.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        (inner, NodeShape::Round)
    } else if let Some(inner) = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        (inner, NodeShape::Diamond)
    } else {
        return Some((id.to_string(), None, None));
    };
    let clean = inner.trim().trim_matches('"').trim_matches('\'').replace("<br/>", " ").replace("<br>", " ");
    Some((id.to_string(), Some(clean), Some(shape)))
}

/// Find the next edge operator in `s` outside brackets/quotes.
/// Returns `(start_byte, end_byte, optional_label, dashed)`.
fn find_edge_op(s: &str) -> Option<(usize, usize, Option<String>, bool)> {
    let bytes = s.as_bytes();
    let mut depth = 0i32;
    let mut in_quote = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            in_quote = !in_quote;
            i += 1;
            continue;
        }
        if !in_quote {
            if matches!(b, b'[' | b'(' | b'{') {
                depth += 1;
                i += 1;
                continue;
            }
            if matches!(b, b']' | b')' | b'}') {
                depth = (depth - 1).max(0);
                i += 1;
                continue;
            }
            if depth == 0 {
                let tail = &s[i..];
                // Labeled forms: -- text --> or -. text .-> or == text ==>
                if let Some((len, lbl, dashed)) = match_inline_labeled_arrow(tail) {
                    return Some((i, i + len, Some(lbl), dashed));
                }
                // Plain arrows: -.->, ==>, -->, ---, -.->|lbl|, -->|lbl|, ==>|lbl|
                for (op, dashed) in [("-.->", true), ("==>", false), ("-->", false), ("---", false), ("-.-", true)] {
                    if let Some(after) = tail.strip_prefix(op) {
                        let mut end = i + op.len();
                        let mut label = None;
                        let after_ws = after.trim_start();
                        if let Some(rest) = after_ws.strip_prefix('|') {
                            if let Some(close) = rest.find('|') {
                                let lbl = rest[..close].trim();
                                if !lbl.is_empty() {
                                    label = Some(lbl.to_string());
                                }
                                let consumed = (after.len() - after_ws.len()) + 1 + close + 1;
                                end += consumed;
                            }
                        }
                        return Some((i, end, label, dashed));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn match_inline_labeled_arrow(tail: &str) -> Option<(usize, String, bool)> {
    for (prefix, suffix, dashed) in [("-- ", " -->", false), ("-. ", " .->", true), ("== ", " ==>", false)] {
        if let Some(rest) = tail.strip_prefix(prefix) {
            if let Some(pos) = rest.find(suffix) {
                let lbl = rest[..pos].trim();
                if !lbl.is_empty() && !lbl.contains('\n') {
                    return Some((prefix.len() + pos + suffix.len(), lbl.to_string(), dashed));
                }
            }
        }
    }
    None
}

fn render_flowchart(source_lines: &[String], max_width: usize, bg: Rgb, theme: &Theme) -> Option<RenderedDiagram> {
    let header = source_lines.iter().map(|l| l.trim()).find(|l| !l.is_empty() && !l.starts_with("%%"))?;
    let mut graph = Graph::new(parse_direction(header));
    let mut started = false;
    for raw in source_lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !started {
            started = true;
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("subgraph ")
            || lower == "end"
            || lower.starts_with("classdef ")
            || lower.starts_with("class ")
            || lower.starts_with("style ")
            || lower.starts_with("linkstyle ")
            || lower.starts_with("click ")
            || lower.starts_with("direction ")
        {
            continue;
        }
        parse_flowchart_statement(line, &mut graph);
    }
    if graph.nodes.is_empty() {
        return None;
    }
    Some(layout_and_draw_graph(&graph, max_width, bg, theme))
}

fn parse_flowchart_statement(line: &str, graph: &mut Graph) {
    let mut rest = line;
    let mut prev_group: Vec<usize> = Vec::new();
    while let Some((op_start, op_end, label, dashed)) = find_edge_op(rest) {
        let lhs = rest[..op_start].trim();
        let left_ids = if !lhs.is_empty() {
            upsert_group(lhs, graph)
        } else {
            prev_group.clone()
        };
        rest = rest[op_end..].trim();
        // Next segment ends at the next edge op or end of line.
        let rhs_end = find_edge_op(rest).map(|(s, _, _, _)| s).unwrap_or(rest.len());
        let rhs = rest[..rhs_end].trim();
        let right_ids = upsert_group(rhs, graph);
        for &a in &left_ids {
            for &b in &right_ids {
                graph.add_edge(a, b, label.clone(), dashed);
            }
        }
        prev_group = right_ids;
        rest = rest[rhs_end..].trim();
    }
    if prev_group.is_empty() && !rest.is_empty() {
        let _ = upsert_group(rest, graph);
    }
}

fn upsert_group(segment: &str, graph: &mut Graph) -> Vec<usize> {
    let mut out = Vec::new();
    for part in segment.split('&') {
        if let Some((id, label, shape)) = parse_node_token(part) {
            out.push(graph.upsert(&id, label, shape));
        }
    }
    out
}

fn render_state(source_lines: &[String], max_width: usize, bg: Rgb, theme: &Theme) -> Option<RenderedDiagram> {
    let mut graph = Graph::new(Direction::TopDown);
    let mut started = false;
    for raw in source_lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !started {
            started = true;
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("direction lr") {
            graph.direction = Direction::LeftRight;
            continue;
        }
        if lower.starts_with("note ") || lower == "{" || lower == "}" || lower.starts_with("state ") && !line.contains("-->") {
            // Handle `state "Label" as id`
            if let Some(rest) = line.strip_prefix("state ") {
                if let Some((lbl_part, id_part)) = rest.split_once(" as ") {
                    let lbl = lbl_part.trim().trim_matches('"').to_string();
                    let id = id_part.trim();
                    if !id.is_empty() {
                        graph.upsert(id, Some(lbl), Some(NodeShape::Round));
                    }
                }
            }
            continue;
        }
        if let Some((lhs, rhs)) = line.split_once("-->") {
            let from_id = lhs.trim();
            let (to_raw, label) = match rhs.split_once(':') {
                Some((t, l)) => (t.trim(), Some(l.trim().to_string()).filter(|s| !s.is_empty())),
                None => (rhs.trim(), None),
            };
            if from_id.is_empty() || to_raw.is_empty() {
                continue;
            }
            let from_lbl = if from_id == "[*]" { Some("●".to_string()) } else { None };
            let to_lbl = if to_raw == "[*]" { Some("◉".to_string()) } else { None };
            let from_key = if from_id == "[*]" { "__start__" } else { from_id };
            let to_key = if to_raw == "[*]" { "__end__" } else { to_raw };
            let a = graph.upsert(from_key, from_lbl, Some(NodeShape::Round));
            let b = graph.upsert(to_key, to_lbl, Some(NodeShape::Round));
            graph.add_edge(a, b, label, false);
        }
    }
    if graph.nodes.is_empty() {
        return None;
    }
    Some(layout_and_draw_graph(&graph, max_width, bg, theme))
}

fn render_relation_graph(source_lines: &[String], max_width: usize, bg: Rgb, theme: &Theme) -> Option<RenderedDiagram> {
    let mut graph = Graph::new(Direction::TopDown);
    let mut started = false;
    let rel_ops = ["<|--", "--|>", "*--", "--*", "o--", "--o", "..|>", "<|..", "-->", "<--", "..>", "<..", "||--o{", "}o--||", "||--|{", "}|--||", "||--||", "}o--o{"];
    for raw in source_lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !started {
            started = true;
            continue;
        }
        for op in rel_ops {
            if let Some((lhs, rhs)) = line.split_once(op) {
                let left = lhs.trim().trim_matches('"');
                let (right_raw, label) = match rhs.split_once(':') {
                    Some((r, l)) => (r.trim().trim_matches('"'), Some(l.trim().trim_matches('"').to_string()).filter(|s| !s.is_empty())),
                    None => (rhs.trim().trim_matches('"'), None),
                };
                if !left.is_empty() && !right_raw.is_empty() {
                    let a = graph.upsert(left, None, Some(NodeShape::Rect));
                    let b = graph.upsert(right_raw, None, Some(NodeShape::Rect));
                    graph.add_edge(a, b, label, op.contains(".."));
                }
                break;
            }
        }
    }
    if graph.nodes.is_empty() {
        return None;
    }
    Some(layout_and_draw_graph(&graph, max_width, bg, theme))
}

/// Assign topological layers to nodes (cycle-safe BFS/longest-path).
fn assign_layers(graph: &Graph) -> Vec<Vec<usize>> {
    let n = graph.nodes.len();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in &graph.edges {
        if e.from != e.to {
            adj[e.from].push(e.to);
            indeg[e.to] += 1;
        }
    }
    let mut layer = vec![0usize; n];
    let mut queue: VecDeque<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    if queue.is_empty() {
        queue.push_back(0);
    }
    let mut visited = vec![false; n];
    while let Some(u) = queue.pop_front() {
        visited[u] = true;
        for &v in &adj[u] {
            if layer[v] < layer[u] + 1 && !visited[v] {
                layer[v] = layer[u] + 1;
            }
            if indeg[v] > 0 {
                indeg[v] -= 1;
                if indeg[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
    }
    let max_l = layer.iter().copied().max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_l + 1];
    for (idx, &l) in layer.iter().enumerate() {
        layers[l].push(idx);
    }
    if matches!(graph.direction, Direction::BottomTop | Direction::RightLeft) {
        layers.reverse();
    }
    layers.retain(|l| !l.is_empty());
    layers
}

fn box_width(node: &Node, max_box: usize) -> usize {
    let extra = if node.shape == NodeShape::Diamond { 4 } else { 0 };
    (ui::width(&node.label) + extra + 4).clamp(5, max_box.max(7))
}

fn layout_and_draw_graph(graph: &Graph, max_width: usize, bg: Rgb, theme: &Theme) -> RenderedDiagram {
    let layers = assign_layers(graph);
    let border_fg = theme.panel_focused_border;
    let text_fg = theme.text;
    let arrow_fg = theme.text_muted;
    let label_fg = theme.ansi[3]; // yellow/gold for edge labels
    let avail = max_width.max(24);

    match graph.direction {
        Direction::LeftRight | Direction::RightLeft => {
            draw_graph_lr(graph, &layers, avail, bg, border_fg, text_fg, arrow_fg, label_fg)
        }
        Direction::TopDown | Direction::BottomTop => {
            draw_graph_td(graph, &layers, avail, bg, border_fg, text_fg, arrow_fg, label_fg)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_graph_td(
    graph: &Graph,
    layers: &[Vec<usize>],
    avail: usize,
    bg: Rgb,
    border_fg: Rgb,
    text_fg: Rgb,
    arrow_fg: Rgb,
    label_fg: Rgb,
) -> RenderedDiagram {
    let box_h = 3usize;
    let v_gap = 3usize;
    let h_gap = 3usize;
    let mut positions: Vec<(usize, usize, usize)> = vec![(0, 0, 0); graph.nodes.len()]; // (x, y, w)

    let mut total_w = 0usize;
    let mut y = 0usize;
    for row in layers {
        let count = row.len().max(1);
        let max_box = avail.saturating_sub(h_gap * (count - 1)) / count;
        let mut x = 0usize;
        for &idx in row {
            let w = box_width(&graph.nodes[idx], max_box.max(10));
            positions[idx] = (x, y, w);
            x += w + h_gap;
        }
        total_w = total_w.max(x.saturating_sub(h_gap));
        y += box_h + v_gap;
    }
    let total_h = y.saturating_sub(v_gap).max(box_h);
    let mut grid = Grid::new(total_w + 2, total_h, text_fg);

    // Draw edges first so boxes sit cleanly on top.
    for edge in &graph.edges {
        let (fx, fy, fw) = positions[edge.from];
        let (tx, ty, tw) = positions[edge.to];
        let fcx = fx + fw / 2;
        let tcx = tx + tw / 2;
        let line_ch = if edge.dashed { '┆' } else { '│' };
        let hz_ch = if edge.dashed { '┄' } else { '─' };
        if ty > fy + box_h - 1 {
            let start_y = fy + box_h;
            let end_y = ty.saturating_sub(1);
            let mid_y = start_y + (end_y.saturating_sub(start_y)) / 2;
            for cy in start_y..=mid_y {
                grid.set(fcx, cy, line_ch, arrow_fg, false);
            }
            if fcx != tcx {
                let (lo, hi) = if fcx < tcx { (fcx, tcx) } else { (tcx, fcx) };
                for cx in (lo + 1)..hi {
                    grid.set(cx, mid_y, hz_ch, arrow_fg, false);
                }
                grid.set(fcx, mid_y, if fcx < tcx { '└' } else { '┘' }, arrow_fg, false);
                grid.set(tcx, mid_y, if fcx < tcx { '┐' } else { '┌' }, arrow_fg, false);
            }
            for cy in (mid_y + 1)..end_y {
                grid.set(tcx, cy, line_ch, arrow_fg, false);
            }
            grid.set(tcx, end_y, '▼', arrow_fg, false);
        }
    }

    // Draw edge labels in a second pass so subsequent edges never overwrite them.
    for edge in &graph.edges {
        if let Some(lbl) = &edge.label {
            let (fx, fy, fw) = positions[edge.from];
            let (tx, ty, tw) = positions[edge.to];
            if ty > fy + box_h - 1 {
                let start_y = fy + box_h;
                let end_y = ty.saturating_sub(1);
                let mid_y = start_y + (end_y.saturating_sub(start_y)) / 2;
                let fcx = fx + fw / 2;
                let tcx = tx + tw / 2;
                let lx = ((fcx + tcx) / 2).saturating_sub(ui::width(lbl) / 2);
                grid.put_str(lx, mid_y, lbl, label_fg, false);
            }
        }
    }

    // Draw nodes.
    for (idx, node) in graph.nodes.iter().enumerate() {
        let (x, y, w) = positions[idx];
        grid.draw_box(x, y, w, box_h, node.shape, &node.label, border_fg, text_fg);
    }

    grid.into_spans(bg)
}

#[allow(clippy::too_many_arguments)]
fn draw_graph_lr(
    graph: &Graph,
    layers: &[Vec<usize>],
    avail: usize,
    bg: Rgb,
    border_fg: Rgb,
    text_fg: Rgb,
    arrow_fg: Rgb,
    label_fg: Rgb,
) -> RenderedDiagram {
    let box_h = 3usize;
    let v_gap = 1usize;
    let col_count = layers.len().max(1);
    let h_gap = 5usize;
    let max_box = avail.saturating_sub(h_gap * (col_count - 1)) / col_count;

    let mut positions: Vec<(usize, usize, usize)> = vec![(0, 0, 0); graph.nodes.len()];
    let mut x = 0usize;
    let mut total_h = 0usize;
    for col in layers {
        let col_w = col.iter().map(|&idx| box_width(&graph.nodes[idx], max_box.max(10))).max().unwrap_or(8);
        let mut y = 0usize;
        for &idx in col {
            let w = box_width(&graph.nodes[idx], max_box.max(10));
            positions[idx] = (x, y, w);
            y += box_h + v_gap;
        }
        total_h = total_h.max(y.saturating_sub(v_gap));
        x += col_w + h_gap;
    }
    let total_w = x.saturating_sub(h_gap);
    let mut grid = Grid::new(total_w + 2, total_h.max(box_h), text_fg);

    for edge in &graph.edges {
        let (fx, fy, fw) = positions[edge.from];
        let (tx, ty, _tw) = positions[edge.to];
        let fcy = fy + box_h / 2;
        let tcy = ty + box_h / 2;
        let hz_ch = if edge.dashed { '┄' } else { '─' };
        let vt_ch = if edge.dashed { '┆' } else { '│' };
        if tx > fx + fw {
            let sx = fx + fw;
            let ex = tx.saturating_sub(1);
            let mx = sx + (ex.saturating_sub(sx)) / 2;
            for cx in sx..mx {
                grid.set(cx, fcy, hz_ch, arrow_fg, false);
            }
            if fcy != tcy {
                let (lo, hi) = if fcy < tcy { (fcy, tcy) } else { (tcy, fcy) };
                for cy in (lo + 1)..hi {
                    grid.set(mx, cy, vt_ch, arrow_fg, false);
                }
                grid.set(mx, fcy, if fcy < tcy { '┐' } else { '┘' }, arrow_fg, false);
                grid.set(mx, tcy, if fcy < tcy { '└' } else { '┌' }, arrow_fg, false);
            } else {
                grid.set(mx, fcy, hz_ch, arrow_fg, false);
            }
            for cx in (mx + 1)..ex {
                grid.set(cx, tcy, hz_ch, arrow_fg, false);
            }
            grid.set(ex, tcy, '▶', arrow_fg, false);
            if let Some(lbl) = &edge.label {
                let ly = fcy.saturating_sub(1);
                grid.put_str(sx + 1, ly, lbl, label_fg, false);
            }
        }
    }

    for (idx, node) in graph.nodes.iter().enumerate() {
        let (x, y, w) = positions[idx];
        grid.draw_box(x, y, w, box_h, node.shape, &node.label, border_fg, text_fg);
    }

    grid.into_spans(bg)
}

// ---------------------------------------------------------------------------
// Sequence diagrams
// ---------------------------------------------------------------------------

struct SeqMessage {
    from: usize,
    to: usize,
    label: String,
    dashed: bool,
}

fn render_sequence(source_lines: &[String], max_width: usize, bg: Rgb, theme: &Theme) -> Option<RenderedDiagram> {
    let mut participants: Vec<(String, String)> = Vec::new(); // (id, label)
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut messages: Vec<SeqMessage> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut ensure_part = |id: &str, label: Option<&str>| -> usize {
        if let Some(&idx) = index.get(id) {
            if let Some(l) = label {
                participants[idx].1 = l.to_string();
            }
            idx
        } else {
            let idx = participants.len();
            participants.push((id.to_string(), label.unwrap_or(id).to_string()));
            index.insert(id.to_string(), idx);
            seen.insert(id.to_string());
            idx
        }
    };

    let arrows = ["-->>", "->>", "-->", "->", "--x", "-x", "--)", "-)"];
    let mut started = false;
    for raw in source_lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        if !started {
            started = true;
            continue;
        }
        if let Some(rest) = line.strip_prefix("participant ").or_else(|| line.strip_prefix("actor ")) {
            if let Some((id, lbl)) = rest.split_once(" as ") {
                ensure_part(id.trim(), Some(lbl.trim()));
            } else {
                ensure_part(rest.trim(), None);
            }
            continue;
        }
        for op in arrows {
            if let Some((lhs, rhs)) = line.split_once(op) {
                let from = lhs.trim();
                let (to, msg) = match rhs.split_once(':') {
                    Some((t, m)) => (t.trim().trim_start_matches(['+', '-']), m.trim()),
                    None => (rhs.trim().trim_start_matches(['+', '-']), ""),
                };
                if !from.is_empty() && !to.is_empty() {
                    let a = ensure_part(from, None);
                    let b = ensure_part(to, None);
                    messages.push(SeqMessage {
                        from: a,
                        to: b,
                        label: msg.to_string(),
                        dashed: op.starts_with("--"),
                    });
                }
                break;
            }
        }
    }

    if participants.is_empty() {
        return None;
    }

    let n = participants.len();
    let avail = max_width.max(32);
    let col_gap = 4usize;
    let max_box = avail.saturating_sub(col_gap * n.saturating_sub(1)) / n;
    let mut xs: Vec<(usize, usize)> = Vec::with_capacity(n); // (x, w)
    let mut cur_x = 0usize;
    for (_, lbl) in &participants {
        let w = (ui::width(lbl) + 4).clamp(6, max_box.max(8));
        xs.push((cur_x, w));
        cur_x += w + col_gap;
    }
    let total_w = cur_x.saturating_sub(col_gap);
    let total_h = 3 + messages.len() * 2 + 1;
    let border_fg = theme.panel_focused_border;
    let text_fg = theme.text;
    let arrow_fg = theme.text_muted;
    let label_fg = theme.ansi[3];

    let mut grid = Grid::new(total_w + 2, total_h, text_fg);

    // Draw lifelines.
    for &(x, w) in &xs {
        let cx = x + w / 2;
        for y in 3..total_h {
            grid.set(cx, y, '│', arrow_fg, false);
        }
    }

    // Draw participant header boxes.
    for (i, (_, lbl)) in participants.iter().enumerate() {
        let (x, w) = xs[i];
        grid.draw_box(x, 0, w, 3, NodeShape::Rect, lbl, border_fg, text_fg);
    }

    // Draw messages.
    for (mi, msg) in messages.iter().enumerate() {
        let y_lbl = 3 + mi * 2;
        let y_arr = y_lbl + 1;
        let fc = xs[msg.from].0 + xs[msg.from].1 / 2;
        let tc = xs[msg.to].0 + xs[msg.to].1 / 2;
        let hz = if msg.dashed { '┄' } else { '─' };
        if fc == tc {
            grid.put_str(fc + 1, y_lbl, &msg.label, label_fg, false);
            grid.put_str(fc, y_arr, "╰─▶", arrow_fg, false);
        } else if fc < tc {
            let span = tc.saturating_sub(fc + 1);
            let lbl = truncate_display(&msg.label, span.max(4));
            let lx = fc + 1 + span.saturating_sub(ui::width(&lbl)) / 2;
            grid.put_str(lx, y_lbl, &lbl, label_fg, false);
            for cx in (fc + 1)..tc.saturating_sub(1) {
                grid.set(cx, y_arr, hz, arrow_fg, false);
            }
            grid.set(tc.saturating_sub(1), y_arr, '▶', arrow_fg, false);
        } else {
            let span = fc.saturating_sub(tc + 1);
            let lbl = truncate_display(&msg.label, span.max(4));
            let lx = tc + 1 + span.saturating_sub(ui::width(&lbl)) / 2;
            grid.put_str(lx, y_lbl, &lbl, label_fg, false);
            grid.set(tc + 1, y_arr, '◀', arrow_fg, false);
            for cx in (tc + 2)..fc {
                grid.set(cx, y_arr, hz, arrow_fg, false);
            }
        }
    }

    Some(grid.into_spans(bg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_lines(diag: &RenderedDiagram) -> Vec<String> {
        diag.lines
            .iter()
            .map(|row| row.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect()
    }

    #[test]
    fn renders_flowchart_td_with_boxes_and_arrows() {
        let src: Vec<String> = [
            "graph TD",
            "    A[Start] --> B{Check}",
            "    B -->|Yes| C(Done)",
            "    B -.->|No| D[Retry]",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let theme = Theme::builtin();
        let diag = render(&src, 80, theme.editor_bg, &theme).expect("should render flowchart");
        let text = plain_lines(&diag).join("\n");
        assert!(text.contains("Start"), "missing Start:\n{text}");
        assert!(text.contains("Check"), "missing Check:\n{text}");
        assert!(text.contains("Done"), "missing Done:\n{text}");
        assert!(text.contains("Retry"), "missing Retry:\n{text}");
        assert!(text.contains('▼'), "missing down arrow:\n{text}");
        assert!(text.contains("Yes"), "missing edge label Yes:\n{text}");
    }

    #[test]
    fn renders_flowchart_lr_with_right_arrows() {
        let src: Vec<String> = ["flowchart LR", "  A[Input] --> B[Process] --> C[Output]"]
            .into_iter()
            .map(String::from)
            .collect();
        let theme = Theme::builtin();
        let diag = render(&src, 80, theme.editor_bg, &theme).expect("should render LR flowchart");
        let text = plain_lines(&diag).join("\n");
        assert!(text.contains("Input") && text.contains("Process") && text.contains("Output"), ":\n{text}");
        assert!(text.contains('▶'), "missing right arrow:\n{text}");
    }

    #[test]
    fn renders_sequence_diagram() {
        let src: Vec<String> = [
            "sequenceDiagram",
            "  participant U as User",
            "  participant S as Server",
            "  U->>S: Request",
            "  S-->>U: Response",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let theme = Theme::builtin();
        let diag = render(&src, 80, theme.editor_bg, &theme).expect("should render sequence");
        let text = plain_lines(&diag).join("\n");
        assert!(text.contains("User") && text.contains("Server"), ":\n{text}");
        assert!(text.contains("Request") && text.contains("Response"), ":\n{text}");
        assert!(text.contains('▶') && text.contains('◀'), "missing arrows:\n{text}");
    }

    #[test]
    fn renders_state_diagram() {
        let src: Vec<String> = [
            "stateDiagram-v2",
            "  [*] --> Idle",
            "  Idle --> Running : start",
            "  Running --> [*]",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let theme = Theme::builtin();
        let diag = render(&src, 80, theme.editor_bg, &theme).expect("should render state");
        let text = plain_lines(&diag).join("\n");
        assert!(text.contains("Idle") && text.contains("Running") && text.contains("start"), ":\n{text}");
    }

    #[test]
    fn returns_none_for_unsupported_diagram() {
        let src: Vec<String> = ["pie title Pets", "  \"Dogs\" : 386"].into_iter().map(String::from).collect();
        let theme = Theme::builtin();
        assert!(render(&src, 80, theme.editor_bg, &theme).is_none());
    }
}
