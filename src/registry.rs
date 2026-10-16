//! The registry (PROTOCOL.md §1): one JSON file per running Craft app, listing its MCP endpoint and
//! its open documents. Titles and paths only; no content.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::http::Url;

/// The protocol family this hub speaks; files of another major version are ignored.
pub const PROTOCOL: &str = "aitocraft/1";
/// A heartbeat older than this means the app is gone (or hung).
pub const STALE_MS: u64 = 30_000;
/// Registry files larger than this are ignored.
const MAX_FILE: u64 = 1024 * 1024;
/// At most this many registry files are read.
const MAX_FILES: usize = 256;

/// One running app instance.
#[derive(Clone, Debug, PartialEq)]
pub struct AppEntry {
    /// The file's stem, `<app>-<pid>`: the instance's key.
    pub key: String,
    /// `gridcraft`, `wordcraft`, `deckcraft`.
    pub app: String,
    /// `GridCraft`.
    pub title: String,
    pub version: String,
    pub pid: u32,
    pub mcp: Url,
    pub heartbeat: u64,
    /// When one of the app's windows last came to the front (0: not said; documents may say it themselves).
    pub focused_at: u64,
    pub documents: Vec<Doc>,
}

/// One open document.
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub id: String,
    pub title: String,
    pub path: Option<String>,
    pub frontmost: bool,
    /// When this document last came to the front (its own `focusedAt`, else the app's; 0: not said).
    pub focused_at: u64,
    /// The object as the app wrote it (openQuestions, listeners, fields added later).
    pub raw: Value,
}

/// The registry folder: `$AITOCRAFT_REGISTRY_DIR`, else this system's place for it.
pub fn dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("AITOCRAFT_REGISTRY_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    default_dir()
}

pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/ArtCraft/agents"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("ArtCraft").join("agents"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|d| d.join("ArtCraft").join("agents"))
    }
}

/// Whether `protocol` is of the major version this hub speaks (`aitocraft/1`, `aitocraft/1.2`).
pub fn protocol_ok(protocol: &str) -> bool {
    protocol == PROTOCOL || protocol.strip_prefix(PROTOCOL).is_some_and(|rest| rest.starts_with('.'))
}

fn title_case(app: &str) -> String {
    let mut c = app.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// Parses one registry file. `key` is the file's stem.
pub fn parse(key: &str, text: &str) -> Result<AppEntry, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let protocol = v.get("protocol").and_then(Value::as_str).unwrap_or("");
    if !protocol_ok(protocol) {
        return Err(format!("protocol {protocol:?}, not {PROTOCOL}"));
    }
    let app = v.get("app").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("no app")?.to_ascii_lowercase();
    let title =
        v.get("appTitle").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| title_case(&app));
    let pid = v.get("pid").and_then(Value::as_u64).and_then(|p| u32::try_from(p).ok()).ok_or("no pid")?;
    let mcp = Url::parse(v.get("mcp").and_then(Value::as_str).ok_or("no mcp")?)?;
    let heartbeat = v.get("heartbeat").and_then(Value::as_u64).ok_or("no heartbeat")?;
    let focused_at = v.get("focusedAt").and_then(Value::as_u64).unwrap_or(0);
    let version = v.get("version").and_then(Value::as_str).unwrap_or("").to_string();
    let mut documents = Vec::new();
    for d in v.get("documents").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(500) {
        let Some(id) = d.get("id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else { continue };
        if documents.iter().any(|x: &Doc| x.id == id) {
            continue;
        }
        let path = d.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        let title = d
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| path.as_deref().and_then(|p| Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Untitled".into());
        documents.push(Doc {
            id: id.to_string(),
            title,
            path,
            frontmost: d.get("frontmost").and_then(Value::as_bool).unwrap_or(false),
            focused_at: d.get("focusedAt").and_then(Value::as_u64).unwrap_or(focused_at),
            raw: d.clone(),
        });
    }
    Ok(AppEntry { key: key.to_string(), app, title, version, pid, mcp, heartbeat, focused_at, documents })
}

/// What a registry file says about its app.
#[derive(Debug, PartialEq)]
pub enum Verdict {
    Live(AppEntry),
    /// The process is gone: the file may be deleted.
    Dead,
    /// The heartbeat is old (the app may be hung); ignored but kept.
    Stale,
    Invalid(String),
}

pub fn judge(key: &str, text: &str, now: u64, alive: &dyn Fn(u32) -> bool) -> Verdict {
    let e = match parse(key, text) {
        Ok(e) => e,
        Err(m) => return Verdict::Invalid(m),
    };
    if !alive(e.pid) {
        return Verdict::Dead;
    }
    // A heartbeat from the future (clock moved back) counts as fresh.
    if now.saturating_sub(e.heartbeat) > STALE_MS {
        return Verdict::Stale;
    }
    Verdict::Live(e)
}

/// The live apps in `dir`, sorted by key. Deletes the files of processes that are gone.
pub fn scan(dir: &Path, now: u64, alive: &dyn Fn(u32) -> bool) -> Vec<AppEntry> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out = Vec::new();
    for ent in rd.flatten().take(MAX_FILES) {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(key) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string) else { continue };
        if key.starts_with('.') {
            continue;
        }
        let Ok(meta) = ent.metadata() else { continue };
        if !meta.is_file() || meta.len() > MAX_FILE {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        match judge(&key, &text, now, alive) {
            Verdict::Live(e) => out.push(e),
            Verdict::Dead => {
                let _ = std::fs::remove_file(&path);
            }
            Verdict::Stale | Verdict::Invalid(_) => {}
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    out
}
