#![cfg(feature = "bridge")]
//! `ndcart ai stand-in` as a real process: the binary starts, prints its address, and a client
//! (the studio's own `DirectChat`) gets a valid spec out of it. `fix-once` takes two asks.

use ndcart_bridge::{ChatOptions, DirectChat, Msg};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

struct Guard(Child);
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(behaviour: &str) -> (Guard, String) {
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

fn system_prompt() -> String {
    // The pack's map prompt starts with this sentence; the stand-in recognises the kind by it.
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/ai/system_map.md"
    ))
    .unwrap()
}

fn valid(reply: &str) -> bool {
    let v: serde_json::Value = match serde_json::from_str(reply) {
        Ok(v) => v,
        Err(_) => return false,
    };
    v["floors"].is_array()
        && v.get("props").is_none()
        && v["floors"][0]["rooms"][0].get("props").is_none()
}

#[tokio::test]
async fn the_stand_in_binary_serves_a_spec_and_fix_once_needs_a_second_ask() {
    let (_g, base) = spawn("good");
    assert!(base.starts_with("http://127.0.0.1:"), "{base}");
    let chat = DirectChat::new(&base, "ndcart-local", None).unwrap();
    let reply = chat
        .chat_messages(
            &[
                Msg::system(system_prompt()),
                Msg::user("Design this map: a crypt"),
            ],
            &ChatOptions::default(),
        )
        .await
        .unwrap();
    assert!(valid(&reply.content), "{}", reply.content);

    let (_g2, base) = spawn("fix-once");
    let chat = DirectChat::new(&base, "ndcart-local", None).unwrap();
    let mut msgs = vec![
        Msg::system(system_prompt()),
        Msg::user("Design this map: a crypt"),
    ];
    let mut attempts = 0;
    loop {
        attempts += 1;
        let r = chat
            .chat_messages(&msgs, &ChatOptions::default())
            .await
            .unwrap();
        if valid(&r.content) {
            break;
        }
        assert!(attempts < 5, "never fixed");
        msgs.push(Msg::assistant(r.content));
        msgs.push(Msg::user("The reply was refused. Reply again."));
    }
    assert_eq!(attempts, 2);
}
