//! A deterministic fake model server: the test double for every AI test, and a demo mode
//! (`ndcart ai stand-in --port 8099`) that lets a person try the studio's AI buttons with no
//! model at all. It speaks the slice of the OpenAI and llama.cpp APIs the studio uses:
//! `/health`, `/props`, `/v1/models`, `/v1/chat/completions`. It binds loopback only and is a
//! test and demo tool, never a model: its replies are the pack's worked examples, picked by
//! how many words they share with the request.
//!
//! Behaviours (`--behaviour`):
//! - `good`: the best-matching example, always.
//! - `fix-once`: the first turn of a conversation gets a broken reply (an invented field, or
//!   not JSON at all); a turn that follows an assistant message gets the right one.
//! - `garbage`: never a spec.
//! - `slow`: waits `--delay-ms` (default 2000) before answering.
//! - `no-schema`: answers 400 when the request carries `response_format`.
//!
//! `GET /__stats` reports what it has seen (requests, those that carried a schema, the last
//! few request bodies) so a test can count asks.

use crate::aipack::{self, Kind};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Behaviour {
    Good,
    FixOnce,
    Garbage,
    Slow,
    NoSchema,
}

impl Behaviour {
    pub fn parse(s: &str) -> Option<Behaviour> {
        Some(match s {
            "good" => Behaviour::Good,
            "fix-once" => Behaviour::FixOnce,
            "garbage" => Behaviour::Garbage,
            "slow" => Behaviour::Slow,
            "no-schema" => Behaviour::NoSchema,
            _ => return None,
        })
    }
}

/// How the stand-in behaves.
#[derive(Clone, Debug)]
pub struct Options {
    pub behaviour: Behaviour,
    /// `/health` answers 503 for this many calls first (a model that is still loading).
    pub loading: usize,
    /// The ids `/v1/models` serves.
    pub models: Vec<String>,
    /// How long `slow` waits.
    pub delay_ms: u64,
    /// Replies to give in order, instead of picking from the examples (`{{}}`-free raw text).
    /// Once used up, the normal pick resumes.
    pub script: Vec<String>,
    /// Answer `/props` like llama.cpp does (otherwise 404, like a hosted API).
    pub llama_props: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            behaviour: Behaviour::Good,
            loading: 0,
            models: vec!["ndcart-local".into()],
            delay_ms: 2000,
            script: Vec::new(),
            llama_props: true,
        }
    }
}

/// What the stand-in has seen.
#[derive(Default)]
pub struct Stats {
    pub health: AtomicUsize,
    pub chats: AtomicUsize,
    pub with_schema: AtomicUsize,
    pub bodies: Mutex<Vec<Value>>,
}

impl Stats {
    pub fn chats(&self) -> usize {
        self.chats.load(Ordering::SeqCst)
    }
    pub fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().expect("stats").clone()
    }
}

struct Shared {
    opts: Options,
    stats: Arc<Stats>,
    script: Mutex<VecDeque<String>>,
}

type St = Arc<Shared>;

/// The router (tests mount it themselves).
pub fn router(opts: Options) -> (Router, Arc<Stats>) {
    let stats = Arc::new(Stats::default());
    let shared = Arc::new(Shared {
        script: Mutex::new(opts.script.iter().cloned().collect()),
        opts,
        stats: stats.clone(),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/props", get(props))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/__stats", get(stats_route))
        .with_state(shared);
    (app, stats)
}

async fn health(State(s): State<St>) -> StatusCode {
    if s.stats.health.fetch_add(1, Ordering::SeqCst) < s.opts.loading {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    }
}

async fn props(State(s): State<St>) -> (StatusCode, Json<Value>) {
    if s.opts.llama_props {
        (
            StatusCode::OK,
            Json(json!({
                "build_info": "ndcart-stand-in",
                "default_generation_settings": {"n_ctx": 8192},
            })),
        )
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"error": "not found"})))
    }
}

async fn models(State(s): State<St>) -> Json<Value> {
    Json(json!({
        "object": "list",
        "data": s.opts.models.iter().map(|m| json!({"id": m, "object": "model"})).collect::<Vec<_>>(),
    }))
}

async fn stats_route(State(s): State<St>) -> Json<Value> {
    Json(json!({
        "chats": s.stats.chats(),
        "with_schema": s.stats.with_schema.load(Ordering::SeqCst),
        "bodies": s.stats.bodies(),
    }))
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(str::to_string)
        .collect()
}

/// The example whose request shares the most words with `user` (the first on a tie).
fn best_example(user: &str, world: bool) -> Option<aipack::Example> {
    let want = words(user);
    aipack::examples()
        .into_iter()
        .filter(|e| e.file.contains("world-") == world && !e.file.ends_with("map-refine.json"))
        .max_by_key(|e| {
            let have = words(&e.request);
            (want.iter().filter(|w| have.contains(w)).count(), {
                // `max_by_key` keeps the last of equals; rank earlier files higher.
                usize::MAX - e.file.len()
            })
        })
}

/// What kind of request this is, from the system prompt (which may have house style added
/// after the pack's text).
fn kind_of(system: &str) -> Option<Kind> {
    [
        Kind::Map,
        Kind::World,
        Kind::Refine,
        Kind::Tags,
        Kind::ThemePack,
        Kind::Outline,
        Kind::Edit,
    ]
    .into_iter()
    .find(|k| {
        // The opening sentence names the task; placeholders further in need not match.
        let head: String = aipack::system(*k).chars().take(40).collect();
        system.starts_with(&head)
    })
}

/// Break a spec on purpose: an invented field on the first room.
fn break_spec(spec: &mut Value) {
    if let Some(room) = spec
        .pointer_mut("/floors/0/rooms/0")
        .and_then(Value::as_object_mut)
    {
        room.insert("props".into(), json!(["barrel"]));
    } else if let Some(o) = spec.as_object_mut() {
        o.insert("props".into(), json!(["barrel"]));
    }
}

fn pick(system: &str, user: &str, repair_turn: bool, behaviour: Behaviour) -> String {
    let broken = behaviour == Behaviour::FixOnce && !repair_turn;
    match kind_of(system) {
        Some(Kind::Map) | Some(Kind::World) => {
            let world = kind_of(system) == Some(Kind::World);
            let Some(e) = best_example(user, world) else {
                return "{}".into();
            };
            let mut spec = e.spec;
            // One floor of a stepwise draft: the first floor of the best match.
            if user.contains("Now write ONLY floor") {
                if let Some(f) = spec["floors"].get(0).cloned() {
                    spec["floors"] = json!([f]);
                }
            }
            if broken {
                break_spec(&mut spec);
            }
            spec.to_string()
        }
        Some(Kind::Edit) => {
            // The floor's description is the JSON line after "Floor:"; rename its first room.
            let room = user
                .lines()
                .find_map(|l| serde_json::from_str::<Value>(l).ok())
                .and_then(|v| v["rooms"][0]["id"].as_str().map(str::to_string))
                .unwrap_or_default();
            let room = if broken { "nope".to_string() } else { room };
            json!({"edits": [{"op": "rename_room", "room": room, "name": "Renamed by the stand-in"}]})
                .to_string()
        }
        Some(Kind::Outline) => {
            if broken {
                "Here is the plan.".into()
            } else {
                json!({"name": "Stand-in Keep", "floors": [
                    {"name": "Upper", "purpose": "the entrance", "rooms": 4, "links_to": ["Lower"]},
                    {"name": "Lower", "purpose": "the vaults", "rooms": 4, "links_to": []},
                ]})
                .to_string()
            }
        }
        Some(Kind::Refine) => {
            let e = aipack::examples()
                .into_iter()
                .find(|e| e.file.ends_with("map-refine.json"))
                .expect("refine example");
            let mut spec = e.spec;
            if broken {
                break_spec(&mut spec);
            }
            spec.to_string()
        }
        Some(Kind::Tags) => {
            if broken {
                "tags: crate, wooden".into()
            } else {
                json!({"tags": ["crate", "wooden", "storage", "cargo"]}).to_string()
            }
        }
        Some(Kind::ThemePack) => {
            if broken {
                "Here is a theme pack plan.".into()
            } else {
                json!({
                    "name": "Stand-in pack",
                    "style": "flat painted tabletop art",
                    "palette": ["#2b2d42", "#8d99ae", "#edf2f4", "#ef233c"],
                    "materials": [
                        {"name": "Stone", "prompt": "grey stone floor, top-down", "tags": ["stone"]},
                        {"name": "Wood", "prompt": "oak planks, top-down", "tags": ["wood"]},
                        {"name": "Moss", "prompt": "mossy ground, top-down", "tags": ["moss"]},
                        {"name": "Sand", "prompt": "dry sand, top-down", "tags": ["sand"]},
                    ],
                    "props": [
                        {"name": "Barrel", "prompt": "a barrel, top-down", "tags": ["barrel"], "placement": "floor"},
                        {"name": "Crate", "prompt": "a crate, top-down", "tags": ["crate"], "placement": "floor"},
                        {"name": "Torch", "prompt": "a wall torch, top-down", "tags": ["torch"], "placement": "wall"},
                        {"name": "Table", "prompt": "a table, top-down", "tags": ["table"], "placement": "floor"},
                        {"name": "Chair", "prompt": "a chair, top-down", "tags": ["chair"], "placement": "floor"},
                        {"name": "Chest", "prompt": "a chest, top-down", "tags": ["chest"], "placement": "floor"},
                    ],
                })
                .to_string()
            }
        }
        None => "OK".into(),
    }
}

async fn chat(State(s): State<St>, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
    s.stats.chats.fetch_add(1, Ordering::SeqCst);
    let has_schema = body.get("response_format").is_some();
    if has_schema {
        s.stats.with_schema.fetch_add(1, Ordering::SeqCst);
    }
    {
        let mut b = s.stats.bodies.lock().expect("stats");
        if b.len() < 200 {
            b.push(body.clone());
        }
    }
    if s.opts.behaviour == Behaviour::NoSchema && has_schema {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "response_format is not supported by this server"}})),
        );
    }
    if s.opts.behaviour == Behaviour::Slow {
        tokio::time::sleep(std::time::Duration::from_millis(s.opts.delay_ms)).await;
    }
    let msgs = body["messages"].as_array().cloned().unwrap_or_default();
    let text_of = |role: &str, last: bool| -> String {
        let mut it = msgs.iter().filter(|m| m["role"] == role);
        let m = if last { it.next_back() } else { it.next() };
        m.and_then(|m| m["content"].as_str())
            .unwrap_or("")
            .to_string()
    };
    let system = text_of("system", false);
    // Match on the first user turn: it carries the request; later turns are complaints.
    let user = text_of("user", false);
    let repair_turn = msgs.iter().any(|m| m["role"] == "assistant");
    let scripted = s.script.lock().expect("script").pop_front();
    let content = match (scripted, s.opts.behaviour) {
        (Some(r), _) => r,
        (None, Behaviour::Garbage) => "I am sorry, I cannot help with that.".to_string(),
        (None, b) => pick(&system, &user, repair_turn, b),
    };
    (
        StatusCode::OK,
        Json(json!({
            "id": "standin",
            "object": "chat.completion",
            "model": body["model"],
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": "stop",
            }],
            "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
        })),
    )
}

#[cfg(all(test, feature = "bridge"))]
/// A running stand-in (for tests): its `/v1` address and what it has seen.
#[allow(dead_code)]
pub struct Handle {
    pub base: String,
    pub origin: String,
    pub stats: Arc<Stats>,
}

/// Start a stand-in on a free loopback port, in this process.
#[cfg(all(test, feature = "bridge"))]
pub async fn spawn(opts: Options) -> Handle {
    let (app, stats) = router(opts);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = l.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    // A freed port can be handed out again; forget what the last server there taught us.
    ndcart_bridge::caps::forget(&format!("http://{addr}/v1"));
    Handle {
        base: format!("http://{addr}/v1"),
        origin: format!("http://{addr}"),
        stats,
    }
}

/// `ndcart ai stand-in --port N [--behaviour B] [--delay-ms N] [--loading N]`: serve until
/// killed. Prints the address as the first line of stdout (so a test can use `--port 0`).
pub async fn run(args: Vec<String>) -> Result<(), String> {
    let mut opts = Options::default();
    let mut port: u16 = 8099;
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--port" => {
                port = val("--port")?
                    .parse()
                    .map_err(|_| "--port needs a number".to_string())?
            }
            "--behaviour" | "--behavior" => {
                let v = val("--behaviour")?;
                opts.behaviour = Behaviour::parse(&v).ok_or_else(|| {
                    format!("unknown behaviour '{v}' (good, fix-once, garbage, slow, no-schema)")
                })?;
            }
            "--delay-ms" => {
                opts.delay_ms = val("--delay-ms")?
                    .parse()
                    .map_err(|_| "--delay-ms needs a number".to_string())?
            }
            "--loading" => {
                opts.loading = val("--loading")?
                    .parse()
                    .map_err(|_| "--loading needs a number".to_string())?
            }
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let (app, _) = router(opts.clone());
    let l = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let addr = l.local_addr().map_err(|e| e.to_string())?;
    println!("http://{addr}/v1");
    eprintln!(
        "ndcart stand-in model ({:?}) on http://{addr}: a test and demo tool, not a model. \
         In the studio's AI settings use address http://{addr}/v1 and model {}.",
        opts.behaviour, opts.models[0]
    );
    axum::serve(l, app).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn post(app: &Router, body: Value) -> (u16, Value) {
        let r = app
            .clone()
            .oneshot(
                Request::post("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let st = r.status().as_u16();
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        (st, serde_json::from_slice(&bytes).unwrap_or_default())
    }

    fn app(behaviour: Behaviour) -> (Router, Arc<Stats>) {
        router(Options {
            behaviour,
            ..Options::default()
        })
    }

    fn turn(system: Kind, user: &str) -> Value {
        json!({"model":"m","messages":[
            {"role":"system","content": aipack::system(system)},
            {"role":"user","content": user}]})
    }

    fn content(v: &Value) -> String {
        v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn good_picks_the_closest_example_and_fix_once_breaks_only_the_first_turn() {
        let (a, stats) = app(Behaviour::Good);
        let (_, v) = post(&a, turn(Kind::Map, "a crypt with an ossuary")).await;
        let text = content(&v);
        assert!(crate::speccheck::check_map(&text).ok(), "{text}");
        assert!(text.contains("Ossuary") || text.contains("Crypt"), "{text}");
        assert_eq!(stats.chats(), 1);

        let (a, _) = app(Behaviour::FixOnce);
        let (_, first) = post(&a, turn(Kind::Map, "a crypt")).await;
        let text = content(&first);
        assert!(!crate::speccheck::check_map(&text).ok(), "{text}");
        let mut again = turn(Kind::Map, "a crypt");
        again["messages"].as_array_mut().unwrap().extend([
            json!({"role":"assistant","content": text}),
            json!({"role":"user","content":"fix it"}),
        ]);
        let (_, second) = post(&a, again).await;
        let text = content(&second);
        assert!(crate::speccheck::check_map(&text).ok(), "{text}");
    }

    #[tokio::test]
    async fn no_schema_refuses_a_response_format_and_garbage_is_never_a_spec() {
        let (a, stats) = app(Behaviour::NoSchema);
        let mut b = turn(Kind::Map, "a shop");
        b["response_format"] = json!({"type": "json_object"});
        assert_eq!(post(&a, b).await.0, 400);
        assert_eq!(post(&a, turn(Kind::Map, "a shop")).await.0, 200);
        assert_eq!(stats.with_schema.load(Ordering::SeqCst), 1);
        let (a, _) = app(Behaviour::Garbage);
        let (_, v) = post(&a, turn(Kind::Map, "a shop")).await;
        assert!(crate::speccheck::extract_json(&content(&v)).is_none());
    }

    #[tokio::test]
    async fn world_refine_tags_and_theme_pack_each_get_their_own_shape() {
        let (a, _) = app(Behaviour::Good);
        let (_, v) = post(&a, turn(Kind::World, "an archipelago of islands")).await;
        assert!(crate::speccheck::check_world(&content(&v)).ok());
        let (_, v) = post(&a, turn(Kind::Refine, "add a barracks")).await;
        assert!(crate::speccheck::check_map(&content(&v)).ok());
        let (_, v) = post(&a, turn(Kind::Tags, "Prop name: Crate")).await;
        assert!(content(&v).contains("\"tags\""));
        let (_, v) = post(&a, turn(Kind::ThemePack, "Theme: ice")).await;
        assert!(content(&v).contains("palette"));
    }
}
