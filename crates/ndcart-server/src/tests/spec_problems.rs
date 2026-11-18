//! `/propose` reports what the floor linter finds about a spec, by the spec's room ids.

use super::*;

async fn propose(router: &Router, id: &str, spec: serde_json::Value) -> serde_json::Value {
    let (st, b) = call(
        router,
        "POST",
        &format!("/api/docs/{id}/propose"),
        serde_json::json!({ "spec": spec }).to_string().into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{}", String::from_utf8_lossy(&b));
    jv(&b)
}

#[tokio::test]
async fn propose_returns_no_problems_for_a_clean_spec_and_names_rooms_for_a_dirty_one() {
    let router = build_router(test_app());
    let (id, _) = new_doc(&router, "Lint").await;
    let clean = serde_json::json!({"name":"Inn","floors":[{"name":"G","rooms":[
        {"id":"hall","name":"Hall","w":8,"h":6},{"id":"bar","name":"Taproom","w":8,"h":5}],
        "links":[{"a":"hall","b":"bar","kind":"door"}]}]});
    let r = propose(&router, &id, clean).await;
    assert_eq!(r["problems"], serde_json::json!([]), "{r}");

    let dirty = serde_json::json!({"name":"Jail","floors":[{"name":"G","rooms":[
        {"id":"a","name":"Cell","w":5,"h":5},{"id":"b","name":"Cell","w":5,"h":5},
        {"id":"w","name":"Warden","w":6,"h":5}],
        "links":[{"a":"a","b":"w","kind":"door"},{"a":"b","b":"w","kind":"door"}]}]});
    let r = propose(&router, &id, dirty).await;
    let dup = r["problems"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "duplicate_room_name")
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(dup["rooms"], serde_json::json!(["a", "b"]));
    assert_eq!(dup["floor"], "G");
}
