//! nd-world HTTP client: login, capability probe, publish A (plan §9).
//!
//! Publish uses the *per-part* routes (`/maps/schematic/new`, `/upload`,
//! `/grid`, `/walls`, `/lights`, `/rooms`), which both the hybrid and the
//! Rust-only nd-world image serve and which update a map in place. The
//! one-call `/maps/schematic/import-uvtt` exists on Python only, so it is not
//! used for publishing (the probe still reports it). Every cap is checked
//! before the first request so a refused publish never leaves a half-made map.

use ndcart_core::{Light, Wall};
use reqwest::{multipart, redirect, Client, Method, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};

/// A published battle map read back from nd-world.
#[derive(Debug)]
pub struct Published {
    pub walls: Vec<Wall>,
    pub lights: Vec<Light>,
    /// The room grid's cell size in picture pixels, when the map has rooms.
    pub cell_px: Option<f64>,
    pub image: Vec<u8>,
    pub image_ext: String,
}

/// One image to make through nd-world's imagegen queue.
#[derive(Clone, Debug)]
pub struct ImageRequest {
    pub prompt: String,
    pub negative: String,
    pub width: u32,
    pub height: u32,
    pub steps: u32,
    /// -1 lets the server pick.
    pub seed: i64,
    /// An image path on the server to start from (never nd-world's own art).
    pub init_image: Option<String>,
    pub init_strength: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImageJob {
    /// pending, running, done, error, cancelled or interrupted.
    pub status: String,
    pub error: Option<String>,
    pub urls: Vec<String>,
}

pub const WALL_CAP: usize = 5_000;
pub const LIGHT_CAP: usize = 60;

#[derive(Debug)]
pub enum BridgeError {
    Unreachable(String),
    BadCredentials,
    TwoFactorRequired,
    RateLimited(String),
    NotGm,
    /// Not logged in (or not allowed to edit maps in this world).
    AuthRequired,
    /// 501 from the Rust-only image: the route needs Python.
    NeedsPython(String),
    NotFound(String),
    Rejected(String),
    /// A check that failed locally, before any request.
    Precheck(String),
    /// A step failed after the map was created: `slug` lets the caller retry.
    Partial {
        slug: String,
        step: &'static str,
        cause: Box<BridgeError>,
    },
    Protocol(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use BridgeError::*;
        match self {
            Unreachable(m) => write!(f, "cannot reach nd-world: {m}"),
            BadCredentials => write!(f, "incorrect email or password"),
            TwoFactorRequired => write!(f, "a two-step code is required"),
            RateLimited(m) => write!(f, "{m}"),
            NotGm => write!(f, "this account is not a GM, so it cannot publish maps"),
            AuthRequired => write!(f, "not logged in to nd-world, or not allowed to edit maps"),
            NeedsPython(m) => write!(f, "this nd-world image cannot do that: {m}"),
            NotFound(m) => write!(f, "not found: {m}"),
            Rejected(m) => write!(f, "nd-world refused it: {m}"),
            Precheck(m) => write!(f, "{m}"),
            Partial { slug, step, cause } => {
                write!(
                    f,
                    "map '{slug}' was created but the {step} step failed: {cause}"
                )
            }
            Protocol(m) => write!(f, "unexpected answer from nd-world: {m}"),
        }
    }
}
impl std::error::Error for BridgeError {}

/// What `GET /health` and the route facts tell us about the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub build: String,
    /// `rust` (Python-free image), `hybrid` or `python`.
    pub mode: String,
    pub python_free: bool,
    /// `POST /maps/schematic/import-uvtt` — Python only, never used to publish.
    pub import_uvtt: bool,
    /// The per-part publish routes — served by every image.
    pub per_part_publish: bool,
    /// `GET .../walls.json` and `rooms.json` (reopening a published map from
    /// nd-world): the Rust-only gateway answers 501 for both (checked live).
    pub read_back: bool,
}

pub struct PublishMap {
    pub name: String,
    pub canvas_w: u32,
    pub canvas_h: u32,
    /// Pixels per grid square in the picture (nd-world's `cell_size`).
    pub cell_px: f64,
    /// Encoded background; `image_ext` is `webp`, `avif`, `png` or `jpg`.
    pub image: Vec<u8>,
    pub image_ext: String,
    pub walls: Vec<Wall>,
    pub lights: Vec<Light>,
    /// nd-world room-tool state (see [`crate::rooms::to_nd_world`]); `None` skips it.
    pub rooms: Option<Value>,
}

#[derive(Debug, Default)]
pub struct PublishReport {
    pub slug: String,
    pub created: bool,
    pub warnings: Vec<String>,
    pub steps: Vec<&'static str>,
}

/// A region (world) map for nd-world's image-map viewer: the baked picture plus
/// its markers and regions as the viewer's own overlay JSON (`lat`/`lng` are
/// Leaflet CRS.Simple: `lat` counts up from the bottom, `lng` along the width).
pub struct PublishRegion {
    pub name: String,
    pub width_px: u32,
    pub height_px: u32,
    pub image: Vec<u8>,
    pub image_ext: String,
    pub markers: Vec<Value>,
    pub regions: Vec<Value>,
    /// Replace the map's marker/region overlay with these. On a re-publish this
    /// discards markers added by hand in nd-world (there is no JSON read route
    /// to merge with), so it can be switched off.
    pub replace_overlay: bool,
}

pub const OVERLAY_CAP: usize = 500;

pub struct Bridge {
    http: Client,
    base: String,
    world: Option<String>,
}

#[derive(Deserialize)]
struct LoginAnswer {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    requires_2fa: bool,
    #[serde(default)]
    detail: String,
    user: Option<LoginUser>,
}
#[derive(Deserialize)]
struct LoginUser {
    #[serde(default)]
    is_gm: bool,
}

async fn detail_of(resp: Response) -> String {
    let text = resp.text().await.unwrap_or_default();
    serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str().map(str::to_string)))
        .unwrap_or_else(|| text.chars().take(200).collect())
}

async fn check(resp: Response) -> Result<Response, BridgeError> {
    let s = resp.status();
    if s.is_success() || s.is_redirection() {
        return Ok(resp);
    }
    Err(match s {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => BridgeError::AuthRequired,
        StatusCode::NOT_IMPLEMENTED => BridgeError::NeedsPython(detail_of(resp).await),
        StatusCode::NOT_FOUND => BridgeError::NotFound(detail_of(resp).await),
        _ => BridgeError::Rejected(format!("{} {}", s.as_u16(), detail_of(resp).await)),
    })
}

fn mime_for(ext: &str) -> Option<&'static str> {
    match ext {
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        _ => None,
    }
}

impl Bridge {
    pub fn new(base_url: &str, world: Option<&str>) -> Result<Self, BridgeError> {
        let http = Client::builder()
            .cookie_store(true)
            .redirect(redirect::Policy::none())
            .user_agent(concat!("ndcart/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        Ok(Bridge {
            http,
            base: base_url.trim_end_matches('/').to_string(),
            world: world.map(str::to_string),
        })
    }

    /// Every request targets the pinned world with `?w=<slug>` (overrides the cookie).
    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, format!("{}{}", self.base, path));
        match &self.world {
            Some(w) => r.query(&[("w", w.as_str())]),
            None => r,
        }
    }

    async fn send(&self, r: RequestBuilder) -> Result<Response, BridgeError> {
        r.send()
            .await
            .map_err(|e| BridgeError::Unreachable(e.to_string()))
    }

    pub async fn login(
        &self,
        email: &str,
        password: &str,
        totp: Option<&str>,
    ) -> Result<(), BridgeError> {
        let mut body = json!({"email": email, "password": password});
        if let Some(t) = totp {
            body["totp_code"] = json!(t);
        }
        let resp = self
            .send(self.req(Method::POST, "/api/login").json(&body))
            .await?;
        let status = resp.status();
        let ans: LoginAnswer = resp
            .json()
            .await
            .map_err(|e| BridgeError::Protocol(format!("login answer: {e}")))?;
        match status {
            s if s.is_success() && ans.ok => {
                if ans.user.is_some_and(|u| u.is_gm) {
                    Ok(())
                } else {
                    Err(BridgeError::NotGm)
                }
            }
            StatusCode::TOO_MANY_REQUESTS => Err(BridgeError::RateLimited(ans.detail)),
            StatusCode::UNAUTHORIZED if ans.requires_2fa => Err(BridgeError::TwoFactorRequired),
            StatusCode::UNAUTHORIZED => Err(BridgeError::BadCredentials),
            s => Err(BridgeError::Protocol(format!("login answered {s}"))),
        }
    }

    pub async fn probe(&self) -> Result<Probe, BridgeError> {
        let resp = self
            .send(self.http.get(format!("{}/health", self.base)))
            .await?;
        let v: Value = check(resp)
            .await?
            .json()
            .await
            .map_err(|e| BridgeError::Protocol(format!("/health: {e}")))?;
        if v.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(BridgeError::Protocol("/health did not say ok".into()));
        }
        let mode = v
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("python")
            .to_string();
        let python_free = mode == "rust";
        Ok(Probe {
            build: v
                .get("build")
                .and_then(Value::as_str)
                .unwrap_or("dev")
                .to_string(),
            python_free,
            import_uvtt: !python_free,
            per_part_publish: true,
            read_back: !python_free,
            mode,
        })
    }

    /// One non-streaming chat turn through nd-world's AI queue
    /// (`POST /api/ai/chat` -> `{"result": text}`). The model is whatever the
    /// world has configured unless `model` names one.
    pub async fn chat(
        &self,
        system: &str,
        user: &str,
        model: Option<&str>,
    ) -> Result<String, BridgeError> {
        let body = json!({
            "messages": [{"role": "user", "content": user}],
            "system": system,
            "model": model.unwrap_or(""),
            "options": {},
        });
        let resp = self
            .send(self.req(Method::POST, "/api/ai/chat").json(&body))
            .await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BridgeError::AuthRequired);
        }
        let text = resp
            .text()
            .await
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        if !status.is_success() {
            return Err(BridgeError::Rejected(format!(
                "{status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| BridgeError::Protocol(format!("/api/ai/chat answer: {e}")))?;
        let out = v
            .get("result")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::Protocol("/api/ai/chat had no result".into()))?;
        // nd-world reports a backend failure as a bracketed sentinel string ("[AI error: …]",
        // "[AI unavailable: …]", "[empty response …]") with HTTP 200, not as an error status.
        let t = out.trim_start();
        if t.starts_with("[AI ") || t.starts_with("[empty response") {
            return Err(BridgeError::Rejected(
                out.trim().chars().take(300).collect(),
            ));
        }
        Ok(out.to_string())
    }

    /// The names of the world's locations (`GET /api/entities/picker`, kind
    /// "location"), so generated names can agree with existing lore.
    pub async fn location_names(&self) -> Result<Vec<String>, BridgeError> {
        let resp = self
            .send(self.req(Method::GET, "/api/entities/picker"))
            .await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BridgeError::AuthRequired);
        }
        let v: Value = check(resp)
            .await?
            .json()
            .await
            .map_err(|e| BridgeError::Protocol(format!("entities/picker answer: {e}")))?;
        let list = v
            .get("entities")
            .and_then(Value::as_array)
            .ok_or_else(|| BridgeError::Protocol("entities/picker had no entities list".into()))?;
        Ok(list
            .iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("location"))
            .filter_map(|e| e.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect())
    }

    /// Queue an image on nd-world's durable imagegen jobs
    /// (`POST /api/ai/imagegen/jobs` -> `{job_id}`); one GPU, so jobs run in turn.
    pub async fn imagegen_start(&self, req: &ImageRequest) -> Result<i64, BridgeError> {
        let mut body = json!({
            "prompt": req.prompt, "negative": req.negative,
            "width": req.width, "height": req.height,
            "steps": req.steps, "seed": req.seed,
        });
        if let Some(init) = &req.init_image {
            body["init_image"] = json!(init);
            body["init_strength"] = json!(req.init_strength);
        }
        let resp = self
            .send(self.req(Method::POST, "/api/ai/imagegen/jobs").json(&body))
            .await?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(BridgeError::AuthRequired);
        }
        let v: Value = check(resp)
            .await?
            .json()
            .await
            .map_err(|e| BridgeError::Protocol(format!("imagegen job answer: {e}")))?;
        v.get("job_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| BridgeError::Protocol("imagegen answer had no job_id".into()))
    }

    /// `GET /api/ai/imagegen/jobs/{id}`.
    pub async fn imagegen_status(&self, id: i64) -> Result<ImageJob, BridgeError> {
        let resp = self
            .send(self.req(Method::GET, &format!("/api/ai/imagegen/jobs/{id}")))
            .await?;
        let v: Value = check(resp)
            .await?
            .json()
            .await
            .map_err(|e| BridgeError::Protocol(format!("imagegen status: {e}")))?;
        Ok(ImageJob {
            status: v
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            error: v.get("error").and_then(Value::as_str).map(str::to_string),
            urls: v
                .get("urls")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|u| u.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Fetch a finished image (`/uploads/...`) with the session cookie. Only a
    /// path on the same server is accepted, never another host.
    pub async fn fetch_image(&self, url: &str) -> Result<Vec<u8>, BridgeError> {
        if !url.starts_with('/') || url.starts_with("//") || url.contains("..") {
            return Err(BridgeError::Protocol(format!(
                "unexpected image address {url:?}"
            )));
        }
        let resp = self
            .send(self.http.get(format!("{}{}", self.base, url)))
            .await?;
        let bytes = check(resp)
            .await?
            .bytes()
            .await
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(BridgeError::Protocol("the image is too large".into()));
        }
        Ok(bytes.to_vec())
    }

    /// A published battle map as nd-world holds it: walls and lights from
    /// `walls.json`, the room grid's cell size from `rooms.json` (both Python-image
    /// routes; the Rust-only image answers 501 and this says so), and the picture
    /// from `/uploads/schematics/<slug>.<ext>`.
    pub async fn fetch_published(&self, slug: &str) -> Result<Published, BridgeError> {
        if slug.is_empty()
            || !slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(BridgeError::Precheck(
                "a map slug is lowercase letters, digits and hyphens".into(),
            ));
        }
        let get_json = |path: String| async move {
            let resp = self.send(self.req(Method::GET, &path)).await?;
            let status = resp.status();
            if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
                return Err(BridgeError::AuthRequired);
            }
            if status == StatusCode::NOT_IMPLEMENTED {
                return Err(BridgeError::NeedsPython(path));
            }
            let v: Value = check(resp)
                .await?
                .json()
                .await
                .map_err(|e| BridgeError::Protocol(format!("{path}: {e}")))?;
            Ok::<Value, BridgeError>(v)
        };
        let state = get_json(format!("/maps/schematic/{slug}/walls.json")).await?;
        let walls: Vec<Wall> =
            serde_json::from_value(state.get("walls").cloned().unwrap_or(json!([])))
                .map_err(|e| BridgeError::Protocol(format!("walls.json walls: {e}")))?;
        let lights: Vec<Light> =
            serde_json::from_value(state.get("lights").cloned().unwrap_or(json!([])))
                .map_err(|e| BridgeError::Protocol(format!("walls.json lights: {e}")))?;
        let cell_px = get_json(format!("/maps/schematic/{slug}/rooms.json"))
            .await
            .ok()
            .and_then(|v| v.get("cell").and_then(Value::as_f64))
            .filter(|c| *c >= 8.0);
        let mut image = None;
        for ext in ["webp", "png", "jpg", "avif"] {
            let resp = self
                .send(
                    self.http
                        .get(format!("{}/uploads/schematics/{slug}.{ext}", self.base)),
                )
                .await?;
            if resp.status().is_success() {
                let bytes = resp
                    .bytes()
                    .await
                    .map_err(|e| BridgeError::Protocol(e.to_string()))?;
                if bytes.len() > 64 * 1024 * 1024 {
                    return Err(BridgeError::Protocol("the picture is too large".into()));
                }
                image = Some((bytes.to_vec(), ext.to_string()));
                break;
            }
        }
        let (image, image_ext) =
            image.ok_or_else(|| BridgeError::NotFound(format!("the picture of map {slug}")))?;
        Ok(Published {
            walls,
            lights,
            cell_px,
            image,
            image_ext,
        })
    }

    fn precheck(map: &PublishMap) -> Result<(), BridgeError> {
        let bad = |m: String| Err(BridgeError::Precheck(m));
        if map.walls.len() > WALL_CAP {
            return bad(format!(
                "{} walls; nd-world stores at most {WALL_CAP}",
                map.walls.len()
            ));
        }
        if map.lights.len() > LIGHT_CAP {
            return bad(format!(
                "{} lights; nd-world stores at most {LIGHT_CAP}",
                map.lights.len()
            ));
        }
        if !(100..=20_000).contains(&map.canvas_w) || !(100..=20_000).contains(&map.canvas_h) {
            return bad("the picture must be 100..=20000 px a side for nd-world".into());
        }
        if mime_for(&map.image_ext).is_none() {
            return bad(format!(
                "picture type {:?} is not png/webp/avif/jpg",
                map.image_ext
            ));
        }
        if map.image.is_empty() {
            return bad("no picture to publish".into());
        }
        if map.name.trim().is_empty() {
            return bad("the map needs a name".into());
        }
        Ok(())
    }

    /// Publish (or, with `existing_slug`, update in place). A re-publish keeps
    /// the map's canvas, so the picture must keep its size.
    pub async fn publish(
        &self,
        map: &PublishMap,
        existing_slug: Option<&str>,
    ) -> Result<PublishReport, BridgeError> {
        Self::precheck(map)?;
        let mut report = PublishReport::default();

        let slug = match existing_slug {
            Some(s) => s.to_string(),
            None => {
                let form = [
                    ("name", map.name.trim().to_string()),
                    ("canvas_width", map.canvas_w.to_string()),
                    ("canvas_height", map.canvas_h.to_string()),
                    ("canvas_bg", "dark".to_string()),
                ];
                let resp = check(
                    self.send(self.req(Method::POST, "/maps/schematic/new").form(&form))
                        .await?,
                )
                .await?;
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| BridgeError::Protocol("map creation gave no Location".into()))?;
                let slug = loc
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| BridgeError::Protocol(format!("odd Location {loc:?}")))?
                    .to_string();
                report.created = true;
                report.steps.push("create");
                slug
            }
        };
        report.slug = slug.clone();
        let part = |step: &'static str, cause: BridgeError| BridgeError::Partial {
            slug: slug.clone(),
            step,
            cause: Box::new(cause),
        };
        let path = |tail: &str| format!("/maps/schematic/{slug}/{tail}");

        // background
        let mime = mime_for(&map.image_ext).expect("prechecked");
        let part_file = multipart::Part::bytes(map.image.clone())
            .file_name(format!("map.{}", map.image_ext))
            .mime_str(mime)
            .map_err(|e| BridgeError::Protocol(e.to_string()))?;
        let upload = self
            .req(Method::POST, &path("upload"))
            .multipart(multipart::Form::new().part("file", part_file));
        check(self.send(upload).await.map_err(|e| part("picture", e))?)
            .await
            .map_err(|e| part("picture", e))?;
        report.steps.push("picture");

        // grid
        let grid = json!({"grid_type": "square", "config": {
            "cell_size": map.cell_px, "offset_x": 0, "offset_y": 0,
            "unit_per_cell": 5, "unit_label": "ft"}});
        self.json_step("grid", &path("grid"), &grid, &mut report, &part, false)
            .await?;
        self.json_step(
            "walls",
            &path("walls"),
            &json!({"walls": map.walls}),
            &mut report,
            &part,
            true,
        )
        .await?;
        self.json_step(
            "lights",
            &path("lights"),
            &json!({"lights": map.lights}),
            &mut report,
            &part,
            true,
        )
        .await?;
        if let Some(rooms) = &map.rooms {
            self.json_step("rooms", &path("rooms"), rooms, &mut report, &part, true)
                .await?;
        }
        Ok(report)
    }

    fn precheck_region(map: &PublishRegion) -> Result<(), BridgeError> {
        let bad = |m: String| Err(BridgeError::Precheck(m));
        if map.markers.len() > OVERLAY_CAP || map.regions.len() > OVERLAY_CAP {
            return bad(format!(
                "{} markers / {} regions; nd-world keeps at most {OVERLAY_CAP} of each",
                map.markers.len(),
                map.regions.len()
            ));
        }
        if !(100..=20_000).contains(&map.width_px) || !(100..=20_000).contains(&map.height_px) {
            return bad("the picture must be 100..=20000 px a side for nd-world".into());
        }
        if !matches!(
            map.image_ext.as_str(),
            "webp" | "avif" | "png" | "jpg" | "jpeg" | "gif"
        ) {
            return bad(format!(
                "picture type {:?} is not one nd-world accepts",
                map.image_ext
            ));
        }
        if map.image.is_empty() {
            return bad("no picture to publish".into());
        }
        if map.name.trim().is_empty() {
            return bad("the map needs a name".into());
        }
        Ok(())
    }

    /// Publish a region map as an nd-world image map: create it (or, with
    /// `existing_slug`, replace its picture), then set its markers and regions.
    pub async fn publish_region(
        &self,
        map: &PublishRegion,
        existing_slug: Option<&str>,
    ) -> Result<PublishReport, BridgeError> {
        Self::precheck_region(map)?;
        let mut report = PublishReport::default();
        let mime = mime_for(&map.image_ext).unwrap_or("application/octet-stream");
        let file = |m: &PublishRegion| {
            multipart::Part::bytes(m.image.clone())
                .file_name(format!("map.{}", m.image_ext))
                .mime_str(mime)
                .map_err(|e| BridgeError::Protocol(e.to_string()))
        };
        let slug = match existing_slug {
            Some(s) => s.to_string(),
            None => {
                let form = multipart::Form::new()
                    .text("name", map.name.trim().to_string())
                    .text("width", map.width_px.to_string())
                    .text("height", map.height_px.to_string())
                    .part("image_file", file(map)?);
                let resp = check(
                    self.send(self.req(Method::POST, "/maps/new").multipart(form))
                        .await?,
                )
                .await?;
                let loc = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or_else(|| BridgeError::Protocol("map creation gave no Location".into()))?;
                let slug = loc
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| BridgeError::Protocol(format!("odd Location {loc:?}")))?
                    .to_string();
                report.created = true;
                report.steps.push("create");
                slug
            }
        };
        report.slug = slug.clone();
        let part = |step: &'static str, cause: BridgeError| BridgeError::Partial {
            slug: slug.clone(),
            step,
            cause: Box::new(cause),
        };
        if !report.created {
            // A re-publish: swap the picture in place.
            let up = self
                .req(Method::POST, &format!("/maps/{slug}/upload"))
                .multipart(multipart::Form::new().part("file", file(map)?));
            check(self.send(up).await.map_err(|e| part("picture", e))?)
                .await
                .map_err(|e| part("picture", e))?;
            report.steps.push("picture");
        }
        if report.created || map.replace_overlay {
            let body = json!({"custom_markers": map.markers, "custom_regions": map.regions});
            self.json_step(
                "overlay",
                &format!("/api/maps/{slug}/overlay"),
                &body,
                &mut report,
                &part,
                false,
            )
            .await?;
        }
        Ok(report)
    }

    async fn json_step(
        &self,
        step: &'static str,
        path: &str,
        body: &Value,
        report: &mut PublishReport,
        part: &dyn Fn(&'static str, BridgeError) -> BridgeError,
        collect_warnings: bool,
    ) -> Result<(), BridgeError> {
        let resp = self
            .send(self.req(Method::POST, path).json(body))
            .await
            .map_err(|e| part(step, e))?;
        let resp = check(resp).await.map_err(|e| part(step, e))?;
        if collect_warnings {
            if let Ok(v) = resp.json::<Value>().await {
                for w in v
                    .get("warnings")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(w) = w.as_str() {
                        report.warnings.push(format!("{step}: {w}"));
                    }
                }
            }
        }
        report.steps.push(step);
        Ok(())
    }
}
