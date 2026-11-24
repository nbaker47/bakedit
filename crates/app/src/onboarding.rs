//! First-launch & command-palette onboarding wizard (3 pages) plus the standalone scrollable
//! Keymaps & Nav Layer guide.
//!
//! Pages of [`OnboardingModal`]:
//! 1. **Agent Dots** — terminal-styled visualization, explanation, and `[x] Enable Agent Dots` toggle.
//! 2. **Keymaps & Nav Layer** — scrollable keyboard diagram + navigation commands (also openable
//!    directly via `bakedit::ShowKeymapsGuide`).
//! 3. **Theme** — live-preview selector for the four built-in themes in order:
//!    `Ghostty` (default), `Terminal` (follow terminal), `Catppuccin Mocha`, and `Light`.

use std::path::Path;

use bakedit_core::{Key, KeyEvent, KeyKind, MouseEvent, MouseKind, Rgb, Theme};
use ratatui::layout::Rect;

use crate::dots::tui as dots_tui;
use crate::themes::BUILTIN_CHOICES;
use crate::ui::{self, Canvas, Style};

/// One styled line in the scrollable Keymaps & Nav Layer guide.
#[derive(Debug, Clone)]
struct GuideLine {
    spans: Vec<(String, GuideTone)>,
}

#[derive(Debug, Clone, Copy)]
enum GuideTone {
    Text,
    Muted,
    Heading,
    Accent,
    Key,
    Success,
    Warning,
}

impl GuideLine {
    fn plain(text: impl Into<String>, tone: GuideTone) -> GuideLine {
        GuideLine { spans: vec![(text.into(), tone)] }
    }

    fn blank() -> GuideLine {
        GuideLine { spans: Vec::new() }
    }

    fn pair(key: impl Into<String>, desc: impl Into<String>) -> GuideLine {
        let k = key.into();
        let pad = 26usize.saturating_sub(ui::width(&k));
        GuideLine {
            spans: vec![
                ("  ".into(), GuideTone::Text),
                (k, GuideTone::Key),
                (" ".repeat(pad.max(2)), GuideTone::Text),
                (desc.into(), GuideTone::Text),
            ],
        }
    }
}

/// Scrollable Keymaps & Nav Layer reference, used both standalone (`bakedit::ShowKeymapsGuide`)
/// and as Page 2 of the onboarding wizard.
#[derive(Debug, Clone)]
pub struct KeymapsGuide {
    lines: Vec<GuideLine>,
    pub scroll: usize,
    last_body_height: usize,
}

impl KeymapsGuide {
    /// Build the guide, adapting shortcut labels when `cmd_keys` is false (legacy terminals).
    pub fn new(_cmd_keys: bool) -> KeymapsGuide {
        let mut lines = Vec::new();
        lines.push(GuideLine::plain("HOLD-; NAV LAYER (arrows & modifiers on home row)", GuideTone::Heading));
        lines.push(GuideLine::plain("  Hold ';' with your right pinky so your hands never leave home row:", GuideTone::Muted));
        lines.push(GuideLine::blank());
        lines.push(GuideLine {
            spans: vec![
                ("    LEFT HAND (Arrows)               RIGHT HAND (Modifiers + Trigger)".into(), GuideTone::Accent),
            ],
        });
        lines.push(GuideLine::plain("   ┌───┬───┬───┬───┐                ┌───┬───┬───┬───┬───┐", GuideTone::Muted));
        lines.push(GuideLine {
            spans: vec![
                ("   │   │ ".into(), GuideTone::Muted),
                ("E".into(), GuideTone::Key),
                (" │   │   │  ".into(), GuideTone::Muted),
                ("E = ↑".into(), GuideTone::Success),
                ("          │ ".into(), GuideTone::Muted),
                ("H".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("J".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("K".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("L".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                (";".into(), GuideTone::Warning),
                (" │".into(), GuideTone::Muted),
            ],
        });
        lines.push(GuideLine::plain("   ├───┼───┼───┼───┤                └───┴───┴───┴───┴───┘", GuideTone::Muted));
        lines.push(GuideLine {
            spans: vec![
                ("   │ ".into(), GuideTone::Muted),
                ("S".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("D".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("F".into(), GuideTone::Key),
                (" │   │  ".into(), GuideTone::Muted),
                ("S/D/F = ← ↓ →".into(), GuideTone::Success),
                ("    Ctrl Shf Cmd Opt ".into(), GuideTone::Muted),
                ("HOLD".into(), GuideTone::Warning),
            ],
        });
        lines.push(GuideLine::plain("   └───┴───┴───┴───┘", GuideTone::Muted));
        lines.push(GuideLine::blank());
        lines.push(GuideLine::pair("; + e / s / d / f", "Move cursor up / left / down / right"));
        lines.push(GuideLine::pair("; + j + e / s / d / f", "Select text while moving (Shift + arrows)"));
        lines.push(GuideLine::pair("; + h + e / s / d / f", "Jump 5 lines or to line start / end (Ctrl + arrows)"));
        lines.push(GuideLine::pair("; + l + s / f", "Jump word left / word right (Option + arrows)"));
        lines.push(GuideLine::pair("; + h + l + arrows", "Move focus between herdr panes (Ctrl+Opt+arrows)"));
        lines.push(GuideLine::pair("; + h + j + l + arrows", "Swap / rearrange herdr panes (Ctrl+Opt+Shift+arrows)"));
        lines.push(GuideLine::pair("ctrl-alt-space", "Focus Agent Dots floating bar (also ctrl-alt-shift-space)"));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("HOLD-SPACE NUMBER LAYER (1–0 on home row)", GuideTone::Heading));
        lines.push(GuideLine::plain("  Hold Space (≥160ms) to turn home row into 1–0 (Shift → ! @ # $ % ^ & * ( )):", GuideTone::Muted));
        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("   ┌───┬───┬───┬───┬───┬───┬───┬───┬───┬───┐", GuideTone::Muted));
        lines.push(GuideLine {
            spans: vec![
                ("   │ ".into(), GuideTone::Muted),
                ("A".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("S".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("D".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("F".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("G".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("H".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("J".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("K".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                ("L".into(), GuideTone::Key),
                (" │ ".into(), GuideTone::Muted),
                (";".into(), GuideTone::Key),
                (" │".into(), GuideTone::Muted),
            ],
        });
        lines.push(GuideLine {
            spans: vec![
                ("   │ ".into(), GuideTone::Muted),
                ("1".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("2".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("3".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("4".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("5".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("6".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("7".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("8".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("9".into(), GuideTone::Success),
                (" │ ".into(), GuideTone::Muted),
                ("0".into(), GuideTone::Success),
                (" │".into(), GuideTone::Muted),
            ],
        });
        lines.push(GuideLine::plain("   └───┴───┴───┴───┴───┴───┴───┴───┴───┴───┘", GuideTone::Muted));
        lines.push(GuideLine {
            spans: vec![
                ("             ┌───────────────────┐".into(), GuideTone::Muted),
            ],
        });
        lines.push(GuideLine {
            spans: vec![
                ("             │    ".into(), GuideTone::Muted),
                ("HOLD SPACE".into(), GuideTone::Warning),
                ("     │  ".into(), GuideTone::Muted),
                ("quick tap still types space".into(), GuideTone::Muted),
            ],
        });
        lines.push(GuideLine::plain("             └───────────────────┘", GuideTone::Muted));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("NAV MODE vs EDIT MODE", GuideTone::Heading));
        lines.push(GuideLine::pair("ctrl-alt-i / ctrl-shift-i", "Toggle between Nav mode (default) and Edit mode"));
        lines.push(GuideLine::pair("Esc", "Return to Nav mode (or close active modal)"));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("FILES, SEARCH & COMMANDS", GuideTone::Heading));
        lines.push(GuideLine::pair("ctrl-p", "File finder (fuzzy open, works from any panel)"));
        lines.push(GuideLine::pair("ctrl-shift-p / ctrl-alt-p", "Command palette (run any command)"));
        lines.push(GuideLine::pair("ctrl-l", "Go to line number (negative counts from end)"));
        lines.push(GuideLine::pair("ctrl-alt-f", "Project search across all files (herdr search in Terminal)"));
        lines.push(GuideLine::pair("ctrl-alt-h", "Project search & replace across all files"));
        lines.push(GuideLine::pair("ctrl-f", "Find in current file"));
        lines.push(GuideLine::pair("ctrl-s / ctrl-alt-s", "Save file / Save all"));
        lines.push(GuideLine::pair("ctrl-n / ctrl-alt-n", "New file / New folder in file tree"));
        lines.push(GuideLine::pair("ctrl-k ( [ { \" ' ` <", "Select between brackets or quotes"));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("PANELS & DOCKS", GuideTone::Heading));
        lines.push(GuideLine::pair("ctrl-j / ctrl-alt-j", "Toggle terminal panel (herdr) / Terminal fullscreen"));
        lines.push(GuideLine::pair("ctrl-g / ctrl-alt-g", "Toggle git panel / Project diff fullscreen"));
        lines.push(GuideLine::pair("ctrl-alt-e", "Focus / reveal file explorer (second press collapses)"));
        lines.push(GuideLine::pair("ctrl-e", "Focus centre code editor"));
        lines.push(GuideLine::pair("ctrl-alt-z", "Toggle all docks / fullscreen focused pane"));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("HERDR PANES & WORKSPACES", GuideTone::Heading));
        lines.push(GuideLine::pair("ctrl-alt-arrows", "Focus neighbour pane (hops tab / workspace at edge)"));
        lines.push(GuideLine::pair("ctrl-alt-shift-arrows", "Move / swap focused pane"));
        lines.push(GuideLine::pair("ctrl-alt-z / c / a", "Zoom / collapse / expand focused pane"));
        lines.push(GuideLine::pair("ctrl-alt-1..9", "Switch herdr workspace 1..9 (code tabs outside Terminal)"));
        lines.push(GuideLine::pair("shift-↑/↓", "Scroll focused herdr agent pane up / down"));

        lines.push(GuideLine::blank());
        lines.push(GuideLine::plain("TABS, AGENT DOTS & APPEARANCE", GuideTone::Heading));
        lines.push(GuideLine::pair("ctrl-w / ctrl-k w", "Close current code tab / Close all code tabs"));
        lines.push(GuideLine::pair("ctrl-alt-t", "Reopen closed code tab"));
        lines.push(GuideLine::pair("ctrl-alt-left / right", "Previous / next open code tab (or ctrl-{ / ctrl-})"));
        lines.push(GuideLine::pair("ctrl-alt-space", "Focus Agent Dots bar (←/→ pick · ↑/Enter open · Esc close)"));
        lines.push(GuideLine::pair("Command Palette", "bakedit: onboarding / keymaps guide / select theme / dots"));

        KeymapsGuide { lines, scroll: 0, last_body_height: 16 }
    }

    /// Scroll by `delta` lines (negative = up, positive = down), clamped to content bounds.
    pub fn scroll_by(&mut self, delta: isize) {
        let max_scroll = self.lines.len().saturating_sub(self.last_body_height);
        if delta < 0 {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs());
        } else {
            self.scroll = (self.scroll + delta as usize).min(max_scroll);
        }
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    pub fn scroll_bottom(&mut self) {
        self.scroll = self.lines.len().saturating_sub(self.last_body_height);
    }

    /// Render the scrollable body into `area`.
    pub fn render_body(&mut self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        if area.width < 10 || area.height == 0 {
            return;
        }
        self.last_body_height = area.height as usize;
        let max_scroll = self.lines.len().saturating_sub(self.last_body_height);
        self.scroll = self.scroll.min(max_scroll);

        let bg = theme.surface;
        canvas.fill(area, bg);
        let content_w = area.width.saturating_sub(2) as usize;
        let visible_end = (self.scroll + self.last_body_height).min(self.lines.len());

        for (row, line) in self.lines[self.scroll..visible_end].iter().enumerate() {
            let y = area.y + row as u16;
            let mut x = area.x;
            let mut used = 0usize;
            for (text, tone) in &line.spans {
                if used >= content_w {
                    break;
                }
                let clipped = ui::truncate_end(text, content_w - used);
                let w = ui::width(&clipped);
                let style = tone_style(*tone, bg, theme);
                canvas.text(x, y, &clipped, style, u16::MAX);
                x += w as u16;
                used += w;
            }
        }

        // Scrollbar on rightmost column when content exceeds viewport.
        if self.lines.len() > self.last_body_height && area.width >= 3 {
            let sb_x = area.x + area.width - 1;
            let h = area.height as usize;
            let thumb_h = ((h * h) / self.lines.len()).clamp(1, h);
            let thumb_y = if max_scroll == 0 {
                0
            } else {
                (self.scroll * (h - thumb_h)) / max_scroll
            };
            for r in 0..h {
                let is_thumb = r >= thumb_y && r < thumb_y + thumb_h;
                let ch = if is_thumb { "█" } else { "│" };
                let color = if is_thumb { theme.panel_focused_border } else { theme.border };
                canvas.text(sb_x, area.y + r as u16, ch, Style::colors(color, bg), u16::MAX);
            }
        }
    }

    /// Render as a standalone centered modal (`bakedit::ShowKeymapsGuide`).
    pub fn render_modal(&mut self, canvas: &mut Canvas<'_>, screen: Rect, theme: &Theme) {
        let w = screen.width.saturating_sub(6).min(82).max(40).min(screen.width);
        let h = screen.height.saturating_sub(4).min(34).max(12).min(screen.height);
        let area = Rect::new(
            screen.x + (screen.width.saturating_sub(w)) / 2,
            screen.y + (screen.height.saturating_sub(h)) / 2,
            w,
            h,
        );
        let bg = theme.surface;
        canvas.fill(area, bg);
        canvas.box_border(area, Style::colors(theme.panel_focused_border, bg));

        // Title bar
        canvas.text(
            area.x + 2,
            area.y + 1,
            "Keymaps & Nav Layer Guide",
            Style::colors(theme.text, bg).bold(), u16::MAX);
        let pos = format!("{}/{}", (self.scroll + 1).min(self.lines.len()), self.lines.len());
        let pw = ui::width(&pos) as u16;
        if area.width > pw + 30 {
            canvas.text(area.x + area.width - pw - 2, area.y + 1, &pos, Style::colors(theme.text_muted, bg), u16::MAX);
        }
        canvas.text(
            area.x + 1,
            area.y + 2,
            &"─".repeat(area.width.saturating_sub(2) as usize),
            Style::colors(theme.border, bg), u16::MAX);

        let body_h = area.height.saturating_sub(6);
        self.render_body(canvas, Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(3), body_h), theme);

        // Footer
        let fy = area.y + area.height - 2;
        canvas.text(
            area.x + 1,
            fy - 1,
            &"─".repeat(area.width.saturating_sub(2) as usize),
            Style::colors(theme.border, bg), u16::MAX);
        canvas.text(
            area.x + 2,
            fy,
            &ui::truncate_end("↑/↓ j/k PgUp/PgDn: scroll · Home/End: jump · Esc / q: close", area.width.saturating_sub(4) as usize),
            Style::colors(theme.text_muted, bg), u16::MAX);
    }

    /// Handle a key in the standalone guide modal. Returns `true` to close.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        if key.kind == KeyKind::Release {
            return false;
        }
        match key.key {
            Key::Escape | Key::Char('q') => return true,
            Key::Up | Key::Char('k') => self.scroll_by(-1),
            Key::Down | Key::Char('j') => self.scroll_by(1),
            Key::PageUp => self.scroll_by(-(self.last_body_height.saturating_sub(2) as isize).max(1)),
            Key::PageDown | Key::Char(' ') => self.scroll_by((self.last_body_height.saturating_sub(2) as isize).max(1)),
            Key::Home | Key::Char('g') => self.scroll_top(),
            Key::End | Key::Char('G') => self.scroll_bottom(),
            _ => {}
        }
        false
    }

    /// Handle mouse scroll in the guide.
    pub fn handle_mouse(&mut self, event: &MouseEvent) -> bool {
        match event.kind {
            MouseKind::ScrollUp => {
                self.scroll_by(-3);
                true
            }
            MouseKind::ScrollDown => {
                self.scroll_by(3);
                true
            }
            _ => false,
        }
    }
}

fn tone_style(tone: GuideTone, bg: Rgb, theme: &Theme) -> Style {
    match tone {
        GuideTone::Text => Style::colors(theme.text, bg),
        GuideTone::Muted => Style::colors(theme.text_muted, bg),
        GuideTone::Heading => Style::colors(theme.panel_focused_border, bg).bold(),
        GuideTone::Accent => Style::colors(theme.ansi[6], bg).bold(),
        GuideTone::Key => Style::colors(theme.warning, bg).bold(),
        GuideTone::Success => Style::colors(theme.success, bg).bold(),
        GuideTone::Warning => Style::colors(theme.ansi[5], bg).bold(),
    }
}

/// Which step of the 3-page onboarding wizard is currently visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingStep {
    AgentDots = 0,
    Keymaps = 1,
    Theme = 2,
}

/// What happened when the user pressed a key inside [`OnboardingModal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnboardingOutcome {
    /// Stay open.
    Continue,
    /// The user moved onto a theme on Page 3: preview it live in the shell.
    PreviewTheme(String),
    /// User completed Page 3: persist `agent_dots` and `theme`, mark onboarded, close.
    Finish { agent_dots: bool, theme: String },
    /// User pressed Escape to skip: restore `original_theme`, mark onboarded, close.
    Cancel { restore_theme: Option<String> },
}

/// Interactive 3-page onboarding wizard modal.
#[derive(Debug, Clone)]
pub struct OnboardingModal {
    pub step: OnboardingStep,
    pub agent_dots: bool,
    pub guide: KeymapsGuide,
    pub theme_idx: usize,
    pub original_theme: Option<String>,
    pub pulse_on: bool,
}

impl OnboardingModal {
    pub fn new(current_agent_dots: bool, current_theme: Option<&str>, cmd_keys: bool) -> OnboardingModal {
        let theme_idx = current_theme
            .and_then(|name| BUILTIN_CHOICES.iter().position(|(n, _)| *n == name))
            .unwrap_or(0);
        OnboardingModal {
            step: OnboardingStep::AgentDots,
            agent_dots: current_agent_dots,
            guide: KeymapsGuide::new(cmd_keys),
            theme_idx,
            original_theme: current_theme.map(str::to_string),
            pulse_on: true,
        }
    }

    pub fn selected_theme_name(&self) -> &'static str {
        BUILTIN_CHOICES[self.theme_idx.min(BUILTIN_CHOICES.len() - 1)].0
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> OnboardingOutcome {
        if key.kind == KeyKind::Release {
            return OnboardingOutcome::Continue;
        }
        if key.key == Key::Escape {
            return OnboardingOutcome::Cancel { restore_theme: self.original_theme.clone() };
        }
        match self.step {
            OnboardingStep::AgentDots => match key.key {
                Key::Char(' ') => self.agent_dots = !self.agent_dots,
                Key::Char('y') | Key::Char('Y') => self.agent_dots = true,
                Key::Char('n') | Key::Char('N') => self.agent_dots = false,
                Key::Up | Key::Down => self.agent_dots = !self.agent_dots,
                Key::Enter | Key::Right | Key::Tab => self.step = OnboardingStep::Keymaps,
                _ => {}
            },
            OnboardingStep::Keymaps => match key.key {
                Key::Up | Key::Char('k') => self.guide.scroll_by(-1),
                Key::Down | Key::Char('j') => self.guide.scroll_by(1),
                Key::PageUp => self.guide.scroll_by(-10),
                Key::PageDown => self.guide.scroll_by(10),
                Key::Home => self.guide.scroll_top(),
                Key::End => self.guide.scroll_bottom(),
                Key::Left | Key::Backspace => self.step = OnboardingStep::AgentDots,
                Key::Enter | Key::Right | Key::Tab => {
                    self.step = OnboardingStep::Theme;
                    return OnboardingOutcome::PreviewTheme(self.selected_theme_name().to_string());
                }
                _ => {}
            },
            OnboardingStep::Theme => match key.key {
                Key::Up | Key::Char('k') => {
                    self.theme_idx = (self.theme_idx + BUILTIN_CHOICES.len() - 1) % BUILTIN_CHOICES.len();
                    return OnboardingOutcome::PreviewTheme(self.selected_theme_name().to_string());
                }
                Key::Down | Key::Char('j') | Key::Tab => {
                    self.theme_idx = (self.theme_idx + 1) % BUILTIN_CHOICES.len();
                    return OnboardingOutcome::PreviewTheme(self.selected_theme_name().to_string());
                }
                Key::Left | Key::Backspace => {
                    self.step = OnboardingStep::Keymaps;
                }
                Key::Enter => {
                    return OnboardingOutcome::Finish {
                        agent_dots: self.agent_dots,
                        theme: self.selected_theme_name().to_string(),
                    };
                }
                _ => {}
            },
        }
        OnboardingOutcome::Continue
    }

    pub fn handle_mouse(&mut self, event: &MouseEvent) -> bool {
        if self.step == OnboardingStep::Keymaps {
            return self.guide.handle_mouse(event);
        }
        false
    }

    pub fn render(&mut self, canvas: &mut Canvas<'_>, screen: Rect, theme: &Theme) {
        let w = screen.width.saturating_sub(6).min(84).max(46).min(screen.width);
        let h = screen.height.saturating_sub(4).min(30).max(16).min(screen.height);
        let area = Rect::new(
            screen.x + (screen.width.saturating_sub(w)) / 2,
            screen.y + (screen.height.saturating_sub(h)) / 2,
            w,
            h,
        );
        let bg = theme.surface;
        canvas.fill(area, bg);
        canvas.box_border(area, Style::colors(theme.panel_focused_border, bg));

        // Top step bar
        let steps = [
            (OnboardingStep::AgentDots, "1. Agent Dots"),
            (OnboardingStep::Keymaps, "2. Keymaps & Nav"),
            (OnboardingStep::Theme, "3. Theme"),
        ];
        let mut sx = area.x + 2;
        canvas.text(sx, area.y + 1, "bakedit setup", Style::colors(theme.text, bg).bold(), u16::MAX);
        sx += 15;
        for (i, (st, label)) in steps.iter().enumerate() {
            let active = *st == self.step;
            let done = (*st as usize) < (self.step as usize);
            let pill = format!(" {label} ");
            let pw = ui::width(&pill) as u16;
            if sx + pw + 2 >= area.x + area.width {
                break;
            }
            let style = if active {
                Style::colors(theme.text, theme.element_selected).bold()
            } else if done {
                Style::colors(theme.success, bg)
            } else {
                Style::colors(theme.text_muted, bg)
            };
            canvas.text(sx, area.y + 1, &pill, style, u16::MAX);
            sx += pw;
            if i + 1 < steps.len() && sx + 2 < area.x + area.width {
                canvas.text(sx, area.y + 1, " › ", Style::colors(theme.border, bg), u16::MAX);
                sx += 3;
            }
        }
        canvas.text(
            area.x + 1,
            area.y + 2,
            &"─".repeat(area.width.saturating_sub(2) as usize),
            Style::colors(theme.border, bg), u16::MAX);

        let body = Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), area.height.saturating_sub(6));
        match self.step {
            OnboardingStep::AgentDots => self.render_agent_dots_page(canvas, body, theme),
            OnboardingStep::Keymaps => self.guide.render_body(canvas, body, theme),
            OnboardingStep::Theme => self.render_theme_page(canvas, body, theme),
        }

        // Footer bar
        let fy = area.y + area.height - 2;
        canvas.text(
            area.x + 1,
            fy - 1,
            &"─".repeat(area.width.saturating_sub(2) as usize),
            Style::colors(theme.border, bg), u16::MAX);
        let hint = match self.step {
            OnboardingStep::AgentDots => "Space: toggle Agent Dots · Enter / →: Next · Esc: Skip",
            OnboardingStep::Keymaps => "↑/↓ PgUp/PgDn: scroll · ←: Back · Enter / →: Next · Esc: Skip",
            OnboardingStep::Theme => "↑/↓: preview theme · ←: Back · Enter: Save & Finish · Esc: Cancel",
        };
        canvas.text(
            area.x + 2,
            fy,
            &ui::truncate_end(hint, area.width.saturating_sub(4) as usize),
            Style::colors(theme.text_muted, bg), u16::MAX);
    }

    fn render_agent_dots_page(&self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        let bg = theme.surface;
        let mut y = area.y;
        let max_w = area.width as usize;

        canvas.text(
            area.x,
            y,
            &ui::truncate_end("Do you want Agent Dots bundled with bakedit?", max_w),
            Style::colors(theme.text, bg).bold(), u16::MAX);
        y += 1;
        canvas.text(
            area.x,
            y,
            &ui::truncate_end(
                "Monitors your herdr AI agents (agy, claude, codex) and floats a terminal-style",
                max_w,
            ),
            Style::colors(theme.text_muted, bg), u16::MAX);
        y += 1;
        canvas.text(
            area.x,
            y,
            &ui::truncate_end(
                "bar at the bottom-center of your screen when you leave bakedit (macOS & Linux).",
                max_w,
            ),
            Style::colors(theme.text_muted, bg), u16::MAX);
        y += 1;
        canvas.text(
            area.x,
            y,
            &ui::truncate_end(
                "Focus anywhere with Ctrl+Opt+Space (or Ctrl+Opt+Shift+Space): ←/→ pick · ↑/Enter open · Esc close.",
                max_w,
            ),
            Style::colors(theme.text_accent, bg), u16::MAX);
        y += 2;

        // Interactive toggle row
        let check = if self.agent_dots { "[x]" } else { "[ ]" };
        let toggle_bg = theme.element_selected;
        let toggle_label = format!(" {check} Enable Agent Dots (floating bottom-center bar + inline status strip) ");
        canvas.text(
            area.x,
            y,
            &ui::truncate_end(&toggle_label, max_w),
            Style::colors(if self.agent_dots { theme.success } else { theme.text }, toggle_bg).bold(), u16::MAX);
        y += 2;

        // Live terminal-style Agent Dots visualization
        if y + 8 <= area.y + area.height {
            let preview_area = Rect::new(area.x, y, area.width, (area.y + area.height).saturating_sub(y));
            dots_tui::render_preview(canvas, preview_area, theme, self.pulse_on);
        }
    }

    fn render_theme_page(&self, canvas: &mut Canvas<'_>, area: Rect, theme: &Theme) {
        let bg = theme.surface;
        let mut y = area.y;
        let max_w = area.width as usize;

        canvas.text(
            area.x,
            y,
            &ui::truncate_end("Choose your theme (live preview):", max_w),
            Style::colors(theme.text, bg).bold(), u16::MAX);
        y += 2;

        for (idx, &(name, desc)) in BUILTIN_CHOICES.iter().enumerate() {
            if y >= area.y + area.height {
                break;
            }
            let selected = idx == self.theme_idx;
            let row_bg = if selected { theme.element_selected } else { bg };
            canvas.fill(Rect::new(area.x, y, area.width, 1), row_bg);
            let marker = if selected { "▸ " } else { "  " };
            canvas.text(area.x, y, marker, Style::colors(theme.panel_focused_border, row_bg).bold(), u16::MAX);
            canvas.text(
                area.x + 2,
                y,
                name,
                if selected { Style::colors(theme.text, row_bg).bold() } else { Style::colors(theme.text, row_bg) }, u16::MAX);
            let desc_x = area.x + 22;
            if desc_x < area.x + area.width {
                let avail = (area.x + area.width - desc_x) as usize;
                canvas.text(desc_x, y, &ui::truncate_end(desc, avail), Style::colors(theme.text_muted, row_bg), u16::MAX);
            }
            y += 1;
        }

        y += 1;
        // Live syntax & colour swatch preview card
        if y + 6 <= area.y + area.height {
            let card = Rect::new(area.x, y, area.width, (area.y + area.height - y).min(7));
            canvas.fill(card, theme.editor_bg);
            canvas.box_border(card, Style::colors(theme.border, theme.editor_bg));
            let kw = theme.syntax("keyword").and_then(|s| s.color).unwrap_or(theme.ansi[5]);
            let fn_c = theme.syntax("function").and_then(|s| s.color).unwrap_or(theme.ansi[4]);
            let str_c = theme.syntax("string").and_then(|s| s.color).unwrap_or(theme.ansi[2]);
            let cm_c = theme.syntax("comment").and_then(|s| s.color).unwrap_or(theme.text_muted);

            let cx = card.x + 2;
            canvas.text(cx, card.y + 1, "// Live theme preview", Style::colors(cm_c, theme.editor_bg).italic(), u16::MAX);
            if card.height >= 5 {
                canvas.text(cx, card.y + 2, "fn ", Style::colors(kw, theme.editor_bg).bold(), u16::MAX);
                canvas.text(cx + 3, card.y + 2, "greet", Style::colors(fn_c, theme.editor_bg), u16::MAX);
                canvas.text(cx + 8, card.y + 2, "() { ", Style::colors(theme.editor_fg, theme.editor_bg), u16::MAX);
                canvas.text(cx + 13, card.y + 2, "\"hello bakedit\"", Style::colors(str_c, theme.editor_bg), u16::MAX);
                canvas.text(cx + 28, card.y + 2, " }", Style::colors(theme.editor_fg, theme.editor_bg), u16::MAX);
            }
            if card.height >= 6 {
                let mut sx = cx;
                for (label, col) in [
                    ("● ok", theme.success),
                    ("◐ work", theme.warning),
                    ("▲ err", theme.error),
                    ("◆ accent", theme.panel_focused_border),
                ] {
                    canvas.text(sx, card.y + 4, label, Style::colors(col, theme.editor_bg).bold(), u16::MAX);
                    sx += ui::width(label) as u16 + 3;
                }
            }
        }
    }
}

/// Path to the marker file recording that first-launch onboarding has been shown.
pub fn marker_path(state_dir: &Path) -> std::path::PathBuf {
    state_dir.join("onboarded")
}

/// Whether onboarding should open automatically on startup.
pub fn should_auto_open(state_dir: &Path) -> bool {
    if cfg!(test)
        || std::env::var_os("TERM_PROGRAM").is_some_and(|v| v == "ptyshot")
        || std::env::var_os("BAKEDIT_SKIP_ONBOARDING").is_some()
    {
        return false;
    }
    !marker_path(state_dir).exists()
}

/// Write the `onboarded` marker file so first-launch onboarding won't pop up again.
pub fn mark_onboarded(state_dir: &Path) {
    let path = marker_path(state_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, "1\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use bakedit_core::Mods;
    use ratatui::buffer::Buffer;

    fn press(key: Key) -> KeyEvent {
        KeyEvent::new(key, Mods::empty())
    }

    #[test]
    fn onboarding_walks_all_three_pages_and_saves_choices() {
        let mut modal = OnboardingModal::new(false, Some("Ghostty"), true);
        assert_eq!(modal.step, OnboardingStep::AgentDots);
        assert!(!modal.agent_dots);

        // Toggle Agent Dots on Page 1.
        assert_eq!(modal.handle_key(&press(Key::Char(' '))), OnboardingOutcome::Continue);
        assert!(modal.agent_dots);

        // Advance to Page 2 (Keymaps & Nav).
        assert_eq!(modal.handle_key(&press(Key::Enter)), OnboardingOutcome::Continue);
        assert_eq!(modal.step, OnboardingStep::Keymaps);
        modal.handle_key(&press(Key::Down));
        assert_eq!(modal.guide.scroll, 1);

        // Advance to Page 3 (Theme) -> previews default Ghostty.
        assert_eq!(
            modal.handle_key(&press(Key::Enter)),
            OnboardingOutcome::PreviewTheme("Ghostty".into())
        );
        assert_eq!(modal.step, OnboardingStep::Theme);

        // Down -> Terminal, Down -> Catppuccin Mocha, Down -> Light.
        assert_eq!(
            modal.handle_key(&press(Key::Down)),
            OnboardingOutcome::PreviewTheme("Terminal".into())
        );
        assert_eq!(
            modal.handle_key(&press(Key::Down)),
            OnboardingOutcome::PreviewTheme("Catppuccin Mocha".into())
        );
        assert_eq!(
            modal.handle_key(&press(Key::Down)),
            OnboardingOutcome::PreviewTheme("Light".into())
        );

        // Enter finishes with both choices.
        assert_eq!(
            modal.handle_key(&press(Key::Enter)),
            OnboardingOutcome::Finish { agent_dots: true, theme: "Light".into() }
        );
    }

    #[test]
    fn onboarding_pages_render_without_panicking() {
        let theme = Theme::builtin();
        let area = Rect::new(0, 0, 110, 36);
        let mut modal = OnboardingModal::new(true, Some("Ghostty"), true);

        for step in [OnboardingStep::AgentDots, OnboardingStep::Keymaps, OnboardingStep::Theme] {
            modal.step = step;
            let mut buf = Buffer::empty(area);
            let mut canvas = Canvas::new(&mut buf, area);
            modal.render(&mut canvas, area, &theme);
            let text = crate::dump::to_text(&buf);
            assert!(text.contains("bakedit setup"));
        }
    }

    #[test]
    fn standalone_keymaps_guide_scrolls_and_closes() {
        let theme = Theme::builtin();
        let area = Rect::new(0, 0, 100, 24);
        let mut guide = KeymapsGuide::new(true);
        let mut buf = Buffer::empty(area);
        let mut canvas = Canvas::new(&mut buf, area);
        guide.render_modal(&mut canvas, area, &theme);
        let text = crate::dump::to_text(&buf);
        assert!(text.contains("HOLD-; NAV LAYER"));
        assert!(!guide.handle_key(&press(Key::Down)));
        assert!(guide.scroll > 0);
        assert!(guide.handle_key(&press(Key::Escape)));
    }
}
