//! "Send to my Claude Code session" (Claude Code channels): after `attach`, the hub follows the
//! document's `GET /events?document=<id>` stream and passes each question on to the session as
//! `notifications/claude/channel`.

use std::collections::{HashSet, VecDeque};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::http::{self, Url};
use crate::util::{lock, pct, short};

const MAX_LINE: usize = 1024 * 1024;
const SEEN: usize = 4096;

/// Where a question was asked, said the way each app says it.
pub fn place(q: &Value) -> Option<String> {
    let s = |k: &str| q.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    if let Some(p) = s("place") {
        return Some(p);
    }
    if let Some(p) = q.pointer("/where/label").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        return Some(p.to_string());
    }
    if let Some(r) = s("range") {
        return Some(match s("label") {
            Some(l) => format!("{r} ({l})"),
            None => r,
        });
    }
    if let Some(n) = q.get("paragraph").and_then(Value::as_u64) {
        return Some(match s("quote") {
            Some(quote) => format!("paragraph {n} (“{}”)", short(&quote, 120)),
            None => format!("paragraph {n}"),
        });
    }
    if let Some(n) = q.get("slide").or_else(|| q.pointer("/where/slide")).and_then(Value::as_u64) {
        return Some(format!("slide {n}"));
    }
    None
}

fn ref_label(r: &Value) -> String {
    for k in ["label", "place", "range", "ref"] {
        if let Some(s) = r.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()) {
            return s.to_string();
        }
    }
    match r.get("anchor") {
        Some(Value::String(s)) => s.clone(),
        Some(a) => short(&a.to_string(), 160),
        None => short(&r.to_string(), 160),
    }
}

/// The channel notification for one question (or ping) of a document.
pub fn notification(q: &Value, document: &str, title: &str, app_title: &str) -> Option<Value> {
    let id = q.get("id").or_else(|| q.get("question_id")).and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let kind = q.get("kind").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("question");
    let content = if kind == "ping" {
        format!(
            "Connection check from {app_title} ({title}): answer it at once with \"here\" — answer_question {{document: \"{document}\", id: \"{id}\", text: \"here\"}}. Don't investigate."
        )
    } else {
        let text = q.get("question").or_else(|| q.get("text")).and_then(Value::as_str).unwrap_or("").trim();
        let on = place(q).map(|p| format!(", on {p}")).unwrap_or_default();
        let mut c = format!("Question from the user in {app_title}, {title}{on}: {text}");
        if let Some(refs) = q.get("refs").and_then(Value::as_array).filter(|r| !r.is_empty())
            && !text.contains("⟦1:")
        {
            let list: Vec<String> = refs
                .iter()
                .take(50)
                .map(|r| {
                    let n = r.get("n").map(|n| n.to_string()).unwrap_or_else(|| "?".into());
                    format!("⟦{}⟧ = {}", n.trim_matches('"'), ref_label(r))
                })
                .collect();
            c.push_str(&format!("\nWhat the user pointed at: {}", list.join("; ")));
        }
        c.push_str(&format!(
            "\n(question {id}, document {document}: claim_question, investigate with the document's tools, then answer_question {{document, id, text, links}}.)"
        ));
        c
    };
    Some(json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        "params": {
            "content": content,
            // Keys must be identifiers (letters, digits, underscores).
            "meta": {"document": document, "question_id": id, "kind": kind, "app": app_title},
        },
    }))
}

/// A running follower; dropping it stops the thread.
pub struct Follower {
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

impl Drop for Follower {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(s) = lock(&self.socket).take() {
            http::shut(&s);
        }
    }
}

/// What a follower needs to know.
pub struct Target {
    pub events: Url,
    pub document: String,
    pub title: String,
    pub app_title: String,
}

/// The `/events` URL of a document on the server at `mcp`.
pub fn events_url(mcp: &Url, document: &str, listener: Option<&str>) -> Url {
    let mut path = format!("/events?document={}", pct(document));
    if let Some(l) = listener {
        path.push_str(&format!("&listener={}", pct(l)));
    }
    mcp.with_path(&path)
}

/// Follows the document's question stream, sending each new question through `emit`, and
/// reconnects while the app is away. `emit` returns false when the output is gone.
pub fn follow(t: Target, emit: Arc<dyn Fn(&Value) -> bool + Send + Sync>) -> Option<Follower> {
    let stop = Arc::new(AtomicBool::new(false));
    let socket: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));
    let f = Follower { stop: stop.clone(), socket: socket.clone() };
    let spawned = std::thread::Builder::new().name("aitocraft-channel".into()).spawn(move || {
        let mut seen: HashSet<String> = HashSet::new();
        let mut order: VecDeque<String> = VecDeque::new();
        while !stop.load(Ordering::Acquire) {
            let headers = [("Accept", "application/x-ndjson, text/event-stream".to_string())];
            // No read timeout: the stream is quiet until someone asks; detach shuts the socket.
            if let Ok((head, mut body, stream)) = http::request("GET", &t.events, &headers, None, Duration::from_secs(24 * 3600))
                && head.status == 200
            {
                if let Ok(s) = stream.try_clone() {
                    *lock(&socket) = Some(s);
                }
                if stop.load(Ordering::Acquire) {
                    break;
                }
                while let Ok(Some(line)) = http::next_line(&mut body, MAX_LINE) {
                    let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line.trim());
                    if data.is_empty() || data.starts_with(':') || line.starts_with("event:") {
                        continue;
                    }
                    let Ok(q) = serde_json::from_str::<Value>(data) else { continue };
                    if q.get("event").and_then(Value::as_str).is_some_and(|e| e != "question") {
                        continue;
                    }
                    // An app that doesn't filter by document yet sends every document's questions.
                    if q.get("document").and_then(Value::as_str).is_some_and(|d| d != t.document) {
                        continue;
                    }
                    let Some(n) = notification(&q, &t.document, &t.title, &t.app_title) else { continue };
                    let qid = n.pointer("/params/meta/question_id").and_then(Value::as_str).unwrap_or("").to_string();
                    if !seen.insert(qid.clone()) {
                        continue;
                    }
                    order.push_back(qid);
                    if order.len() > SEEN
                        && let Some(old) = order.pop_front()
                    {
                        seen.remove(&old);
                    }
                    if stop.load(Ordering::Acquire) || !emit(&n) {
                        return;
                    }
                }
                lock(&socket).take();
            }
            for _ in 0..20 {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
    spawned.ok().map(|_| f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_app_says_where() {
        assert_eq!(place(&json!({"range": "Sheet1!C16", "label": "Revenue"})).unwrap(), "Sheet1!C16 (Revenue)");
        assert_eq!(place(&json!({"paragraph": 13, "quote": "within 30 days"})).unwrap(), "paragraph 13 (“within 30 days”)");
        assert_eq!(place(&json!({"where": {"slide": 4, "label": "slide 4 · Pricing"}})).unwrap(), "slide 4 · Pricing");
        assert_eq!(place(&json!({"place": "B5", "range": "x"})).unwrap(), "B5");
        assert_eq!(place(&json!({})), None);
    }

    #[test]
    fn refs_are_spelled_out() {
        let q = json!({"id": "q2", "text": "Why is ⟦1⟧ bigger than ⟦2⟧?", "refs": [{"n": 1, "range": "C16"}, {"n": 2, "anchor": {"slide": 3}}]});
        let n = notification(&q, "deckcraft:9:1", "pitch.pptx", "DeckCraft").unwrap();
        let c = n["params"]["content"].as_str().unwrap();
        assert!(c.contains("⟦1⟧ = C16"), "{c}");
        assert!(c.contains("⟦2⟧ = {\"slide\":3}"), "{c}");
    }
}
