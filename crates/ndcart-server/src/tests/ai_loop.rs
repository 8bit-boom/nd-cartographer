//! The AI loop end to end against the stand-in model server: the studio's own routes, a real
//! `DirectChat` over HTTP, no network beyond loopback and no model.

use super::*;
use crate::ai_standin::{self, Behaviour, Options};

async fn app_for(opts: Options) -> (Router, ai_standin::Handle) {
    let h = ai_standin::spawn(opts).await;
    let app = test_app();
    *app.ai.lock().unwrap() = ai_settings::StoredAiSettings {
        direct_url: Some(h.base.clone()),
        direct_model: Some("ndcart-local".into()),
        ..Default::default()
    };
    (build_router(app), h)
}

async fn post_json(
    router: &Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (st, b) = call(router, "POST", uri, body.to_string().into_bytes()).await;
    (st, jv(&b))
}

#[tokio::test]
async fn a_good_model_drafts_a_valid_spec_in_one_ask() {
    let (router, h) = app_for(Options::default()).await;
    let (st, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["attempts"], 1);
    assert!(v["spec"]["floors"].is_array());
    assert_eq!(h.stats.chats(), 1);
}

#[tokio::test]
async fn the_repair_ask_is_a_four_message_conversation_that_quotes_the_complaint() {
    let (router, h) = app_for(Options {
        behaviour: Behaviour::FixOnce,
        ..Options::default()
    })
    .await;
    let (st, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["attempts"], 2);
    let bodies = h.stats.bodies();
    assert_eq!(bodies.len(), 2);
    let msgs = bodies[1]["messages"].as_array().unwrap();
    let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant", "user"]);
    assert!(
        msgs[3]["content"]
            .as_str()
            .unwrap()
            .contains("Unknown field(s)"),
        "{}",
        msgs[3]["content"]
    );
}

#[tokio::test]
async fn the_theme_pack_route_repairs_a_reply_that_is_not_json() {
    let (router, h) = app_for(Options {
        behaviour: Behaviour::FixOnce,
        ..Options::default()
    })
    .await;
    let (st, v) = post_json(
        &router,
        "/api/docs/none/theme-pack",
        serde_json::json!({"theme":"ice"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["plan"]["palette"].is_array());
    assert_eq!(h.stats.chats(), 2);
}

#[tokio::test]
async fn a_model_that_never_gets_it_right_ends_in_a_plain_error_after_three_asks() {
    let (router, h) = app_for(Options {
        behaviour: Behaviour::Garbage,
        ..Options::default()
    })
    .await;
    let (st, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    assert!(
        v["error"].as_str().unwrap_or_default().contains("3 tries"),
        "{v}"
    );
    assert_eq!(h.stats.chats(), 3);
}

#[tokio::test]
async fn a_llama_cpp_shaped_server_gets_the_schema_and_is_told_not_to_think() {
    let (router, h) = app_for(Options::default()).await;
    post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    let b = &h.stats.bodies()[0];
    assert_eq!(b["response_format"]["json_schema"]["name"], "map_spec");
    assert_eq!(
        b["response_format"]["json_schema"]["schema"]["title"],
        "MapSpec"
    );
    assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);
}

#[tokio::test]
async fn a_server_without_schema_support_succeeds_on_the_fallback_and_is_not_asked_again() {
    let (router, h) = app_for(Options {
        behaviour: Behaviour::NoSchema,
        ..Options::default()
    })
    .await;
    let (st, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // One refused try with the schema, one plain.
    assert_eq!(h.stats.chats(), 2);
    assert_eq!(
        h.stats
            .with_schema
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let (st, _) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a shop"}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h.stats.chats(), 3);
    assert_eq!(
        h.stats
            .with_schema
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn the_thinking_setting_lifts_the_no_think_request() {
    let (router, h) = app_for(Options::default()).await;
    let (st, v) = post_json(
        &router,
        "/api/ai-settings",
        serde_json::json!({"thinking": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["settings"]["thinking"], true);
    post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt"}),
    )
    .await;
    assert!(h.stats.bodies()[0].get("chat_template_kwargs").is_none());
}

async fn poll_job(
    router: &Router,
    id: &str,
    until: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    for _ in 0..200 {
        let (st, b) = call(router, "GET", &format!("/api/ai/jobs/{id}"), vec![]).await;
        assert_eq!(st, StatusCode::OK);
        let v = jv(&b);
        if until(&v) {
            return v;
        }
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
    panic!("job {id} never reached the wanted state");
}

#[tokio::test]
async fn a_background_draft_goes_from_asking_to_done_with_its_spec() {
    let (router, h) = app_for(Options {
        behaviour: Behaviour::Slow,
        delay_ms: 300,
        ..Options::default()
    })
    .await;
    let (st, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt","background":true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["job"].as_str().unwrap().to_string();
    let asking = poll_job(&router, &id, |j| j["phase"] == "asking").await;
    assert_eq!(
        (asking["attempt"].as_u64(), asking["max_attempts"].as_u64()),
        (Some(1), Some(3))
    );
    assert_eq!(asking["status"], "running");
    let done = poll_job(&router, &id, |j| j["status"] != "running").await;
    assert_eq!(done["status"], "done", "{done}");
    assert_eq!(done["phase"], "done");
    assert!(done["spec"]["floors"].is_array());
    assert_eq!(done["attempts"], 1);
    assert_eq!(h.stats.chats(), 1);
}

#[tokio::test]
async fn a_repairing_draft_shows_its_try_and_stop_ends_it_within_a_second() {
    let (router, _h) = app_for(Options {
        behaviour: Behaviour::Slow,
        delay_ms: 5000,
        ..Options::default()
    })
    .await;
    let (_, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt","background":true}),
    )
    .await;
    let id = v["job"].as_str().unwrap().to_string();
    poll_job(&router, &id, |j| j["phase"] == "asking").await;
    let t = std::time::Instant::now();
    let (st, _) = post_json(
        &router,
        &format!("/api/ai/jobs/{id}/cancel"),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let j = poll_job(&router, &id, |j| j["status"] == "cancelled").await;
    assert!(t.elapsed() < std::time::Duration::from_secs(1));
    assert_eq!(j["message"], "Cancelled.");
}

#[tokio::test]
async fn a_background_draft_that_needs_repair_reports_the_second_try() {
    let (router, _h) = app_for(Options {
        behaviour: Behaviour::FixOnce,
        ..Options::default()
    })
    .await;
    let (_, v) = post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a crypt","background":true}),
    )
    .await;
    let id = v["job"].as_str().unwrap().to_string();
    let done = poll_job(&router, &id, |j| j["status"] != "running").await;
    assert_eq!(
        (done["status"].as_str(), done["attempts"].as_u64()),
        (Some("done"), Some(2)),
        "{done}"
    );
}

#[tokio::test]
async fn with_the_cache_on_the_same_request_twice_asks_the_model_once() {
    let h = ai_standin::spawn(Options::default()).await;
    let dir = std::env::temp_dir().join(format!("ndcart-cache-route-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut app = test_app();
    Arc::get_mut(&mut app).unwrap().data_dir = Some(dir.clone());
    *app.ai.lock().unwrap() = ai_settings::StoredAiSettings {
        direct_url: Some(h.base.clone()),
        direct_model: Some("ndcart-local".into()),
        cache: Some(true),
        ..Default::default()
    };
    let router = build_router(app);
    let body = serde_json::json!({"prompt":"a crypt"});
    let (_, first) = post_json(&router, "/api/draft", body.clone()).await;
    let (_, second) = post_json(&router, "/api/draft", body.clone()).await;
    assert_eq!(h.stats.chats(), 1, "the second answer came from the cache");
    assert_eq!(first["spec"], second["spec"]);
    assert_eq!(second["attempts"], 0);
    // A different request misses; "Ask again" skips the read and refreshes the entry.
    post_json(
        &router,
        "/api/draft",
        serde_json::json!({"prompt":"a shop"}),
    )
    .await;
    assert_eq!(h.stats.chats(), 2);
    let mut again = body.clone();
    again["nocache"] = true.into();
    post_json(&router, "/api/draft", again).await;
    assert_eq!(h.stats.chats(), 3);
    // Cache off (the default) never reads: another app on the same dir without the setting.
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_cache_is_off_by_default() {
    let (router, h) = app_for(Options::default()).await;
    let body = serde_json::json!({"prompt":"a crypt"});
    post_json(&router, "/api/draft", body.clone()).await;
    post_json(&router, "/api/draft", body).await;
    assert_eq!(h.stats.chats(), 2);
}
