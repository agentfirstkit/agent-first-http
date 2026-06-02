//! Pointer/keyboard event replay via Input.dispatch*. Preserves
//! operator-supplied `performance.now()` timestamps as inter-event delays
//! so trajectory/dwell-time entropy from real input is kept end-to-end
//! (`architecture.md §9`).

use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::ops_panel::screencast::resolve_page_target;
use crate::sdk::cdp::ws_client::Connection;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpsInputEvent {
    PointerMove {
        x: f32,
        y: f32,
        timestamp_ms: f64,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: String,
        timestamp_ms: f64,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: String,
        timestamp_ms: f64,
    },
    Wheel {
        x: f32,
        y: f32,
        dx: f32,
        dy: f32,
        timestamp_ms: f64,
    },
    KeyDown {
        key: String,
        code: String,
        modifiers: u32,
        timestamp_ms: f64,
    },
    KeyUp {
        key: String,
        code: String,
        modifiers: u32,
        timestamp_ms: f64,
    },
    /// A whole string pasted by the operator (clipboard → Input.insertText).
    /// Inserted at the focused element's caret in one shot — the operator's
    /// clipboard never reaches the target browser, so we relay the text
    /// itself rather than a Ctrl/⌘+V keystroke (which would paste the
    /// target's own, unrelated clipboard).
    InsertText {
        text: String,
        timestamp_ms: f64,
    },
}

impl OpsInputEvent {
    fn timestamp_ms(&self) -> f64 {
        match self {
            Self::PointerMove { timestamp_ms, .. }
            | Self::PointerDown { timestamp_ms, .. }
            | Self::PointerUp { timestamp_ms, .. }
            | Self::Wheel { timestamp_ms, .. }
            | Self::KeyDown { timestamp_ms, .. }
            | Self::KeyUp { timestamp_ms, .. }
            | Self::InsertText { timestamp_ms, .. } => *timestamp_ms,
        }
    }

    fn to_cdp(&self) -> (&'static str, Value) {
        match self {
            Self::PointerMove { x, y, .. } => (
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mouseMoved",
                    "x": x,
                    "y": y,
                    "button": "none",
                }),
            ),
            Self::PointerDown { x, y, button, .. } => (
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mousePressed",
                    "x": x,
                    "y": y,
                    "button": button,
                    "clickCount": 1,
                }),
            ),
            Self::PointerUp { x, y, button, .. } => (
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mouseReleased",
                    "x": x,
                    "y": y,
                    "button": button,
                    "clickCount": 1,
                }),
            ),
            Self::Wheel { x, y, dx, dy, .. } => (
                "Input.dispatchMouseEvent",
                serde_json::json!({
                    "type": "mouseWheel",
                    "x": x,
                    "y": y,
                    "deltaX": dx,
                    "deltaY": dy,
                }),
            ),
            Self::KeyDown {
                key,
                code,
                modifiers,
                ..
            } => {
                let mut params = serde_json::json!({
                    "type": "keyDown",
                    "key": key,
                    "code": code,
                    "modifiers": modifiers,
                    "text": one_char_text(key, *modifiers),
                });
                if let Some(vk) = virtual_key_code(key) {
                    params["windowsVirtualKeyCode"] = vk.into();
                    params["nativeVirtualKeyCode"] = vk.into();
                }
                ("Input.dispatchKeyEvent", params)
            }
            Self::KeyUp {
                key,
                code,
                modifiers,
                ..
            } => {
                let mut params = serde_json::json!({
                    "type": "keyUp",
                    "key": key,
                    "code": code,
                    "modifiers": modifiers,
                });
                if let Some(vk) = virtual_key_code(key) {
                    params["windowsVirtualKeyCode"] = vk.into();
                    params["nativeVirtualKeyCode"] = vk.into();
                }
                ("Input.dispatchKeyEvent", params)
            }
            Self::InsertText { text, .. } => (
                "Input.insertText",
                serde_json::json!({
                    "text": text,
                }),
            ),
        }
    }
}

/// If `key` is a single printable character, return it as the `text` field
/// for Input.dispatchKeyEvent so the page sees a real keypress. Otherwise
/// (Enter, Backspace, ArrowLeft, …) leave it out — CDP will synthesize the
/// right behavior from key/code.
///
/// When Ctrl or ⌘ is held the keystroke is a shortcut (select-all, reload, …),
/// not text, so we suppress `text` regardless of the key — otherwise chromium
/// types the bare letter instead of running the shortcut. Shift/Alt don't
/// count: Shift+a is still "A", and AltGr layouts produce real characters.
fn one_char_text(key: &str, modifiers: u32) -> Value {
    const CTRL: u32 = 2;
    const META: u32 = 4;
    if modifiers & (CTRL | META) != 0 {
        return Value::Null;
    }
    let mut chars = key.chars();
    let first = chars.next();
    let second = chars.next();
    match (first, second) {
        (Some(c), None) if !c.is_control() => Value::String(c.to_string()),
        _ => Value::Null,
    }
}

/// Windows virtual-key code for a DOM `key` value, or `None` when there isn't a
/// meaningful one. CDP needs this for any key whose effect is an *action*
/// rather than inserted text: Backspace/Delete/Enter/Tab/arrows won't edit, and
/// Ctrl/⌘+letter shortcuts won't fire, unless `windowsVirtualKeyCode` is set —
/// the `text` field alone only covers character insertion. Letters and digits
/// map to their ASCII-uppercase code (the VK code equals the uppercase ASCII
/// value); named editing/navigation keys use the fixed table below.
fn virtual_key_code(key: &str) -> Option<i64> {
    let named = match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Escape" => 27,
        " " | "Spacebar" => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    // Single ASCII letter/digit: VK code is the uppercase ASCII value.
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c.to_ascii_uppercase() as i64),
        _ => None,
    }
}

/// Run the input-replay loop: accept JSON events, schedule them to
/// preserve inter-event timing, and dispatch via CDP.
pub async fn run(client_ws: WebSocket, browser_ws_url: &str) {
    let conn = match Connection::connect(browser_ws_url, None).await {
        Ok(c) => c,
        Err(_) => {
            let _ = close_with_error(client_ws, "browser connect failed").await;
            return;
        }
    };
    let Some(target_id) = resolve_page_target(&conn).await else {
        let _ = close_with_error(client_ws, "no page target available").await;
        return;
    };
    let attach = match conn
        .send(
            "Target.attachToTarget",
            &serde_json::json!({"targetId": target_id, "flatten": true}),
            None,
        )
        .await
    {
        Ok(v) => v,
        Err(_) => {
            let _ = close_with_error(client_ws, "attach failed").await;
            return;
        }
    };
    let Some(session_id) = attach["sessionId"].as_str().map(str::to_string) else {
        let _ = close_with_error(client_ws, "no session id").await;
        return;
    };

    replay_loop(client_ws, conn, session_id).await;
}

async fn replay_loop(client_ws: WebSocket, conn: Connection, session_id: String) {
    let (_tx, mut rx) = client_ws.split();
    let mut first_event_ts: Option<f64> = None;
    let mut relay_start = Instant::now();

    while let Some(Ok(msg)) = rx.next().await {
        let payload = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let event: OpsInputEvent = match serde_json::from_str(&payload) {
            Ok(e) => e,
            Err(_) => continue,
        };

        if first_event_ts.is_none() {
            first_event_ts = Some(event.timestamp_ms());
            relay_start = Instant::now();
        }
        let target_offset_ms = event.timestamp_ms() - first_event_ts.unwrap_or(0.0);
        let elapsed_ms = relay_start.elapsed().as_millis() as f64;
        if target_offset_ms > elapsed_ms {
            let sleep_ms = (target_offset_ms - elapsed_ms) as u64;
            tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        }

        let (method, params) = event.to_cdp();
        let _ = conn.send(method, &params, Some(&session_id)).await;
    }

    let _ = conn
        .send(
            "Target.detachFromTarget",
            &serde_json::json!({"sessionId": session_id}),
            None,
        )
        .await;
    conn.close();
}

async fn close_with_error(mut client_ws: WebSocket, reason: &str) -> Result<(), axum::Error> {
    let body = serde_json::json!({
        "code": "ops_error",
        "channel": "input",
        "error": reason,
    });
    let _ = client_ws.send(Message::Text(body.to_string().into())).await;
    client_ws.close().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_move_round_trips() {
        let ev = OpsInputEvent::PointerMove {
            x: 12.5,
            y: 34.0,
            timestamp_ms: 1_234.5,
        };
        let json = serde_json::to_string(&ev).unwrap_or_default();
        let back: OpsInputEvent = serde_json::from_str(&json).unwrap();
        match back {
            OpsInputEvent::PointerMove { x, y, .. } => {
                assert!((x - 12.5).abs() < 1e-3);
                assert!((y - 34.0).abs() < 1e-3);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn pointer_down_converts_to_mousepressed() {
        let ev = OpsInputEvent::PointerDown {
            x: 10.0,
            y: 20.0,
            button: "left".into(),
            timestamp_ms: 0.0,
        };
        let (method, params) = ev.to_cdp();
        assert_eq!(method, "Input.dispatchMouseEvent");
        assert_eq!(params["type"], "mousePressed");
        assert_eq!(params["x"], 10.0);
        assert_eq!(params["button"], "left");
    }

    #[test]
    fn keydown_with_printable_key_gets_text_field() {
        let ev = OpsInputEvent::KeyDown {
            key: "a".into(),
            code: "KeyA".into(),
            modifiers: 0,
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert_eq!(params["text"], "a");
    }

    #[test]
    fn keydown_with_special_key_has_no_text() {
        let ev = OpsInputEvent::KeyDown {
            key: "Enter".into(),
            code: "Enter".into(),
            modifiers: 0,
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert!(params["text"].is_null(), "{params}");
    }

    #[test]
    fn keydown_printable_with_ctrl_has_no_text() {
        // Ctrl+A is select-all, not typing "a" — `text` must be suppressed so
        // chromium runs the shortcut instead of inserting the letter.
        let ev = OpsInputEvent::KeyDown {
            key: "a".into(),
            code: "KeyA".into(),
            modifiers: 2, // Ctrl
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert!(params["text"].is_null(), "{params}");
    }

    #[test]
    fn keydown_printable_with_meta_has_no_text() {
        let ev = OpsInputEvent::KeyDown {
            key: "r".into(),
            code: "KeyR".into(),
            modifiers: 4, // Meta/⌘
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert!(params["text"].is_null(), "{params}");
    }

    #[test]
    fn keydown_printable_with_shift_keeps_text() {
        // Shift is not a shortcut modifier — Shift+a still produces text.
        let ev = OpsInputEvent::KeyDown {
            key: "A".into(),
            code: "KeyA".into(),
            modifiers: 8, // Shift
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert_eq!(params["text"], "A");
    }

    #[test]
    fn backspace_carries_virtual_key_code() {
        // Without windowsVirtualKeyCode chromium ignores Backspace, so deletes
        // do nothing — the bug this guards against.
        let ev = OpsInputEvent::KeyDown {
            key: "Backspace".into(),
            code: "Backspace".into(),
            modifiers: 0,
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert_eq!(params["windowsVirtualKeyCode"], 8);
        assert_eq!(params["nativeVirtualKeyCode"], 8);
        assert!(params["text"].is_null(), "{params}");
    }

    #[test]
    fn enter_and_arrows_carry_virtual_key_code() {
        for (key, vk) in [("Enter", 13), ("ArrowLeft", 37), ("Delete", 46)] {
            let ev = OpsInputEvent::KeyDown {
                key: key.into(),
                code: key.into(),
                modifiers: 0,
                timestamp_ms: 0.0,
            };
            let (_, params) = ev.to_cdp();
            assert_eq!(params["windowsVirtualKeyCode"], vk, "{key}");
        }
    }

    #[test]
    fn letter_carries_uppercase_virtual_key_code() {
        // Ctrl+A needs VK 65 to trigger select-all even though text is dropped.
        let ev = OpsInputEvent::KeyDown {
            key: "a".into(),
            code: "KeyA".into(),
            modifiers: 2, // Ctrl
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert_eq!(params["windowsVirtualKeyCode"], 65);
        assert!(params["text"].is_null(), "{params}");
    }

    #[test]
    fn keyup_also_carries_virtual_key_code() {
        let ev = OpsInputEvent::KeyUp {
            key: "Backspace".into(),
            code: "Backspace".into(),
            modifiers: 0,
            timestamp_ms: 0.0,
        };
        let (_, params) = ev.to_cdp();
        assert_eq!(params["type"], "keyUp");
        assert_eq!(params["windowsVirtualKeyCode"], 8);
    }

    #[test]
    fn insert_text_converts_to_input_inserttext() {
        let ev = OpsInputEvent::InsertText {
            text: "SuperSecretPassword!".into(),
            timestamp_ms: 0.0,
        };
        let (method, params) = ev.to_cdp();
        assert_eq!(method, "Input.insertText");
        assert_eq!(params["text"], "SuperSecretPassword!");
    }

    #[test]
    fn insert_text_round_trips_from_snake_case_tag() {
        let json = r#"{"type":"insert_text","text":"héllo","timestamp_ms":5.0}"#;
        let back: OpsInputEvent = serde_json::from_str(json).unwrap();
        match back {
            OpsInputEvent::InsertText { text, .. } => assert_eq!(text, "héllo"),
            _ => panic!("wrong variant"),
        }
    }
}
