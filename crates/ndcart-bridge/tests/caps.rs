//! Endpoint detection and constrained decoding (plan 3.2), against wiremock stand-ins.

use ndcart_bridge::caps::{self, EndpointKind};
use ndcart_bridge::{ChatOptions, DirectChat, Msg};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn ok_reply() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": "{\"a\":1}"}, "finish_reason": "stop"}]
    }))
}

async fn models(server: &MockServer, owned_by: &str) {
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"data": [{"id": "ndcart-local", "owned_by": owned_by}]})),
        )
        .mount(server)
        .await;
}

async fn chat_bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/chat/completions")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn msgs() -> Vec<Msg> {
    vec![Msg::system("s"), Msg::user("u")]
}

fn schema() -> Value {
    json!({"type": "object", "properties": {"a": {"type": "integer"}}})
}

#[tokio::test]
async fn a_llama_cpp_props_answer_means_llama_cpp_and_the_schema_rides_along() {
    let server = MockServer::start().await;
    models(&server, "llamacpp").await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"build_info": "b1", "default_generation_settings": {"n_ctx": 8192}}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_reply())
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let c = caps::probe(&base, None).await;
    assert_eq!(c.kind, EndpointKind::LlamaCpp);
    assert_eq!((c.ctx, c.json_schema), (Some(8192), Some(true)));
    assert_eq!(c.model_ids, ["ndcart-local"]);

    let chat = DirectChat::new(&base, "ndcart-local", None).unwrap();
    chat.chat_structured(&msgs(), Some(("spec", &schema())), &ChatOptions::default())
        .await
        .unwrap();
    let b = &chat_bodies(&server).await[0];
    assert_eq!(b["response_format"]["type"], "json_schema");
    assert_eq!(b["response_format"]["json_schema"]["name"], "spec");
    assert_eq!(b["response_format"]["json_schema"]["strict"], true);
    assert_eq!(b["response_format"]["json_schema"]["schema"], schema());
    assert_eq!(b["chat_template_kwargs"]["enable_thinking"], false);

    // Thinking can be turned back on.
    let chat = chat.with_thinking(true);
    chat.chat_structured(&msgs(), Some(("spec", &schema())), &ChatOptions::default())
        .await
        .unwrap();
    assert!(chat_bodies(&server).await[1]
        .get("chat_template_kwargs")
        .is_none());
}

#[tokio::test]
async fn a_server_that_refuses_the_schema_is_asked_again_without_it_and_remembered() {
    let server = MockServer::start().await;
    models(&server, "someone").await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(
            json!({"response_format": {"type": "json_schema"}}),
        ))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"message": "unsupported: response_format"}})),
        )
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_reply())
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let chat = DirectChat::new(&base, "ndcart-local", None).unwrap();
    let r = chat
        .chat_structured(&msgs(), Some(("spec", &schema())), &ChatOptions::default())
        .await
        .unwrap();
    assert_eq!(r.content, "{\"a\":1}");
    let bodies = chat_bodies(&server).await;
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[0].get("response_format").is_some() && bodies[1].get("response_format").is_none()
    );
    // Later calls do not send it at all.
    chat.chat_structured(&msgs(), Some(("spec", &schema())), &ChatOptions::default())
        .await
        .unwrap();
    let bodies = chat_bodies(&server).await;
    assert_eq!(bodies.len(), 3);
    assert!(bodies[2].get("response_format").is_none());
    assert_eq!(caps::probe(&base, None).await.json_schema, Some(false));
}

#[tokio::test]
async fn an_openai_like_endpoint_never_gets_chat_template_kwargs() {
    let server = MockServer::start().await;
    models(&server, "openai").await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ok_reply())
        .mount(&server)
        .await;
    let base = format!("{}/v1", server.uri());
    let chat = DirectChat::new(&base, "ndcart-local", None).unwrap();
    chat.chat_structured(&msgs(), Some(("spec", &schema())), &ChatOptions::default())
        .await
        .unwrap();
    assert_eq!(
        caps::probe(&base, None).await.kind,
        EndpointKind::OpenAiLike
    );
    let b = &chat_bodies(&server).await[0];
    assert!(b.get("chat_template_kwargs").is_none());
    // Unknown support is tried once, and a success is remembered as support.
    assert!(b.get("response_format").is_some());
    assert_eq!(caps::probe(&base, None).await.json_schema, Some(true));
}

#[tokio::test]
async fn ollama_and_unsloth_studio_are_told_apart() {
    let server = MockServer::start().await;
    caps::forget(&format!("{}/v1", server.uri()));
    models(&server, "ollama").await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.5.1"})))
        .mount(&server)
        .await;
    let c = caps::probe(&format!("{}/v1", server.uri()), None).await;
    assert_eq!((c.kind, c.json_schema), (EndpointKind::Ollama, Some(true)));
    let server = MockServer::start().await;
    models(&server, "Unsloth").await;
    // A freed port can be handed out again; forget what the last server there was.
    caps::forget(&format!("{}/v1", server.uri()));
    let c = caps::probe(&format!("{}/v1", server.uri()), None).await;
    assert_eq!((c.kind, c.json_schema), (EndpointKind::UnslothStudio, None));
    // Nothing answering is Unknown, not an error.
    let c = caps::probe("http://127.0.0.1:9/v1", None).await;
    assert_eq!(c.kind, EndpointKind::Unknown);
}
