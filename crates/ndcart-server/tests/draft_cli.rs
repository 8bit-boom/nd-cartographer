#![cfg(feature = "bridge")]
//! `ndcart draft` as a real process: batches against the stand-in model server, the template
//! draft with no endpoint at all, `--check`, and `--accept-into` refusing a busy data folder.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stand_in() -> (Guard, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ndcart"))
        .args(["ai", "stand-in", "--port", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn stand-in");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (Guard(child), line.trim().to_string())
}

fn draft(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ndcart"))
        .arg("draft")
        .args(args)
        .env_remove("ND_CARTO_LLM_URL")
        .env_remove("ND_CARTO_LLM_MODEL")
        .env_remove("ND_CARTO_LLM_KEY")
        .env_remove("ND_CARTO_LLM_KEY_FILE")
        .env_remove("ND_CARTO_SERVER")
        .env_remove("ND_CARTO_DATA_DIR")
        .output()
        .expect("run ndcart draft")
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ndcart-draft-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn a_batch_of_three_with_one_forced_failure_gives_two_files_a_report_and_exit_1() {
    let (_g, base) = stand_in();
    let d = tmp("batch");
    let too_long = "x".repeat(2001);
    let prompts = d.join("p.jsonl");
    std::fs::write(
        &prompts,
        format!("\"a crypt\"\n{{\"prompt\":\"{too_long}\"}}\n\"a small shop\"\n"),
    )
    .unwrap();
    let out = d.join("out");
    let o = draft(&[
        "--batch",
        prompts.to_str().unwrap(),
        "--out-dir",
        out.to_str().unwrap(),
        "--llm-url",
        &base,
        "--llm-model",
        "ndcart-local",
    ]);
    assert_eq!(
        o.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('0'))
        .collect();
    assert_eq!(files.len(), 2, "{files:?}");
    let rep: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap()).unwrap();
    assert_eq!(
        (rep["ok"].as_u64(), rep["failed"].as_u64()),
        (Some(2), Some(1))
    );
    assert_eq!(rep["items"][1]["status"], "failed");
    assert!(rep["items"][0]["attempts"].as_u64().unwrap() >= 1);
    // What came out is a spec the studio's own check accepts.
    for f in &files {
        let c = draft(&["--check", out.join(f).to_str().unwrap()]);
        assert_eq!(
            c.status.code(),
            Some(0),
            "{f}: {}",
            String::from_utf8_lossy(&c.stdout)
        );
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_template_draft_needs_no_endpoint_and_check_refuses_a_bad_spec() {
    let d = tmp("tpl");
    let f = d.join("t.json");
    let o = draft(&[
        "a crypt with a secret door",
        "--template",
        "--out",
        f.to_str().unwrap(),
    ]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        draft(&["--check", f.to_str().unwrap()]).status.code(),
        Some(0)
    );
    let bad = d.join("bad.json");
    std::fs::write(&bad, r#"{"name":"X","floors":[{"name":"G","rooms":[{"id":"a","name":"A","w":5,"h":5,"props":[]}]}]}"#).unwrap();
    let c = draft(&["--check", bad.to_str().unwrap()]);
    assert_eq!(c.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&c.stdout).contains("error:"));
    // Without --template and with no model configured, it says what to set.
    let o = draft(&["a crypt"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("ND_CARTO_LLM_URL"));
    let _ = std::fs::remove_dir_all(&d);
}
