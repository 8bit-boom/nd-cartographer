//! The MCP stdio surface (plan §21.17, §22.6): the handshake, the tool list,
//! and a tool call against a real data dir — as an integration test, so
//! CARGO_BIN_EXE_ndcart names the built binary.

use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn mcp_handshake_lists_tools_and_serves_a_tool_call() {
    let dir = std::env::temp_dir().join(format!("ndcart-mcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let exe = env!("CARGO_BIN_EXE_ndcart");
    let mut child = Command::new(exe)
        .arg("mcp")
        .arg("--data-dir")
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ndcart mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_stats","arguments":{"doc":"Golden"}}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"bogus"}"#,
        ] {
            writeln!(stdin, "{line}").unwrap();
        }
        stdin.flush().unwrap();
    }
    // Give the child a moment to answer, then close stdin so it exits.
    std::thread::sleep(std::time::Duration::from_millis(700));
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let errtext = String::from_utf8_lossy(&out.stderr).to_string();

    assert!(
        text.contains("protocolVersion"),
        "stdout was: {text:?} · stderr was: {errtext:?}"
    );
    assert!(text.contains("get_floor_spec"), "{text}");
    assert!(text.contains("docs"), "{text}");
    assert!(text.contains("unknown method bogus"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
