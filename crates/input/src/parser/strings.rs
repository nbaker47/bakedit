//! String sequences (OSC, DCS and friends) and paste bodies.

use bakedit_core::{ColorSlot, InputEvent, Rgb, TerminalReply};

use super::StringKind;

/// Emit the reply a complete string sequence stands for, if it is one bakedit asked for.
/// Everything else is dropped.
pub(super) fn dispatch(kind: StringKind, body: &[u8], out: &mut Vec<InputEvent>) {
    match kind {
        StringKind::Osc => osc(body, out),
        StringKind::Dcs => {
            // XTVERSION answers `DCS > | name and version ST`.
            if let Some(version) = body.strip_prefix(b">|") {
                let version = String::from_utf8_lossy(version).into_owned();
                out.push(InputEvent::Reply(TerminalReply::Version(version)));
            }
        }
        StringKind::Apc | StringKind::Pm | StringKind::Sos => {}
    }
}

/// OSC 10, 11 and 12 report the foreground, background and cursor colours; OSC 4 reports
/// palette entries as `index ; colour` pairs.
fn osc(body: &[u8], out: &mut Vec<InputEvent>) {
    let Ok(text) = std::str::from_utf8(body) else {
        return;
    };
    let Some((code, rest)) = text.split_once(';') else {
        return;
    };
    let mut report = |slot: ColorSlot, spec: &str| {
        if let Some(rgb) = parse_color(spec) {
            out.push(InputEvent::Reply(TerminalReply::Color { slot, rgb }));
        }
    };
    let mut parts = rest.split(';');
    match code {
        "10" => report(ColorSlot::Foreground, parts.next().unwrap_or("")),
        "11" => report(ColorSlot::Background, parts.next().unwrap_or("")),
        "12" => report(ColorSlot::Cursor, parts.next().unwrap_or("")),
        "4" => {
            while let (Some(index), Some(spec)) = (parts.next(), parts.next()) {
                if let Ok(index) = index.parse::<u8>() {
                    report(ColorSlot::Palette(index), spec);
                }
            }
        }
        _ => {}
    }
}

/// Parse a colour as terminals report it: `rgb:R/G/B` with one to four hex digits per
/// channel, `rgba:R/G/B/A` (the alpha is ignored), or `#` followed by one to four hex
/// digits per channel. Every channel is scaled to 8 bits.
pub(super) fn parse_color(spec: &str) -> Option<Rgb> {
    let spec = spec.trim();
    if let Some(hex) = spec.strip_prefix('#') {
        if !hex.is_ascii() || hex.len() % 3 != 0 {
            return None;
        }
        let digits = hex.len() / 3;
        return Some(Rgb::new(
            channel(&hex[..digits])?,
            channel(&hex[digits..2 * digits])?,
            channel(&hex[2 * digits..])?,
        ));
    }
    let (scheme, channels) = spec.split_once(':')?;
    let wanted = match scheme.to_ascii_lowercase().as_str() {
        "rgb" => 3,
        "rgba" => 4,
        _ => return None,
    };
    let channels: Vec<&str> = channels.split('/').collect();
    if channels.len() != wanted {
        return None;
    }
    Some(Rgb::new(
        channel(channels[0])?,
        channel(channels[1])?,
        channel(channels[2])?,
    ))
}

/// One colour channel of one to four hex digits, scaled so that all-ones is 255.
fn channel(hex: &str) -> Option<u8> {
    if hex.is_empty() || hex.len() > 4 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(hex, 16).ok()?;
    let max = (1u32 << (4 * hex.len())) - 1;
    Some(((value * 255 + max / 2) / max) as u8)
}

/// Cut off a UTF-8 character left incomplete at the end of `bytes`, as happens when a paste
/// is truncated at the cap.
pub(super) fn trim_partial_utf8(bytes: &mut Vec<u8>) {
    // The lead byte of the last character is at most three bytes before the end.
    let Some(back) = bytes
        .iter()
        .rev()
        .take(4)
        .position(|&b| !(0x80..=0xbf).contains(&b))
    else {
        return;
    };
    let lead_at = bytes.len() - 1 - back;
    let needed = match bytes[lead_at] {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xff => 4,
        _ => 1,
    };
    if bytes.len() - lead_at < needed {
        bytes.truncate(lead_at);
    }
}

/// A paste body as text: invalid UTF-8 becomes U+FFFD, and CRLF and lone CR become LF.
pub(super) fn paste_text(body: Vec<u8>) -> String {
    let text = match String::from_utf8(body) {
        Ok(text) => text,
        Err(err) => String::from_utf8_lossy(err.as_bytes()).into_owned(),
    };
    if !text.contains('\r') {
        return text;
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}
