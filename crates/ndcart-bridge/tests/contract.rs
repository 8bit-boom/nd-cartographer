//! Contract tests against a faked nd-world (plan §12: wiremock per route).
//! They pin the request shapes read out of nd-world's own handlers.

use ndcart_bridge::{Bridge, BridgeError, PublishMap};
use ndcart_core::{Light, Wall, WallKind};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn wall(i: usize) -> Wall {
    Wall {
        id: format!("w{i}"),
        pts: vec![[0.0, i as f64], [10.0, i as f64]],
        kind: WallKind::Wall,
        state: None,
    }
}

fn map() -> PublishMap {
    PublishMap {
        name: "Vault".into(),
        canvas_w: 640,
        canvas_h: 480,
        cell_px: 32.0,
        image: vec![1, 2, 3],
        image_ext: "webp".into(),
        walls: vec![wall(1)],
        lights: vec![Light {
            id: "l1".into(),
            x: 64.0,
            y: 64.0,
            range: 6.0,
            color: "#ffaa00".into(),
            intensity: 0.8,
            on: true,
            label: None,
        }],
        rooms: Some(
            json!({"cell": 32, "cols": 1, "rows": 1, "cells": [0, 1], "spaces": [], "marks": []}),
        ),
    }
}

async fn ok_json(server: &MockServer, p: &str, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path(p))
        .and(query_param("w", "neon"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn login_outcomes() {
    let s = MockServer::start().await;
    let b = Bridge::new(&s.uri(), Some("neon")).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .and(body_json(json!({"email": "a@b.c", "password": "pw"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"ok": true, "user": {"id": 1, "email": "a@b.c", "is_gm": true}}),
            ),
        )
        .mount(&s)
        .await;
    b.login("a@b.c", "pw", None).await.unwrap();

    let s = MockServer::start().await;
    let b = Bridge::new(&s.uri(), None).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"ok": false, "requires_2fa": true, "detail": "Two-step code required."}),
        ))
        .mount(&s)
        .await;
    assert!(matches!(
        b.login("a@b.c", "pw", None).await,
        Err(BridgeError::TwoFactorRequired)
    ));

    let s = MockServer::start().await;
    let b = Bridge::new(&s.uri(), None).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"ok": false, "detail": "Incorrect email or password."})),
        )
        .mount(&s)
        .await;
    assert!(matches!(
        b.login("a@b.c", "no", None).await,
        Err(BridgeError::BadCredentials)
    ));

    let s = MockServer::start().await;
    let b = Bridge::new(&s.uri(), None).unwrap();
    Mock::given(method("POST"))
        .and(path("/api/login"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true, "user": {"id": 2, "is_gm": false}})),
        )
        .mount(&s)
        .await;
    assert!(matches!(
        b.login("p@b.c", "pw", None).await,
        Err(BridgeError::NotGm)
    ));
}

#[tokio::test]
async fn probe_tells_the_rust_only_image_from_hybrid() {
    for (mode, free) in [("rust", true), ("hybrid", false), ("python", false)] {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/health"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"status": "ok", "build": "abc1234", "mode": mode})),
            )
            .mount(&s)
            .await;
        let p = Bridge::new(&s.uri(), None).unwrap().probe().await.unwrap();
        assert_eq!(p.python_free, free, "{mode}");
        assert_eq!(p.import_uvtt, !free, "{mode}: import-uvtt is Python-only");
        assert!(p.per_part_publish);
        assert_eq!(p.build, "abc1234");
    }
}

#[tokio::test]
async fn publish_runs_every_step_in_order_against_the_pinned_world() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/new"))
        .and(query_param("w", "neon"))
        .respond_with(
            ResponseTemplate::new(303).insert_header("location", "/maps/schematic/vault-2"),
        )
        .expect(1)
        .mount(&s)
        .await;
    ok_json(&s, "/maps/schematic/vault-2/upload", json!({})).await;
    Mock::given(method("POST")).and(path("/maps/schematic/vault-2/grid")).and(query_param("w", "neon"))
        .and(body_json(json!({"grid_type": "square", "config": {"cell_size": 32.0, "offset_x": 0, "offset_y": 0, "unit_per_cell": 5, "unit_label": "ft"}})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1).mount(&s).await;
    ok_json(
        &s,
        "/maps/schematic/vault-2/walls",
        json!({"walls": [], "warnings": ["1 wall clipped"]}),
    )
    .await;
    ok_json(
        &s,
        "/maps/schematic/vault-2/lights",
        json!({"lights": [], "warnings": []}),
    )
    .await;
    ok_json(
        &s,
        "/maps/schematic/vault-2/rooms",
        json!({"rooms": {}, "warnings": []}),
    )
    .await;

    let r = Bridge::new(&s.uri(), Some("neon"))
        .unwrap()
        .publish(&map(), None)
        .await
        .unwrap();
    assert_eq!(r.slug, "vault-2");
    assert!(r.created);
    assert_eq!(
        r.steps,
        ["create", "picture", "grid", "walls", "lights", "rooms"]
    );
    assert_eq!(r.warnings, ["walls: 1 wall clipped"]);

    // The walls body is nd-world's shape: {"walls":[{id, pts, kind}]}.
    let reqs = s.received_requests().await.unwrap();
    let walls_req = reqs
        .iter()
        .find(|q| q.url.path().ends_with("/walls"))
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&walls_req.body).unwrap();
    assert_eq!(v["walls"][0]["kind"], "wall");
    assert_eq!(v["walls"][0]["pts"], json!([[0.0, 1.0], [10.0, 1.0]]));
    let up = reqs
        .iter()
        .find(|q| q.url.path().ends_with("/upload"))
        .unwrap();
    let ct = up.headers.get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("multipart/form-data"), "{ct}");
    let text = String::from_utf8_lossy(&up.body);
    assert!(text.contains("filename=\"map.webp\"") && text.contains("image/webp"));
}

#[tokio::test]
async fn republish_updates_in_place_without_creating() {
    let s = MockServer::start().await;
    for p in ["upload", "grid", "walls", "lights", "rooms"] {
        Mock::given(method("POST"))
            .and(path(format!("/maps/schematic/vault/{p}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
            .expect(1)
            .mount(&s)
            .await;
    }
    let r = Bridge::new(&s.uri(), None)
        .unwrap()
        .publish(&map(), Some("vault"))
        .await
        .unwrap();
    assert!(!r.created);
    assert_eq!(r.steps, ["picture", "grid", "walls", "lights", "rooms"]);
}

#[tokio::test]
async fn caps_are_refused_before_any_request() {
    let s = MockServer::start().await; // no mocks: any request would 404 and be counted
    let b = Bridge::new(&s.uri(), None).unwrap();
    let mut m = map();
    m.walls = (0..5_001).map(wall).collect();
    assert!(matches!(
        b.publish(&m, None).await,
        Err(BridgeError::Precheck(_))
    ));
    let mut m = map();
    m.lights = (0..61)
        .map(|i| Light {
            id: format!("l{i}"),
            ..m.lights[0].clone()
        })
        .collect();
    assert!(matches!(
        b.publish(&m, None).await,
        Err(BridgeError::Precheck(_))
    ));
    let mut m = map();
    m.canvas_w = 50;
    assert!(matches!(
        b.publish(&m, None).await,
        Err(BridgeError::Precheck(_))
    ));
    let mut m = map();
    m.image_ext = "gif".into();
    assert!(matches!(
        b.publish(&m, None).await,
        Err(BridgeError::Precheck(_))
    ));
    assert!(s.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failure_after_creation_reports_the_slug_to_retry() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/new"))
        .respond_with(ResponseTemplate::new(303).insert_header("location", "/maps/schematic/vault"))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/vault/upload"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/vault/grid"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/vault/walls"))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(json!({"detail": "walls must be a list"})),
        )
        .mount(&s)
        .await;
    match Bridge::new(&s.uri(), None)
        .unwrap()
        .publish(&map(), None)
        .await
    {
        Err(BridgeError::Partial { slug, step, cause }) => {
            assert_eq!((slug.as_str(), step), ("vault", "walls"));
            assert!(
                matches!(*cause, BridgeError::Rejected(m) if m.contains("walls must be a list"))
            );
        }
        other => panic!("expected Partial, got {other:?}"),
    }
}

#[tokio::test]
async fn status_codes_map_to_plain_errors() {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/new"))
        .respond_with(
            ResponseTemplate::new(501)
                .set_body_json(json!({"detail": "This request needs the Python app"})),
        )
        .mount(&s)
        .await;
    assert!(matches!(
        Bridge::new(&s.uri(), None).unwrap().publish(&map(), None).await,
        Err(BridgeError::NeedsPython(m)) if m.contains("Python")
    ));
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/maps/schematic/new"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&s)
        .await;
    assert!(matches!(
        Bridge::new(&s.uri(), None)
            .unwrap()
            .publish(&map(), None)
            .await,
        Err(BridgeError::AuthRequired)
    ));
    // Nothing listening at all.
    let dead = Bridge::new("http://127.0.0.1:1", None).unwrap();
    assert!(matches!(
        dead.probe().await,
        Err(BridgeError::Unreachable(_))
    ));
}
