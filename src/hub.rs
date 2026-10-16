//! The hub: MCP server state. It reads the registry, merges the apps' tools, binds conversations
//! to documents (`attach`) and routes every call to the right app and document.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::app::{AppConn, result_json};
use crate::channel::{self, Follower, Target};
use crate::registry::{self, AppEntry, Doc};
use crate::util::{lock, now_ms, short, str_at, tool_err, tool_ok};

/// MCP versions the hub speaks to its client.
const SUPPORTED: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// The hub's own tools; the apps' tools of these names are reached through them.
const OWN: &[&str] = &["list_documents", "attach", "detach", "app_tool"];
/// Tools that take a `listener` (PROTOCOL.md §2.4), whether or not their schema says so.
const LISTENER_TOOLS: &[&str] = &["list_questions", "wait_for_questions", "claim_question", "answer_question"];
/// How long a failed tools/list waits before it is asked again.
const RETRY_TOOLS_MS: u64 = 3000;
/// How often a followed document's listener is refreshed (apps drop a listener after 90 s).
pub const KEEPALIVE_MS: u64 = 30_000;

pub const INSTRUCTIONS: &str = "AItoCraft connects you to the documents the user has open in the ArtCraft Craft apps: \
GridCraft (spreadsheets), WordCraft (documents) and DeckCraft (slide decks). The user decides which document this conversation \
works on, usually by saying \"we're working on budget.xlsx\" (or \"on the open file\").\n\
1. Call list_documents, then attach {document: \"<file name, part of it, or id>\", label: \"<this project's or chat's name>\"}. \
Omit document to attach to the file in front. Keep the document id that attach returns, and read get_file_context {document} once: \
what the author wants every agent to know about the file.\n\
2. Pass that id as `document` on EVERY other call. One AItoCraft serves several conversations and documents at once; \
a call without `document` is refused when it isn't clear which document you mean.\n\
3. Questions: the user asks them on cells, passages or slides without leaving the document. If this session receives channel \
events from aitocraft (Claude Code with channels), simply handle each event as it arrives. Otherwise (Claude Desktop, Cowork, \
Claude Code without channels) call wait_for_questions {document} in a loop: handle what it returns, then call it again; when it \
returns nothing, call it again. Keep going until the user says stop.\n\
4. For each question: claim_question {document, id}, investigate with the document's tools (get_context, get_topic, read_range, \
read_document, read_slides, …), then answer_question {document, id, text}. Answer first, briefly, with links inside the text \
([B1](cell:Sheet1!B1), [§12](para:12), [slide 4](slide:4), [3.2](place:3.2), [source](https://…)).\n\
5. Each question belongs to a topic (its `topic`: id, number, status). Work in that topic: add_places {topic, places} for places \
a change here implies (with a reason), widen_place when the answer is about more than was selected, add_note for what will still \
matter after the chat, set_status open|revise|info. Never set Agreed: propose_agreed {topic, reasons}; only the user agrees.\n\
6. Changes: inside a topic never edit the document directly and never ask for confirmation in the chat. Propose: \
run_command {document, topic, option?, command, params, reason} records the change on the topic's draft (option \"A\" by \
default), then describe_option {document, topic, option, title, why}. Offer options B, C… only when there is a real choice. The \
user applies or rejects it in the document. Check a proposal with read_document / read_range / read_slides {view: \"proposal\", \
topic, option}. Reads mark what pending proposals touch (`pending`) and questions carry `pendingNearby`: take them into account.\n\
7. Events come the same way as questions (channel events, or items with `event` in wait_for_questions): proposal_applied \
(done, nothing to do); proposal_rejected (its comment also comes as a follow-up question: redo the proposal there); \
proposal_outdated (the document changed: read again and redo it); withdrawn (the user took the question back: drop it).\n\
8. A question with kind \"ping\" is a connection check: answer_question with \"here\" at once; don't investigate.\n\
9. Markers ⟦1⟧, ⟦2⟧… in a question's text are what the user pointed at (clicked, selected) while asking; the question's refs \
say which cell, passage or slide each one is.\n\
10. File context: questions carry a short summary (`fileContext`). When you learn something about the whole file that will \
still matter (its purpose, audience, conventions), add_file_context or update_file_context.\n\
11. Client data reads as {{placeholder}}: refer to it by its placeholder and never try to find the real value. When an app \
says its agent connection is off, tell the user and stop.";

/// How the hub was set up.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The registry folder (`None`: no apps are ever found).
    pub registry_dir: Option<PathBuf>,
    /// Where the last known tools of each app are kept between runs (`None`: not kept).
    pub cache_path: Option<PathBuf>,
    /// Force channel events on or off (default: on for Claude Code and clients that declare channels).
    pub channels: Option<bool>,
    /// The folder the client started us in (Claude Code: the project), for default labels.
    pub cwd_name: Option<String>,
}

impl Config {
    /// The setup for a real run: the system's registry, a tool cache next to it.
    pub fn from_env() -> Config {
        let registry_dir = registry::dir();
        let cache_path = registry_dir.as_deref().and_then(Path::parent).map(|p| p.join("aitocraft-tools.json"));
        let channels = match std::env::var("AITOCRAFT_CHANNELS").ok().as_deref() {
            Some("1" | "on" | "true") => Some(true),
            Some("0" | "off" | "false") => Some(false),
            _ => None,
        };
        let cwd_name = std::env::current_dir().ok().and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()));
        Config { registry_dir, cache_path, channels, cwd_name }
    }
}

/// Where protocol output goes (stdout); replies and notifications never interleave.
pub type Out = Arc<Mutex<Box<dyn Write + Send>>>;

struct Instance {
    entry: AppEntry,
    conn: Arc<AppConn>,
    tools: Option<Vec<Value>>,
    tried: u64,
}

struct Binding {
    doc: String,
    title: String,
    key: String,
    app_title: String,
    listener: Option<String>,
    label: String,
    follower: Option<Follower>,
}

#[derive(Clone)]
struct Cached {
    title: String,
    tools: Vec<Value>,
}

#[derive(Default)]
struct State {
    client: String,
    client_channels: bool,
    initialized: bool,
    apps: BTreeMap<String, Instance>,
    cache: BTreeMap<String, Cached>,
    bindings: BTreeMap<String, Binding>,
    /// The tool list the client last fetched, and the last one it was told about.
    fetched_sig: Option<String>,
    notified_sig: Option<String>,
    last_keepalive: u64,
}

pub struct Hub {
    cfg: Config,
    out: Out,
    st: Mutex<State>,
    refreshing: Mutex<()>,
    cancelled: Mutex<HashSet<String>>,
    me: Weak<Hub>,
}

/// Where a call goes.
struct Route {
    conn: Arc<AppConn>,
    args: Value,
}

impl Hub {
    pub fn new(cfg: Config, out: Out) -> Arc<Hub> {
        let cache = cfg.cache_path.as_deref().map(load_cache).unwrap_or_default();
        Arc::new_cyclic(|me| Hub {
            cfg,
            out,
            st: Mutex::new(State { cache, ..State::default() }),
            refreshing: Mutex::new(()),
            cancelled: Mutex::new(HashSet::new()),
            me: me.clone(),
        })
    }

    /// Writes one message to the client. False when the output is gone.
    pub fn send(&self, v: &Value) -> bool {
        let mut w = lock(&self.out);
        writeln!(w, "{v}").and_then(|()| w.flush()).is_ok()
    }

    /// Marks a request as cancelled by the client: its reply is dropped.
    pub fn cancel(&self, id: &Value) {
        lock(&self.cancelled).insert(id.to_string());
    }

    /// Whether the reply to `id` should still be sent (and forgets the cancellation).
    pub fn wanted(&self, id: &Value) -> bool {
        !lock(&self.cancelled).remove(&id.to_string())
    }

    /// Starts the background watcher: the registry every second, listeners every 30 s.
    pub fn watch(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("aitocraft-watch".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let Some(hub) = me.upgrade() else { return };
                hub.refresh();
                hub.keepalive();
            }
        });
    }

    // ---- registry and tools ----

    /// Re-reads the registry, fetches the tools of new apps, drops bindings of closed documents,
    /// and tells the client when its tool list changed. Returns whether it changed.
    pub fn refresh(&self) -> bool {
        let _one = lock(&self.refreshing);
        let now = now_ms();
        let entries = match &self.cfg.registry_dir {
            Some(d) => registry::scan(d, now, &crate::util::pid_alive),
            None => vec![],
        };
        let mut closed: Vec<Binding> = vec![];
        let to_fetch: Vec<(String, Arc<AppConn>)> = {
            let mut st = lock(&self.st);
            let keys: HashSet<&str> = entries.iter().map(|e| e.key.as_str()).collect();
            st.apps.retain(|k, _| keys.contains(k.as_str()));
            for e in &entries {
                match st.apps.get_mut(&e.key) {
                    Some(i) if i.entry.mcp == e.mcp && i.entry.version == e.version => i.entry = e.clone(),
                    _ => {
                        let conn = Arc::new(AppConn::new(e.mcp.clone(), &e.title));
                        st.apps.insert(e.key.clone(), Instance { entry: e.clone(), conn, tools: None, tried: 0 });
                    }
                }
            }
            let gone: Vec<String> = st
                .bindings
                .iter()
                .filter(|(_, b)| !st.apps.get(&b.key).is_some_and(|i| i.entry.documents.iter().any(|d| d.id == b.doc)))
                .map(|(k, _)| k.clone())
                .collect();
            for k in gone {
                if let Some(b) = st.bindings.remove(&k) {
                    closed.push(b);
                }
            }
            st.apps
                .iter()
                .filter(|(_, i)| i.tools.is_none() && now.saturating_sub(i.tried) >= RETRY_TOOLS_MS)
                .map(|(k, i)| (k.clone(), i.conn.clone()))
                .collect()
        };
        for b in closed {
            if b.follower.is_some() {
                self.send(&json!({
                    "jsonrpc": "2.0", "method": "notifications/claude/channel",
                    "params": {"content": format!("{} was closed in {}; this session is no longer attached to it.", b.title, b.app_title),
                               "meta": {"document": b.doc, "kind": "closed", "app": b.app_title}},
                }));
            }
        }
        let mut cache_changed = false;
        for (key, conn) in to_fetch {
            let r = conn.list_tools();
            let mut st = lock(&self.st);
            let Some(i) = st.apps.get_mut(&key) else { continue };
            i.tried = now_ms();
            match r {
                Ok(tools) => {
                    i.tools = Some(tools.clone());
                    let (app, title) = (i.entry.app.clone(), i.entry.title.clone());
                    let fresh = Cached { title, tools };
                    if st.cache.get(&app).is_none_or(|c| c.tools != fresh.tools || c.title != fresh.title) {
                        st.cache.insert(app, fresh);
                        cache_changed = true;
                    }
                }
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "aitocraft: {e}");
                }
            }
        }
        if cache_changed && let Some(p) = &self.cfg.cache_path {
            let snapshot = lock(&self.st).cache.clone();
            save_cache(p, &snapshot);
        }
        let (notify, changed) = {
            let mut st = lock(&self.st);
            let sig = signature(&self.tool_list(&st));
            let changed = st.fetched_sig.as_ref().is_some_and(|f| *f != sig);
            let notify = st.initialized && changed && st.notified_sig.as_ref() != Some(&sig);
            if notify {
                st.notified_sig = Some(sig);
            }
            (notify, changed)
        };
        if notify {
            self.send(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
        }
        changed
    }

    /// Refreshes the listeners of followed documents (channel mode), so the app keeps showing
    /// "Listening" while this session is there.
    fn keepalive(&self) {
        let now = now_ms();
        let calls: Vec<(Arc<AppConn>, Value)> = {
            let mut st = lock(&self.st);
            if now.saturating_sub(st.last_keepalive) < KEEPALIVE_MS {
                return;
            }
            st.last_keepalive = now;
            st.bindings
                .values()
                .filter(|b| b.follower.is_some())
                .filter_map(|b| {
                    let l = b.listener.clone()?;
                    let conn = st.apps.get(&b.key)?.conn.clone();
                    Some((conn, json!({"document": b.doc, "listener": l, "status": "pending"})))
                })
                .collect()
        };
        for (conn, args) in calls {
            let _ = conn.call_tool("list_questions", &args, Duration::from_secs(5));
        }
    }

    /// The tools the client sees: the hub's own, then every app's (live, or last known), merged.
    fn tool_list(&self, st: &State) -> Vec<Value> {
        let mut per_app: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
        for i in st.apps.values() {
            if let Some(t) = &i.tools {
                per_app.entry(i.entry.app.clone()).or_insert_with(|| (i.entry.title.clone(), t.clone()));
            }
        }
        for (app, c) in &st.cache {
            per_app.entry(app.clone()).or_insert_with(|| (c.title.clone(), c.tools.clone()));
        }
        let apps: Vec<(String, Vec<Value>)> = per_app.into_values().collect();
        let mut out = own_tools();
        out.extend(merge_tools(&apps));
        out
    }

    // ---- MCP ----

    /// Handles one JSON-RPC message; `None` for notifications.
    pub fn handle(&self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = msg.get("id").cloned() else {
            match method {
                "notifications/initialized" => lock(&self.st).initialized = true,
                "notifications/cancelled" => {
                    if let Some(r) = params.get("requestId") {
                        self.cancel(r);
                    }
                }
                _ => {}
            }
            return None;
        };
        // A reply to something we never ask: nothing to do.
        msg.get("method")?;
        let ok = |r: Value| json!({"jsonrpc": "2.0", "id": id, "result": r});
        Some(match method {
            "initialize" => ok(self.initialize(&params)),
            "ping" => ok(json!({})),
            "tools/list" => {
                self.refresh();
                let mut st = lock(&self.st);
                let tools = self.tool_list(&st);
                let sig = signature(&tools);
                st.fetched_sig = Some(sig.clone());
                st.notified_sig = Some(sig);
                st.initialized = true;
                ok(json!({"tools": tools}))
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
                ok(self.call(name, args))
            }
            "resources/list" => ok(json!({"resources": []})),
            "resources/templates/list" => ok(json!({"resourceTemplates": []})),
            "prompts/list" => ok(json!({"prompts": []})),
            "logging/setLevel" => ok(json!({})),
            other => json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {other}")}}),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(SUPPORTED[0]);
        let version = if SUPPORTED.contains(&asked) { asked } else { SUPPORTED[0] };
        {
            let mut st = lock(&self.st);
            st.client = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("").to_string();
            st.client_channels =
                st.client.to_ascii_lowercase().contains("claude-code") || params.pointer("/capabilities/experimental/claude~1channel").is_some();
        }
        json!({
            "protocolVersion": version,
            "capabilities": {"tools": {"listChanged": true}, "experimental": {"claude/channel": {}}},
            "serverInfo": {"name": "aitocraft", "title": "AItoCraft", "version": env!("CARGO_PKG_VERSION")},
            "instructions": INSTRUCTIONS,
        })
    }

    fn channels(&self, st: &State) -> bool {
        self.cfg.channels.unwrap_or(st.client_channels)
    }

    /// One tool call; always a tool result (errors are results with `isError`).
    pub fn call(&self, name: &str, args: Value) -> Value {
        match name {
            "list_documents" => self.list_documents(),
            "attach" => self.attach(&args),
            "detach" => self.detach(&args),
            "app_tool" => {
                let Some(tool) = str_at(&args, "tool") else { return tool_err("app_tool needs `tool`: the name of the app's tool") };
                let mut inner = args.get("arguments").cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
                if let Some(d) = args.get("document").filter(|d| d.is_string())
                    && let Some(o) = inner.as_object_mut()
                {
                    o.insert("document".into(), d.clone());
                }
                if OWN.contains(&tool) {
                    return self.call(tool, inner);
                }
                self.forward(tool, inner)
            }
            "" => tool_err("tools/call needs a tool name"),
            _ => self.forward(name, args),
        }
    }

    fn list_documents(&self) -> Value {
        self.refresh();
        let st = lock(&self.st);
        let mut docs = vec![];
        for i in st.apps.values() {
            for d in &i.entry.documents {
                let mut o = d.raw.as_object().cloned().unwrap_or_default();
                o.insert("id".into(), json!(d.id));
                o.insert("title".into(), json!(d.title));
                o.insert("app".into(), json!(i.entry.app));
                o.insert("appTitle".into(), json!(i.entry.title));
                o.insert("attachedHere".into(), json!(st.bindings.contains_key(&d.id)));
                docs.push(Value::Object(o));
            }
        }
        if docs.is_empty() {
            return tool_ok(&json!({"documents": [], "note": no_documents(&st)}));
        }
        let apps: Vec<Value> = st
            .apps
            .values()
            .map(|i| json!({"app": i.entry.app, "appTitle": i.entry.title, "version": i.entry.version, "documents": i.entry.documents.len()}))
            .collect();
        tool_ok(
            &json!({"documents": docs, "apps": apps, "next": "attach {document: <id or name>, label: <this project's name>}, then pass document on every call"}),
        )
    }

    fn label(&self, st: &State, given: Option<&str>) -> String {
        let client = match st.client.as_str() {
            "claude-ai" => "Claude".to_string(),
            c if c.to_ascii_lowercase().contains("claude-code") => "Claude Code".to_string(),
            "" => "AItoCraft".to_string(),
            c => c.to_string(),
        };
        match given {
            Some(g) if g.to_lowercase().starts_with(&client.to_lowercase()) => short(g, 80),
            Some(g) => format!("{client} · {}", short(g, 60)),
            None if client == "Claude Code" => match &self.cfg.cwd_name {
                Some(n) => format!("{client} · {n}"),
                None => client,
            },
            None => client,
        }
    }

    fn attach(&self, args: &Value) -> Value {
        self.refresh();
        let picked = {
            let st = lock(&self.st);
            let found = match str_at(args, "document") {
                Some(q) => resolve(&st, q),
                None => frontmost(&st),
            };
            found.and_then(|(key, doc)| {
                let i = st.apps.get(&key).ok_or("that app just quit")?;
                let has_attach = i.tools.as_ref().is_some_and(|t| t.iter().any(|t| t.get("name").and_then(Value::as_str) == Some("attach")));
                let d = i.entry.documents.iter().find(|d| d.id == doc).cloned();
                Ok((
                    key.clone(),
                    d,
                    i.conn.clone(),
                    i.entry.title.clone(),
                    i.entry.app.clone(),
                    has_attach,
                    self.label(&st, str_at(args, "label")),
                    self.channels(&st),
                ))
            })
        };
        let (key, doc, conn, app_title, app, has_attach, label, channels) = match picked {
            Ok((k, Some(d), c, t, a, h, l, ch)) => (k, d, c, t, a, h, l, ch),
            Ok(_) => return tool_err("that document just closed"),
            Err(e) => return tool_err(e),
        };
        let listener = if has_attach {
            match conn.call_tool("attach", &json!({"document": doc.id, "label": label}), Duration::from_secs(10)) {
                Ok(r) if r.get("isError").and_then(Value::as_bool) == Some(true) => {
                    return tool_err(format!("{app_title} refused to attach to {}: {}", doc.title, crate::util::text_of(&r)));
                }
                Ok(r) => result_json(&r).as_ref().and_then(|v| str_at(v, "listener")).map(str::to_string),
                Err(e) => return tool_err(e),
            }
        } else {
            None
        };
        let follower = if channels { self.follow(&conn, &doc, &app_title, listener.as_deref()) } else { None };
        let following = follower.is_some();
        let old = lock(&self.st).bindings.insert(
            doc.id.clone(),
            Binding {
                doc: doc.id.clone(),
                title: doc.title.clone(),
                key,
                app_title: app_title.clone(),
                listener: listener.clone(),
                label: label.clone(),
                follower,
            },
        );
        if let Some(old) = old
            && let Some(l) = old.listener
            && Some(&l) != listener.as_ref()
        {
            let _ = conn.call_tool("detach", &json!({"document": doc.id, "listener": l}), Duration::from_secs(5));
        }
        let how = if following {
            "Questions asked in this document arrive in this session as channel events from aitocraft: answer each as it comes. If none ever arrive (channels not enabled), call wait_for_questions {document} in a loop instead."
        } else {
            "Now call wait_for_questions {document} in a loop and answer what it returns (claim_question, investigate, answer_question)."
        };
        let mut note = format!("Attached to {} in {app_title}. Pass document \"{}\" on every call. {how}", doc.title, doc.id);
        if !has_attach {
            note.push_str(&format!(
                " ({app_title} {} doesn't know attach yet: the user won't see this conversation as listening.)",
                lock(&self.st).apps.values().find(|i| i.entry.app == app).map(|i| i.entry.version.clone()).unwrap_or_default()
            ));
        }
        tool_ok(&json!({
            "document": doc.id,
            "title": doc.title,
            "path": doc.path,
            "app": app,
            "appTitle": app_title,
            "listener": listener,
            "label": label,
            "channel": following,
            "next": note,
        }))
    }

    fn follow(&self, conn: &Arc<AppConn>, doc: &Doc, app_title: &str, listener: Option<&str>) -> Option<Follower> {
        let me = self.me.clone();
        let emit: Arc<dyn Fn(&Value) -> bool + Send + Sync> = Arc::new(move |v: &Value| me.upgrade().is_some_and(|h| h.send(v)));
        channel::follow(
            Target {
                events: channel::events_url(&conn.url, &doc.id, listener),
                document: doc.id.clone(),
                title: doc.title.clone(),
                app_title: app_title.to_string(),
            },
            emit,
        )
    }

    fn detach(&self, args: &Value) -> Value {
        let (b, conn) = {
            let mut st = lock(&self.st);
            let key = match str_at(args, "document") {
                Some(q) => {
                    let q_l = q.to_lowercase();
                    let hits: Vec<String> =
                        st.bindings.values().filter(|b| b.doc == q || b.title.to_lowercase().contains(&q_l)).map(|b| b.doc.clone()).collect();
                    match hits.as_slice() {
                        [one] => one.clone(),
                        [] => return tool_err(format!("this AItoCraft isn't attached to “{q}”; attached: {}", attached_list(&st))),
                        _ => return tool_err(format!("“{q}” matches several attached documents ({}); pass the id", attached_list(&st))),
                    }
                }
                None => match st.bindings.keys().collect::<Vec<_>>().as_slice() {
                    [one] => (*one).clone(),
                    [] => return tool_err("nothing is attached"),
                    _ => return tool_err(format!("several documents are attached ({}); pass document", attached_list(&st))),
                },
            };
            let Some(b) = st.bindings.remove(&key) else { return tool_err("nothing is attached") };
            let conn = st.apps.get(&b.key).map(|i| i.conn.clone());
            (b, conn)
        };
        if let (Some(l), Some(conn)) = (&b.listener, conn) {
            let _ = conn.call_tool("detach", &json!({"document": b.doc, "listener": l}), Duration::from_secs(5));
        }
        tool_ok(&json!({"detached": b.doc, "title": b.title}))
    }

    fn route(&self, st: &State, name: &str, mut args: Map<String, Value>) -> Result<Route, String> {
        let has = |i: &Instance| i.tools.as_ref().is_some_and(|ts| ts.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name)));
        let cands: Vec<&String> = st.apps.iter().filter(|(_, i)| has(i)).map(|(k, _)| k).collect();
        let asked = args.remove("document").and_then(|d| d.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
        let (key, doc): (String, Option<String>) = match asked {
            Some(q) => {
                let (key, id) = resolve(st, &q)?;
                if !cands.contains(&&key) {
                    let i = st.apps.get(&key).ok_or("that app just quit")?;
                    let title = i.entry.documents.iter().find(|d| d.id == id).map(|d| d.title.as_str()).unwrap_or("");
                    let elsewhere = tool_owners(st, name);
                    return Err(if elsewhere.is_empty() {
                        format!("{} has no tool `{name}` ({title} is open in {})", i.entry.title, i.entry.title)
                    } else {
                        format!("`{name}` is a {} tool, and {title} is open in {}", elsewhere.join("/"), i.entry.title)
                    });
                }
                (key, Some(id))
            }
            None => {
                if cands.is_empty() {
                    return Err(missing_tool(st, name));
                }
                let bound: Vec<&Binding> = st.bindings.values().filter(|b| cands.contains(&&b.key)).collect();
                match bound.as_slice() {
                    [b] => (b.key.clone(), Some(b.doc.clone())),
                    [] => {
                        let docs: Vec<(&String, &Doc)> = cands
                            .iter()
                            .filter_map(|k| st.apps.get(*k).map(|i| (*k, i)))
                            .flat_map(|(k, i)| i.entry.documents.iter().map(move |d| (k, d)))
                            .collect();
                        match docs.as_slice() {
                            [(k, d)] => ((*k).clone(), Some(d.id.clone())),
                            [] if cands.len() == 1 => (cands[0].clone(), None),
                            [] => return Err(no_documents(st)),
                            _ => {
                                return Err(format!(
                                    "Which document? Several are open ({}) and this conversation hasn't attached to one. Call attach {{document: \"<name>\"}} first, then pass `document` on every call.",
                                    open_list(st)
                                ));
                            }
                        }
                    }
                    _ => {
                        return Err(format!(
                            "Several documents are attached ({}): pass `document` with the id of the one you mean.",
                            bound.iter().map(|b| format!("{} = {}", b.title, b.doc)).collect::<Vec<_>>().join(", ")
                        ));
                    }
                }
            }
        };
        let i = st.apps.get(&key).ok_or("that app just quit")?;
        if let Some(d) = &doc {
            args.insert("document".into(), json!(d));
            let wants_listener = LISTENER_TOOLS.contains(&name)
                || i.tools
                    .as_ref()
                    .and_then(|ts| ts.iter().find(|t| t.get("name").and_then(Value::as_str) == Some(name)))
                    .is_some_and(|t| t.pointer("/inputSchema/properties/listener").is_some());
            if wants_listener
                && !args.contains_key("listener")
                && let Some(l) = st.bindings.get(d).and_then(|b| b.listener.clone())
            {
                args.insert("listener".into(), json!(l));
            }
        }
        Ok(Route { conn: i.conn.clone(), args: Value::Object(args) })
    }

    fn forward(&self, name: &str, args: Value) -> Value {
        self.refresh();
        let map = args.as_object().cloned().unwrap_or_default();
        let routed = {
            let st = lock(&self.st);
            self.route(&st, name, map.clone())
        };
        // A tool nobody has yet: maybe an app just started.
        let routed = match routed {
            Err(_)
                if !lock(&self.st)
                    .apps
                    .values()
                    .any(|i| i.tools.as_ref().is_some_and(|ts| ts.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name)))) =>
            {
                self.refresh();
                let st = lock(&self.st);
                self.route(&st, name, map.clone())
            }
            r => r,
        };
        let r = match routed {
            Ok(r) => r,
            Err(e) => return tool_err(e),
        };
        let timeout = if name == "wait_for_questions" {
            let secs = r.args.get("timeout_seconds").and_then(Value::as_f64).filter(|x| x.is_finite()).unwrap_or(25.0).clamp(0.0, 300.0);
            Duration::from_secs_f64(secs + 30.0)
        } else {
            Duration::from_secs(120)
        };
        let mut result = match r.conn.call_tool(name, &r.args, timeout) {
            Ok(v) => v,
            Err(e) => return tool_err(e),
        };
        // The app dropped our listener (quiet for over 90 s, or it restarted): attach again and retry once.
        if result.get("isError").and_then(Value::as_bool) == Some(true)
            && r.args.get("listener").is_some()
            && let Some(doc) = r.args.get("document").and_then(Value::as_str)
        {
            let text = crate::util::text_of(&result).to_lowercase();
            let dropped = ["not attached", "attach again", "unknown", "not found", "no listener", "expired", "dropped"];
            if text.contains("listener") && dropped.iter().any(|w| text.contains(w)) {
                let label = lock(&self.st).bindings.get(doc).map(|b| b.label.clone());
                if let Some(label) = label
                    && let Ok(a) = r.conn.call_tool("attach", &json!({"document": doc, "label": label}), Duration::from_secs(10))
                    && let Some(l) = result_json(&a).as_ref().and_then(|v| str_at(v, "listener")).map(str::to_string)
                {
                    if let Some(b) = lock(&self.st).bindings.get_mut(doc) {
                        b.listener = Some(l.clone());
                    }
                    let mut args = r.args.clone();
                    args["listener"] = json!(l);
                    if let Ok(v) = r.conn.call_tool(name, &args, timeout) {
                        result = v;
                    }
                }
            }
        }
        if result.get("content").is_none() {
            return tool_ok(&result);
        }
        result
    }
}

// ---- documents ----

fn all_docs(st: &State) -> Vec<(&String, &AppEntry, &Doc)> {
    st.apps.iter().flat_map(|(k, i)| i.entry.documents.iter().map(move |d| (k, &i.entry, d))).collect()
}

fn describe(e: &AppEntry, d: &Doc) -> String {
    format!("{} ({})", d.title, e.title)
}

fn open_list(st: &State) -> String {
    let docs = all_docs(st);
    if docs.is_empty() {
        return "none".into();
    }
    docs.iter().take(30).map(|(_, e, d)| describe(e, d)).collect::<Vec<_>>().join(", ")
}

fn attached_list(st: &State) -> String {
    if st.bindings.is_empty() {
        return "none".into();
    }
    st.bindings.values().map(|b| format!("{} = {}", b.title, b.doc)).collect::<Vec<_>>().join(", ")
}

fn no_documents(st: &State) -> String {
    if st.apps.is_empty() {
        "No Craft app is running. Ask the user to open the document in GridCraft, WordCraft or DeckCraft, then try again.".into()
    } else {
        let names: Vec<&str> = st.apps.values().map(|i| i.entry.title.as_str()).collect();
        format!("No document is open in {}. Ask the user to open one, then try again.", names.join(", "))
    }
}

fn tool_owners(st: &State, name: &str) -> Vec<String> {
    let has = |ts: &[Value]| ts.iter().any(|t| t.get("name").and_then(Value::as_str) == Some(name));
    let mut out: Vec<String> = st.apps.values().filter(|i| i.tools.as_deref().is_some_and(has)).map(|i| i.entry.title.clone()).collect();
    for c in st.cache.values() {
        if has(&c.tools) && !out.contains(&c.title) {
            out.push(c.title.clone());
        }
    }
    out
}

fn missing_tool(st: &State, name: &str) -> String {
    let owners = tool_owners(st, name);
    if !owners.is_empty() {
        let s = if owners.len() == 1 { "isn't" } else { "aren't" };
        return format!(
            "{} {s} running (`{name}` is a {} tool). Ask the user to open the document there, then try again.",
            owners.join(" and "),
            owners.join("/")
        );
    }
    if let Some(i) = st.apps.values().find(|i| i.tools.is_none()) {
        return format!("{} hasn't answered yet; try again in a moment.", i.entry.title);
    }
    format!("Unknown tool `{name}`.")
}

fn stem(title: &str) -> &str {
    match title.rsplit_once('.') {
        Some((s, ext)) if !s.is_empty() && !ext.is_empty() && ext.len() <= 5 && !ext.contains(' ') => s,
        _ => title,
    }
}

fn expand_home(p: &str) -> String {
    match (p.strip_prefix("~/"), std::env::var("HOME")) {
        (Some(rest), Ok(h)) => format!("{h}/{rest}"),
        _ => p.to_string(),
    }
}

/// Finds a document by id, path, file name, name without extension, or part of its name.
fn resolve(st: &State, query: &str) -> Result<(String, String), String> {
    let q = query.trim().trim_matches(['"', '\'', '“', '”', '«', '»']).trim();
    let docs = all_docs(st);
    if docs.is_empty() {
        return Err(no_documents(st));
    }
    if let Some((k, _, d)) = docs.iter().find(|(_, _, d)| d.id == q) {
        return Ok(((*k).clone(), d.id.clone()));
    }
    // An id of a document that is gone.
    let parts: Vec<&str> = q.split(':').collect();
    if parts.len() == 3 && parts[1].chars().all(|c| c.is_ascii_digit()) && !parts[1].is_empty() {
        let app = parts[0].to_ascii_lowercase();
        return Err(match st.apps.values().find(|i| i.entry.app == app) {
            Some(i) => format!("document {q} is not open in {} (any more); open: {}", i.entry.title, open_list(st)),
            None => format!("{} isn't running (document {q} was open in it); open: {}", registry_title(st, &app), open_list(st)),
        });
    }
    let ql = q.to_lowercase();
    let path_q = expand_home(q);
    let pick = |hits: Vec<&(&String, &AppEntry, &Doc)>| -> Option<Result<(String, String), String>> {
        match hits.as_slice() {
            [] => None,
            [(k, _, d)] => Some(Ok(((*k).clone(), d.id.clone()))),
            many => Some(Err(format!(
                "“{q}” matches several documents: {}. Say which (or pass its id).",
                many.iter()
                    .map(|(_, e, d)| format!("{} ({}, {}, id {})", d.title, e.title, d.path.as_deref().unwrap_or("unsaved"), d.id))
                    .collect::<Vec<_>>()
                    .join("; ")
            ))),
        }
    };
    let file_name = |d: &Doc| d.path.as_deref().and_then(|p| Path::new(p).file_name()).map(|n| n.to_string_lossy().to_lowercase());
    let tiers: [&dyn Fn(&Doc) -> bool; 3] = [
        &|d| d.title.to_lowercase() == ql || d.path.as_deref() == Some(path_q.as_str()) || file_name(d).as_deref() == Some(ql.as_str()),
        &|d| stem(&d.title).to_lowercase() == ql,
        &|d| d.title.to_lowercase().contains(&ql) || (q.contains('/') && d.path.as_deref().is_some_and(|p| p.contains(path_q.as_str()))),
    ];
    for tier in tiers {
        if let Some(r) = pick(docs.iter().filter(|(_, _, d)| tier(d)).collect()) {
            return r;
        }
    }
    Err(format!("no document named “{q}”; open: {}", open_list(st)))
}

fn registry_title(st: &State, app: &str) -> String {
    st.cache.get(app).map(|c| c.title.clone()).unwrap_or_else(|| {
        let mut c = app.chars();
        c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
    })
}

/// The document in front: the most recently focused of the documents in front of their apps, else
/// the only one in front, else the only one open.
fn frontmost(st: &State) -> Result<(String, String), String> {
    let docs = all_docs(st);
    if docs.is_empty() {
        return Err(no_documents(st));
    }
    let front: Vec<_> = docs.iter().filter(|(_, _, d)| d.frontmost).collect();
    if let Some((k, _, d)) = front.iter().filter(|(_, _, d)| d.focused_at > 0).max_by_key(|(_, _, d)| d.focused_at) {
        return Ok(((*k).clone(), d.id.clone()));
    }
    match (front.as_slice(), docs.as_slice()) {
        ([(k, _, d)], _) | ([], [(k, _, d)]) => Ok(((*k).clone(), d.id.clone())),
        ([], _) => Err(format!("No document is in front; say which one: {}", open_list(st))),
        (many, _) => Err(format!(
            "Several apps have a document in front ({}); say which one.",
            many.iter().map(|(_, e, d)| describe(e, d)).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ---- tools ----

fn doc_prop() -> Value {
    json!({"type": "string", "description": "Which document: the id attach returned (or its file name). Needed when this AItoCraft has several documents attached."})
}

fn own_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "list_documents",
            "description": "Documents open right now in GridCraft, WordCraft and DeckCraft: id, title, path, app, whether it is in front, open questions, who is listening. Start here, then attach.",
            "inputSchema": {"type": "object", "properties": {}},
            "annotations": {"readOnlyHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "attach",
            "description": "Bind this conversation to a document the user names (\"we're working on budget.xlsx\"), so its questions come here. Returns the document id: pass it as `document` on every other call.",
            "inputSchema": {"type": "object", "properties": {
                "document": {"type": "string", "description": "The document's id, file name or part of it (\"budget\"). Omit for the document in front."},
                "label": {"type": "string", "description": "This conversation's project or topic, shown to the user in the document (\"Acme budget review\")."},
            }},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false},
        }),
        json!({
            "name": "detach",
            "description": "Stop working on a document: its questions no longer come here.",
            "inputSchema": {"type": "object", "properties": {"document": doc_prop()}},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false},
        }),
        json!({
            "name": "app_tool",
            "description": "Call a tool of a document's app by name. Only needed when the app started after this conversation and the tool isn't in your list yet.",
            "inputSchema": {"type": "object", "properties": {
                "document": doc_prop(),
                "tool": {"type": "string", "description": "The app's tool, e.g. \"get_context\"."},
                "arguments": {"type": "object", "description": "The tool's arguments."},
            }, "required": ["tool"]},
            "annotations": {"openWorldHint": false},
        }),
    ]
}

/// Merges the apps' tools by name: one tool per name, with `document` added and `listener` taken
/// out (the hub adds it). `apps` is (app title, tools).
pub fn merge_tools(apps: &[(String, Vec<Value>)]) -> Vec<Value> {
    let mut by_name: BTreeMap<String, Vec<(&str, &Value)>> = BTreeMap::new();
    for (title, tools) in apps {
        for t in tools {
            let Some(name) = t.get("name").and_then(Value::as_str) else { continue };
            if OWN.contains(&name) {
                continue;
            }
            let list = by_name.entry(name.to_string()).or_default();
            if !list.iter().any(|(a, _)| a == title) {
                list.push((title.as_str(), t));
            }
        }
    }
    let mut out = vec![];
    for list in by_name.into_values() {
        let Some((_, base)) = list.first() else { continue };
        let mut t = base.as_object().cloned().unwrap_or_default();
        // Input schema: every app's properties; required only what all require.
        let mut props = Map::new();
        let mut required: Option<Vec<String>> = None;
        for (_, tool) in &list {
            let schema = tool.get("inputSchema");
            if let Some(p) = schema.and_then(|s| s.get("properties")).and_then(Value::as_object) {
                for (k, v) in p {
                    props.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
            let req: Vec<String> = schema
                .and_then(|s| s.get("required"))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            required = Some(match required {
                None => req,
                Some(r) => r.into_iter().filter(|x| req.contains(x)).collect(),
            });
        }
        props.remove("listener");
        props.insert("document".into(), doc_prop());
        let required: Vec<String> = required.unwrap_or_default().into_iter().filter(|r| r != "listener" && r != "document").collect();
        let mut schema = json!({"type": "object", "properties": props});
        if !required.is_empty() {
            schema["required"] = json!(required);
        }
        t.insert("inputSchema".into(), schema);
        let titles: Vec<&str> = list.iter().map(|(a, _)| *a).collect();
        let descs: Vec<&str> = list.iter().map(|(_, t)| t.get("description").and_then(Value::as_str).unwrap_or("")).collect();
        let desc = if descs.iter().all(|d| *d == descs[0]) {
            descs[0].to_string()
        } else {
            list.iter().zip(&descs).filter(|(_, d)| !d.is_empty()).map(|((a, _), d)| format!("{a}: {d}")).collect::<Vec<_>>().join("\n")
        };
        t.insert("description".into(), json!(format!("{desc} [{}]", titles.join(", ")).trim_start().to_string()));
        if list.len() > 1 {
            t.remove("outputSchema");
            let read_only = list.iter().all(|(_, t)| t.pointer("/annotations/readOnlyHint").and_then(Value::as_bool) == Some(true));
            if let Some(a) = t.get_mut("annotations").and_then(Value::as_object_mut) {
                a.insert("readOnlyHint".into(), json!(read_only));
            }
        }
        out.push(Value::Object(t));
    }
    out
}

fn signature(tools: &[Value]) -> String {
    serde_json::to_string(tools).unwrap_or_default()
}

fn load_cache(path: &Path) -> BTreeMap<String, Cached> {
    let Ok(meta) = std::fs::metadata(path) else { return BTreeMap::new() };
    if meta.len() > 8 * 1024 * 1024 {
        return BTreeMap::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return BTreeMap::new() };
    let mut out = BTreeMap::new();
    for (app, c) in v.get("apps").and_then(Value::as_object).into_iter().flatten() {
        let tools: Vec<Value> = c.get("tools").and_then(Value::as_array).cloned().unwrap_or_default();
        let title = c.get("title").and_then(Value::as_str).unwrap_or(app).to_string();
        out.insert(app.clone(), Cached { title, tools });
    }
    out
}

fn save_cache(path: &Path, cache: &BTreeMap<String, Cached>) {
    let apps: Map<String, Value> = cache.iter().map(|(k, c)| (k.clone(), json!({"title": c.title, "tools": c.tools}))).collect();
    let text = json!({"note": "The last known tools of each Craft app, so AItoCraft can list them before the app starts.", "apps": apps}).to_string();
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if std::fs::write(&tmp, text).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
