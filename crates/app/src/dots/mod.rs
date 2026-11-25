//! Bundled Agent Dots: terminal-styled monitor for herdr AI coding agents.
//!
//! Three coordinated pieces, all cross-platform (macOS and Linux):
//! - [`DotsWatcher`]: background poller over herdr's unix socket, feeding the inline status
//!   strip inside bakedit (when `"agent_dots": true`).
//! - [`overlay::DotsOverlay`]: floating terminal-style bar at the bottom-center of the screen
//!   that appears when you leave the bakedit window (`FocusLost`) and hides when you return
//!   (`FocusGained`).
//! - [`tui::run`]: standalone `bakedit --dots` terminal TUI (works on macOS, Linux, and SSH).

pub mod overlay;
pub mod tui;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bakedit_core::Theme;
use bakedit_herdr::{list_agents, AgentInfo, AgentStatus, Client};
use ratatui::layout::Rect;

use crate::ui::{self, Canvas, Style};

/// How often the watcher polls herdr for agent states.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Live snapshot of herdr agents for rendering inside bakedit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DotsSnapshot {
    pub agents: Vec<AgentInfo>,
    pub unread: HashSet<String>,
    pub offline: bool,
}

/// Background poller that tracks herdr agents and unread completions without blocking the UI.
pub struct DotsWatcher {
    state: Arc<Mutex<DotsSnapshot>>,
    dirty: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DotsWatcher {
    /// Start polling herdr on a background thread, calling `wake` whenever the snapshot changes.
    pub fn start(wake: impl Fn() + Send + 'static) -> DotsWatcher {
        let state = Arc::new(Mutex::new(DotsSnapshot::default()));
        let dirty = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));

        let state_bg = Arc::clone(&state);
        let dirty_bg = Arc::clone(&dirty);
        let stop_bg = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("bakedit-dots".into())
            .spawn(move || {
                let client = Client::from_env_or_default();
                let mut prev: HashMap<String, AgentInfo> = HashMap::new();
                let mut seeded = false;
                let mut unread: HashSet<String> = HashSet::new();

                while !stop_bg.load(Ordering::Relaxed) {
                    let (agents, offline) = match list_agents(&client) {
                        Ok(list) => (list, false),
                        Err(_) => (Vec::new(), true),
                    };

                    if seeded {
                        for a in &agents {
                            let old = prev.get(&a.pane_id);
                            if a.status == AgentStatus::Working
                                || (a.focused && old.is_some_and(|o| !o.focused))
                            {
                                unread.remove(&a.pane_id);
                            }
                            if let Some(old) = old {
                                let completed = a.completion_seq > old.completion_seq && a.completion_seq > 0;
                                let finished = old.status == AgentStatus::Working
                                    && matches!(a.status, AgentStatus::Idle | AgentStatus::Done);
                                let blocked = old.status != AgentStatus::Blocked
                                    && a.status == AgentStatus::Blocked;
                                if (completed || finished || blocked) && !a.focused {
                                    unread.insert(a.pane_id.clone());
                                }
                            }
                        }
                        let live: HashSet<&str> = agents.iter().map(|a| a.pane_id.as_str()).collect();
                        unread.retain(|id| live.contains(id.as_str()));
                    }

                    prev.clear();
                    for a in &agents {
                        prev.insert(a.pane_id.clone(), a.clone());
                    }
                    if !offline {
                        seeded = true;
                    }

                    let next = DotsSnapshot { agents, unread: unread.clone(), offline };
                    let changed = {
                        let mut guard = state_bg.lock().unwrap_or_else(|e| e.into_inner());
                        if *guard != next {
                            *guard = next;
                            true
                        } else {
                            false
                        }
                    };
                    if changed {
                        dirty_bg.store(true, Ordering::Release);
                        wake();
                    }

                    let deadline = Instant::now() + POLL_INTERVAL;
                    while Instant::now() < deadline && !stop_bg.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(120));
                    }
                }
            })
            .ok();

        DotsWatcher { state, dirty, stop, handle }
    }

    /// Take the latest snapshot if it changed since the last poll.
    pub fn poll(&self) -> Option<DotsSnapshot> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return None;
        }
        Some(self.state.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }
}

impl Drop for DotsWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.take();
    }
}

/// Glyph and theme colour for an agent status pill.
pub fn status_glyph_and_color(
    status: AgentStatus,
    unread: bool,
    pulse_on: bool,
    theme: &Theme,
) -> (&'static str, bakedit_core::Rgb) {
    if unread {
        let color = if pulse_on { theme.ansi[5] } else { theme.warning };
        return ("◆", color);
    }
    match status {
        AgentStatus::Working => (if pulse_on { "◐" } else { "◓" }, theme.warning),
        AgentStatus::Blocked => ("▲", theme.error),
        AgentStatus::Idle | AgentStatus::Done => ("●", theme.success),
        AgentStatus::Unknown => ("○", theme.text_muted),
    }
}

/// Columns [`render_inline_strip`] would use within `max_width`.
pub fn inline_width(snapshot: &DotsSnapshot, max_width: u16, pulse_on: bool, theme: &Theme) -> u16 {
    if max_width < 10 || snapshot.offline || snapshot.agents.is_empty() {
        return 0;
    }
    let mut x = 0u16;
    for (idx, agent) in snapshot.agents.iter().enumerate() {
        let unread = snapshot.unread.contains(&agent.pane_id);
        let (glyph, _) = status_glyph_and_color(agent.status, unread, pulse_on, theme);
        let label = format!(" {glyph} {} ", agent.display_agent());
        let w = ui::width(&label) as u16;
        if x + w > max_width {
            break;
        }
        x += w;
        if idx + 1 < snapshot.agents.len() && x + 1 < max_width {
            x += 1;
        }
    }
    x
}

/// Draw a compact inline Agent Dots summary into a status-bar slice, returning columns used.
pub fn render_inline_strip(
    canvas: &mut Canvas<'_>,
    area: Rect,
    snapshot: &DotsSnapshot,
    pulse_on: bool,
    theme: &Theme,
) -> u16 {
    if area.width < 10 || area.height == 0 || snapshot.offline || snapshot.agents.is_empty() {
        return 0;
    }
    let mut x = area.x;
    let max_x = area.x + area.width;
    let bg = theme.status_bar_bg;
    let sep_style = Style::colors(theme.border, bg);

    for (idx, agent) in snapshot.agents.iter().enumerate() {
        let unread = snapshot.unread.contains(&agent.pane_id);
        let (glyph, color) = status_glyph_and_color(agent.status, unread, pulse_on, theme);
        let name = agent.display_label(14);
        let label = format!(" {glyph} {name} ");
        let w = ui::width(&label) as u16;
        if x + w > max_x {
            break;
        }
        let pill_bg = if unread {
            color.over(bg, 45)
        } else if agent.focused {
            theme.element_selected
        } else {
            bg
        };
        canvas.text(x, area.y, " ", Style::colors(theme.text, pill_bg), 1);
        let gw = ui::width(glyph) as u16;
        canvas.text(x + 1, area.y, glyph, Style::colors(color, pill_bg).bold(), gw);
        canvas.text(
            x + 1 + gw,
            area.y,
            &format!(" {name} "),
            Style::colors(if unread || agent.focused { theme.text } else { theme.text_muted }, pill_bg),
            w.saturating_sub(1 + gw),
        );
        x += w;
        if idx + 1 < snapshot.agents.len() && x + 1 < max_x {
            canvas.text(x, area.y, "│", sep_style, 1);
            x += 1;
        }
    }
    x - area.x
}
