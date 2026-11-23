//! The usage strip: one thin row at the bottom showing how much of each AI subscription's
//! quota is used (Claude, Antigravity, Cursor, Codex ...), always visible.
//!
//! The numbers come from tools the user already runs rather than from vendor APIs:
//! `ai-usagebar usage --json` (the "AI Usage" menu-bar app) reports every subscription it is
//! configured for. What it does not report is read elsewhere: Claude's figures from the
//! cache its status line writes, Antigravity's from `antigravity-usage --json` (as the
//! user's own `ai-usage-monitor` does), and Codex's from the rate limits its CLI logs with
//! every session.
//!
//! The strip always shows the four subscriptions the user has (Claude, Antigravity, Cursor,
//! Codex), in that order: one with no numbers shows a dash rather than disappearing.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bakedit_core::{Rgb, Theme};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::ui::{self, Canvas, Style};

/// How often the numbers are refreshed.
pub const REFRESH: Duration = Duration::from_secs(90);
const TOOL_TIMEOUT: Duration = Duration::from_secs(25);

/// One quota window of one subscription.
#[derive(Debug, Clone, PartialEq)]
pub struct Meter {
    /// Short window name: "5h", "7d", or the vendor's own label when it is neither.
    pub window: String,
    /// 0..=100.
    pub percent: f32,
    /// When the window resets, seconds since the epoch.
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Vendor {
    /// Short name shown in the strip ("cld", "agy", "cur").
    pub short: String,
    pub name: String,
    /// At most one meter per window, the fullest of each.
    pub meters: Vec<Meter>,
    /// Why there are no numbers, if there are none.
    pub error: Option<String>,
}

fn window_name(secs: Option<u64>, label: &str) -> String {
    match secs {
        Some(s) if s <= 6 * 3600 => format!("{}h", (s / 3600).max(1)),
        Some(s) if s <= 36 * 3600 => "1d".to_string(),
        Some(s) if s <= 8 * 86400 => "7d".to_string(),
        Some(_) => "mo".to_string(),
        None => label.split_whitespace().next().unwrap_or("").to_lowercase(),
    }
}

/// Seconds since the epoch for an RFC 3339 UTC timestamp like `2026-11-29T08:00:00.212Z`.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 || bytes[4] != b'-' || bytes[7] != b'-' || (bytes[10] != b'T' && bytes[10] != b' ') {
        return None;
    }
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Offset: Z, or +hh:mm / -hh:mm after optional fractional seconds.
    let rest = &text[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest {
        "Z" | "z" | "" => 0,
        _ => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let hours: i64 = rest.get(1..3)?.parse().ok()?;
            let minutes: i64 = rest.get(4..6).and_then(|m| m.parse().ok()).unwrap_or(0);
            sign * (hours * 3600 + minutes * 60)
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Keep the fullest meter of each window, shortest window first.
fn fullest_per_window(mut meters: Vec<(Option<u64>, Meter)>) -> Vec<Meter> {
    meters.sort_by_key(|meter| meter.0.unwrap_or(u64::MAX));
    let mut out: Vec<Meter> = Vec::new();
    for (_, meter) in meters {
        match out.iter_mut().find(|existing| existing.window == meter.window) {
            Some(existing) if meter.percent > existing.percent => *existing = meter,
            Some(_) => {}
            None => out.push(meter),
        }
    }
    out
}

/// Read the document `ai-usagebar usage --json` prints.
pub fn parse_usagebar(text: &str) -> Option<Vec<Vendor>> {
    let document: Value = serde_json::from_str(text).ok()?;
    let entries = document.get("entries")?.as_array()?;
    let mut vendors = Vec::new();
    for entry in entries {
        let name = entry.get("display_name").and_then(Value::as_str).unwrap_or("?").to_string();
        let short = entry
            .get("short_name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| name.chars().take(3).collect::<String>().to_lowercase());
        let mut meters = Vec::new();
        for metric in entry.get("metrics").and_then(Value::as_array).into_iter().flatten() {
            let Some(percent) = metric.get("percent").and_then(Value::as_f64) else { continue };
            let secs = metric.get("window_secs").and_then(Value::as_u64);
            let label = metric.get("label").and_then(Value::as_str).unwrap_or("");
            meters.push((
                secs,
                Meter {
                    window: window_name(secs, label),
                    percent: percent.clamp(0.0, 100.0) as f32,
                    reset_at: metric.get("reset_at").and_then(Value::as_str).and_then(parse_rfc3339),
                },
            ));
        }
        let error = entry.get("error").and_then(Value::as_str).map(str::to_string);
        vendors.push(Vendor { short, name, meters: fullest_per_window(meters), error });
    }
    Some(vendors)
}

/// Claude's limits from the cache its status line script writes.
pub fn parse_claude_cache(text: &str) -> Option<Vendor> {
    let document: Value = serde_json::from_str(text).ok()?;
    let limits = document.get("rate_limits")?.as_object()?;
    let mut meters = Vec::new();
    for (key, window, secs) in [("five_hour", "5h", 18_000u64), ("seven_day", "7d", 604_800)] {
        let Some(limit) = limits.get(key).filter(|v| v.is_object()) else { continue };
        let Some(percent) = limit.get("used_percentage").and_then(Value::as_f64) else { continue };
        meters.push((
            Some(secs),
            Meter {
                window: window.to_string(),
                percent: percent.clamp(0.0, 100.0) as f32,
                reset_at: limit.get("resets_at").and_then(Value::as_i64),
            },
        ));
    }
    Some(Vendor { short: "cld".into(), name: "Claude".into(), meters: fullest_per_window(meters), error: None })
}

/// Antigravity's quota from `antigravity-usage --json`.
pub fn parse_antigravity(text: &str, now: i64) -> Option<Vendor> {
    let document: Value = serde_json::from_str(text).ok()?;
    let models = document.get("models")?.as_array()?;
    let mut meters = Vec::new();
    for model in models {
        let Some(remaining) = model.get("remainingPercentage").and_then(Value::as_f64) else { continue };
        let reset_at = model.get("timeUntilResetMs").and_then(Value::as_i64).map(|ms| now + ms / 1000);
        meters.push((
            Some(18_000u64),
            Meter { window: "5h".into(), percent: ((1.0 - remaining) * 100.0).clamp(0.0, 100.0) as f32, reset_at },
        ));
    }
    Some(Vendor { short: "agy".into(), name: "Antigravity".into(), meters: fullest_per_window(meters), error: None })
}

/// The subscriptions the strip always shows, in order: (short name, name).
pub const ALWAYS_SHOWN: [(&str, &str); 4] = [("cld", "Claude"), ("agy", "Antigravity"), ("cur", "Cursor"), ("cdx", "Codex")];

/// Whether `vendor` is the subscription called `short` / `name`.
fn is_vendor(vendor: &Vendor, short: &str, name: &str) -> bool {
    vendor.short.eq_ignore_ascii_case(short) || vendor.name.eq_ignore_ascii_case(name)
}

/// The four subscriptions of [`ALWAYS_SHOWN`] first and in that order, each with its numbers
/// from `found` or, when `found` has none for it, with none (drawn as a dash); then any other
/// subscription `found` has numbers for. (ai-usagebar lists every service it knows, set up
/// or not: those without numbers would only be dashes.)
pub fn with_every_vendor(found: Vec<Vendor>) -> Vec<Vendor> {
    let mut rest = found;
    let mut out = Vec::new();
    for (short, name) in ALWAYS_SHOWN {
        match rest.iter().position(|vendor| is_vendor(vendor, short, name)) {
            Some(index) => {
                let mut vendor = rest.remove(index);
                vendor.short = short.to_string();
                out.push(vendor);
            }
            None => out.push(Vendor { short: short.into(), name: name.into(), meters: Vec::new(), error: Some("no numbers".into()) }),
        }
    }
    out.extend(rest.into_iter().filter(|vendor| !vendor.meters.is_empty()));
    out
}

/// Codex's limits from one line of its session log (`~/.codex/sessions/.../rollout-*.jsonl`):
/// the `rate_limits` of a `token_count` event. Windows that have reset since are left out
/// (their use since is unknown); `None` when the line has no limits.
pub fn parse_codex_line(line: &str, now: i64) -> Option<Vendor> {
    let document: Value = serde_json::from_str(line).ok()?;
    let limits = document.get("payload")?.get("rate_limits")?;
    if !limits.is_object() {
        return None;
    }
    let logged_at = document.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339);
    let mut meters = Vec::new();
    for key in ["primary", "secondary"] {
        let Some(limit) = limits.get(key).filter(|v| v.is_object()) else { continue };
        let Some(percent) = limit.get("used_percent").and_then(Value::as_f64) else { continue };
        let minutes = limit.get("window_minutes").and_then(Value::as_u64);
        let reset_at = limit
            .get("resets_at")
            .and_then(Value::as_i64)
            .or_else(|| Some(logged_at? + limit.get("resets_in_seconds").and_then(Value::as_i64)?));
        if reset_at.is_some_and(|at| at <= now) {
            continue;
        }
        let secs = minutes.map(|m| m * 60);
        meters.push((secs, Meter { window: window_name(secs, key), percent: percent.clamp(0.0, 100.0) as f32, reset_at }));
    }
    let error = meters.is_empty().then(|| "no Codex use since its limits last reset".to_string());
    Some(Vendor { short: "cdx".into(), name: "Codex".into(), meters: fullest_per_window(meters), error })
}

/// The newest Codex limits on this machine: the last `rate_limits` line of the newest session
/// logs under `codex_home/sessions` (`YYYY/MM/DD/rollout-*.jsonl`).
pub fn codex_usage(codex_home: &std::path::Path, now: i64) -> Option<Vendor> {
    let sessions = codex_home.join("sessions");
    // Newest first: the folders and files are named by date and time.
    let sorted = |dir: &std::path::Path| -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).map(|read| read.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        entries.sort();
        entries.reverse();
        entries
    };
    let mut logs = Vec::new();
    'walk: for year in sorted(&sessions) {
        for month in sorted(&year) {
            for day in sorted(&month) {
                logs.extend(sorted(&day).into_iter().filter(|p| p.extension().is_some_and(|e| e == "jsonl")));
                if logs.len() >= 3 {
                    break 'walk;
                }
            }
        }
    }
    for log in logs.iter().take(3) {
        let Ok(text) = std::fs::read_to_string(log) else { continue };
        if let Some(vendor) = text.lines().rev().filter(|line| line.contains("\"rate_limits\"")).find_map(|line| parse_codex_line(line, now)) {
            return Some(vendor);
        }
    }
    None
}

fn find_tool(name: &str, extra: &[&str]) -> Option<PathBuf> {
    crate::format::which(name).or_else(|| extra.iter().map(PathBuf::from).find(|p| p.exists()))
}

/// Run a command and return its stdout, giving up after [`TOOL_TIMEOUT`].
fn run(program: &PathBuf, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > TOOL_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    reader.join().ok()
}

fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Collect the current numbers, always for the four subscriptions of [`ALWAYS_SHOWN`].
/// Blocks while the tools run: call it off the UI thread.
pub fn fetch() -> Vec<Vendor> {
    let usagebar = find_tool("ai-usagebar", &["/Applications/AI Usage.app/Contents/MacOS/ai-usagebar"]);
    let mut vendors =
        usagebar.and_then(|tool| run(&tool, &["usage", "--json"])).and_then(|out| parse_usagebar(&out)).unwrap_or_default();
    let has = |vendors: &[Vendor], short: &str, name: &str| vendors.iter().any(|v| is_vendor(v, short, name) && !v.meters.is_empty());
    let home = dirs::home_dir();
    if !has(&vendors, "cld", "Claude") {
        let cache = home.as_ref().map(|home| home.join(".cache/claude-rate-limits.json"));
        if let Some(vendor) = cache.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| parse_claude_cache(&t)) {
            vendors.retain(|v| !is_vendor(v, "cld", "Claude"));
            vendors.push(vendor);
        }
    }
    if !has(&vendors, "agy", "Antigravity") {
        let agy = find_tool("antigravity-usage", &[]);
        if let Some(vendor) = agy.and_then(|tool| run(&tool, &["--json"])).and_then(|out| parse_antigravity(&out, now_unix())) {
            vendors.retain(|v| !is_vendor(v, "agy", "Antigravity"));
            vendors.push(vendor);
        }
    }
    if !has(&vendors, "cdx", "Codex") {
        let codex_home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|home| home.join(".codex")));
        if let Some(vendor) = codex_home.and_then(|dir| codex_usage(&dir, now_unix())) {
            vendors.retain(|v| !is_vendor(v, "cdx", "Codex"));
            vendors.push(vendor);
        }
    }
    with_every_vendor(vendors)
}

/// Refreshes the numbers on a background thread.
pub struct UsageWatcher {
    receiver: std::sync::mpsc::Receiver<Vec<Vendor>>,
    latest: Vec<Vendor>,
}

impl UsageWatcher {
    /// Start refreshing every [`REFRESH`]. `wake` is called when new numbers are ready.
    pub fn start(wake: impl Fn() + Send + 'static) -> UsageWatcher {
        let (sender, receiver) = std::sync::mpsc::channel();
        let _ = std::thread::Builder::new().name("bakedit-usage".into()).spawn(move || loop {
            if sender.send(fetch()).is_err() {
                return;
            }
            wake();
            std::thread::sleep(REFRESH);
        });
        UsageWatcher { receiver, latest: Vec::new() }
    }

    /// The newest numbers. Returns true if they changed since the last call.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(vendors) = self.receiver.try_recv() {
            changed |= vendors != self.latest;
            self.latest = vendors;
        }
        changed
    }

    pub fn vendors(&self) -> &[Vendor] {
        &self.latest
    }
}

/// `2h14m`, `6d5h`, `48m`: the time left until a reset.
pub fn countdown(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (days, hours, minutes) = (seconds / 86_400, (seconds % 86_400) / 3600, (seconds % 3600) / 60);
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else {
        format!("{minutes}m")
    }
}

fn level_color(percent: f32, theme: &Theme) -> Rgb {
    if percent >= 90.0 {
        theme.error
    } else if percent >= 75.0 {
        theme.warning
    } else {
        theme.success
    }
}

/// How much detail fits: everything, then without reset times, then without bars.
#[derive(Clone, Copy, PartialEq)]
enum Detail {
    Full,
    NoCountdown,
    NumbersOnly,
}

fn vendor_width(vendor: &Vendor, detail: Detail, bar: u16, now: i64) -> u16 {
    let mut width = ui::width(&vendor.short) as u16;
    if vendor.meters.is_empty() {
        return width + 2;
    }
    for meter in &vendor.meters {
        // " 5h ▰▰▱▱ 18%"
        width += 1 + ui::width(&meter.window) as u16 + 1;
        if detail != Detail::NumbersOnly {
            width += bar + 1;
        }
        width += format!("{:.0}%", meter.percent).len() as u16;
        if detail == Detail::Full {
            if let Some(reset) = meter.reset_at {
                width += 1 + countdown(reset - now).len() as u16;
            }
        }
    }
    width
}

/// Draw the usage strip in `area` (one row). Vendors that do not fit are dropped from the
/// right after the detail has been reduced as far as it goes.
pub fn render(canvas: &mut Canvas<'_>, area: Rect, vendors: &[Vendor], now: i64, theme: &Theme) {
    let bg = theme.status_bar_bg;
    canvas.fill(area, bg);
    if area.width < 8 || area.height == 0 {
        return;
    }
    let muted = Style::colors(theme.text_muted, bg);
    if vendors.is_empty() {
        canvas.text_truncated(area.x + 1, area.y, "usage: reading the numbers…", muted, area.width - 2);
        return;
    }
    const BAR: u16 = 6;
    const GAP: u16 = 3;
    let available = area.width - 2;
    let total = |detail: Detail| -> u16 {
        vendors.iter().map(|v| vendor_width(v, detail, BAR, now)).sum::<u16>() + GAP * (vendors.len() as u16 - 1)
    };
    let detail = [Detail::Full, Detail::NoCountdown, Detail::NumbersOnly]
        .into_iter()
        .find(|d| total(*d) <= available)
        .unwrap_or(Detail::NumbersOnly);

    let mut x = area.x + 1;
    let right = area.right() - 1;
    for (index, vendor) in vendors.iter().enumerate() {
        let width = vendor_width(vendor, detail, BAR, now);
        if x + width > right {
            break;
        }
        if index > 0 {
            canvas.text(x - 2, area.y, "·", muted, 1);
        }
        x += canvas.text(x, area.y, &vendor.short, Style::colors(theme.text, bg).bold(), width);
        if vendor.meters.is_empty() {
            // No numbers: say so quietly rather than leave a gap that looks like zero.
            canvas.text(x + 1, area.y, "–", muted, 1);
            x += 2;
        }
        for meter in &vendor.meters {
            let color = level_color(meter.percent, theme);
            x += 1;
            x += canvas.text(x, area.y, &meter.window, muted, 4);
            x += 1;
            if detail != Detail::NumbersOnly {
                let filled = ((meter.percent / 100.0) * BAR as f32).round() as u16;
                let filled = if meter.percent > 0.0 { filled.max(1) } else { 0 }.min(BAR);
                for cell in 0..BAR {
                    let (glyph, style) = if cell < filled {
                        ("▰", Style::colors(color, bg))
                    } else {
                        ("▱", Style::colors(theme.text_muted.lerp(bg, 0.5), bg))
                    };
                    canvas.put(x + cell, area.y, glyph, style);
                }
                x += BAR + 1;
            }
            x += canvas.text(x, area.y, &format!("{:.0}%", meter.percent), Style::colors(color, bg), 5);
            if detail == Detail::Full {
                if let Some(reset) = meter.reset_at {
                    x += 1;
                    x += canvas.text(x, area.y, &countdown(reset - now), muted, 8);
                }
            }
        }
        x += GAP;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    const USAGEBAR: &str = r#"{"entries":[
      {"display_name":"Claude","short_name":"cld","status":"ready","error":null,"metrics":[
        {"label":"Weekly (7d)","percent":18,"reset_at":"2026-11-29T08:00:00.212182Z","window_secs":604800},
        {"label":"Fable (7d)","percent":26,"reset_at":"2026-11-29T08:00:00.212310Z","window_secs":604800},
        {"label":"Session (5h)","percent":41,"reset_at":"2026-11-23T05:00:00Z","window_secs":18000}]},
      {"display_name":"Antigravity","short_name":"agy","status":"ready","error":null,"metrics":[
        {"label":"Gemini","percent":1,"reset_at":"2026-11-23T07:07:10Z","window_secs":18000},
        {"label":"Claude & GPT OSS","percent":93,"reset_at":"2026-11-23T07:18:59Z","window_secs":18000}]},
      {"display_name":"Cursor","short_name":"cur","status":"error","error":"credentials error","metrics":[]}
    ],"schema_version":1}"#;

    /// 2026-11-23T02:40:00Z
    const NOW: i64 = 1_795_401_600;

    #[test]
    fn timestamps() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse_rfc3339("2026-11-23T02:40:00Z"), Some(NOW));
        assert_eq!(parse_rfc3339("2026-11-23T02:40:00.123456Z"), Some(NOW));
        assert_eq!(parse_rfc3339("2026-11-23T04:40:00+02:00"), Some(NOW));
        assert_eq!(parse_rfc3339("2026-11-22T19:40:00-07:00"), Some(NOW));
        for bad in ["", "yesterday", "2026-13-01T00:00:00Z", "2026-11-23", "2026-11-23T25:00:00Z"] {
            assert_eq!(parse_rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn reads_the_usagebar_document() {
        let vendors = parse_usagebar(USAGEBAR).unwrap();
        assert_eq!(vendors.len(), 3);
        let claude = &vendors[0];
        assert_eq!(claude.short, "cld");
        // One meter per window, the fullest, shortest window first.
        assert_eq!(claude.meters.len(), 2);
        assert_eq!((claude.meters[0].window.as_str(), claude.meters[0].percent), ("5h", 41.0));
        assert_eq!((claude.meters[1].window.as_str(), claude.meters[1].percent), ("7d", 26.0));
        assert_eq!(claude.meters[1].reset_at, parse_rfc3339("2026-11-29T08:00:00Z"));
        assert_eq!(vendors[1].meters[0].percent, 93.0);
        assert!(vendors[2].meters.is_empty());
        assert_eq!(vendors[2].error.as_deref(), Some("credentials error"));
        assert_eq!(parse_usagebar("not json"), None);
        assert_eq!(parse_usagebar("{}"), None);
    }

    #[test]
    fn fallback_sources() {
        let claude = parse_claude_cache(
            r#"{"rate_limits":{"seven_day":{"used_percentage":17,"resets_at":1795939200},"five_hour":null},"quota":null}"#,
        )
        .unwrap();
        assert_eq!(claude.meters.len(), 1);
        assert_eq!((claude.meters[0].window.as_str(), claude.meters[0].percent), ("7d", 17.0));
        assert_eq!(claude.meters[0].reset_at, Some(1_795_939_200));
        let agy = parse_antigravity(r#"{"models":[{"modelId":"gemini","remainingPercentage":0.75,"timeUntilResetMs":60000}]}"#, NOW).unwrap();
        assert_eq!(agy.meters[0].percent, 25.0);
        assert_eq!(agy.meters[0].reset_at, Some(NOW + 60));
        assert!(parse_claude_cache("{}").is_none());
    }

    #[test]
    fn the_four_subscriptions_are_always_there_in_order() {
        let found = parse_usagebar(USAGEBAR).unwrap();
        let meter = Meter { window: "5h".into(), percent: 3.0, reset_at: None };
        let extra = Vendor { short: "zzz".into(), name: "Other".into(), meters: vec![meter], error: None };
        let unused = Vendor { short: "opr".into(), name: "Not set up".into(), meters: Vec::new(), error: Some("no key".into()) };
        let mut reordered = vec![extra.clone(), found[2].clone(), unused, found[0].clone()];
        reordered.push(found[1].clone());
        let vendors = with_every_vendor(reordered);
        let shorts: Vec<&str> = vendors.iter().map(|v| v.short.as_str()).collect();
        assert_eq!(shorts, ["cld", "agy", "cur", "cdx", "zzz"], "others only with numbers");
        assert_eq!(vendors[0].meters.len(), 2, "Claude keeps its numbers");
        assert!(vendors[3].meters.is_empty(), "Codex, not reported, is there without numbers");
        let (text, _) = draw(&with_every_vendor(Vec::new()), 60);
        assert_eq!(text, " cld – · agy – · cur – · cdx –", "nothing known yet still names all four");
    }

    #[test]
    fn reads_codex_limits_from_its_session_log() {
        let line = r#"{"timestamp":"2026-11-23T02:00:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,
            "rate_limits":{"limit_id":"codex","primary":{"used_percent":12.0,"window_minutes":300,"resets_at":1795410000},
            "secondary":{"used_percent":40.5,"window_minutes":10080,"resets_at":1795900000},"credits":null,"plan_type":null}}}"#
            .replace('\n', "");
        let codex = parse_codex_line(&line, NOW).unwrap();
        assert_eq!(codex.short, "cdx");
        assert_eq!(codex.meters.iter().map(|m| (m.window.as_str(), m.percent)).collect::<Vec<_>>(), [("5h", 12.0), ("7d", 40.5)]);
        // A week later both windows have reset: what was used since is not known.
        let later = parse_codex_line(&line, NOW + 8 * 86_400).unwrap();
        assert!(later.meters.is_empty());
        assert!(later.error.is_some());
        // The older form: seconds to the reset, counted from when the line was logged.
        let old = r#"{"timestamp":"2026-11-23T02:00:00Z","payload":{"rate_limits":{"primary":{"used_percent":5,"window_minutes":300,"resets_in_seconds":7200}}}}"#;
        assert_eq!(parse_codex_line(old, NOW).unwrap().meters[0].reset_at, parse_rfc3339("2026-11-23T04:00:00Z"));
        assert!(parse_codex_line(r#"{"payload":{"type":"token_count"}}"#, NOW).is_none());
        assert!(parse_codex_line("not json", NOW).is_none());
    }

    #[test]
    fn finds_the_newest_codex_session_log() {
        let home = tempfile::tempdir().unwrap();
        let day = |date: &str| {
            let dir = home.path().join("sessions").join(date);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let line = |percent: u32| {
            format!(r#"{{"timestamp":"2026-11-23T02:00:00Z","payload":{{"rate_limits":{{"primary":{{"used_percent":{percent},"window_minutes":300,"resets_at":1795410000}}}}}}}}"#)
        };
        std::fs::write(day("2026/11/20").join("rollout-2026-11-20T10-00-00-a.jsonl"), line(70) + "\n").unwrap();
        std::fs::write(day("2026/11/22").join("rollout-2026-11-22T09-00-00-b.jsonl"), format!("{}\n{{\"other\":1}}\n", line(30))).unwrap();
        // The newest log has no limits yet (a session just started): the one before has them.
        std::fs::write(day("2026/11/22").join("rollout-2026-11-22T11-00-00-c.jsonl"), "{\"type\":\"session_meta\"}\n").unwrap();
        let codex = codex_usage(home.path(), NOW).unwrap();
        assert_eq!(codex.meters[0].percent, 30.0);
        assert!(codex_usage(&home.path().join("missing"), NOW).is_none());
    }

    #[test]
    fn countdowns() {
        assert_eq!(countdown(45 * 60), "45m");
        assert_eq!(countdown(2 * 3600 + 14 * 60), "2h14m");
        assert_eq!(countdown(2 * 3600 + 4 * 60), "2h04m");
        assert_eq!(countdown(6 * 86_400 + 5 * 3600), "6d5h");
        assert_eq!(countdown(-30), "0m");
    }

    fn draw(vendors: &[Vendor], width: u16) -> (String, Buffer) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        let theme = Theme::builtin();
        {
            let mut canvas = Canvas::new(&mut buf, area);
            render(&mut canvas, area, vendors, NOW, &theme);
        }
        (crate::dump::to_text(&buf).trim_end().to_string(), buf)
    }

    #[test]
    fn draws_meters_with_levels() {
        let vendors = parse_usagebar(USAGEBAR).unwrap();
        let (text, buf) = draw(&vendors, 140);
        assert_eq!(text, " cld 5h ▰▰▱▱▱▱ 41% 2h20m 7d ▰▰▱▱▱▱ 26% 6d5h · agy 5h ▰▰▰▰▰▰ 93% 4h38m · cur –");
        let theme = Theme::builtin();
        let percent_col = text.chars().position(|c| c == '9').unwrap() as u16;
        assert_eq!(buf[(percent_col, 0)].fg, ui::color(theme.error), "93% is drawn in the error colour");
        let first_bar = text.chars().position(|c| c == '▰').unwrap() as u16;
        assert_eq!(buf[(first_bar, 0)].fg, ui::color(theme.success));
    }

    #[test]
    fn detail_gives_way_before_vendors_do() {
        let vendors = parse_usagebar(USAGEBAR).unwrap();
        let (text, _) = draw(&vendors, 70);
        assert_eq!(text, " cld 5h ▰▰▱▱▱▱ 41% 7d ▰▰▱▱▱▱ 26% · agy 5h ▰▰▰▰▰▰ 93% · cur –");
        let (text, _) = draw(&vendors, 44);
        assert_eq!(text, " cld 5h 41% 7d 26% · agy 5h 93% · cur –");
        let (text, _) = draw(&vendors, 24);
        assert_eq!(text, " cld 5h 41% 7d 26%", "what does not fit is left off, never cut in half");
        for width in [0, 3, 8, 12] {
            draw(&vendors, width);
        }
    }

    #[test]
    fn says_when_there_are_no_numbers_yet() {
        let (text, _) = draw(&[], 60);
        assert_eq!(text, " usage: reading the numbers…");
    }

    #[test]
    fn a_trace_of_usage_still_shows_one_segment() {
        let vendors = vec![Vendor {
            short: "agy".into(),
            name: "Antigravity".into(),
            meters: vec![Meter { window: "5h".into(), percent: 1.0, reset_at: None }, Meter { window: "7d".into(), percent: 0.0, reset_at: None }],
            error: None,
        }];
        let (text, _) = draw(&vendors, 60);
        assert_eq!(text, " agy 5h ▰▱▱▱▱▱ 1% 7d ▱▱▱▱▱▱ 0%");
    }
}
