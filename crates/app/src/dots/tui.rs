//! Standalone terminal-style Agent Dots TUI (`bakedit --dots`, cross-platform macOS & Linux)
//! and the live terminal visualization used on Onboarding Page 1.

use std::collections::{HashMap, HashSet};
use std::io::{self, BufWriter, Stdout, Write};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use bakedit_core::{InputEvent, Key, KeyEvent, KeyKind, Mods, Rgb, Theme};
use bakedit_herdr::{
    focus_agent, list_agents, prompt_agent, read_agent_output, read_claude_history,
    send_agent_keys, split_output_and_status, AgentInfo, AgentStatus, Client,
};
use bakedit_input::{host as seq, Parser};
use crossbeam_channel::{unbounded, RecvTimeoutError};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;

use crate::dots::status_glyph_and_color;
use crate::host::{self, Host, HostEvent};
use crate::settings;
use crate::themes;
use crate::ui::{self, Canvas, Style};

/// One styled span produced by parsing ANSI SGR escape sequences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiSpan {
    pub text: String,
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Parse one line of ANSI-escaped terminal output into styled spans using `theme`'s palette.
pub fn parse_ansi_line(line: &str, theme: &Theme) -> Vec<AnsiSpan> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut fg: Option<Rgb> = None;
    let mut bg: Option<Rgb> = None;
    let mut bold = false;
    let mut dim = false;
    let mut italic = false;
    let mut underline = false;

    let flush = |buf: &mut String,
                 spans: &mut Vec<AnsiSpan>,
                 fg: Option<Rgb>,
                 bg: Option<Rgb>,
                 bold: bool,
                 dim: bool,
                 italic: bool,
                 underline: bool| {
        if !buf.is_empty() {
            spans.push(AnsiSpan {
                text: std::mem::take(buf),
                fg,
                bg,
                bold,
                dim,
                italic,
                underline,
            });
        }
    };

    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            if bytes[i + 1] == b'[' {
                flush(&mut buf, &mut spans, fg, bg, bold, dim, italic, underline);
                i += 2;
                let start = i;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                let final_byte = bytes.get(i).copied().unwrap_or(0);
                let params = &line[start..i.min(bytes.len())];
                if i < bytes.len() {
                    i += 1;
                }
                if final_byte == b'm' {
                    let codes: Vec<u16> = if params.is_empty() {
                        vec![0]
                    } else {
                        params.split(';').map(|p| p.parse::<u16>().unwrap_or(0)).collect()
                    };
                    let mut idx = 0;
                    while idx < codes.len() {
                        match codes[idx] {
                            0 => {
                                fg = None;
                                bg = None;
                                bold = false;
                                dim = false;
                                italic = false;
                                underline = false;
                            }
                            1 => bold = true,
                            2 => dim = true,
                            3 => italic = true,
                            4 => underline = true,
                            22 => {
                                bold = false;
                                dim = false;
                            }
                            23 => italic = false,
                            24 => underline = false,
                            30..=37 => fg = Some(theme.ansi[(codes[idx] - 30) as usize]),
                            39 => fg = None,
                            40..=47 => bg = Some(theme.ansi[(codes[idx] - 40) as usize]),
                            49 => bg = None,
                            90..=97 => fg = Some(theme.ansi[(codes[idx] - 90 + 8) as usize]),
                            100..=107 => bg = Some(theme.ansi[(codes[idx] - 100 + 8) as usize]),
                            38 => {
                                if codes.get(idx + 1) == Some(&5) {
                                    if let Some(&n) = codes.get(idx + 2) {
                                        fg = Some(color_256(n as u8, theme));
                                        idx += 2;
                                    }
                                } else if codes.get(idx + 1) == Some(&2) && idx + 4 < codes.len() {
                                    fg = Some(Rgb::new(codes[idx + 2] as u8, codes[idx + 3] as u8, codes[idx + 4] as u8));
                                    idx += 4;
                                }
                            }
                            48 => {
                                if codes.get(idx + 1) == Some(&5) {
                                    if let Some(&n) = codes.get(idx + 2) {
                                        bg = Some(color_256(n as u8, theme));
                                        idx += 2;
                                    }
                                } else if codes.get(idx + 1) == Some(&2) && idx + 4 < codes.len() {
                                    bg = Some(Rgb::new(codes[idx + 2] as u8, codes[idx + 3] as u8, codes[idx + 4] as u8));
                                    idx += 4;
                                }
                            }
                            _ => {}
                        }
                        idx += 1;
                    }
                }
                continue;
            } else if bytes[i + 1] == b']' {
                i += 2;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
        }
        let ch = line[i..].chars().next().unwrap();
        buf.push(ch);
        i += ch.len_utf8();
    }
    flush(&mut buf, &mut spans, fg, bg, bold, dim, italic, underline);
    spans
}

fn color_256(n: u8, theme: &Theme) -> Rgb {
    if n < 16 {
        return theme.ansi[n as usize];
    }
    if n < 232 {
        let idx = n - 16;
        let levels: [u8; 6] = [0, 95, 135, 175, 215, 255];
        return Rgb::new(levels[(idx / 36) as usize], levels[((idx % 36) / 6) as usize], levels[(idx % 6) as usize]);
    }
    let v = 8 + (n - 232) * 10;
    Rgb::new(v, v, v)
}

/// Draw a terminal-style Agent Dots visualization (used on Onboarding Page 1).
pub fn render_preview(canvas: &mut Canvas<'_>, area: Rect, theme: &Theme, pulse_on: bool) {
    if area.width < 34 || area.height < 8 {
        return;
    }
    let bg = theme.surface;
    let border_style = Style::colors(theme.border, bg);
    let accent_style = Style::colors(theme.panel_focused_border, bg);
    let muted = Style::colors(theme.text_muted, bg);
    let text = Style::colors(theme.text, bg);

    // Mini drawer preview box (top portion)
    let drawer_h = area.height.saturating_sub(4).min(8);
    let drawer_w = area.width.min(66);
    let dx = area.x + (area.width.saturating_sub(drawer_w)) / 2;
    let dy = area.y;

    canvas.fill(Rect::new(dx, dy, drawer_w, drawer_h), bg);
    canvas.box_border(Rect::new(dx, dy, drawer_w, drawer_h), border_style);

    if drawer_h >= 6 {
        // Header row inside mini drawer
        canvas.text(dx + 2, dy + 1, "●", Style::colors(theme.success, bg).bold(), u16::MAX);
        canvas.text(dx + 4, dy + 1, "agy · w3:p1", text.bold(), u16::MAX);
        canvas.text(dx + 17, dy + 1, "[Idle]", Style::colors(theme.success, bg), u16::MAX);
        let right_hint = "Esc:close  ↓:bar";
        if drawer_w > 38 {
            canvas.text(dx + drawer_w - ui::width(right_hint) as u16 - 2, dy + 1, right_hint, muted, u16::MAX);
        }
        // Sample ANSI-coloured transcript lines
        if drawer_h >= 7 {
            canvas.text(dx + 2, dy + 2, "─".repeat((drawer_w - 4) as usize).as_str(), border_style, u16::MAX);
            canvas.text(dx + 2, dy + 3, "• Built ", text, u16::MAX);
            canvas.text(dx + 10, dy + 3, "Agent Dots", Style::colors(theme.ansi[4], bg).bold(), u16::MAX);
            canvas.text(dx + 20, dy + 3, " — tests passing (242 ok)", text, u16::MAX);
            canvas.text(dx + 2, dy + 4, "[starbase]", Style::colors(theme.success, bg).bold(), u16::MAX);
            canvas.text(dx + 13, dy + 4, "Gemini 4 · ctx 82% left", Style::colors(theme.ansi[6], bg), u16::MAX);
        }
        // Mini composer line
        let comp_y = dy + drawer_h - 2;
        canvas.text(dx + 2, comp_y, "> ", accent_style.bold(), u16::MAX);
        canvas.text(dx + 4, comp_y, "Send message to agy (w3:p1)...", muted, u16::MAX);
    }

    // Bottom-center floating terminal-style Agent Dots bar preview
    let bar_y = dy + drawer_h + 1;
    if bar_y + 3 <= area.y + area.height {
        let bar_w = area.width.min(62);
        let bx = area.x + (area.width.saturating_sub(bar_w)) / 2;
        canvas.fill(Rect::new(bx, bar_y, bar_w, 3), bg);
        canvas.box_border(Rect::new(bx, bar_y, bar_w, 3), accent_style);

        let unread_color = if pulse_on { theme.ansi[5] } else { theme.warning };
        let mut x = bx + 2;
        let pills: &[(&str, &str, Rgb, bool)] = &[
            ("◆", "agy·p1", unread_color, true),
            ("◐", "claude·p2", theme.warning, false),
            ("●", "agy·p3", theme.success, false),
            ("▲", "claude·p4", theme.error, false),
        ];
        for (idx, &(glyph, name, color, unread)) in pills.iter().enumerate() {
            let pill = format!(" {glyph} {name} ");
            let pw = ui::width(&pill) as u16;
            if x + pw + 1 >= bx + bar_w {
                break;
            }
            let pbg = if unread { color.over(bg, 50) } else { theme.elevated_surface };
            canvas.text(x, bar_y + 1, " ", Style::colors(theme.text, pbg), u16::MAX);
            canvas.text(x + 1, bar_y + 1, glyph, Style::colors(color, pbg).bold(), u16::MAX);
            canvas.text(
                x + 1 + ui::width(glyph) as u16,
                bar_y + 1,
                &format!(" {name} "),
                Style::colors(if unread { theme.text } else { theme.text_muted }, pbg), u16::MAX);
            x += pw;
            if idx + 1 < pills.len() && x + 1 < bx + bar_w - 1 {
                canvas.text(x, bar_y + 1, " ", Style::colors(theme.text, bg), u16::MAX);
                x += 1;
            }
        }
    }
}

/// State of the standalone `bakedit --dots` TUI.
pub struct DotsTui {
    pub agents: Vec<AgentInfo>,
    pub unread: HashSet<String>,
    pub selected_idx: usize,
    pub drawer_open: bool,
    pub output_lines: Vec<String>,
    pub status_lines: Vec<String>,
    pub scroll_from_bottom: usize,
    pub auto_scroll: bool,
    pub draft: String,
    pub banner: Option<(bool, String)>,
    pub pulse_on: bool,
    pub offline: bool,
    prev_agents: HashMap<String, AgentInfo>,
    seeded: bool,
}

impl Default for DotsTui {
    fn default() -> DotsTui {
        DotsTui {
            agents: Vec::new(),
            unread: HashSet::new(),
            selected_idx: 0,
            drawer_open: false,
            output_lines: Vec::new(),
            status_lines: Vec::new(),
            scroll_from_bottom: 0,
            auto_scroll: true,
            draft: String::new(),
            banner: None,
            pulse_on: false,
            offline: false,
            prev_agents: HashMap::new(),
            seeded: false,
        }
    }
}

impl DotsTui {
    /// Currently highlighted agent, if any.
    pub fn selected(&self) -> Option<&AgentInfo> {
        self.agents.get(self.selected_idx)
    }

    /// Update the agent list and unread transitions.
    pub fn apply_agents(&mut self, agents: Vec<AgentInfo>, offline: bool) {
        self.offline = offline;
        if self.seeded {
            for a in &agents {
                let old = self.prev_agents.get(&a.pane_id);
                if a.status == AgentStatus::Working
                    || (a.focused && old.is_some_and(|o| !o.focused))
                {
                    self.unread.remove(&a.pane_id);
                }
                if let Some(old) = old {
                    let completed = a.completion_seq > old.completion_seq && a.completion_seq > 0;
                    let finished = old.status == AgentStatus::Working
                        && matches!(a.status, AgentStatus::Idle | AgentStatus::Done);
                    let blocked = old.status != AgentStatus::Blocked && a.status == AgentStatus::Blocked;
                    let viewing = self.drawer_open
                        && self.selected().is_some_and(|s| s.pane_id == a.pane_id);
                    if (completed || finished || blocked) && !a.focused && !viewing {
                        self.unread.insert(a.pane_id.clone());
                    }
                }
            }
        }
        self.prev_agents.clear();
        for a in &agents {
            self.prev_agents.insert(a.pane_id.clone(), a.clone());
        }
        if !offline {
            self.seeded = true;
        }
        let prev_pane = self.selected().map(|a| a.pane_id.clone());
        self.agents = agents;
        if let Some(id) = prev_pane {
            if let Some(pos) = self.agents.iter().position(|a| a.pane_id == id) {
                self.selected_idx = pos;
            } else if !self.agents.is_empty() {
                self.selected_idx = self.selected_idx.min(self.agents.len() - 1);
            } else {
                self.selected_idx = 0;
            }
        }
    }

    /// Refresh the selected agent's transcript and status footer from herdr (plus Claude JSONL).
    pub fn refresh_selected_output(&mut self, client: &Client) {
        let Some(agent) = self.selected().cloned() else { return };
        if let Ok(raw) = read_agent_output(client, &agent.pane_id, 160, true) {
            let (mut body, status) = split_output_and_status(&raw);
            if agent.agent == "claude" {
                if let Some(sid) = agent.session_id.as_deref() {
                    if let Some(hist) = read_claude_history(sid, 35) {
                        if matches!(agent.status, AgentStatus::Working | AgentStatus::Blocked) {
                            body = format!("{hist}\n\n\x1b[2;33m── live pane ──\x1b[0m\n{body}");
                        } else {
                            body = hist;
                        }
                    }
                }
            }
            self.output_lines = body.lines().map(str::to_string).collect();
            self.status_lines = status;
            if self.auto_scroll {
                self.scroll_from_bottom = 0;
            }
        }
    }

    /// Render the TUI into `canvas`.
    pub fn render(&self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        canvas.fill(area, theme.background);
        let bar_h = 3u16.min(area.height);
        let bar_y = area.y + area.height.saturating_sub(bar_h);

        if self.drawer_open && area.height > 7 {
            let drawer_area = Rect::new(area.x, area.y, area.width, bar_y.saturating_sub(area.y));
            self.render_drawer(canvas, drawer_area, theme);
        }
        self.render_bar(canvas, Rect::new(area.x, bar_y, area.width, bar_h), theme);
    }

    fn render_bar(&self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        if area.height == 0 || area.width < 10 {
            return;
        }
        let bg = theme.surface;
        canvas.fill(area, bg);
        if area.height >= 3 {
            canvas.box_border(
                area,
                Style::colors(if self.drawer_open { theme.border } else { theme.panel_focused_border }, bg),
            );
        }
        let y = if area.height >= 3 { area.y + 1 } else { area.y };
        let inner_x = area.x + 2;
        let max_x = area.x + area.width.saturating_sub(2);

        if self.offline {
            canvas.text(inner_x, y, "○ herdr offline — press q to quit", Style::colors(theme.text_muted, bg), u16::MAX);
            return;
        }
        if self.agents.is_empty() {
            canvas.text(inner_x, y, "○ No herdr agents running — press q to quit", Style::colors(theme.text_muted, bg), u16::MAX);
            return;
        }

        let mut x = inner_x;
        for (idx, agent) in self.agents.iter().enumerate() {
            let unread = self.unread.contains(&agent.pane_id);
            let selected = idx == self.selected_idx;
            let (glyph, color) = status_glyph_and_color(agent.status, unread, self.pulse_on, theme);
            let label = format!(" {glyph} {}·{} ", agent.display_agent(), agent.pane_id);
            let w = ui::width(&label) as u16;
            if x + w > max_x {
                break;
            }
            let pill_bg = if selected {
                theme.element_selected
            } else if unread {
                color.over(bg, 48)
            } else {
                theme.elevated_surface
            };
            let fg = if selected || unread { theme.text } else { theme.text_muted };
            canvas.text(x, y, " ", Style::colors(fg, pill_bg), u16::MAX);
            canvas.text(x + 1, y, glyph, Style::colors(color, pill_bg).bold(), u16::MAX);
            canvas.text(
                x + 1 + ui::width(glyph) as u16,
                y,
                &format!(" {}·{} ", agent.display_agent(), agent.pane_id),
                if selected { Style::colors(fg, pill_bg).bold() } else { Style::colors(fg, pill_bg) }, u16::MAX);
            x += w + 1;
        }

        let hint = if self.drawer_open { "Esc:close ↓:bar" } else { "←/→:pick ↑/Enter:chat q:quit" };
        let hw = ui::width(hint) as u16;
        if max_x > x + hw + 1 {
            canvas.text(max_x - hw, y, hint, Style::colors(theme.text_muted, bg), u16::MAX);
        }
    }

    fn render_drawer(&self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 6 {
            return;
        }
        let bg = theme.background;
        canvas.fill(area, bg);
        canvas.box_border(area, Style::colors(theme.panel_focused_border, bg));

        // Header
        let header_y = area.y + 1;
        if let Some(agent) = self.selected() {
            let (glyph, color) = status_glyph_and_color(agent.status, false, self.pulse_on, theme);
            canvas.text(area.x + 2, header_y, glyph, Style::colors(color, bg).bold(), u16::MAX);
            let title = format!(" {} · {}  ({})  [{}]", agent.display_agent(), agent.pane_id, agent.short_dir(), agent.status.label());
            canvas.text(area.x + 3, header_y, &ui::truncate_end(&title, area.width.saturating_sub(24) as usize), Style::colors(theme.text, bg).bold(), u16::MAX);
        }
        let scroll_hint = "Shift+↑/↓:scroll  Ctrl+F:focus";
        let shw = ui::width(scroll_hint) as u16;
        if area.width > shw + 26 {
            canvas.text(area.x + area.width - shw - 2, header_y, scroll_hint, Style::colors(theme.text_muted, bg), u16::MAX);
        }

        // Layout rows inside drawer:
        // header (1) + divider (1) + output (flex) + status (0..2) + composer (1)
        let status_h = self.status_lines.len().min(2) as u16;
        let composer_y = area.y + area.height - 2;
        let status_y = composer_y.saturating_sub(status_h);
        let body_top = header_y + 2;
        let body_h = status_y.saturating_sub(body_top) as usize;

        canvas.text(area.x + 1, header_y + 1, &"─".repeat(area.width.saturating_sub(2) as usize), Style::colors(theme.border, bg), u16::MAX);

        // Transcript body (ANSI coloured)
        let total = self.output_lines.len();
        let max_scroll = total.saturating_sub(body_h);
        let scroll_off = self.scroll_from_bottom.min(max_scroll);
        let end = total.saturating_sub(scroll_off);
        let start = end.saturating_sub(body_h);
        let inner_w = area.width.saturating_sub(4) as usize;

        for (row, line) in self.output_lines[start..end].iter().enumerate() {
            let y = body_top + row as u16;
            let mut x = area.x + 2;
            let mut used = 0usize;
            for span in parse_ansi_line(line, theme) {
                if used >= inner_w {
                    break;
                }
                let clipped = ui::truncate_end(&span.text, inner_w - used);
                let w = ui::width(&clipped);
                let mut st = Style::colors(span.fg.unwrap_or(theme.text), span.bg.unwrap_or(bg));
                if span.bold {
                    st = st.bold();
                }
                if span.dim {
                    st = st.dim();
                }
                if span.italic {
                    st = st.italic();
                }
                if span.underline {
                    st = st.underline();
                }
                canvas.text(x, y, &clipped, st, u16::MAX);
                x += w as u16;
                used += w;
            }
        }

        // Extracted status lines (ANSI coloured)
        for (i, line) in self.status_lines.iter().take(status_h as usize).enumerate() {
            let y = status_y + i as u16;
            canvas.fill(Rect::new(area.x + 1, y, area.width - 2, 1), theme.surface);
            let mut x = area.x + 2;
            let mut used = 0usize;
            for span in parse_ansi_line(line, theme) {
                if used >= inner_w {
                    break;
                }
                let clipped = ui::truncate_end(&span.text, inner_w - used);
                let w = ui::width(&clipped);
                let mut st = Style::colors(span.fg.unwrap_or(theme.text_muted), theme.surface);
                if span.bold {
                    st = st.bold();
                }
                canvas.text(x, y, &clipped, st, u16::MAX);
                x += w as u16;
                used += w;
            }
        }

        // Composer line
        canvas.fill(Rect::new(area.x + 1, composer_y, area.width - 2, 1), theme.elevated_surface);
        canvas.text(area.x + 2, composer_y, "> ", Style::colors(theme.panel_focused_border, theme.elevated_surface).bold(), u16::MAX);
        let prompt_w = area.width.saturating_sub(6) as usize;
        if self.draft.is_empty() {
            let placeholder = if let Some((is_err, msg)) = &self.banner {
                let c = if *is_err { theme.error } else { theme.success };
                canvas.text(area.x + 4, composer_y, &ui::truncate_end(msg, prompt_w), Style::colors(c, theme.elevated_surface), u16::MAX);
                None
            } else {
                Some("Type a message and press Enter (↓ returns to bar)...")
            };
            if let Some(ph) = placeholder {
                canvas.text(area.x + 4, composer_y, &ui::truncate_end(ph, prompt_w), Style::colors(theme.text_muted, theme.elevated_surface), u16::MAX);
            }
        } else {
            // Keep the tail visible so the word being typed is never clipped off-screen.
            let tail = tail_fit(&self.draft, prompt_w);
            canvas.text(area.x + 4, composer_y, tail, Style::colors(theme.text, theme.elevated_surface), u16::MAX);
        }
    }
}

fn tail_fit(s: &str, max_cols: usize) -> &str {
    if ui::width(s) <= max_cols {
        return s;
    }
    let mut start = s.len();
    let mut cols = 0usize;
    for (idx, ch) in s.char_indices().rev() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + w > max_cols {
            break;
        }
        cols += w;
        start = idx;
    }
    &s[start..]
}

/// Run the standalone terminal-style Agent Dots TUI (`bakedit --dots`).
pub fn run() -> anyhow::Result<()> {
    host::install_panic_hook();
    let loaded = settings::load();
    let (theme, _) = themes::load(&loaded.settings);
    let _host = Host::enter(host::editor_options()).context("could not enter terminal raw mode")?;
    let backend = CrosstermBackend::new(BufWriter::with_capacity(1 << 15, io::stdout()));
    let mut screen: ratatui::Terminal<CrosstermBackend<BufWriter<Stdout>>> =
        ratatui::Terminal::new(backend).context("could not initialize terminal")?;

    let (tx, rx) = unbounded::<HostEvent>();
    {
        let tx = tx.clone();
        host::spawn_reader(move |ev| {
            let _ = tx.send(ev);
        })?;
    }
    {
        let tx = tx.clone();
        host::spawn_signal_watcher(move |ev| {
            let _ = tx.send(ev);
        })?;
    }

    let client = Client::from_env_or_default().allow_live_mutations(true);
    let mut tui = DotsTui::default();
    let mut parser = Parser::new();
    let mut last_poll = Instant::now() - Duration::from_secs(10);
    let mut last_pulse = Instant::now();
    let mut dirty = true;

    loop {
        let now = Instant::now();
        if now.duration_since(last_poll) >= Duration::from_millis(1500) {
            last_poll = now;
            match list_agents(&client) {
                Ok(list) => tui.apply_agents(list, false),
                Err(_) => tui.apply_agents(Vec::new(), true),
            }
            if tui.drawer_open {
                tui.refresh_selected_output(&client);
            }
            dirty = true;
        }
        if now.duration_since(last_pulse) >= Duration::from_millis(550) {
            last_pulse = now;
            tui.pulse_on = !tui.pulse_on;
            dirty = true;
        }
        if dirty {
            dirty = false;
            let _ = screen.backend_mut().write_all(seq::SYNC_BEGIN.as_bytes());
            screen.draw(|frame| {
                let area = frame.area();
                let mut canvas = Canvas::new(frame.buffer_mut(), area);
                tui.render(&mut canvas, area, &theme);
            })?;
            let _ = screen.backend_mut().write_all(seq::SYNC_END.as_bytes());
            let _ = screen.backend_mut().flush();
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(HostEvent::Terminate | HostEvent::Closed) => break,
            Ok(HostEvent::Resized) => dirty = true,
            Ok(HostEvent::Bytes(bytes)) => {
                for ev in parser.feed(&bytes) {
                    match ev {
                        InputEvent::Paste(text) if tui.drawer_open => {
                            tui.draft.push_str(&text);
                            dirty = true;
                        }
                        InputEvent::Key(key) if key.kind != KeyKind::Release => {
                            if handle_tui_key(&mut tui, &key, &client) {
                                return Ok(());
                            }
                            dirty = true;
                        }
                        _ => {}
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Handle one keypress in `bakedit --dots`. Returns `true` when the user quits.
fn handle_tui_key(tui: &mut DotsTui, key: &KeyEvent, client: &Client) -> bool {
    if key.key == Key::Char('c') && key.mods == Mods::CTRL {
        return true;
    }
    if !tui.drawer_open {
        match (key.key, key.mods) {
            (Key::Char('q'), m) if m.is_empty() => return true,
            (Key::Escape, _) => return true,
            (Key::Left, _) | (Key::Char('h'), _) => {
                if !tui.agents.is_empty() {
                    tui.selected_idx = (tui.selected_idx + tui.agents.len() - 1) % tui.agents.len();
                }
            }
            (Key::Right, _) | (Key::Char('l'), _) | (Key::Tab, _) => {
                if !tui.agents.is_empty() {
                    tui.selected_idx = (tui.selected_idx + 1) % tui.agents.len();
                }
            }
            (Key::Up, _) | (Key::Enter, _) | (Key::Char(' '), _) => {
                if let Some(a) = tui.selected().cloned() {
                    tui.unread.remove(&a.pane_id);
                    tui.drawer_open = true;
                    tui.auto_scroll = true;
                    tui.scroll_from_bottom = 0;
                    tui.refresh_selected_output(client);
                }
            }
            _ => {}
        }
        return false;
    }

    // Drawer is open
    match (key.key, key.mods) {
        (Key::Escape, _) => {
            tui.drawer_open = false;
        }
        (Key::Down, m) if m.is_empty() => {
            tui.drawer_open = false;
        }
        (Key::Up, m) if m == Mods::SHIFT || m == (Mods::SHIFT | Mods::CMD) => {
            let step = if m.contains(Mods::CMD) { 9 } else { 3 };
            tui.scroll_from_bottom = tui.scroll_from_bottom.saturating_add(step);
            tui.auto_scroll = false;
        }
        (Key::Down, m) if m == Mods::SHIFT || m == (Mods::SHIFT | Mods::CMD) => {
            let step = if m.contains(Mods::CMD) { 9 } else { 3 };
            tui.scroll_from_bottom = tui.scroll_from_bottom.saturating_sub(step);
            if tui.scroll_from_bottom == 0 {
                tui.auto_scroll = true;
            }
        }
        (Key::Char('f'), m) if m == Mods::CTRL => {
            if let Some(a) = tui.selected() {
                let _ = focus_agent(client, &a.pane_id);
            }
        }
        (Key::Char('v'), m) if m == Mods::CTRL || m == Mods::CMD => {
            if let Some(path) = save_clipboard_image() {
                if !tui.draft.is_empty() && !tui.draft.ends_with(' ') && !tui.draft.ends_with('\n') {
                    tui.draft.push(' ');
                }
                tui.draft.push_str(&path);
                tui.draft.push(' ');
                tui.banner = Some((false, format!("Attached image: {path}")));
            } else if let Some(clip) = crate::clipboard::Clipboard::new().get() {
                tui.draft.push_str(&clip.text);
                tui.banner = None;
            }
        }
        (Key::Enter, m) if m.is_empty() => {
            let text = tui.draft.trim().to_string();
            if let Some(a) = tui.selected().cloned() {
                if text.is_empty() && a.status == AgentStatus::Blocked {
                    let _ = send_agent_keys(client, &a.pane_id, &["Enter"]);
                    tui.banner = Some((false, "Approved (Enter)".into()));
                } else if !text.is_empty() {
                    match prompt_agent(client, &a.pane_id, &text) {
                        Ok(()) => {
                            tui.draft.clear();
                            tui.auto_scroll = true;
                            tui.scroll_from_bottom = 0;
                            tui.banner = Some((false, format!("Sent to {}", a.pane_id)));
                        }
                        Err(e) => tui.banner = Some((true, e.to_string())),
                    }
                }
            }
        }
        (Key::Backspace, _) => {
            tui.draft.pop();
        }
        (Key::Char(c), m) if m.is_empty() || m == Mods::SHIFT => {
            tui.draft.push(c);
            tui.banner = None;
        }
        _ => {}
    }
    false
}

/// If the system clipboard holds an image (and no plain text), save it to a temp PNG and
/// return its path so the agent can read it.
fn save_clipboard_image() -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis();
    let path = format!("/tmp/bakedit-dots-paste-{stamp:x}.png");

    #[cfg(target_os = "macos")]
    {
        let script = r#"use framework "AppKit"
on run argv
    set outPath to item 1 of argv
    set pb to current application's NSPasteboard's generalPasteboard()
    set types to pb's types()
    if (types's containsObject:(current application's NSPasteboardTypeString)) as boolean then return "no"
    set hasPng to (types's containsObject:(current application's NSPasteboardTypePNG)) as boolean
    set hasTiff to (types's containsObject:(current application's NSPasteboardTypeTIFF)) as boolean
    if not hasPng and not hasTiff then return "no"
    set img to current application's NSImage's alloc()'s initWithPasteboard:pb
    if img is missing value then return "no"
    set tiff to img's TIFFRepresentation()
    if tiff is missing value then return "no"
    set rep to current application's NSBitmapImageRep's imageRepWithData:tiff
    if rep is missing value then return "no"
    set png to rep's representationUsingType:(current application's NSBitmapImageFileTypePNG) |properties|:(missing value)
    if png is missing value then return "no"
    png's writeToFile:outPath atomically:true
    return "ok"
end run"#;
        let out = Command::new("osascript")
            .args(["-e", script, "--", &path])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == "ok" {
            return Some(path);
        }
        None
    }

    #[cfg(not(target_os = "macos"))]
    {
        let png = Command::new("wl-paste")
            .args(["--no-newline", "--type", "image/png"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success() && !o.stdout.is_empty())
            .or_else(|| {
                Command::new("xclip")
                    .args(["-selection", "clipboard", "-t", "image/png", "-o"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                    .ok()
                    .filter(|o| o.status.success() && !o.stdout.is_empty())
            })?;
        std::fs::write(&path, &png.stdout).ok()?;
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn parses_ansi_colors_into_spans() {
        let theme = Theme::builtin();
        let spans = parse_ansi_line("hi \x1b[1;32mok\x1b[0m!", &theme);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text, "hi ");
        assert_eq!(spans[1].text, "ok");
        assert!(spans[1].bold);
        assert_eq!(spans[1].fg, Some(theme.ansi[2]));
        assert_eq!(spans[2].text, "!");
    }

    #[test]
    fn renders_preview_cleanly() {
        let theme = Theme::builtin();
        let area = Rect::new(0, 0, 72, 13);
        let mut buf = Buffer::empty(area);
        let mut canvas = Canvas::new(&mut buf, area);
        render_preview(&mut canvas, area, &theme, true);
        let text = crate::dump::to_text(&buf);
        assert!(text.contains("agy · w3:p1"));
        assert!(text.contains("claude·p2"));
    }

    #[test]
    fn tail_fit_keeps_end_of_long_draft_visible() {
        assert_eq!(tail_fit("hello world", 5), "world");
        assert_eq!(tail_fit("short", 10), "short");
    }
}
