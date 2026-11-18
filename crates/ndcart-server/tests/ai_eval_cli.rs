#![cfg(feature = "bridge")]
//! `ndcart ai eval` as a real process against the stand-in server as a real process: the
//! report on stdout, the file written by `--out`, and the exit codes.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stand_in(behaviour: &str) -> (Guard, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ndcart"))
        .args(["ai", "stand-in", "--port", "0", "--behaviour", behaviour])
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

fn eval(base: &str, extra: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ndcart"));
    cmd.args(["ai", "eval", "--url", base, "--model", "ndcart-local"])
        .args(extra)
        .env_remove("ND_CARTO_LLM_URL")
        .env_remove("ND_CARTO_LLM_MODEL")
        .env_remove("ND_CARTO_LLM_KEY")
        .env_remove("ND_CARTO_LLM_KEY_FILE")
        .env_remove("ND_CARTO_SERVER");
    cmd.output().expect("run ndcart ai eval")
}

#[test]
fn eval_writes_the_report_and_exits_zero_for_a_good_model() {
    let (_g, base) = stand_in("good");
    let dir = std::env::temp_dir().join(format!("ndcart-eval-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("report.json");
    let o = eval(
        &base,
        &[
            "--set",
            "map",
            "--case",
            "shop",
            "--case",
            "crypt",
            "--json",
            "--out",
            out.to_str().unwrap(),
            "--data-dir",
            dir.to_str().unwrap(),
        ],
    );
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    let printed: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(printed, written);
    assert_eq!(printed["verdict"], "good");
    assert_eq!(printed["summary"]["valid_pct"], 100.0);
    assert_eq!(printed["cases"].as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn eval_prints_a_table_and_exits_3_for_a_model_that_cannot_do_it() {
    let (_g, base) = stand_in("garbage");
    let o = eval(&base, &["--set", "map", "--case", "shop"]);
    assert_eq!(o.status.code(), Some(3));
    let text = String::from_utf8_lossy(&o.stdout);
    assert!(text.contains("TOO WEAK") && text.contains("shop"), "{text}");
}

#[test]
fn eval_says_what_is_wrong_with_the_arguments_and_exits_1() {
    let o = eval("http://127.0.0.1:9/v1", &["--set", "nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("unknown set"));
    let o = eval("http://127.0.0.1:9/v1", &["--repeat", "0"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--repeat must be"));
}
