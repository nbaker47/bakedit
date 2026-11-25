//! Agent discovery, output reading, and messaging over herdr's socket API.
//!
//! Used by bakedit's bundled Agent Dots (both the inline status strip, the standalone
//! `bakedit --dots` terminal TUI, and the macOS/Linux floating overlay).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::client::{Client, HerdrError};

/// Lifecycle state reported by herdr for an AI coding agent pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentStatus {
    /// Waiting for the next prompt.
    Idle,
    /// Actively thinking or running tools.
    Working,
    /// Waiting for user confirmation / approval.
    Blocked,
    /// Finished a turn.
    Done,
    /// Status could not be classified.
    Unknown,
}

impl AgentStatus {
    /// Parse herdr's `agent_status` string.
    pub fn parse(raw: &str) -> AgentStatus {
        match raw {
            "idle" => AgentStatus::Idle,
            "working" => AgentStatus::Working,
            "blocked" => AgentStatus::Blocked,
            "done" => AgentStatus::Done,
            _ => AgentStatus::Unknown,
        }
    }

    /// Short human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            AgentStatus::Idle => "Idle",
            AgentStatus::Working => "Working",
            AgentStatus::Blocked => "Needs Input",
            AgentStatus::Done => "Done",
            AgentStatus::Unknown => "Unknown",
        }
    }
}

/// One AI coding agent tracked by herdr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInfo {
    /// herdr pane id (e.g. `"w3:p1"`).
    pub pane_id: String,
    /// herdr workspace id (e.g. `"w3"`).
    pub workspace_id: String,
    /// herdr tab id (e.g. `"w3:t1"`).
    pub tab_id: String,
    /// Agent kind (e.g. `"agy"`, `"claude"`, `"codex"`).
    pub agent: String,
    /// Current lifecycle state.
    pub status: AgentStatus,
    /// Active working directory of the foreground process.
    pub cwd: String,
    /// Terminal title without escape codes.
    pub title: String,
    /// Monotonic counter incremented when a turn completes.
    pub completion_seq: u64,
    /// Monotonic counter incremented on status transitions.
    pub state_change_seq: u64,
    /// Whether this pane currently has focus in herdr.
    pub focused: bool,
    /// Optional agent session id (e.g. Claude Code UUID or Antigravity conversation UUID).
    pub session_id: Option<String>,
    /// Optional human-readable session name (e.g. from `agy` `/rename` or Claude title).
    pub session_name: Option<String>,
}

impl AgentInfo {
    /// Last path component of `cwd`, for compact labels.
    pub fn short_dir(&self) -> &str {
        let trimmed = self.cwd.trim_end_matches('/');
        trimmed.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or(&self.cwd)
    }

    /// Display name for the agent kind.
    pub fn display_agent(&self) -> &str {
        if self.agent.is_empty() {
            "agent"
        } else {
            &self.agent
        }
    }

    /// Compact pill label: prefers `session_name` (truncated to `max_chars`), falling back
    /// to [`Self::display_agent`].
    pub fn display_label(&self, max_chars: usize) -> String {
        if let Some(name) = self.session_name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let count = name.chars().count();
            if count <= max_chars || max_chars < 2 {
                return name.to_string();
            }
            let mut out: String = name.chars().take(max_chars - 1).collect();
            out.push('…');
            return out;
        }
        self.display_agent().to_string()
    }
}

/// Extract `title:"..."` from an Antigravity CLI `.pbtxt` annotation file.
pub fn parse_pbtxt_title(raw: &str) -> Option<String> {
    for line in raw.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("title:") else { continue };
        let rest = rest.trim();
        let Some(inner) = rest.strip_prefix('"') else { continue };
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        out.push(escaped);
                    }
                }
                '"' => {
                    let s = out.trim();
                    return (!s.is_empty()).then(|| s.to_string());
                }
                _ => out.push(c),
            }
        }
    }
    None
}

/// Read explicit `/rename <name>` commands from `~/.gemini/antigravity-cli/history.jsonl`,
/// returning a map of `conversationId -> latest renamed title`.
pub fn parse_agy_renames(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in raw.lines() {
        if !line.contains("/rename") {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(line) else { continue };
        let Some(display) = val.get("display").and_then(Value::as_str) else { continue };
        let Some(cid) = val.get("conversationId").and_then(Value::as_str) else { continue };
        let trimmed = display.trim();
        let Some(rest) = trimmed.strip_prefix("/rename") else { continue };
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let name = rest.trim();
        if !name.is_empty() {
            map.insert(cid.to_string(), name.to_string());
        }
    }
    map
}

fn read_agy_renames() -> HashMap<String, String> {
    let Some(home) = std::env::var_os("HOME") else { return HashMap::new() };
    let path = PathBuf::from(home).join(".gemini/antigravity-cli/history.jsonl");
    let Ok(raw) = fs::read_to_string(path) else { return HashMap::new() };
    parse_agy_renames(&raw)
}

/// Extract a compact model + effort label (e.g. `"Neon (Medium)"`, `"Sonnet 5.5"`) from an
/// agent pane's visible status bar lines (`[Code] [cluster] Gemini 4 Neon (Medium)`).
pub fn extract_model_label(visible: &str) -> Option<String> {
    for line in visible.lines().rev().take(10) {
        let mut s = line.trim();
        if !s.starts_with('[') {
            continue;
        }
        // Strip leading `[tag]` segments such as `[Code] [starbase-prod]`.
        let mut stripped_any = false;
        while s.starts_with('[') {
            let Some(end) = s.find(']') else { break };
            s = s[end + 1..].trim_start();
            stripped_any = true;
        }
        if !stripped_any || s.is_empty() {
            continue;
        }
        // Strip common vendor prefixes so compact pills read `"Neon (Medium)"` / `"Sonnet 5.5"`.
        for prefix in ["Gemini 4 ", "Gemini 3 ", "Gemini 2.5 ", "Gemini ", "Claude "] {
            if let Some(rest) = s.strip_prefix(prefix) {
                s = rest.trim();
                break;
            }
        }
        if !s.is_empty() && s.len() <= 40 {
            return Some(s.to_string());
        }
    }
    None
}

fn read_pane_model_label(client: &Client, pane_id: &str) -> Option<String> {
    let val: Value = client
        .call(
            "agent.read",
            json!({
                "target": pane_id,
                "source": "visible",
                "format": "text",
                "strip_ansi": true,
            }),
        )
        .ok()?;
    let text = extract_read_text(&val);
    extract_model_label(&text)
}

fn resolve_session_name(
    client: &Client,
    pane_id: &str,
    agent: &str,
    session_id: Option<&str>,
    title: &str,
    agy_renames: &HashMap<String, String>,
) -> Option<String> {
    if agent == "agy" {
        if let Some(sid) = session_id {
            if let Some(name) = agy_renames.get(sid) {
                return Some(name.clone());
            }
        }
        return read_pane_model_label(client, pane_id);
    }
    if agent == "claude" {
        let t = title.trim();
        if !t.is_empty()
            && !t.eq_ignore_ascii_case("claude")
            && !t.eq_ignore_ascii_case("claude code")
            && !t.starts_with("claude ")
        {
            return Some(t.to_string());
        }
        return read_pane_model_label(client, pane_id);
    }
    None
}

/// List all agents currently known to herdr.
pub fn list_agents(client: &Client) -> Result<Vec<AgentInfo>, HerdrError> {
    let val: Value = client.call("agent.list", json!({}))?;
    let Some(items) = val.get("agents").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let agy_renames = read_agy_renames();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(pane_id) = item.get("pane_id").and_then(Value::as_str) else {
            continue;
        };
        let cwd = item
            .get("foreground_cwd")
            .and_then(Value::as_str)
            .or_else(|| item.get("cwd").and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let session_id = item
            .get("agent_session")
            .and_then(|s| s.get("value"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let agent_kind = item.get("agent").and_then(Value::as_str).unwrap_or("agent").to_string();
        let title = item.get("terminal_title_stripped").and_then(Value::as_str).unwrap_or("").to_string();
        let session_name = resolve_session_name(client, pane_id, &agent_kind, session_id.as_deref(), &title, &agy_renames);
        out.push(AgentInfo {
            pane_id: pane_id.to_string(),
            workspace_id: item.get("workspace_id").and_then(Value::as_str).unwrap_or("").to_string(),
            tab_id: item.get("tab_id").and_then(Value::as_str).unwrap_or("").to_string(),
            agent: agent_kind,
            status: AgentStatus::parse(item.get("agent_status").and_then(Value::as_str).unwrap_or("unknown")),
            cwd,
            title,
            completion_seq: item.get("completion_seq").and_then(Value::as_u64).unwrap_or(0),
            state_change_seq: item.get("state_change_seq").and_then(Value::as_u64).unwrap_or(0),
            focused: item.get("focused").and_then(Value::as_bool).unwrap_or(false),
            session_id,
            session_name,
        });
    }
    Ok(out)
}

/// Read recent terminal output from an agent pane, falling back to `"visible"` if herdr
/// rejects `"recent"` with `agent_not_idle` while the agent is actively working.
pub fn read_agent_output(
    client: &Client,
    target: &str,
    lines: u32,
    ansi: bool,
) -> Result<String, HerdrError> {
    let format = if ansi { "ansi" } else { "text" };
    let params = json!({
        "target": target,
        "source": "recent",
        "lines": lines,
        "format": format,
        "strip_ansi": !ansi,
    });
    match client.call("agent.read", params) {
        Ok(val) => Ok(extract_read_text(&val)),
        Err(HerdrError::Server { code, .. }) if code == "agent_not_idle" => {
            let fallback = json!({
                "target": target,
                "source": "visible",
                "format": format,
                "strip_ansi": !ansi,
            });
            let val: Value = client.call("agent.read", fallback)?;
            Ok(extract_read_text(&val))
        }
        Err(err) => Err(err),
    }
}

fn extract_read_text(val: &Value) -> String {
    let inner = val.get("read").unwrap_or(val);
    if let Some(s) = inner.get("text").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(s) = inner.get("output").and_then(Value::as_str) {
        return s.to_string();
    }
    if let Some(lines) = inner.get("lines").and_then(Value::as_array) {
        let joined: Vec<&str> = lines.iter().filter_map(Value::as_str).collect();
        return joined.join("\n");
    }
    String::new()
}

/// Send a chat prompt to an agent pane.
pub fn prompt_agent(client: &Client, target: &str, text: &str) -> Result<(), HerdrError> {
    let _: Value = client.call("agent.prompt", json!({ "target": target, "text": text }))?;
    Ok(())
}

/// Send key names (e.g. `["Enter"]`, `["Escape"]`) to an agent pane.
pub fn send_agent_keys(client: &Client, target: &str, keys: &[&str]) -> Result<(), HerdrError> {
    let _: Value = client.call("agent.send_keys", json!({ "target": target, "keys": keys }))?;
    Ok(())
}

/// Focus an agent's pane in herdr.
pub fn focus_agent(client: &Client, target: &str) -> Result<(), HerdrError> {
    let _: Value = client.call("agent.focus", json!({ "target": target }))?;
    Ok(())
}

/// Remove CSI and OSC escape sequences from `input`.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ('@'..='~').contains(&ch) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn is_rule_line(plain: &str) -> bool {
    let t = plain.trim();
    t.starts_with("────────") || t.starts_with("━━━━━━━━")
}

fn is_prompt_line(plain: &str) -> bool {
    let t = plain.trim();
    t == ">"
        || t == "❯"
        || t.starts_with("> ")
        || t.starts_with("❯ ")
        || t.starts_with("❯\u{00a0}")
}

/// Separate a TUI's bottom prompt box (`───` / `>` / `───`) and status line(s) from the
/// scrollable transcript, preserving ANSI escape codes in both parts.
pub fn split_output_and_status(raw: &str) -> (String, Vec<String>) {
    let lines: Vec<&str> = raw.lines().collect();
    let plain: Vec<String> = lines.iter().map(|l| strip_ansi(l)).collect();

    // Trim leading/trailing blank lines first.
    let mut start = 0;
    while start < lines.len() && plain[start].trim().is_empty() {
        start += 1;
    }
    let mut end = lines.len();
    while end > start && plain[end - 1].trim().is_empty() {
        end -= 1;
    }
    if start >= end {
        return ("(no output yet)".to_string(), Vec::new());
    }
    let lines = &lines[start..end];
    let plain = &plain[start..end];

    let mut bottom_rule = None;
    let mut non_empty = 0usize;
    for i in (0..lines.len()).rev() {
        if plain[i].trim().is_empty() {
            continue;
        }
        if is_rule_line(&plain[i]) {
            bottom_rule = Some(i);
            break;
        }
        non_empty += 1;
        if non_empty >= 8 {
            break;
        }
    }
    let Some(b_idx) = bottom_rule else {
        return (lines.join("\n"), Vec::new());
    };

    let mut top_rule = None;
    let mut prompt = None;
    let min_search = b_idx.saturating_sub(6);
    for i in (min_search..b_idx).rev() {
        if plain[i].trim().is_empty() {
            continue;
        }
        if is_rule_line(&plain[i]) {
            top_rule = Some(i);
            break;
        }
        if is_prompt_line(&plain[i]) {
            prompt = Some(i);
        }
    }
    let Some(cut_before) = top_rule.or(prompt) else {
        return (lines.join("\n"), Vec::new());
    };

    let status_lines: Vec<String> = lines[b_idx + 1..]
        .iter()
        .enumerate()
        .filter(|(offset, _)| !plain[b_idx + 1 + offset].trim().is_empty())
        .map(|(_, line)| line.trim().to_string())
        .collect();

    let mut body_end = cut_before;
    while body_end > 0 {
        let t = plain[body_end - 1].trim();
        if t.is_empty() || t == "Jump to bottom (click) ↓" {
            body_end -= 1;
        } else {
            break;
        }
    }
    let body = if body_end == 0 {
        "(no output yet)".to_string()
    } else {
        lines[..body_end].join("\n")
    };
    (body, status_lines)
}

/// Lightweight Markdown-to-ANSI renderer for Claude Code JSONL transcripts.
pub fn render_markdown_to_ansi(md: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in md.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push(format!("  \x1b[94m{line}\x1b[0m"));
            continue;
        }
        if trimmed.starts_with('#') {
            let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
            if hashes <= 6 && trimmed.as_bytes().get(hashes) == Some(&b' ') {
                let title = render_inline_markdown(&trimmed[hashes + 1..]);
                out.push(format!("\x1b[1;93m{title}\x1b[0m"));
                continue;
            }
        }
        let bullet = if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent_len = line.len() - line.trim_start().len();
            format!("{}• {}", &line[..indent_len], rest)
        } else {
            line.to_string()
        };
        out.push(render_inline_markdown(&bullet));
    }
    out.join("\n")
}

fn render_inline_markdown(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut out = String::with_capacity(line.len() + 16);
    let mut i = 0;
    while i < bytes.len() {
        // Inline code: `...`
        if bytes[i] == b'`' {
            if let Some(end) = line[i + 1..].find('`') {
                let inner = &line[i + 1..i + 1 + end];
                out.push_str("\x1b[94m");
                out.push_str(inner);
                out.push_str("\x1b[0m");
                i += end + 2;
                continue;
            }
        }
        // Bold: **...**
        if line[i..].starts_with("**") {
            if let Some(end) = line[i + 2..].find("**") {
                let inner = &line[i + 2..i + 2 + end];
                out.push_str("\x1b[1;97m");
                out.push_str(inner);
                out.push_str("\x1b[0m");
                i += end + 4;
                continue;
            }
        }
        // Link: [label](url)
        if bytes[i] == b'[' {
            if let Some(close_br) = line[i + 1..].find("](") {
                let after = i + 1 + close_br + 2;
                if let Some(close_paren) = line[after..].find(')') {
                    let label = &line[i + 1..i + 1 + close_br];
                    out.push_str("\x1b[4;94m");
                    out.push_str(label);
                    out.push_str("\x1b[0m");
                    i = after + close_paren + 1;
                    continue;
                }
            }
        }
        let ch = line[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Read recent conversation turns from Claude Code's JSONL transcript under
/// `~/.claude/projects/*/<session_id>.jsonl`, formatted with ANSI colours.
pub fn read_claude_history(session_id: &str, max_blocks: usize) -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let projects_dir = home.join(".claude").join("projects");
    let path = find_session_file(&projects_dir, session_id)?;
    read_claude_history_from_path(&path, max_blocks)
}

fn find_session_file(projects_dir: &PathBuf, session_id: &str) -> Option<PathBuf> {
    let entries = fs::read_dir(projects_dir).ok()?;
    let filename = format!("{session_id}.jsonl");
    for entry in entries.flatten() {
        let candidate = entry.path().join(&filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Parse a Claude Code JSONL file at `path` into ANSI-formatted conversation blocks.
pub fn read_claude_history_from_path(path: &std::path::Path, max_blocks: usize) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    const TAIL_BYTES: u64 = 512 * 1024;
    let skipped = len > TAIL_BYTES;
    if skipped {
        let _ = file.seek(SeekFrom::Start(len - TAIL_BYTES));
    }
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;
    let mut lines = buf.lines();
    if skipped {
        lines.next(); // drop partial first line
    }
    let mut blocks = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(kind) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(msg) = obj.get("message") else {
            continue;
        };
        if kind == "user" {
            if let Some(text) = msg.get("content").and_then(Value::as_str) {
                let t = text.trim();
                if t.is_empty()
                    || t.starts_with("This session is being continued from a previous conversation")
                    || t.starts_with("<local-command-")
                    || t.starts_with("<command-name>")
                {
                    continue;
                }
                blocks.push(format!("\x1b[1;96m❯ {t}\x1b[0m"));
            }
        } else if kind == "assistant" {
            if let Some(items) = msg.get("content").and_then(Value::as_array) {
                let mut parts = Vec::new();
                for it in items {
                    match it.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(txt) = it.get("text").and_then(Value::as_str) {
                                let t = txt.trim();
                                if !t.is_empty() {
                                    parts.push(render_markdown_to_ansi(t));
                                }
                            }
                        }
                        Some("tool_use") => {
                            if let Some(name) = it.get("name").and_then(Value::as_str) {
                                parts.push(format!("\x1b[2;36m⚙ {name}\x1b[0m"));
                            }
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    blocks.push(parts.join("\n"));
                }
            }
        }
    }
    if blocks.is_empty() {
        return None;
    }
    let start = blocks.len().saturating_sub(max_blocks);
    Some(blocks[start..].join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_tui_prompt_and_status_preserving_ansi() {
        let raw = "\n  Hello \x1b[34mworld\x1b[0m\n\n\x1b[90m────────────────────\x1b[0m\n\x1b[34m>\x1b[0m\n\x1b[90m────────────────────\x1b[0m\n\x1b[32m[Code]\x1b[0m Gemini 4\nctx 80% left\n";
        let (body, status) = split_output_and_status(raw);
        assert_eq!(body, "  Hello \x1b[34mworld\x1b[0m");
        assert_eq!(status, vec!["\x1b[32m[Code]\x1b[0m Gemini 4", "ctx 80% left"]);
    }

    #[test]
    fn renders_claude_markdown_to_ansi() {
        let md = "## Summary\n- Check `values.yaml` and **v1.18.4** in [PR #1](https://example.com)";
        let out = render_markdown_to_ansi(md);
        assert!(out.contains("\x1b[1;93mSummary\x1b[0m"));
        assert!(out.contains("• Check \x1b[94mvalues.yaml\x1b[0m and \x1b[1;97mv1.18.4\x1b[0m"));
        assert!(out.contains("\x1b[4;94mPR #1\x1b[0m"));
    }

    #[test]
    fn reads_claude_jsonl_history() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sess.jsonl");
        let content = concat!(
            r#"{"type":"user","message":{"content":"hello claude"}}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"},{"type":"text","text":"All **done**."}]}}"#, "\n",
        );
        fs::write(&file, content).unwrap();
        let out = read_claude_history_from_path(&file, 10).unwrap();
        assert!(out.contains("❯ hello claude"));
        assert!(out.contains("⚙ Bash"));
        assert!(out.contains("\x1b[1;97mdone\x1b[0m"));
    }
}
