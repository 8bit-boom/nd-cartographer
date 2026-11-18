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

/// Send JSON-RPC lines, return the answers by id.
fn converse(lines: &[&str]) -> std::collections::BTreeMap<i64, serde_json::Value> {
    let exe = env!("CARGO_BIN_EXE_ndcart");
    let mut child = Command::new(exe)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ndcart mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for l in lines {
            writeln!(stdin, "{l}").unwrap();
        }
        stdin.flush().unwrap();
    }
    // Closing stdin ends the loop once every line is answered.
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["id"].as_i64().map(|id| (id, v)))
        .collect()
}

#[test]
fn mcp_serves_the_instruction_pack_validates_specs_and_reads_resources() {
    let pack_toml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/ai/pack.toml"),
    )
    .unwrap();
    let version = pack_toml
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .unwrap()
        .trim()
        .trim_matches('"')
        .to_string();
    let unlinked = serde_json::json!({"name": "M", "floors": [{"name": "G", "rooms": [
        {"id": "hall", "name": "Hall", "w": 6, "h": 5},
        {"id": "orphan", "name": "Closet", "w": 3, "h": 3}]}]});
    let call = serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "validate_spec", "arguments": {"spec": unlinked}}})
    .to_string();
    let answers = converse(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        &call,
        r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"ndcart://ai/pack/reference.md"}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{"uri":"ndcart://ai/pack/nope.md"}}"#,
    ]);
    assert!(answers[&1]["result"]["capabilities"]["resources"].is_object());
    let names: Vec<String> = answers[&2]["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    for n in [
        "get_instructions",
        "get_spec_schema",
        "validate_spec",
        "lint_floor",
    ] {
        assert!(names.contains(&n.to_string()), "{names:?}");
    }
    // A spec with an unlinked room is a normal answer that names the room, not a tool error.
    let r = &answers[&3]["result"];
    assert_ne!(r["isError"], serde_json::json!(true), "{r}");
    let v: serde_json::Value =
        serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["errors"][0].as_str().unwrap().contains("'orphan'"), "{v}");
    let uris: Vec<&str> = answers[&4]["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"ndcart://ai/pack/reference.md"), "{uris:?}");
    let text = answers[&5]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.contains(&format!("pack {version}")), "{text}");
    assert_eq!(answers[&6]["error"]["code"], -32002);
}
