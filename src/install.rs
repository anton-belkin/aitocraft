//! Installing the hub once: the `aitocraft` entry in Claude Desktop's `claude_desktop_config.json`
//! (`--install-desktop`), and the lines for Claude Desktop and Claude Code (`--print-config`).
//!
//! Claude Desktop keeps that file in memory and writes it back when it quits, so an entry added
//! while it runs is lost. The entry is therefore written at once only when Claude isn't running;
//! otherwise a background copy of this program waits until Claude has quit, then writes. Other
//! servers and settings are kept as they are, the old file is copied aside first, and the new one
//! goes through a temporary file and a rename.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

/// The server's name in clients' configs (`server:aitocraft` for Claude Code channels).
pub const SERVER_NAME: &str = "aitocraft";
/// Longest wait for Claude to quit.
const WAIT_SECS: u64 = 30 * 60;

/// Where Claude Desktop keeps its config on this system.
pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/Claude/claude_desktop_config.json"))
    } else if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Claude").join("claude_desktop_config.json"))
    } else {
        home.map(|h| h.join(".config/Claude/claude_desktop_config.json"))
    }
}

/// The config entry that starts `exe`.
pub fn entry(exe: &Path) -> Value {
    json!({"command": exe.to_string_lossy()})
}

/// What a config says about us.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Configured {
    /// No Claude Desktop config folder on this machine.
    NoClaude,
    Missing,
    /// There, but pointing somewhere else (another build).
    Different,
    Yes,
}

pub fn configured(path: &Path, want: &Value) -> Configured {
    let Ok(text) = std::fs::read_to_string(path) else {
        return if path.parent().is_some_and(Path::exists) { Configured::Missing } else { Configured::NoClaude };
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return Configured::Missing };
    match v.get("mcpServers").and_then(|s| s.get(SERVER_NAME)) {
        None => Configured::Missing,
        Some(e) if e == want => Configured::Yes,
        Some(_) => Configured::Different,
    }
}

/// Adds (or replaces) `mcpServers.aitocraft`, keeping everything else; copies the old file to
/// `<file>.bak-aitocraft-<ms>` first. Refuses a file that isn't a JSON object. Returns the backup.
pub fn write(path: &Path, want: &Value) -> Result<Option<PathBuf>, String> {
    let (mut v, backup) = match std::fs::read_to_string(path) {
        Ok(text) => {
            let v: Value = serde_json::from_str(&text).map_err(|e| format!("{} isn't valid JSON ({e}); not touching it", path.display()))?;
            if !v.is_object() {
                return Err(format!("{} isn't a JSON object; not touching it", path.display()));
            }
            let mut name = path.as_os_str().to_os_string();
            name.push(format!(".bak-aitocraft-{}", crate::util::now_ms()));
            let backup = PathBuf::from(name);
            std::fs::write(&backup, text).map_err(|e| format!("backup {}: {e}", backup.display()))?;
            (v, Some(backup))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            (json!({}), None)
        }
        Err(e) => return Err(format!("{}: {e}; not touching it", path.display())),
    };
    let Some(o) = v.as_object_mut() else { return Err("not a JSON object".into()) };
    let servers = o.entry("mcpServers").or_insert_with(|| json!({}));
    let Some(servers) = servers.as_object_mut() else { return Err(format!("`mcpServers` in {} isn't an object; not touching it", path.display())) };
    servers.insert(SERVER_NAME.into(), want.clone());
    let mut text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    text.push('\n');
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".aitocraft-tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(backup)
}

/// Whether Claude Desktop is running (macOS and Windows; elsewhere it is assumed not to be).
pub fn claude_running() -> bool {
    if cfg!(target_os = "macos") {
        let Ok(out) = std::process::Command::new("/bin/ps").args(["-axo", "comm="]).output() else { return false };
        String::from_utf8_lossy(&out.stdout).lines().any(|l| l.trim_end().ends_with("/Claude.app/Contents/MacOS/Claude"))
    } else if cfg!(target_os = "windows") {
        let Ok(out) = std::process::Command::new("tasklist").args(["/FI", "IMAGENAME eq Claude.exe", "/NH"]).output() else { return false };
        String::from_utf8_lossy(&out.stdout).contains("Claude.exe")
    } else {
        false
    }
}

/// This program, as the config should start it.
pub fn exe() -> Result<PathBuf, String> {
    let p = std::env::current_exe().map_err(|e| format!("can't tell where aitocraft is: {e}"))?;
    Ok(std::fs::canonicalize(&p).unwrap_or(p))
}

fn log_path() -> Option<PathBuf> {
    crate::registry::dir().and_then(|d| d.parent().map(|p| p.join("aitocraft-install.log")))
}

fn log(line: &str) {
    if let Some(p) = log_path() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
            let _ = writeln!(f, "{} {line}", crate::util::now_ms());
        }
    }
}

fn done_message(path: &Path, backup: Option<&Path>) -> String {
    let b = backup.map(|b| format!(" (the old file is kept as {})", b.display())).unwrap_or_default();
    format!("Added AItoCraft to {}{b}. Open Claude, start a chat where the work belongs and say: we're working on <file name>.", path.display())
}

/// `--install-desktop`: the exit code.
pub fn install_desktop(waiting: bool) -> i32 {
    let mut err = std::io::stderr();
    let exe = match exe() {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 1;
        }
    };
    let Some(path) = config_path() else {
        let _ = writeln!(err, "no place for Claude Desktop's config on this system");
        return 1;
    };
    let want = entry(&exe);
    if waiting {
        return wait_and_write(&path, &want);
    }
    match configured(&path, &want) {
        Configured::Yes => {
            println!("AItoCraft is already in {} — nothing to do.", path.display());
            return 0;
        }
        Configured::NoClaude => println!(
            "Claude Desktop doesn't seem to be installed ({} is missing); writing the config anyway.",
            path.parent().map(|p| p.display().to_string()).unwrap_or_default()
        ),
        Configured::Different => println!("Replacing the aitocraft entry in {} (it pointed elsewhere).", path.display()),
        Configured::Missing => {}
    }
    if exe.components().any(|c| c.as_os_str() == "target") {
        println!(
            "Note: this is a build in target/; Claude will run {} until you install it elsewhere and run --install-desktop again.",
            exe.display()
        );
    }
    if !claude_running() {
        return match write(&path, &want) {
            Ok(backup) => {
                println!("{}", done_message(&path, backup.as_deref()));
                0
            }
            Err(e) => {
                let _ = writeln!(err, "{e}");
                1
            }
        };
    }
    // Claude would overwrite the file when it quits: wait for that in the background.
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("--install-desktop")
        .arg("--wait")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match cmd.spawn() {
        Ok(_) => {
            println!(
                "Claude Desktop is running, and it rewrites its config when it quits. Quit Claude (Cmd+Q): AItoCraft adds itself right after, then open Claude again. (Waiting in the background for up to 30 minutes; log: {}.)",
                log_path().map(|p| p.display().to_string()).unwrap_or_else(|| "none".into())
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "couldn't start waiting for Claude to quit ({e}). Quit Claude Desktop and run aitocraft --install-desktop again.");
            1
        }
    }
}

fn wait_and_write(path: &Path, want: &Value) -> i32 {
    log("waiting for Claude Desktop to quit");
    for _ in 0..WAIT_SECS {
        std::thread::sleep(Duration::from_secs(1));
        if claude_running() {
            continue;
        }
        // Let it finish writing its own copy first.
        std::thread::sleep(Duration::from_millis(1500));
        if claude_running() {
            continue;
        }
        return match write(path, want) {
            Ok(backup) => {
                log(&done_message(path, backup.as_deref()));
                0
            }
            Err(e) => {
                log(&format!("failed: {e}"));
                1
            }
        };
    }
    log("Claude Desktop didn't quit within 30 minutes; run aitocraft --install-desktop again");
    1
}

/// `--print-config`: the entries for Claude Desktop and Claude Code.
pub fn print_config() -> i32 {
    let exe = match exe() {
        Ok(e) => e,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "{e}");
            return 1;
        }
    };
    let exe_s = exe.to_string_lossy();
    let entry = serde_json::to_string_pretty(&json!({"mcpServers": {SERVER_NAME: entry(&exe)}})).unwrap_or_default();
    let quoted = if exe_s.contains(' ') { format!("\"{exe_s}\"") } else { exe_s.to_string() };
    println!(
        "Claude Desktop / Cowork — add to {}:",
        config_path().map(|p| p.display().to_string()).unwrap_or_else(|| "claude_desktop_config.json".into())
    );
    println!("{entry}");
    println!("(or let AItoCraft do it safely: {quoted} --install-desktop)");
    println!();
    println!("Claude Code — once, for every project:");
    println!("  claude mcp add --scope user {SERVER_NAME} -- {quoted}");
    println!("Questions pushed into the session (channels, research preview):");
    println!("  claude --dangerously-load-development-channels server:{SERVER_NAME}");
    println!();
    println!("Then, in the chat or session where the work belongs: \"We're working on budget.xlsx.\"");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aitocraft-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn adds_the_entry_and_keeps_everything_else() {
        let d = dir("keep");
        let p = d.join("claude_desktop_config.json");
        std::fs::write(
            &p,
            r#"{"globalShortcut": "Alt+Space", "mcpServers": {"other": {"command": "x", "env": {"TOKEN": "secret"}}}, "preferences": {"a": 1}}"#,
        )
        .unwrap();
        let want = entry(Path::new("/usr/local/bin/aitocraft"));
        assert_eq!(configured(&p, &want), Configured::Missing);
        let backup = write(&p, &want).unwrap().unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["env"]["TOKEN"], "secret");
        assert_eq!(v["globalShortcut"], "Alt+Space");
        assert_eq!(v["preferences"]["a"], 1);
        assert_eq!(v["mcpServers"]["aitocraft"]["command"], "/usr/local/bin/aitocraft");
        assert!(std::fs::read_to_string(backup).unwrap().contains("secret"));
        assert_eq!(configured(&p, &want), Configured::Yes);
        assert_eq!(configured(&p, &entry(Path::new("/elsewhere"))), Configured::Different);
        assert!(!d.join("claude_desktop_config.json.aitocraft-tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_config_is_left_alone_and_a_missing_one_is_made() {
        let d = dir("broken");
        let p = d.join("claude_desktop_config.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(write(&p, &json!({"command": "x"})).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json");
        std::fs::write(&p, "[1, 2]").unwrap();
        assert!(write(&p, &json!({"command": "x"})).is_err());
        std::fs::write(&p, r#"{"mcpServers": 3}"#).unwrap();
        assert!(write(&p, &json!({"command": "x"})).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), r#"{"mcpServers": 3}"#);
        let q = d.join("new/claude_desktop_config.json");
        assert_eq!(configured(&q, &json!({})), Configured::NoClaude);
        assert_eq!(write(&q, &json!({"command": "x"})).unwrap(), None);
        assert_eq!(configured(&q, &json!({"command": "x"})), Configured::Yes);
        let _ = std::fs::remove_dir_all(&d);
    }
}
