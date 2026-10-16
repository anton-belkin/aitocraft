//! MCP over stdio: newline-delimited JSON-RPC 2.0 on stdin/stdout, nothing else on stdout.
//! Tool calls run on their own threads (a `wait_for_questions` of one conversation must not hold
//! up another's calls); everything else is answered in order.

use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::hub::Hub;

/// Longest message accepted on stdin.
pub const MAX_LINE: usize = 16 * 1024 * 1024;
/// Most tool calls running at once; more wait their turn in the reading thread.
const MAX_INFLIGHT: usize = 64;

enum Line {
    Text(Vec<u8>),
    TooLong,
    End,
}

fn read_line(r: &mut dyn BufRead) -> std::io::Result<Line> {
    let mut buf = Vec::new();
    let n = r.take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(Line::End);
    }
    if buf.len() > MAX_LINE && buf.last() != Some(&b'\n') {
        // Skip the rest of this line.
        loop {
            let mut rest = Vec::new();
            let n = r.take(1024 * 1024).read_until(b'\n', &mut rest)?;
            if n == 0 || rest.last() == Some(&b'\n') {
                break;
            }
        }
        return Ok(Line::TooLong);
    }
    Ok(Line::Text(buf))
}

fn parse_error(msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": msg}})
}

/// Serves until `input` closes, then waits a little for calls still running.
pub fn serve(hub: Arc<Hub>, input: impl Read) -> std::io::Result<()> {
    let mut r = BufReader::new(input);
    let inflight = Arc::new(AtomicUsize::new(0));
    loop {
        let bytes = match read_line(&mut r)? {
            Line::End => break,
            Line::TooLong => {
                hub.send(&parse_error("message too long"));
                continue;
            }
            Line::Text(b) => b,
        };
        let text = String::from_utf8_lossy(&bytes);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => {
                hub.send(&parse_error("parse error"));
                continue;
            }
        };
        match msg {
            Value::Array(batch) => {
                let replies: Vec<Value> = batch.iter().take(100).filter_map(|m| hub.handle(m)).collect();
                if !replies.is_empty() {
                    hub.send(&Value::Array(replies));
                }
            }
            Value::Object(_) => {
                let is_call = msg.get("method").and_then(Value::as_str) == Some("tools/call") && msg.get("id").is_some();
                if is_call && inflight.load(Ordering::Acquire) < MAX_INFLIGHT {
                    inflight.fetch_add(1, Ordering::AcqRel);
                    let (h, n) = (hub.clone(), inflight.clone());
                    let worker = {
                        let msg = msg.clone();
                        std::thread::Builder::new().name("aitocraft-call".into()).spawn(move || {
                            answer(&h, &msg);
                            n.fetch_sub(1, Ordering::AcqRel);
                        })
                    };
                    if worker.is_err() {
                        inflight.fetch_sub(1, Ordering::AcqRel);
                        answer(&hub, &msg);
                    }
                } else {
                    answer(&hub, &msg);
                }
            }
            _ => {
                hub.send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request"}}));
            }
        }
    }
    let until = Instant::now() + Duration::from_secs(3);
    while inflight.load(Ordering::Acquire) > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn answer(hub: &Hub, msg: &Value) {
    if let Some(reply) = hub.handle(msg) {
        let wanted = msg.get("id").is_none_or(|id| hub.wanted(id));
        if wanted {
            hub.send(&reply);
        }
    }
}
