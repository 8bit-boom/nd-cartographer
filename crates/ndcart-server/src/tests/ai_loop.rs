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
