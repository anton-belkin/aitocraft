//! Small helpers shared by the modules.

use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Locks a mutex, carrying on after a panic elsewhere (nothing here leaves state half-written).
pub fn lock<T: ?Sized>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Whether a process with this pid exists (on Unix; elsewhere the heartbeat decides alone).
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    let Ok(p) = i32::try_from(pid) else { return false };
    if p <= 0 {
        return false;
    }
    // Signal 0 checks for the process without touching it; EPERM means it exists but isn't ours.
    let r = unsafe { kill(p, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1)
}

#[cfg(not(unix))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}

/// Percent-encodes everything but unreserved characters (for a query value).
pub fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// At most `n` characters of `s`, with an ellipsis when cut.
pub fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut t: String = s.chars().take(n).collect();
    t.push('…');
    t
}

/// The text parts of a tool result, joined.
pub fn text_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
}

/// A successful tool result carrying `v` as pretty JSON text.
pub fn tool_ok(v: &Value) -> Value {
    let text = serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string());
    json!({"content": [{"type": "text", "text": text}], "isError": false})
}

/// A failed tool result: what went wrong, said so the agent can act on it.
pub fn tool_err(msg: impl Into<String>) -> Value {
    json!({"content": [{"type": "text", "text": msg.into()}], "isError": true})
}

/// The string at `key`, if it is a non-empty string.
pub fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}
