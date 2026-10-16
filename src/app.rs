//! A connection to one app's MCP server (Streamable HTTP): initialize once, keep the session id if
//! the server gives one, and accept a reply either as JSON or as a server-sent event.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{self, Url};
use crate::util::lock;

/// The MCP version the hub asks the apps for.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
const INIT_TIMEOUT: Duration = Duration::from_secs(5);

pub struct AppConn {
    pub url: Url,
    /// `GridCraft`: how errors name the app.
    pub title: String,
    session: Mutex<Option<String>>,
    version: Mutex<Option<String>>,
    ready: AtomicBool,
    ids: AtomicU64,
}

enum PostError {
    Unreachable(String),
    Status(u16, String),
    Protocol(String),
}

impl AppConn {
    pub fn new(url: Url, title: &str) -> AppConn {
        AppConn {
            url,
            title: title.to_string(),
            session: Mutex::new(None),
            version: Mutex::new(None),
            ready: AtomicBool::new(false),
            ids: AtomicU64::new(1),
        }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![("Content-Type", "application/json".to_string()), ("Accept", "application/json, text/event-stream".to_string())];
        if let Some(s) = lock(&self.session).clone() {
            h.push(("Mcp-Session-Id", s));
        }
        if let Some(v) = lock(&self.version).clone() {
            h.push(("MCP-Protocol-Version", v));
        }
        h
    }

    /// Posts one message; the reply to it, if it has an id.
    fn post(&self, msg: &Value, timeout: Duration) -> Result<Option<Value>, PostError> {
        let body = msg.to_string();
        let (head, mut r, _stream) =
            http::request("POST", &self.url, &self.headers(), Some(body.as_bytes()), timeout).map_err(PostError::Unreachable)?;
        if let Some(sid) = head.header("mcp-session-id").filter(|s| !s.is_empty()) {
            *lock(&self.session) = Some(sid.to_string());
        }
        let Some(id) = msg.get("id") else { return Ok(None) };
        if !(200..300).contains(&head.status) {
            let text = http::read_all(&mut r, 4096).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            return Err(PostError::Status(head.status, text));
        }
        if head.status == 202 {
            return Err(PostError::Protocol("the server accepted the request but sent no reply".into()));
        }
        let ctype = head.header("content-type").unwrap_or("").to_ascii_lowercase();
        if ctype.starts_with("text/event-stream") {
            loop {
                match http::next_sse_data(&mut r, http::MAX_BODY) {
                    Ok(Some(data)) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&data)
                            && let Some(found) = reply_to(&v, id)
                        {
                            return Ok(Some(found));
                        }
                    }
                    Ok(None) => return Err(PostError::Protocol("the reply stream ended without the reply".into())),
                    Err(e) => return Err(PostError::Unreachable(e)),
                }
            }
        }
        let bytes = http::read_all(&mut r, http::MAX_BODY).map_err(PostError::Unreachable)?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| PostError::Protocol(format!("the reply isn't JSON: {e}")))?;
        reply_to(&v, id).map(Some).ok_or_else(|| PostError::Protocol("the reply doesn't answer the request".into()))
    }

    fn describe(&self, e: PostError) -> String {
        match e {
            PostError::Unreachable(m) => format!("{} isn't answering at {} ({m}). Is it still open?", self.title, self.url),
            PostError::Status(s, body) => format!(
                "{} answered HTTP {s} at {}{}",
                self.title,
                self.url,
                if body.is_empty() { String::new() } else { format!(": {}", crate::util::short(body.trim(), 300)) }
            ),
            PostError::Protocol(m) => format!("{}: {m}", self.title),
        }
    }

    fn ensure_init(&self) -> Result<(), String> {
        if self.ready.load(Ordering::Acquire) {
            return Ok(());
        }
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": "aitocraft", "title": "AItoCraft", "version": env!("CARGO_PKG_VERSION")}},
        });
        let reply = self.post(&msg, INIT_TIMEOUT).map_err(|e| self.describe(e))?.unwrap_or(Value::Null);
        if let Some(err) = reply.get("error") {
            return Err(format!("{} refused to start a session: {}", self.title, err.get("message").and_then(Value::as_str).unwrap_or("error")));
        }
        if let Some(v) = reply.pointer("/result/protocolVersion").and_then(Value::as_str) {
            *lock(&self.version) = Some(v.to_string());
        }
        let _ = self.post(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), INIT_TIMEOUT);
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    /// One JSON-RPC request; its `result`, or the error said plainly.
    pub fn rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        self.ensure_init()?;
        let msg = json!({"jsonrpc": "2.0", "id": self.ids.fetch_add(1, Ordering::Relaxed), "method": method, "params": params});
        let reply = match self.post(&msg, timeout) {
            // The app restarted on the same port, or forgot our session: start a new one, once.
            Err(PostError::Status(400 | 404, _)) if lock(&self.session).is_some() => {
                *lock(&self.session) = None;
                self.ready.store(false, Ordering::Release);
                self.ensure_init()?;
                self.post(&msg, timeout)
            }
            r => r,
        }
        .map_err(|e| {
            // Next time, start over (the app may have restarted).
            self.ready.store(false, Ordering::Release);
            self.describe(e)
        })?
        .unwrap_or(Value::Null);
        if let Some(err) = reply.get("error") {
            return Err(format!("{}: {}", self.title, err.get("message").and_then(Value::as_str).unwrap_or("error")));
        }
        Ok(reply.get("result").cloned().unwrap_or(Value::Null))
    }

    /// The app's tools (all pages).
    pub fn list_tools(&self) -> Result<Vec<Value>, String> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let r = self.rpc("tools/list", params, INIT_TIMEOUT)?;
            for t in r.get("tools").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
                if t.get("name").and_then(Value::as_str).is_some_and(|n| !n.is_empty()) {
                    tools.push(t.clone());
                }
            }
            match r.get("nextCursor").and_then(Value::as_str) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(tools)
    }

    /// Calls a tool; the app's result as it gave it (`content`, `isError`, …).
    pub fn call_tool(&self, name: &str, args: &Value, timeout: Duration) -> Result<Value, String> {
        self.rpc("tools/call", json!({"name": name, "arguments": args}), timeout)
    }
}

fn reply_to(v: &Value, id: &Value) -> Option<Value> {
    match v {
        Value::Array(a) => a.iter().find(|m| m.get("id") == Some(id)).cloned(),
        m if m.get("id") == Some(id) && (m.get("result").is_some() || m.get("error").is_some()) => Some(m.clone()),
        _ => None,
    }
}

/// The JSON object a tool result's text holds, if it holds one.
pub fn result_json(result: &Value) -> Option<Value> {
    if let Some(s) = result.get("structuredContent").filter(|s| s.is_object()) {
        return Some(s.clone());
    }
    serde_json::from_str::<Value>(crate::util::text_of(result).trim()).ok()
}
