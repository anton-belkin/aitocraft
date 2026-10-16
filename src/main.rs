use std::io::Write;
use std::sync::{Arc, Mutex};

use aitocraft::hub::{Config, Hub, Out};

const HELP: &str = "aitocraft — one MCP server for every document open in GridCraft, WordCraft and DeckCraft.

Usage:
  aitocraft                     serve MCP on stdin/stdout (what Claude starts)
  aitocraft --install-desktop   add AItoCraft to Claude Desktop / Cowork (safely; waits for Claude to quit)
  aitocraft --print-config      print the entries for Claude Desktop and Claude Code
  aitocraft --list              list the documents open right now
  aitocraft --version

Environment:
  AITOCRAFT_REGISTRY_DIR   the registry folder (default: ~/Library/Application Support/ArtCraft/agents)
  AITOCRAFT_CHANNELS=0|1   force Claude Code channel events off or on
";

fn list() -> i32 {
    let Some(dir) = aitocraft::registry::dir() else {
        eprintln!("no registry folder on this system");
        return 1;
    };
    let apps = aitocraft::registry::scan(&dir, aitocraft::util::now_ms(), &aitocraft::util::pid_alive);
    if apps.is_empty() {
        println!("No Craft app is running (registry: {}).", dir.display());
        return 0;
    }
    for a in apps {
        println!("{} {} (pid {}, {})", a.title, a.version, a.pid, a.mcp);
        if a.documents.is_empty() {
            println!("  no document open");
        }
        for d in a.documents {
            let front = if d.frontmost { " · in front" } else { "" };
            println!("  {}  {}  [{}]{front}", d.title, d.path.as_deref().unwrap_or("(unsaved)"), d.id);
        }
    }
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flags: Vec<&str> = args.iter().map(String::as_str).collect();
    let code = match flags.as_slice() {
        [] => {
            let out: Out = Arc::new(Mutex::new(Box::new(std::io::stdout())));
            let hub = Hub::new(Config::from_env(), out);
            hub.refresh();
            hub.watch();
            match aitocraft::stdio::serve(hub, std::io::stdin().lock()) {
                Ok(()) => 0,
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "aitocraft: {e}");
                    1
                }
            }
        }
        ["--install-desktop"] => aitocraft::install::install_desktop(false),
        ["--install-desktop", "--wait"] => aitocraft::install::install_desktop(true),
        ["--print-config"] => aitocraft::install::print_config(),
        ["--list"] => list(),
        ["--version" | "-V"] => {
            println!("aitocraft {}", env!("CARGO_PKG_VERSION"));
            0
        }
        ["--help" | "-h"] => {
            print!("{HELP}");
            0
        }
        _ => {
            let _ = write!(std::io::stderr(), "{HELP}");
            2
        }
    };
    std::process::exit(code);
}
