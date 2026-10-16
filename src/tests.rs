//! The hub against fake apps: registry, attach, routing, refusals, tool lists, channel events, stdio.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::fake::{FakeApp, Reply, tool};
use crate::hub::{Config, Hub, Out};
use crate::registry::{self, Verdict};
use crate::util::{now_ms, text_of};

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Buf {
    fn lines(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap().lines().map(|l| serde_json::from_str(l).expect(l)).collect()
    }
    fn wait_for(&self, ms: u64, f: impl Fn(&Value) -> bool) -> Option<Value> {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            if let Some(v) = self.lines().into_iter().find(|v| f(v)) {
                return Some(v);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aitocraft-test-{name}-{}", std::process::id())).join("agents");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn hub_in(dir: &Path, cache: Option<PathBuf>) -> (Arc<Hub>, Buf) {
    let buf = Buf::default();
    let out: Out = Arc::new(Mutex::new(Box::new(buf.clone())));
    let cfg = Config { registry_dir: Some(dir.to_path_buf()), cache_path: cache, channels: None, cwd_name: Some("acme-project".into()) };
    (Hub::new(cfg, out), buf)
}

fn init(hub: &Hub, client: &str) {
    let r = hub.handle(&json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": client, "version": "1"}}})).unwrap();
    assert_eq!(r["result"]["serverInfo"]["name"], "aitocraft");
    assert!(hub.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
}

/// (is error, text, the JSON the text holds).
fn call(hub: &Hub, name: &str, args: Value) -> (bool, String, Value) {
    let r = hub.handle(&json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": name, "arguments": args}})).unwrap();
    let res = &r["result"];
    let text = text_of(res);
    (res["isError"] == true, text.clone(), serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn tools(hub: &Hub) -> Vec<Value> {
    hub.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})).unwrap()["result"]["tools"].as_array().unwrap().clone()
}

#[test]
fn registry_files_and_staleness() {
    let d = dir("registry");
    let me = std::process::id();
    let now = now_ms();
    let good = json!({"protocol": "aitocraft/1", "app": "GridCraft", "pid": me, "mcp": "http://127.0.0.1:7980/mcp", "heartbeat": now,
        "documents": [{"id": "gridcraft:1:3", "title": "budget.xlsx", "path": "/a/budget.xlsx", "frontmost": true}, {"id": "gridcraft:1:4", "path": "/a/plan.xlsx"}, {"title": "no id"}]});
    std::fs::write(d.join(format!("gridcraft-{me}.json")), good.to_string()).unwrap();
    // A process that has exited: its file is removed.
    let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let mut gone = good.clone();
    gone["pid"] = json!(dead);
    std::fs::write(d.join(format!("wordcraft-{dead}.json")), gone.to_string()).unwrap();
    // Alive but silent for a minute: ignored, kept.
    let mut old = good.clone();
    old["heartbeat"] = json!(now - 60_000);
    std::fs::write(d.join("deckcraft-1.json"), old.to_string()).unwrap();
    let mut v2 = good.clone();
    v2["protocol"] = json!("aitocraft/2");
    std::fs::write(d.join("future-1.json"), v2.to_string()).unwrap();
    let mut remote = good.clone();
    remote["mcp"] = json!("http://10.1.2.3:7980/mcp");
    std::fs::write(d.join("remote-1.json"), remote.to_string()).unwrap();
    std::fs::write(d.join("broken-1.json"), "{ not json").unwrap();
    std::fs::write(d.join("huge-1.json"), " ".repeat(2 * 1024 * 1024)).unwrap();
    std::fs::write(d.join(".gridcraft-1.json.tmp"), good.to_string()).unwrap();

    let apps = registry::scan(&d, now, &crate::util::pid_alive);
    assert_eq!(apps.len(), 1, "{apps:?}");
    let a = &apps[0];
    assert_eq!((a.app.as_str(), a.title.as_str(), a.mcp.port), ("gridcraft", "Gridcraft", 7980));
    assert_eq!(a.documents.len(), 2, "a document without an id is skipped");
    assert_eq!(a.documents[1].title, "plan.xlsx", "the title falls back to the file name");
    assert!(a.documents[0].frontmost);
    assert!(!d.join(format!("wordcraft-{dead}.json")).exists(), "the dead app's file is removed");
    assert!(d.join("deckcraft-1.json").exists(), "a stale file of a live pid is kept");

    assert_eq!(registry::judge("x", &old.to_string(), now, &|_| true), Verdict::Stale);
    assert_eq!(registry::judge("x", &good.to_string(), now, &|_| false), Verdict::Dead);
    assert!(matches!(registry::judge("x", &v2.to_string(), now, &|_| true), Verdict::Invalid(_)));
    assert!(registry::protocol_ok("aitocraft/1.3"));
    assert!(!registry::protocol_ok("aitocraft/10"));
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn attach_by_id_by_name_frontmost_and_ambiguous() {
    let d = dir("attach");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![tool("read_range", "Read cells.", json!({"range": {"type": "string"}}))], Reply::Json);
    let word = FakeApp::start("wordcraft", "WordCraft", vec![], Reply::Sse);
    let t = now_ms();
    grid.register(&d, &[("1", "budget.xlsx", true), ("2", "budget-2027.xlsx", false)], t - 5000);
    word.register(&d, &[("1", "memo.docx", true)], t - 1000);
    let (hub, _out) = hub_in(&d, None);
    init(&hub, "claude-ai");

    let (err, text, v) = call(&hub, "attach", json!({"document": grid.doc("2"), "label": "Acme"}));
    assert!(!err, "{text}");
    assert_eq!(v["document"], grid.doc("2"));
    assert_eq!(v["listener"], format!("L-{}", grid.doc("2")));
    assert_eq!(v["label"], "Claude · Acme");
    assert_eq!(grid.calls_of("attach")[0], json!({"document": grid.doc("2"), "label": "Claude · Acme"}));

    let (err, _, v) = call(&hub, "attach", json!({"document": "MEMO"}));
    assert!(!err);
    assert_eq!(v["document"], word.doc("1"));
    assert_eq!(v["appTitle"], "WordCraft");

    // "budget" is budget.xlsx without its extension: exact enough.
    let (_, _, v) = call(&hub, "attach", json!({"document": "budget"}));
    assert_eq!(v["document"], grid.doc("1"));

    // In front: WordCraft was focused last.
    let (err, _, v) = call(&hub, "attach", json!({}));
    assert!(!err);
    assert_eq!(v["document"], word.doc("1"));
    // A document's own focusedAt wins over its app's.
    let mut reg: Value = serde_json::from_str(&std::fs::read_to_string(d.join(format!("gridcraft-{}.json", std::process::id()))).unwrap()).unwrap();
    reg["documents"][0]["focusedAt"] = json!(t);
    std::fs::write(d.join(format!("gridcraft-{}.json", std::process::id())), reg.to_string()).unwrap();
    let (_, _, v) = call(&hub, "attach", json!({}));
    assert_eq!(v["document"], grid.doc("1"));

    let (err, text, _) = call(&hub, "attach", json!({"document": "budg"}));
    assert!(err);
    assert!(text.contains("matches several documents") && text.contains("budget.xlsx") && text.contains("budget-2027.xlsx"), "{text}");

    let (err, text, _) = call(&hub, "attach", json!({"document": "forecast"}));
    assert!(err);
    assert!(
        text.contains("no document named “forecast”; open: budget.xlsx (GridCraft), budget-2027.xlsx (GridCraft), memo.docx (WordCraft)"),
        "{text}"
    );

    let (err, text, _) = call(&hub, "attach", json!({"document": "deckcraft:123:1"}));
    assert!(err);
    assert!(text.contains("Deckcraft isn't running"), "{text}");

    let (_, _, v) = call(&hub, "list_documents", json!({}));
    let docs = v["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().filter(|d| d["attachedHere"] == true).count() == 3);
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn same_name_tools_are_routed_by_document() {
    let d = dir("route");
    let grid =
        FakeApp::start("gridcraft", "GridCraft", vec![tool("read_range", "Read cells.", json!({"range": {"type": "string"}}))], Reply::Chunked);
    let word = FakeApp::start("wordcraft", "WordCraft", vec![tool("read_document", "Read paragraphs.", json!({}))], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true)], 0);
    word.register(&d, &[("1", "memo.docx", true)], 0);
    let (hub, _out) = hub_in(&d, None);
    init(&hub, "claude-ai");
    call(&hub, "attach", json!({"document": "budget.xlsx"}));
    call(&hub, "attach", json!({"document": "memo.docx"}));

    let (err, text, v) = call(&hub, "get_context", json!({"document": grid.doc("1")}));
    assert!(!err, "{text}");
    assert_eq!(v["app"], "GridCraft");
    let (_, _, v) = call(&hub, "get_context", json!({"document": "memo.docx"}));
    assert_eq!(v["app"], "WordCraft");
    assert_eq!(v["args"]["document"], word.doc("1"), "a name is turned into the id");

    // The hub adds the listener to the calls that take one.
    call(&hub, "list_questions", json!({"document": grid.doc("1"), "status": "pending"}));
    assert_eq!(
        grid.calls_of("list_questions")[0],
        json!({"document": grid.doc("1"), "status": "pending", "listener": format!("L-{}", grid.doc("1"))})
    );
    assert!(grid.calls_of("get_context")[0].get("listener").is_none());

    // Two attached and no `document`: refused, naming both.
    let (err, text, _) = call(&hub, "get_context", json!({}));
    assert!(err);
    assert!(text.contains("Several documents are attached") && text.contains("budget.xlsx") && text.contains("memo.docx"), "{text}");

    // An app-specific tool goes to its app even so.
    let (err, _, v) = call(&hub, "read_range", json!({"range": "A1"}));
    assert!(!err);
    assert_eq!(v["args"], json!({"range": "A1", "document": grid.doc("1")}));
    // …and a document of another app is said to be the wrong place.
    let (err, text, _) = call(&hub, "read_range", json!({"document": "memo.docx"}));
    assert!(err);
    assert!(text.contains("`read_range` is a GridCraft tool, and memo.docx is open in WordCraft"), "{text}");

    // After detaching one, calls without `document` go to the other.
    let (err, _, _) = call(&hub, "detach", json!({"document": "memo"}));
    assert!(!err);
    assert_eq!(word.calls_of("detach")[0]["listener"], format!("L-{}", word.doc("1")));
    let (_, _, v) = call(&hub, "get_context", json!({}));
    assert_eq!(v["app"], "GridCraft");

    // The same through app_tool.
    let (_, _, v) = call(&hub, "app_tool", json!({"document": "memo.docx", "tool": "read_document", "arguments": {"from": 3}}));
    assert_eq!(v["args"], json!({"from": 3, "document": word.doc("1")}));
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn refused_without_attach_when_several_documents_are_open() {
    let d = dir("refuse");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![], Reply::Json);
    let reg = grid.register(&d, &[("1", "a.xlsx", true), ("2", "b.xlsx", false)], 0);
    let (hub, _out) = hub_in(&d, None);
    init(&hub, "claude-ai");
    let (err, text, _) = call(&hub, "get_context", json!({}));
    assert!(err);
    assert!(text.contains("Which document?") && text.contains("a.xlsx (GridCraft), b.xlsx (GridCraft)") && text.contains("attach"), "{text}");
    assert!(grid.calls_of("get_context").is_empty(), "nothing reached the app");

    // With one document open there is nothing to choose.
    grid.register(&d, &[("1", "a.xlsx", true)], 0);
    let (err, text, v) = call(&hub, "get_context", json!({}));
    assert!(!err, "{text}");
    assert_eq!(v["args"]["document"], grid.doc("1"));

    // No app at all.
    std::fs::remove_file(reg).unwrap();
    let (err, text, _) = call(&hub, "attach", json!({"document": "a"}));
    assert!(err);
    assert!(text.contains("No Craft app is running"), "{text}");
    let (err, text, _) = call(&hub, "get_context", json!({}));
    assert!(err);
    assert!(text.contains("GridCraft isn't running"), "the last known tools say whose tool it is: {text}");
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn tool_lists_are_merged_and_changes_announced() {
    let d = dir("tools");
    let cache = d.parent().unwrap().join("aitocraft-tools.json");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![tool("read_range", "Read cells.", json!({"range": {"type": "string"}}))], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true)], 0);
    let (hub, out) = hub_in(&d, Some(cache.clone()));
    init(&hub, "claude-ai");
    let list = tools(&hub);
    let names: Vec<&str> = list.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(&names[..4], ["list_documents", "attach", "detach", "app_tool"]);
    assert_eq!(names.iter().filter(|n| **n == "attach").count(), 1, "the app's attach is behind the hub's");
    assert!(names.contains(&"read_range") && names.contains(&"get_context"));
    let gc = list.iter().find(|t| t["name"] == "list_questions").unwrap();
    assert!(gc["inputSchema"]["properties"]["document"].is_object());
    assert!(gc["inputSchema"]["properties"].get("listener").is_none(), "the hub fills in the listener");
    assert!(gc["inputSchema"].get("additionalProperties").is_none());

    assert!(!hub.refresh(), "nothing changed");
    let word = FakeApp::start(
        "wordcraft",
        "WordCraft",
        vec![
            tool("read_document", "Read paragraphs.", json!({})),
            tool("get_context", "Overview of the document, its outline.", json!({"depth": {"type": "number"}})),
        ],
        Reply::Json,
    );
    word.register(&d, &[("1", "memo.docx", true)], 0);
    assert!(hub.refresh());
    let changed = |v: &Value| v["method"] == "notifications/tools/list_changed";
    assert_eq!(out.lines().iter().filter(|v| changed(v)).count(), 1);
    hub.refresh();
    assert_eq!(out.lines().iter().filter(|v| changed(v)).count(), 1, "announced once until the client fetches again");
    let list = tools(&hub);
    assert!(list.iter().any(|t| t["name"] == "read_document"));
    let gc = list.iter().find(|t| t["name"] == "get_context").unwrap();
    let desc = gc["description"].as_str().unwrap();
    assert!(
        desc.contains("GridCraft: Overview of the open document.")
            && desc.contains("WordCraft: Overview of the document")
            && desc.ends_with("[GridCraft, WordCraft]"),
        "{desc}"
    );
    assert!(gc["inputSchema"]["properties"]["depth"].is_object(), "properties of both apps");

    // The last known tools stay listed after the app quits (and in the next run).
    drop(word);
    std::fs::remove_file(d.join(format!("wordcraft-{}.json", std::process::id()))).unwrap();
    assert!(!hub.refresh());
    assert!(tools(&hub).iter().any(|t| t["name"] == "read_document"));
    let (hub2, _) = hub_in(&d, Some(cache.clone()));
    let (err, text, _) = call(&hub2, "read_document", json!({}));
    assert!(err);
    assert!(text.contains("WordCraft isn't running"), "{text}");
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn questions_reach_a_claude_code_session_as_channel_events() {
    let d = dir("channel");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![], Reply::Json);
    let word = FakeApp::start("wordcraft", "WordCraft", vec![], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true), ("2", "other.xlsx", false)], 0);
    word.register(&d, &[("1", "memo.docx", true)], 0);
    let (hub, out) = hub_in(&d, None);
    init(&hub, "claude-code");
    let initialize =
        hub.handle(&json!({"jsonrpc": "2.0", "id": 9, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}})).unwrap();
    assert!(initialize["result"]["capabilities"]["experimental"]["claude/channel"].is_object());
    assert!(initialize["result"]["instructions"].as_str().unwrap().contains("⟦"));

    let (err, text, v) = call(&hub, "attach", json!({"document": "budget.xlsx"}));
    assert!(!err, "{text}");
    assert_eq!(v["channel"], true);
    assert_eq!(v["label"], "Claude Code · acme-project");
    let until = Instant::now() + Duration::from_secs(3);
    while grid.event_queries.lock().unwrap().is_empty() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    let q = grid.event_queries.lock().unwrap().first().cloned().unwrap();
    assert!(q.starts_with(&format!("/events?document={}", crate::util::pct(&grid.doc("1")))), "{q}");
    assert!(q.contains("&listener=L-"), "{q}");

    grid.push_event(json!({"event": "question", "id": "q3", "document": grid.doc("1"), "range": "Sheet1!C16", "label": "Revenue", "text": "Why did ⟦1⟧ grow?", "kind": "question", "refs": [{"n": 1, "range": "Sheet1!C16"}]}));
    grid.push_event(json!({"event": "question", "id": "q3", "document": grid.doc("1"), "text": "again", "kind": "question"}));
    grid.push_event(json!({"event": "question", "id": "p1", "document": grid.doc("1"), "text": "Is anyone there?", "kind": "ping"}));
    let is_channel = |v: &Value| v["method"] == "notifications/claude/channel";
    let ping = out.wait_for(3000, |v| is_channel(v) && v["params"]["meta"]["kind"] == "ping").expect("the ping came through");
    assert!(ping["params"]["content"].as_str().unwrap().contains("\"here\""));
    let events: Vec<Value> = out.lines().into_iter().filter(is_channel).collect();
    assert_eq!(events.len(), 2, "a question is sent once: {events:?}");
    let e = &events[0]["params"];
    assert_eq!(e["meta"], json!({"document": grid.doc("1"), "question_id": "q3", "kind": "question", "app": "GridCraft"}));
    let c = e["content"].as_str().unwrap();
    assert!(c.contains("GridCraft, budget.xlsx, on Sheet1!C16 (Revenue): Why did ⟦1⟧ grow?") && c.contains("⟦1⟧ = Sheet1!C16"), "{c}");

    // Another document's questions don't come here; a Claude Desktop conversation gets none at all.
    grid.push_event(json!({"event": "question", "id": "q9", "document": grid.doc("2"), "text": "not ours"}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(out.lines().into_iter().filter(is_channel).count(), 2);

    // Detach stops the stream.
    call(&hub, "detach", json!({}));
    grid.push_event(json!({"event": "question", "id": "q10", "document": grid.doc("1"), "text": "after"}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(out.lines().into_iter().filter(is_channel).count(), 2);

    let (desk, desk_out) = hub_in(&d, None);
    init(&desk, "claude-ai");
    let (_, _, v) = call(&desk, "attach", json!({"document": "memo"}));
    assert_eq!(v["channel"], false);
    word.push_event(json!({"event": "question", "id": "w1", "document": word.doc("1"), "text": "hi"}));
    std::thread::sleep(Duration::from_millis(300));
    assert!(desk_out.lines().iter().all(|v| !is_channel(v)));
    assert!(word.event_queries.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn a_closed_document_is_detached_and_the_session_told() {
    let d = dir("closed");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true), ("2", "b.xlsx", false)], 0);
    let (hub, out) = hub_in(&d, None);
    init(&hub, "claude-code");
    call(&hub, "attach", json!({"document": "budget.xlsx"}));
    grid.register(&d, &[("2", "b.xlsx", true)], 0);
    hub.refresh();
    let n = out.wait_for(1000, |v| v["params"]["meta"]["kind"] == "closed").expect("told");
    assert!(n["params"]["content"].as_str().unwrap().contains("budget.xlsx was closed"));
    let (err, text, _) = call(&hub, "detach", json!({}));
    assert!(err && text.contains("nothing is attached"));
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn stdio_framing() {
    let d = dir("stdio");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true)], 0);
    let (hub, out) = hub_in(&d, None);
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"claude-ai"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "",
        "not json",
        r#"[{"jsonrpc":"2.0","id":2,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/x"}]"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_context","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":"s","method":"nope"}"#,
        r#"7"#,
    ]
    .join("\n");
    crate::stdio::serve(hub, input.as_bytes()).unwrap();
    let lines = out.lines();
    assert_eq!(lines.len(), 6, "{lines:?}");
    let by_id = |id: Value| lines.iter().find(|l| l["id"] == id).cloned().unwrap();
    assert_eq!(by_id(json!(1))["result"]["protocolVersion"], "2025-03-26");
    assert!(lines.iter().any(|l| l["error"]["code"] == -32700));
    assert!(lines.iter().any(|l| l.is_array() && l[0]["id"] == 2));
    assert_eq!(by_id(json!(3))["result"]["isError"], false);
    assert_eq!(by_id(json!("s"))["error"]["code"], -32601);
    assert!(lines.iter().any(|l| l["error"]["code"] == -32600));
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn a_cancelled_call_gets_no_reply() {
    let d = dir("cancel");
    let (hub, out) = hub_in(&d, None);
    hub.cancel(&json!(5));
    let input = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"list_documents","arguments":{}}}"#;
    crate::stdio::serve(hub, input.as_bytes()).unwrap();
    assert!(out.lines().is_empty());
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn a_dropped_listener_is_attached_again_and_app_errors_pass_through() {
    let d = dir("dropped");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![tool("off_tool", "Off.", json!({})), tool("off_rpc", "Off.", json!({}))], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true)], 0);
    let (hub, _out) = hub_in(&d, None);
    init(&hub, "claude-ai");
    let (_, _, a) = call(&hub, "attach", json!({"document": "budget", "label": "Acme"}));
    let first = a["listener"].as_str().unwrap().to_string();
    grid.dropped.lock().unwrap().push(first.clone());
    let (err, text, v) = call(&hub, "wait_for_questions", json!({"document": grid.doc("1"), "timeout_seconds": 0}));
    assert!(!err, "{text}");
    assert_eq!(v, json!([]));
    let attaches = grid.calls_of("attach");
    assert_eq!(attaches.len(), 2, "attached again");
    assert_eq!(attaches[1]["label"], "Claude · Acme", "with the same label");
    let waits = grid.calls_of("wait_for_questions");
    assert_eq!(waits.len(), 2);
    assert_eq!(waits[0]["listener"], first.as_str());
    assert_ne!(waits[1]["listener"], first.as_str(), "retried with the new listener");
    call(&hub, "list_questions", json!({"document": grid.doc("1")}));
    assert_eq!(grid.calls_of("list_questions")[0]["listener"], waits[1]["listener"], "later calls use the new one");

    // Off mode: the app's words reach the agent as they are, as a tool result or a JSON-RPC error.
    let off = "the agent connection is off in GridCraft (Agent tab ▸ Connection ▸ Mode)";
    let (err, text, _) = call(&hub, "off_tool", json!({}));
    assert!(err);
    assert_eq!(text, off);
    let (err, text, _) = call(&hub, "off_rpc", json!({}));
    assert!(err);
    assert_eq!(text, off);
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}

#[test]
fn proposal_and_withdrawn_events_reach_the_session_once() {
    let d = dir("events");
    let grid = FakeApp::start("gridcraft", "GridCraft", vec![], Reply::Json);
    grid.register(&d, &[("1", "budget.xlsx", true)], 0);
    let (hub, out) = hub_in(&d, None);
    init(&hub, "claude-code");
    call(&hub, "attach", json!({"document": "budget"}));
    let doc = grid.doc("1");
    grid.push_event(json!({"event": "withdrawn", "id": "q3", "document": doc, "hint": "the user took this question back: drop it"}));
    grid.push_event(json!({"event": "proposal_rejected", "id": "e1", "document": doc, "topic": {"id": "t7", "number": 3}, "option": "A", "comment": "Only 2 years"}));
    grid.push_event(json!({"event": "proposal_rejected", "id": "e1", "document": doc, "topic": {"id": "t7", "number": 3}, "option": "A", "comment": "Only 2 years"}));
    grid.push_event(
        json!({"event": "proposal_applied", "id": "e2", "document": doc, "topic": {"id": "t8", "number": 4}, "option": "B", "title": "Two years"}),
    );
    grid.push_event(json!({"event": "something_else", "id": "e3", "document": doc}));
    let kind = |v: &Value, k: &str| v["method"] == "notifications/claude/channel" && v["params"]["meta"]["kind"] == k;
    out.wait_for(3000, |v| kind(v, "proposal_applied")).expect("applied came through");
    std::thread::sleep(Duration::from_millis(200));
    let lines = out.lines();
    let channel: Vec<&Value> = lines.iter().filter(|v| v["method"] == "notifications/claude/channel").collect();
    assert_eq!(channel.len(), 3, "{channel:?}");
    assert!(lines.iter().any(|v| kind(v, "withdrawn") && v["params"]["meta"]["question_id"] == "q3"));
    let rejected = lines.iter().find(|v| kind(v, "proposal_rejected")).unwrap();
    assert_eq!(rejected["params"]["meta"]["topic"], "t7");
    assert!(rejected["params"]["content"].as_str().unwrap().contains("“Only 2 years”"));
    let _ = std::fs::remove_dir_all(d.parent().unwrap());
}
