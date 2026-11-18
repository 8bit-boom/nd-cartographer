//! The instruction pack and the spec check over HTTP. None of this needs an AI, so it also
//! runs in the `--no-default-features` build.

use super::*;

const GOOD: &str = r#"{"name":"Crypt","floors":[{"name":"G","rooms":[{"id":"a","name":"Nave","w":8,"h":6},{"id":"b","name":"Ossuary","w":5,"h":5}],"links":[{"a":"a","b":"b"}]}]}"#;

async fn get_json(router: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let (st, b) = call(router, "GET", uri, vec![]).await;
    (st, jv(&b))
}

async fn check(router: &Router, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let (st, b) = call(
        router,
        "POST",
        "/api/spec/check",
        body.to_string().into_bytes(),
    )
    .await;
    (st, jv(&b))
}

#[tokio::test]
async fn instructions_carry_the_pack_version_the_prompt_the_schema_and_examples() {
    let router = build_router(test_app());
    let (st, v) = get_json(&router, "/api/ai/instructions").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["version"], aipack::version());
    assert_eq!(v["kind"], "map");
    assert!(v["system"].as_str().unwrap().contains("room graph"));
    assert!(v["reference"].as_str().unwrap().contains(aipack::version()));
    assert!(v["repair"].as_str().unwrap().contains("{{problems}}"));
    assert_eq!(v["schema"]["title"], "MapSpec");
    let ex = v["examples"].as_array().unwrap();
    assert!(
        ex.len() >= 8
            && ex
                .iter()
                .all(|e| e["file"].as_str().unwrap().contains("map-"))
    );

    let (_, w) = get_json(&router, "/api/ai/instructions?kind=world").await;
    assert_eq!(w["schema"]["title"], "WorldSpec");
    assert!(w["examples"].as_array().unwrap().len() >= 3);
    let (_, r) = get_json(&router, "/api/ai/instructions?kind=refine").await;
    assert!(r["examples"][0]["file"]
        .as_str()
        .unwrap()
        .ends_with("map-refine.json"));
    assert!(r["system"].as_str().unwrap().contains("PINNED"));

    let (st, _) = get_json(&router, "/api/ai/instructions?kind=nope").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_schemas_are_served_as_json() {
    let router = build_router(test_app());
    for (name, title) in [("mapspec", "MapSpec"), ("worldspec", "WorldSpec")] {
        let (st, v) = get_json(&router, &format!("/api/ai/schema/{name}")).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["title"], title);
    }
    let (st, _) = get_json(&router, "/api/ai/schema/other").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_bad_spec_gets_the_message_the_ai_loop_would_send_back() {
    let router = build_router(test_app());
    let unlinked = GOOD.replace(r#","links":[{"a":"a","b":"b"}]"#, "");
    let spec: serde_json::Value = serde_json::from_str(&unlinked).unwrap();
    let (st, v) = check(&router, serde_json::json!({ "spec": spec })).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["ok"], false);
    assert_eq!(v["kind"], "map");
    let first = v["errors"][0].as_str().unwrap().to_string();
    assert!(first.contains("not linked"), "{first}");
    #[cfg(feature = "bridge")]
    assert_eq!(first, draft::parse_reply(&unlinked).unwrap_err());
    // The same spec as a string (what an agent that only has text would send).
    let (_, v2) = check(
        &router,
        serde_json::json!({ "spec": format!("```json\n{unlinked}\n```") }),
    )
    .await;
    assert_eq!(v2["errors"][0], v["errors"][0]);
}

#[tokio::test]
async fn unknown_fields_and_problems_are_reported_and_a_good_spec_is_ok() {
    let router = build_router(test_app());
    let with_props = GOOD.replace(r#""id":"b","#, r#""id":"b","props":["bones"],"#);
    let spec: serde_json::Value = serde_json::from_str(&with_props).unwrap();
    let (_, v) = check(&router, serde_json::json!({ "spec": spec, "kind": "map" })).await;
    assert_eq!(v["ok"], false);
    assert_eq!(
        v["unknown_fields"],
        serde_json::json!(["floors[0].rooms[1].props"])
    );
    assert!(v["errors"][0]
        .as_str()
        .unwrap()
        .starts_with("Unknown field(s)"));

    let good: serde_json::Value = serde_json::from_str(GOOD).unwrap();
    let (_, v) = check(&router, serde_json::json!({ "spec": good })).await;
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["errors"], serde_json::json!([]));
    assert!(v["problems"].is_array());

    // A world is recognised by what it carries; bad input is a 400, not a crash.
    let (_, w) = check(
        &router,
        serde_json::json!({ "spec": {"name": "X", "land": 3} }),
    )
    .await;
    assert_eq!(w["kind"], "world");
    assert!(w["errors"][0].as_str().unwrap().contains("land must be"));
    for body in [
        serde_json::json!({}),
        serde_json::json!({ "spec": 5 }),
        serde_json::json!({ "spec": {}, "kind": "dungeon" }),
        serde_json::json!({ "spec": "no json here" }),
    ] {
        let (st, _) = check(&router, body).await;
        assert_eq!(st, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn the_pack_routes_sit_behind_the_token_gate() {
    let router = build_router(test_app()).layer(axum::middleware::from_fn(|req, next| {
        auth::gate("s3cret".to_string(), Default::default(), req, next)
    }));
    for uri in ["/api/ai/instructions", "/api/ai/schema/mapspec"] {
        let r = router
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{uri}");
        let r = router
            .clone()
            .oneshot(
                Request::get(uri)
                    .header("authorization", "Bearer s3cret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK, "{uri}");
    }
}

#[test]
fn the_edit_plan_schema_lists_exactly_the_ops_the_code_knows() {
    use ndcart_core::editplan::Edit;
    use ndcart_core::mapspec::LinkKind;
    let schema: serde_json::Value = serde_json::from_str(aipack::EDITPLAN_SCHEMA).unwrap();
    let mut in_schema: Vec<String> = schema["properties"]["edits"]["items"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap().to_string())
        .collect();
    in_schema.sort();
    let samples = [
        Edit::RenameRoom {
            room: "a".into(),
            name: "n".into(),
        },
        Edit::SetLit {
            room: "a".into(),
            lit: true,
        },
        Edit::SetLink {
            a: "a".into(),
            b: "b".into(),
            kind: LinkKind::Door,
        },
        Edit::SetMaterial {
            room: "a".into(),
            material: "m".into(),
        },
        Edit::Furnish {
            rooms: vec!["a".into()],
        },
        Edit::Note {
            room: "a".into(),
            text: "t".into(),
        },
    ];
    let mut in_code: Vec<String> = samples
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["op"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    in_code.sort();
    assert_eq!(in_schema, in_code);
    // Each sample has exactly the fields its schema entry lists.
    for (e, entry) in samples.iter().zip(
        // schema order is the sample order
        schema["properties"]["edits"]["items"]["oneOf"]
            .as_array()
            .unwrap(),
    ) {
        let mut code: Vec<String> = serde_json::to_value(e)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut sch: Vec<String> = entry["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        code.sort();
        sch.sort();
        assert_eq!(code, sch);
    }
}
