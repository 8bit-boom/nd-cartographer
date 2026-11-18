//! With no AI set up, `/api/draft` answers from keywords instead of 404 (plan 3.6). None of
//! this needs the AI client, so it also runs in the `--no-default-features` build.

use super::*;

fn no_ai_env() -> bool {
    std::env::var("ND_CARTO_SERVER").is_err() && std::env::var("ND_CARTO_LLM_URL").is_err()
}

#[tokio::test]
async fn without_an_ai_the_draft_is_a_valid_keyword_layout_that_says_so() {
    if !no_ai_env() {
        return; // a developer's own environment
    }
    let router = build_router(test_app());
    let (st, b) = call(
        &router,
        "POST",
        "/api/draft",
        br#"{"prompt":"a crypt with a secret chamber, 3 levels"}"#.to_vec(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    let v = jv(&b);
    assert_eq!(v["source"], "template");
    assert!(v["note"].as_str().unwrap().contains("keywords"));
    assert_eq!(v["spec"]["floors"].as_array().unwrap().len(), 3);
    // The spec is the real thing: the checker accepts it.
    let (st, c) = call(
        &router,
        "POST",
        "/api/spec/check",
        serde_json::json!({"spec": v["spec"]})
            .to_string()
            .into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(jv(&c)["ok"], true, "{}", String::from_utf8_lossy(&c));
    // And it goes through the proposal route like any AI spec.
    let (id, _) = new_doc(&router, "T").await;
    let (st, _) = call(
        &router,
        "POST",
        &format!("/api/docs/{id}/propose"),
        serde_json::json!({"spec": v["spec"], "seed": 1})
            .to_string()
            .into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // A background request with no AI is answered at once (there is nothing to wait for).
    let (_, b) = call(
        &router,
        "POST",
        "/api/draft",
        br#"{"prompt":"a tavern","background":true}"#.to_vec(),
    )
    .await;
    assert_eq!(jv(&b)["source"], "template");
}

#[tokio::test]
async fn strict_ai_and_a_bad_prompt_keep_their_errors() {
    if !no_ai_env() {
        return;
    }
    let router = build_router(test_app());
    let (st, _) = call(
        &router,
        "POST",
        "/api/draft",
        br#"{"prompt":"a crypt","strict_ai":true}"#.to_vec(),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &router,
        "POST",
        "/api/draft",
        br#"{"prompt":"  "}"#.to_vec(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}
