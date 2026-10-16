//! The real binary over real pipes: one JSON object per line, nothing else on stdout, and it
//! exits when stdin closes.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[test]
fn speaks_newline_delimited_json_rpc_and_exits_when_stdin_closes() {
    let reg = std::env::temp_dir().join(format!("aitocraft-it-{}", std::process::id())).join("agents");
    std::fs::create_dir_all(&reg).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aitocraft"))
        .env("AITOCRAFT_REGISTRY_DIR", &reg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut send = |v: Value| writeln!(stdin, "{v}").unwrap();
    let mut read = || {
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
    };
    send(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "claude-ai", "version": "1"}}}),
    );
    let init = read();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["capabilities"]["tools"]["listChanged"], true);
    assert!(init["result"]["instructions"].as_str().unwrap().contains("wait_for_questions"));
    send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let list = read();
    assert_eq!(list["id"], 2);
    assert!(list["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "attach"));
    send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "attach", "arguments": {"document": "budget"}}}));
    let r = read();
    assert_eq!(r["id"], 3);
    assert_eq!(r["result"]["isError"], true);
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("No Craft app is running"));
    drop(send);
    drop(stdin);
    let until = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < until, "still running after stdin closed");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success());
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut out, &mut rest).unwrap();
    assert!(rest.is_empty(), "nothing else on stdout: {rest:?}");
    let _ = std::fs::remove_dir_all(reg.parent().unwrap());
}
