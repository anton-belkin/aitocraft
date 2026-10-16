//! A fake Craft app for tests: MCP over HTTP on 127.0.0.1 (JSON, SSE or chunked replies), a
//! `/events` stream, and its registry file.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Clone, Copy, PartialEq)]
pub enum Reply {
    Json,
    Sse,
    Chunked,
}

pub struct FakeApp {
    pub app: &'static str,
    pub title: &'static str,
    pub port: u16,
    pub calls: Arc<Mutex<Vec<(String, Value)>>>,
    pub events: Arc<Mutex<Vec<Value>>>,
    pub event_queries: Arc<Mutex<Vec<String>>>,
    pub inits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

pub fn tool(name: &str, desc: &str, props: Value) -> Value {
    json!({"name": name, "description": desc, "inputSchema": {"type": "object", "properties": props, "additionalProperties": false}, "annotations": {"readOnlyHint": true}})
}

/// The tools every Craft app has.
pub fn common_tools() -> Vec<Value> {
    vec![
        tool("get_context", "Overview of the open document.", json!({})),
        tool("list_questions", "Questions.", json!({"status": {"type": "string"}, "listener": {"type": "string"}})),
        tool("wait_for_questions", "Wait.", json!({"timeout_seconds": {"type": "number"}})),
        tool("answer_question", "Answer.", json!({"id": {"type": "string"}, "text": {"type": "string"}})),
        tool("list_documents", "The app's documents.", json!({})),
        tool("attach", "Attach.", json!({"document": {"type": "string"}, "label": {"type": "string"}})),
        tool("detach", "Detach.", json!({"listener": {"type": "string"}})),
    ]
}

impl FakeApp {
    pub fn start(app: &'static str, title: &'static str, extra: Vec<Value>, reply: Reply) -> FakeApp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut tools: Vec<Value> = common_tools().into_iter().filter(|t| !extra.iter().any(|e| e["name"] == t["name"])).collect();
        tools.extend(extra);
        let f = FakeApp {
            app,
            title,
            port,
            calls: Arc::default(),
            events: Arc::default(),
            event_queries: Arc::default(),
            inits: Arc::default(),
            stop: Arc::default(),
        };
        let (calls, events, queries, inits, stop) = (f.calls.clone(), f.events.clone(), f.event_queries.clone(), f.inits.clone(), f.stop.clone());
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let (calls, events, queries, inits, stop, tools) =
                    (calls.clone(), events.clone(), queries.clone(), inits.clone(), stop.clone(), tools.clone());
                std::thread::spawn(move || serve(s, title, &tools, reply, &calls, &events, &queries, &inits, &stop));
            }
        });
        f
    }

    pub fn mcp(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    /// Writes this app's registry file. `docs`: (doc uid, title, frontmost).
    pub fn register(&self, dir: &Path, docs: &[(&str, &str, bool)], focused_at: u64) -> PathBuf {
        let pid = std::process::id();
        let documents: Vec<Value> = docs
            .iter()
            .map(|(uid, title, front)| json!({"id": self.doc(uid), "title": title, "path": format!("/Users/test/{title}"), "frontmost": front, "openQuestions": 0, "listeners": []}))
            .collect();
        let v = json!({
            "protocol": "aitocraft/1", "app": self.app, "appTitle": self.title, "version": "0.1.0", "pid": pid,
            "mcp": self.mcp(), "heartbeat": crate::util::now_ms(), "focusedAt": focused_at, "documents": documents,
        });
        let p = dir.join(format!("{}-{pid}.json", self.app));
        std::fs::write(&p, v.to_string()).unwrap();
        p
    }

    pub fn doc(&self, uid: &str) -> String {
        format!("{}:{}:{uid}", self.app, std::process::id())
    }

    pub fn calls_of(&self, tool: &str) -> Vec<Value> {
        self.calls.lock().unwrap().iter().filter(|(n, _)| n == tool).map(|(_, a)| a.clone()).collect()
    }

    pub fn push_event(&self, e: Value) {
        self.events.lock().unwrap().push(e);
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<(String, String, Vec<u8>)> {
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut len = 0;
    loop {
        let mut line = String::new();
        r.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some((method, path, body))
}

fn respond(w: &mut TcpStream, reply: Reply, v: &Value, session: bool) {
    let sid = if session { "Mcp-Session-Id: fake-session\r\n" } else { "" };
    let body = v.to_string();
    let _ = match reply {
        Reply::Json => write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{sid}Content-Length: {}\r\n\r\n{body}", body.len()),
        Reply::Sse => {
            let ev = format!(": hello\n\nevent: message\ndata: {body}\n\n");
            write!(w, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{sid}\r\n{ev}")
        }
        Reply::Chunked => {
            let (a, b) = body.split_at(body.len() / 2);
            write!(
                w,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{sid}Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{a}\r\n{:x}\r\n{b}\r\n0\r\n\r\n",
                a.len(),
                b.len()
            )
        }
    };
    let _ = w.flush();
}

#[allow(clippy::too_many_arguments)]
fn serve(
    s: TcpStream,
    title: &str,
    tools: &[Value],
    reply: Reply,
    calls: &Mutex<Vec<(String, Value)>>,
    events: &Mutex<Vec<Value>>,
    queries: &Mutex<Vec<String>>,
    inits: &AtomicUsize,
    stop: &AtomicBool,
) {
    let Ok(mut w) = s.try_clone() else { return };
    let mut r = BufReader::new(s);
    let Some((method, path, body)) = read_request(&mut r) else { return };
    if method == "GET" && path.starts_with("/events") {
        let doc = path.split("document=").nth(1).map(|d| d.split('&').next().unwrap_or("").replace("%3A", ":")).unwrap_or_default();
        queries.lock().unwrap().push(path.clone());
        if write!(w, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\r\n").is_err() {
            return;
        }
        while !stop.load(Ordering::Acquire) {
            let mine: Vec<Value> = {
                let mut ev = events.lock().unwrap();
                let (mine, rest): (Vec<Value>, Vec<Value>) =
                    ev.drain(..).partition(|e| e.get("document").and_then(Value::as_str).is_none_or(|d| d == doc));
                *ev = rest;
                mine
            };
            for e in mine {
                if writeln!(w, "{e}").and_then(|()| w.flush()).is_err() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        return;
    }
    if method != "POST" {
        let _ = write!(w, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else { return };
    let Some(id) = msg.get("id").cloned() else {
        let _ = write!(w, "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        return;
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
        "initialize" => {
            inits.fetch_add(1, Ordering::AcqRel);
            respond(
                &mut w,
                reply,
                &json!({"jsonrpc": "2.0", "id": id, "result": {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": title}}}),
                true,
            );
            return;
        }
        "tools/list" => json!({"tools": tools}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            calls.lock().unwrap().push((name.clone(), args.clone()));
            let out = match name.as_str() {
                "attach" => {
                    let doc = args.get("document").and_then(Value::as_str).unwrap_or("");
                    json!({"listener": format!("L-{doc}"), "document": doc, "title": "?", "app": title})
                }
                "wait_for_questions" => json!([]),
                _ => json!({"app": title, "tool": name, "args": args}),
            };
            json!({"content": [{"type": "text", "text": out.to_string()}], "isError": false})
        }
        _ => {
            respond(&mut w, reply, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no"}}), false);
            return;
        }
    };
    respond(&mut w, reply, &json!({"jsonrpc": "2.0", "id": id, "result": result}), false);
}
