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

/// Events besides questions that are passed on (PROTOCOL.md §6).
pub const EVENTS: &[&str] = &["withdrawn", "proposal_applied", "proposal_rejected", "proposal_outdated"];

/// `topic 3` (or the topic's id) and the topic's id, from a question's or event's `topic`.
fn topic_of(v: &Value) -> Option<(String, String)> {
    match v.get("topic")? {
        Value::Object(t) => {
            let id = t.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let name = match t.get("number").filter(|n| !n.is_null()) {
                Some(Value::String(n)) => format!("topic {n}"),
                Some(n) => format!("topic {n}"),
                None if !id.is_empty() => format!("topic {id}"),
                None => return None,
            };
            Some((name, id))
        }
        Value::String(s) if !s.is_empty() => Some((format!("topic {s}"), s.clone())),
        Value::Number(n) => Some((format!("topic {n}"), n.to_string())),
        _ => None,
    }
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// "Pending proposals nearby: topic 2 option A (…)" for a question's `pendingNearby`.
fn pending_nearby(q: &Value) -> Option<String> {
    let list = q.get("pendingNearby").and_then(Value::as_array).filter(|a| !a.is_empty())?;
    let items: Vec<String> = list
        .iter()
        .take(20)
        .map(|p| {
            let topic = topic_of(p).map(|(n, _)| n).unwrap_or_else(|| "a topic".into());
            let option = str_of(p, "option").map(|o| format!(" option {o}")).unwrap_or_default();
            let summary = str_of(p, "summary").map(|s| format!(" ({})", short(&s, 160))).unwrap_or_default();
            format!("{topic}{option}{summary}")
        })
        .collect();
    Some(format!("Pending proposals nearby: {}. Take them into account; don't propose against them blind.", items.join("; ")))
}

/// The channel notification for one question, ping or event of a document.
pub fn notification(q: &Value, document: &str, title: &str, app_title: &str) -> Option<Value> {
    if let Some(ev) = q.get("event").and_then(Value::as_str).filter(|e| *e != "question") {
        return event_notification(ev, q, document, title, app_title);
    }
    let id = q.get("id").or_else(|| q.get("question_id")).and_then(Value::as_str).filter(|s| !s.is_empty())?;
    let kind = q.get("kind").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("question");
    let topic = topic_of(q);
    let content = if kind == "ping" {
        format!(
            "Connection check from {app_title} ({title}): answer it at once with \"here\" — answer_question {{document: \"{document}\", id: \"{id}\", text: \"here\"}}. Don't investigate."
        )
    } else {
        let text = q.get("question").or_else(|| q.get("text")).and_then(Value::as_str).unwrap_or("").trim();
        let on = place(q).map(|p| format!(", on {p}")).unwrap_or_default();
        let mut c = format!("Question from the user in {app_title}, {title}{on}: {text}");
        if let Some((name, _)) = &topic {
            let t = q.get("topic");
            let status = t.and_then(|t| str_of(t, "status")).or_else(|| str_of(q, "topicStatus"));
            let status_text = t.and_then(|t| str_of(t, "statusText"));
            let about = match (status, status_text) {
                (Some(s), Some(st)) => format!(" (status {s}: {})", short(&st, 200)),
                (Some(s), None) => format!(" (status {s})"),
                _ => String::new(),
            };
            c.push_str(&format!("\nIn {name}{about}: get_topic for its places, notes and discussion so far."));
        }
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
        if let Some(p) = pending_nearby(q) {
            c.push_str(&format!("\n{p}"));
        }
        if let Some(fc) = q.get("fileContext").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
            c.push_str(&format!("\nFile context: {}", short(fc, 600)));
        }
        c.push_str(&format!(
            "\n(question {id}, document {document}: claim_question, investigate with the document's tools, then answer_question {{document, id, text}} with links inside the text. To change the document, propose with run_command {{document, topic, …}}; don't ask for confirmation in the chat.)"
        ));
        c
    };
    let mut meta = json!({"document": document, "question_id": id, "kind": kind, "app": app_title});
    if let Some((_, tid)) = topic.filter(|(_, t)| !t.is_empty()) {
        meta["topic"] = json!(tid);
    }
    Some(json!({
        "jsonrpc": "2.0",
        "method": "notifications/claude/channel",
        // Meta keys must be identifiers (letters, digits, underscores).
        "params": {"content": content, "meta": meta},
    }))
}

fn event_notification(ev: &str, e: &Value, document: &str, title: &str, app_title: &str) -> Option<Value> {
    if !EVENTS.contains(&ev) {
        return None;
    }
    let topic = topic_of(e);
    let tname = topic.as_ref().map(|(n, _)| n.clone()).unwrap_or_else(|| "a topic".into());
    let option = str_of(e, "option");
    let opt = option.as_ref().map(|o| format!(" option {o}")).unwrap_or_default();
    let named = str_of(e, "title").map(|t| format!(" (“{}”)", short(&t, 120))).unwrap_or_default();
    let content = match ev {
        "withdrawn" => {
            format!("The user withdrew question {} in {app_title}, {title}: drop it, don't answer it.", str_of(e, "id").unwrap_or_else(|| "?".into()))
        }
        "proposal_applied" => format!("The user applied{opt}{named} of your proposal in {tname} of {title} ({app_title}). Nothing to do."),
        "proposal_rejected" => {
            let why = str_of(e, "comment").map(|c| format!(": “{}”", short(&c, 600))).unwrap_or_else(|| " without a comment".into());
            format!(
                "The user rejected{opt}{named} of your proposal in {tname} of {title} ({app_title}){why}. A comment also arrives as a follow-up question in the topic: redo the proposal there with run_command {{document, topic, …}}."
            )
        }
        _ => format!(
            "Your proposal{opt}{named} in {tname} of {title} ({app_title}) is out of date: the document changed under it. Read the places again and redo it with run_command {{document, topic, …}}."
        ),
    };
    let mut meta = json!({"document": document, "kind": ev, "app": app_title});
    if let Some((_, tid)) = topic.filter(|(_, t)| !t.is_empty()) {
        meta["topic"] = json!(tid);
    }
    if let Some(o) = option {
        meta["option"] = json!(o);
    }
    if ev == "withdrawn"
        && let Some(id) = str_of(e, "id")
    {
        meta["question_id"] = json!(id);
    }
    Some(json!({"jsonrpc": "2.0", "method": "notifications/claude/channel", "params": {"content": content, "meta": meta}}))
}

/// What a line is sent once by: a question's id, or the event and its id (or the whole line).
fn dedup_key(q: &Value, line: &str) -> String {
    match q.get("event").and_then(Value::as_str).filter(|e| *e != "question") {
        Some(ev) => format!("{ev}:{}", str_of(q, "id").unwrap_or_else(|| line.to_string())),
        None => q.get("id").or_else(|| q.get("question_id")).and_then(Value::as_str).unwrap_or("").to_string(),
    }
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
                    // An app that doesn't filter by document yet sends every document's questions.
                    if q.get("document").and_then(Value::as_str).is_some_and(|d| d != t.document) {
                        continue;
                    }
                    let Some(n) = notification(&q, &t.document, &t.title, &t.app_title) else { continue };
                    let qid = dedup_key(&q, data);
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
    fn topics_proposals_and_file_context_are_said() {
        let q = json!({"id": "q4", "text": "Spread it?", "range": "C9", "topic": {"id": "t7", "number": 3, "status": "open", "statusText": "waiting for the CFO"},
            "pendingNearby": [{"topic": {"id": "t2", "number": 2}, "option": "A", "summary": "C10 = C9/3"}], "fileContext": "Budget 2027 for the board."});
        let n = notification(&q, "gridcraft:1:1", "budget.xlsx", "GridCraft").unwrap();
        let c = n["params"]["content"].as_str().unwrap();
        assert!(c.contains("In topic 3 (status open: waiting for the CFO)"), "{c}");
        assert!(c.contains("Pending proposals nearby: topic 2 option A (C10 = C9/3)"), "{c}");
        assert!(c.contains("File context: Budget 2027 for the board."), "{c}");
        assert!(c.contains("run_command {document, topic"), "{c}");
        assert_eq!(n["params"]["meta"]["topic"], "t7");

        let e = json!({"event": "proposal_rejected", "id": "e1", "topic": {"id": "t7", "number": 3}, "option": "A", "title": "3 years", "comment": "Only 2"});
        let n = notification(&e, "gridcraft:1:1", "budget.xlsx", "GridCraft").unwrap();
        let c = n["params"]["content"].as_str().unwrap();
        assert!(c.contains("rejected option A (“3 years”) of your proposal in topic 3 of budget.xlsx (GridCraft): “Only 2”"), "{c}");
        assert_eq!(
            n["params"]["meta"],
            json!({"document": "gridcraft:1:1", "kind": "proposal_rejected", "app": "GridCraft", "topic": "t7", "option": "A"})
        );
        let w = notification(&json!({"event": "withdrawn", "id": "q5", "document": "d"}), "d", "memo.docx", "WordCraft").unwrap();
        assert_eq!(w["params"]["meta"]["question_id"], "q5");
        assert!(w["params"]["content"].as_str().unwrap().contains("withdrew question q5"));
        assert!(
            notification(&json!({"event": "proposal_outdated", "topic": "t9"}), "d", "x", "X").unwrap()["params"]["content"]
                .as_str()
                .unwrap()
                .contains("out of date")
        );
        assert!(notification(&json!({"event": "hello"}), "d", "x", "X").is_none());
        assert_eq!(dedup_key(&json!({"event": "withdrawn", "id": "q5"}), ""), "withdrawn:q5");
        assert_eq!(dedup_key(&json!({"id": "q5"}), ""), "q5");
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
