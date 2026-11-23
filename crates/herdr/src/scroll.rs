//! Scrolling the focused pane: a port of the `herdr-scroll` script.

use serde_json::json;

use crate::client::{Client, HerdrError};
use crate::layout::nothing_focused;
use crate::query::focused_pane;

/// Lines one notch of a mouse wheel stands for.
const LINES_PER_WHEEL_EVENT: u32 = 3;

/// The most wheel events sent in one go, however many lines were asked for.
const MAX_WHEEL_EVENTS: u32 = 200;

/// SGR mouse button codes of the wheel.
const WHEEL_UP: u8 = 64;
const WHEEL_DOWN: u8 = 65;

/// What [`scroll`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOutcome {
    /// The pane's view of its scrollback moved.
    Scrolled {
        /// How many lines the view now is above the live end of the output.
        offset_from_bottom: u64,
    },
    /// The pane has no scrollback, so wheel events were sent to the program running in it.
    WheelSent {
        /// How many wheel events were sent.
        events: u32,
    },
    /// Nothing happened: no pane is focused, zero lines were asked for, or the view is
    /// already at that end of the scrollback.
    Unchanged,
}

/// Scroll the focused pane `lines` lines up (`up == true`) or down.
///
/// A pane with scrollback (a shell, an agent printing to the normal screen) has its view
/// moved, stopping at either end. A pane without any (a full-screen program on the alternate
/// screen, such as Claude Code) cannot be scrolled by herdr, so the program is sent mouse
/// wheel events instead, as SGR mouse reports at column 2 of the pane's middle row: one
/// event per three lines, at least one and at most 200.
///
/// Unlike the script, which sends one wheel event even for zero lines, zero lines does
/// nothing at all.
pub fn scroll(client: &Client, up: bool, lines: u32) -> Result<ScrollOutcome, HerdrError> {
    if lines == 0 {
        return Ok(ScrollOutcome::Unchanged);
    }
    let pane = match focused_pane(client) {
        Ok(pane) => pane,
        Err(err) if nothing_focused(&err) => return Ok(ScrollOutcome::Unchanged),
        Err(err) => return Err(err),
    };
    let state = pane.scroll.unwrap_or_default();
    if state.max_offset_from_bottom > 0 {
        let current = state.offset_from_bottom;
        let wanted = if up {
            current
                .saturating_add(u64::from(lines))
                .min(state.max_offset_from_bottom)
        } else {
            current.saturating_sub(u64::from(lines))
        };
        if wanted == current {
            return Ok(ScrollOutcome::Unchanged);
        }
        client.call(
            "pane.scroll",
            json!({ "pane_id": pane.pane_id, "offset_from_bottom": wanted }),
        )?;
        return Ok(ScrollOutcome::Scrolled {
            offset_from_bottom: wanted,
        });
    }

    let row = (state.viewport_rows / 2).max(2);
    let button = if up { WHEEL_UP } else { WHEEL_DOWN };
    let events = (lines / LINES_PER_WHEEL_EVENT).clamp(1, MAX_WHEEL_EVENTS);
    let text = format!("\x1b[<{button};2;{row}M").repeat(events as usize);
    client.call(
        "pane.send_text",
        json!({ "pane_id": pane.pane_id, "text": text }),
    )?;
    Ok(ScrollOutcome::WheelSent { events })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::MockServer;
    use serde_json::Value;

    fn current(scroll: Value) -> Value {
        json!({"type": "pane_current", "pane": {
            "pane_id": "w1:p1", "tab_id": "w1:t1", "workspace_id": "w1", "focused": true, "scroll": scroll,
        }})
    }

    fn scrollback(offset: u64, max: u64) -> Value {
        current(
            json!({"offset_from_bottom": offset, "max_offset_from_bottom": max, "viewport_rows": 40}),
        )
    }

    fn server_with(reply: Value) -> MockServer {
        let server = MockServer::start();
        server.ok("pane.current", reply);
        server
    }

    #[test]
    fn scrolls_up_through_the_scrollback() {
        let server = server_with(scrollback(0, 183));
        server.ok("pane.scroll", json!({"type": "pane_info"}));
        let outcome = scroll(&server.client(), true, 3).unwrap();
        assert_eq!(
            outcome,
            ScrollOutcome::Scrolled {
                offset_from_bottom: 3
            }
        );
        let requests = server.requests();
        assert_eq!(requests[0].params, json!({}));
        assert_eq!(
            requests[1].params,
            json!({"pane_id": "w1:p1", "offset_from_bottom": 3})
        );
        server.assert_finished();
    }

    #[test]
    fn scrolls_down_towards_the_live_end() {
        let server = server_with(scrollback(20, 183));
        server.ok("pane.scroll", json!({"type": "pane_info"}));
        assert_eq!(
            scroll(&server.client(), false, 9).unwrap(),
            ScrollOutcome::Scrolled {
                offset_from_bottom: 11
            }
        );
        assert_eq!(server.requests()[1].params["offset_from_bottom"], 11);
    }

    #[test]
    fn stops_at_the_top_and_at_the_bottom() {
        let server = server_with(scrollback(181, 183));
        server.ok("pane.scroll", json!({"type": "pane_info"}));
        assert_eq!(
            scroll(&server.client(), true, 9).unwrap(),
            ScrollOutcome::Scrolled {
                offset_from_bottom: 183
            }
        );

        let server = server_with(scrollback(2, 183));
        server.ok("pane.scroll", json!({"type": "pane_info"}));
        assert_eq!(
            scroll(&server.client(), false, 9).unwrap(),
            ScrollOutcome::Scrolled {
                offset_from_bottom: 0
            }
        );
    }

    #[test]
    fn does_not_call_herdr_when_already_at_the_end() {
        for (up, offset) in [(true, 183), (false, 0)] {
            let server = server_with(scrollback(offset, 183));
            assert_eq!(
                scroll(&server.client(), up, 3).unwrap(),
                ScrollOutcome::Unchanged
            );
            assert_eq!(server.requests().len(), 1);
        }
    }

    #[test]
    fn huge_line_counts_do_not_overflow() {
        let server = server_with(scrollback(u64::MAX - 1, u64::MAX));
        server.ok("pane.scroll", json!({"type": "pane_info"}));
        assert_eq!(
            scroll(&server.client(), true, u32::MAX).unwrap(),
            ScrollOutcome::Scrolled {
                offset_from_bottom: u64::MAX
            }
        );
    }

    #[test]
    fn a_pane_without_scrollback_gets_wheel_events_at_its_middle_row() {
        let server = server_with(scrollback(0, 0));
        server.ok("pane.send_text", json!({"type": "ok"}));
        let outcome = scroll(&server.client(), true, 3).unwrap();
        assert_eq!(outcome, ScrollOutcome::WheelSent { events: 1 });
        assert_eq!(
            server.requests()[1].params,
            json!({"pane_id": "w1:p1", "text": "\u{1b}[<64;2;20M"})
        );
        server.assert_finished();
    }

    #[test]
    fn wheel_events_go_down_too_and_come_one_per_three_lines() {
        let server = server_with(scrollback(0, 0));
        server.ok("pane.send_text", json!({"type": "ok"}));
        let outcome = scroll(&server.client(), false, 9).unwrap();
        assert_eq!(outcome, ScrollOutcome::WheelSent { events: 3 });
        assert_eq!(
            server.requests()[1].params["text"],
            "\u{1b}[<65;2;20M\u{1b}[<65;2;20M\u{1b}[<65;2;20M"
        );
    }

    #[test]
    fn fewer_than_three_lines_still_send_one_wheel_event() {
        for lines in [1, 2, 4, 5] {
            let server = server_with(scrollback(0, 0));
            server.ok("pane.send_text", json!({"type": "ok"}));
            assert_eq!(
                scroll(&server.client(), true, lines).unwrap(),
                ScrollOutcome::WheelSent { events: 1 },
                "{lines}"
            );
        }
    }

    #[test]
    fn the_number_of_wheel_events_is_capped() {
        let server = server_with(scrollback(0, 0));
        server.ok("pane.send_text", json!({"type": "ok"}));
        assert_eq!(
            scroll(&server.client(), true, u32::MAX).unwrap(),
            ScrollOutcome::WheelSent { events: 200 }
        );
        let text = server.requests()[1].params["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(text.matches("\u{1b}[<64;2;20M").count(), 200);
    }

    #[test]
    fn the_wheel_row_is_never_above_row_two() {
        for (rows, row) in [(40, 20), (5, 2), (3, 2), (1, 2), (0, 2), (9, 4)] {
            let server = server_with(current(
                json!({"offset_from_bottom": 0, "max_offset_from_bottom": 0, "viewport_rows": rows}),
            ));
            server.ok("pane.send_text", json!({"type": "ok"}));
            scroll(&server.client(), true, 3).unwrap();
            assert_eq!(
                server.requests()[1].params["text"],
                format!("\u{1b}[<64;2;{row}M"),
                "{rows} rows"
            );
        }
    }

    #[test]
    fn a_pane_reporting_no_scroll_state_is_treated_as_24_rows_without_scrollback() {
        for reply in [current(Value::Null), current(json!({}))] {
            let server = server_with(reply);
            server.ok("pane.send_text", json!({"type": "ok"}));
            assert_eq!(
                scroll(&server.client(), false, 3).unwrap(),
                ScrollOutcome::WheelSent { events: 1 }
            );
            assert_eq!(server.requests()[1].params["text"], "\u{1b}[<65;2;12M");
        }
    }

    #[test]
    fn zero_lines_do_nothing_without_asking_herdr() {
        let server = MockServer::start();
        assert_eq!(
            scroll(&server.client(), true, 0).unwrap(),
            ScrollOutcome::Unchanged
        );
        assert!(server.requests().is_empty());
    }

    #[test]
    fn an_empty_session_is_unchanged() {
        let server = MockServer::start();
        server.fail("pane.current", "pane_not_found", "pane not found");
        assert_eq!(
            scroll(&server.client(), true, 3).unwrap(),
            ScrollOutcome::Unchanged
        );
    }

    #[test]
    fn failures_are_passed_on() {
        let server = MockServer::start();
        server.fail("pane.current", "internal", "boom");
        assert_eq!(
            scroll(&server.client(), true, 3).unwrap_err().code(),
            Some("internal")
        );

        let server = server_with(scrollback(0, 50));
        server.fail("pane.scroll", "pane_not_found", "pane w1:p1 not found");
        assert_eq!(
            scroll(&server.client(), true, 3).unwrap_err().code(),
            Some("pane_not_found")
        );

        let server = server_with(scrollback(0, 0));
        server.fail("pane.send_text", "pane_not_found", "pane w1:p1 not found");
        assert_eq!(
            scroll(&server.client(), true, 3).unwrap_err().code(),
            Some("pane_not_found")
        );
    }
}
