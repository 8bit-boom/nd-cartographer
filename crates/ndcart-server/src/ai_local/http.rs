//! The "Run a model on this computer" panel's HTTP side (`/api/ai/local/*`). The same pieces
//! as the `ndcart ai` commands: the hardware report, the catalog with fit marks, a
//! confirm-the-size download as a background job, start and stop for the managed server, the
//! self-test, and "use this model" (which writes the settings). Every route is POST (or
//! DELETE) behind the studio's cross-site guard, except the read-only `GET /api/ai/local`.

use super::catalog::{self, Entry};
use super::hw::{self, Fit, Hw};
use super::pull::{self, Policy, Progress, Pull};
use super::runtime::{self, Runtime};
use super::serve::{self, Health};
use super::{selftest, setup, status};
use crate::ai_settings;
use crate::App;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

type Reply = (StatusCode, Json<Value>);

fn err(code: StatusCode, msg: impl Into<String>) -> Reply {
    (code, Json(json!({ "error": msg.into() })))
}

fn data_of(app: &App) -> Result<PathBuf, Reply> {
    app.data_dir.clone().ok_or_else(|| {
        err(
            StatusCode::BAD_REQUEST,
            "the studio has no data folder, which local models need. Start it with --data-dir (or ND_CARTO_DATA_DIR).",
        )
    })
}

// ---- the download job ----------------------------------------------------------------------

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct PullJob {
    pub id: String,
    /// `idle`, `running`, `done`, `error` or `cancelled`.
    pub state: &'static str,
    pub done: u64,
    pub total: u64,
    pub file: String,
    pub error: Option<String>,
}

static JOB: Mutex<PullJob> = Mutex::new(PullJob {
    id: String::new(),
    state: "idle",
    done: 0,
    total: 0,
    file: String::new(),
    error: None,
});
static CANCEL: AtomicBool = AtomicBool::new(false);

fn job() -> PullJob {
    JOB.lock().expect("job").clone()
}

/// The Hub the studio downloads from (tests point it at a stand-in).
static POLICY_OVERRIDE: Mutex<Option<Policy>> = Mutex::new(None);

#[cfg(test)]
pub fn set_policy_for_test(p: Option<Policy>) {
    *POLICY_OVERRIDE.lock().expect("policy") = p;
}

fn pull_client() -> Pull {
    let mut p = Pull::from_env();
    if let Some(o) = POLICY_OVERRIDE.lock().expect("policy").clone() {
        p.policy = o;
    }
    p
}

// ---- the hardware report is cached for a little while --------------------------------------

static HW: Mutex<Option<(Instant, Option<PathBuf>, Hw)>> = Mutex::new(None);

pub(crate) fn hardware(rt: Option<&Runtime>) -> Hw {
    let key = rt.map(|r| r.path.clone());
    if let Some((at, k, h)) = HW.lock().expect("hw").as_ref() {
        if *k == key && at.elapsed() < Duration::from_secs(120) {
            return h.clone();
        }
    }
    let h = hw::detect(key.as_deref());
    *HW.lock().expect("hw") = Some((Instant::now(), key, h.clone()));
    h
}

// ---- GET /api/ai/local ---------------------------------------------------------------------

#[derive(Serialize)]
struct CatalogRow<'a> {
    #[serde(flatten)]
    entry: &'a Entry,
    fit: Fit,
    recommended: bool,
    need_bytes: u64,
    installed: bool,
    tested: Option<&'a catalog::Record>,
}

async fn server_view(app: &App) -> Value {
    let stored = app.ai.lock().expect("ai").clone();
    let Some(m) = &stored.managed else {
        return json!({"configured": false});
    };
    let data = app.data_dir.clone();
    let rec = data
        .as_deref()
        .and_then(serve::read_rec)
        .filter(|r| serve::is_alive(r.pid));
    let health = serve::health(m.port()).await;
    json!({
        "configured": true,
        "model_file": m.model_file,
        "model_id": m.model_id,
        "port": m.port(),
        "ctx": m.ctx(),
        "autostart": m.autostart,
        "running": rec.is_some() || health != Health::Down,
        "pid": rec.map(|r| r.pid),
        "health": health,
        "supervisor": serve::status(),
    })
}

async fn local_info(State(app): State<App>) -> Json<Value> {
    let stored = app.ai.lock().expect("ai").clone();
    let rt_setting = stored.managed.as_ref().and_then(|m| m.runtime_path.clone());
    let (rt, machine) = tokio::task::spawn_blocking(move || {
        let rt = runtime::find(rt_setting.as_deref());
        let h = hardware(rt.as_ref());
        (rt, h)
    })
    .await
    .expect("detect");
    let cat = catalog::bundled();
    let data = app.data_dir.clone();
    let records = data
        .as_deref()
        .map(catalog::load_records)
        .unwrap_or_default();
    let installed = data.as_deref().map(status::installed).unwrap_or_default();
    let recs = hw::recommend_tested(&machine, &cat, &records);
    let rows: Vec<CatalogRow> = recs
        .iter()
        .map(|r| CatalogRow {
            entry: r.entry,
            fit: r.fit,
            recommended: r.recommended,
            need_bytes: r.need_bytes,
            installed: installed
                .iter()
                .any(|i| i.catalog_id.as_deref() == Some(r.entry.id.as_str())),
            tested: records.get(&r.entry.id),
        })
        .collect();
    Json(json!({
        "data_dir_ok": data.is_some(),
        "hw": machine,
        "runtime": match &rt {
            Some(r) => json!({"found": true, "path": r.path, "version": r.version, "source": r.source}),
            None => json!({"found": false, "hint": runtime::install_hint_here()}),
        },
        "catalog": rows,
        "installed": installed,
        "server": server_view(&app).await,
        "pull": job(),
    }))
}

// ---- POST /api/ai/local/pull ---------------------------------------------------------------

#[derive(Deserialize)]
struct PullBody {
    id: String,
    #[serde(default)]
    confirm_bytes: Option<u64>,
}

async fn local_pull(
    State(app): State<App>,
    body: Result<Json<PullBody>, axum::extract::rejection::JsonRejection>,
) -> Reply {
    let Ok(Json(b)) = body else {
        return err(
            StatusCode::BAD_REQUEST,
            "body must be {\"id\": <catalog id>, \"confirm_bytes\": <size>}",
        );
    };
    let data = match data_of(&app) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let cat = catalog::bundled();
    let Some(entry) = catalog::find(&cat, &b.id) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("'{}' is not a catalog model", b.id),
        );
    };
    if job().state == "running" {
        return err(StatusCode::CONFLICT, "a download is already running");
    }
    let client = pull_client();
    let target = match pull::parse_target(&entry.id, &cat, false) {
        Ok(t) => t,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let plan = match client.plan(&target).await {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e),
    };
    // The person confirms the real size, which only the Hub knows.
    if b.confirm_bytes != Some(plan.total_bytes) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "confirm the download size first",
                "size_bytes": plan.total_bytes,
                "repo": plan.repo,
                "files": plan.files,
            })),
        );
    }
    if let Err(e) = std::fs::create_dir_all(pull::models_dir(&data)) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot create the models folder: {e}"),
        );
    }
    if let Err(e) = pull::check_budget(&plan, None, (client.free_bytes)(&pull::models_dir(&data))) {
        return err(StatusCode::BAD_REQUEST, e);
    }
    CANCEL.store(false, Ordering::SeqCst);
    *JOB.lock().expect("job") = PullJob {
        id: entry.id.clone(),
        state: "running",
        done: 0,
        total: plan.total_bytes,
        file: String::new(),
        error: None,
    };
    let size = plan.total_bytes;
    tokio::spawn(async move {
        let mut progress = |p: &Progress| {
            let mut j = JOB.lock().expect("job");
            j.done = p.done;
            j.total = p.total;
            j.file = p.file.clone();
        };
        let r = client
            .download(&plan, &data, &mut progress, Some(&CANCEL))
            .await;
        let mut j = JOB.lock().expect("job");
        match r {
            Ok(_) => {
                j.state = "done";
                j.done = j.total;
            }
            Err(e) if CANCEL.load(Ordering::SeqCst) => {
                j.state = "cancelled";
                j.error = Some(e);
            }
            Err(e) => {
                j.state = "error";
                j.error = Some(e);
            }
        }
    });
    (
        StatusCode::ACCEPTED,
        Json(json!({"started": true, "size_bytes": size})),
    )
}

async fn local_pull_cancel() -> Json<Value> {
    CANCEL.store(true, Ordering::SeqCst);
    Json(json!({"ok": true}))
}

// ---- start, stop, use, delete, test --------------------------------------------------------

async fn local_start(State(app): State<App>) -> Reply {
    let data = match data_of(&app) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let stored = app.ai.lock().expect("ai").clone();
    let Some(m) = stored.managed.clone() else {
        return err(
            StatusCode::BAD_REQUEST,
            "no local model is chosen yet: download one and press Use this model",
        );
    };
    let rt = m.runtime_path.clone();
    let Some(rt) = tokio::task::spawn_blocking(move || runtime::find(rt.as_deref()))
        .await
        .expect("find")
    else {
        return err(
            StatusCode::BAD_REQUEST,
            format!(
                "llama-server was not found. {}",
                runtime::install_hint_here()
            ),
        );
    };
    let key = match serve::ensure_key(&data) {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let launch = match serve::Launch::new(&data, &m, rt.path, key) {
        Ok(l) => l,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    match serve::start_nowait(&data, &launch) {
        // Loading takes a while; the panel polls GET /api/ai/local for `health`.
        Ok((rec, _child)) => (
            StatusCode::ACCEPTED,
            Json(json!({"starting": true, "pid": rec.pid, "port": rec.port})),
        ),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

async fn local_stop(State(app): State<App>) -> Reply {
    let data = match data_of(&app) {
        Ok(d) => d,
        Err(e) => return e,
    };
    match tokio::task::spawn_blocking(move || serve::stop(&data, Duration::from_secs(10)))
        .await
        .expect("stop")
    {
        Ok(serve::Stopped::Ended(pid)) => {
            (StatusCode::OK, Json(json!({"stopped": true, "pid": pid})))
        }
        Ok(_) => (
            StatusCode::OK,
            Json(json!({"stopped": false, "detail": "no model server was running"})),
        ),
        Err(e) => err(StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
struct UseBody {
    id: String,
    #[serde(default)]
    autostart: Option<bool>,
}

async fn local_use(
    State(app): State<App>,
    body: Result<Json<UseBody>, axum::extract::rejection::JsonRejection>,
) -> Reply {
    let Ok(Json(b)) = body else {
        return err(
            StatusCode::BAD_REQUEST,
            "body must be {\"id\": <catalog id>}",
        );
    };
    let data = match data_of(&app) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let cat = catalog::bundled();
    let Some(entry) = catalog::find(&cat, &b.id) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("'{}' is not a catalog model", b.id),
        );
    };
    // The file the download wrote (its real name may differ from the catalog's guess).
    let installed = status::installed(&data);
    let Some(file) = installed
        .iter()
        .find(|i| i.catalog_id.as_deref() == Some(entry.id.as_str()))
        .or_else(|| {
            installed
                .iter()
                .find(|i| i.file.starts_with(&format!("{}/", entry.hf_repo)))
        })
    else {
        return err(
            StatusCode::BAD_REQUEST,
            format!("{} is not downloaded yet", entry.id),
        );
    };
    let key = match serve::ensure_key(&data) {
        Ok(k) => k,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e),
    };
    let mut st = app.ai.lock().expect("ai").clone();
    let was = st.managed.clone();
    let autostart = b
        .autostart
        .unwrap_or(was.as_ref().is_some_and(|m| m.autostart));
    setup::use_model(
        &mut st,
        entry,
        &file.file,
        &key,
        setup::UseOpts {
            port: None,
            autostart: false,
        },
    );
    if let Some(m) = st.managed.as_mut() {
        m.autostart = autostart;
    }
    if let Err(e) = ai_settings::save(Some(&data), &st) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, e);
    }
    *app.ai.lock().expect("ai") = st.clone();
    (
        StatusCode::OK,
        Json(json!({
            "ok": true,
            "settings": ai_settings::view(&st),
            "restart_needed": was.as_ref().is_some_and(|m| m.model_file != file.file),
        })),
    )
}

async fn local_delete(State(app): State<App>, UrlPath(id): UrlPath<String>) -> Reply {
    let data = match data_of(&app) {
        Ok(d) => d,
        Err(e) => return e,
    };
    let cat = catalog::bundled();
    let Some(entry) = catalog::find(&cat, &id) else {
        return err(
            StatusCode::NOT_FOUND,
            format!("'{id}' is not a catalog model"),
        );
    };
    let installed = status::installed(&data);
    let Some(file) = installed
        .iter()
        .find(|i| i.catalog_id.as_deref() == Some(entry.id.as_str()))
    else {
        return err(StatusCode::NOT_FOUND, format!("{id} is not downloaded"));
    };
    let stored = app.ai.lock().expect("ai").clone();
    if let Some(m) = &stored.managed {
        if m.model_file == file.file && serve::health(m.port()).await != Health::Down {
            return err(StatusCode::CONFLICT, "this model is running; stop it first");
        }
    }
    let path = pull::models_dir(&data).join(&file.file);
    let _ = std::fs::remove_file(pull::sidecar_path(&path));
    if let Err(e) = std::fs::remove_file(&path) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot delete {}: {e}", path.display()),
        );
    }
    let mut all = catalog::load_records(&data);
    if all.remove(&entry.id).is_some() {
        if let Ok(body) = serde_json::to_string_pretty(&all) {
            let _ =
                crate::store::write_atomic_private(&data.join("ai-verified.json"), body.as_bytes());
        }
    }
    (StatusCode::OK, Json(json!({"deleted": file.file})))
}

#[derive(Deserialize, Default)]
struct TestBody {
    #[serde(default)]
    tools: bool,
}

async fn local_test(State(app): State<App>, body: Option<Json<TestBody>>) -> Reply {
    let tools = body.map(|b| b.0.tools).unwrap_or(false);
    let cfg = match app.resolve_ai() {
        Ok(c) => c,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let report = match selftest::run(&cfg, &selftest::Opts { tools }).await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let id = app
        .ai
        .lock()
        .expect("ai")
        .managed
        .as_ref()
        .and_then(|m| m.model_id.clone());
    let mut v = serde_json::to_value(&report).unwrap_or_default();
    if let (Some(id), Some(data)) = (id, app.data_dir.as_deref()) {
        v["recorded"] = json!(selftest::record(data, &id, &report).unwrap_or(false));
    }
    (StatusCode::OK, Json(v))
}

/// The routes, merged into the studio's router.
pub fn routes() -> Router<App> {
    Router::new()
        .route("/api/ai/local", get(local_info))
        .route("/api/ai/local/pull", post(local_pull))
        .route("/api/ai/local/pull/cancel", post(local_pull_cancel))
        .route("/api/ai/local/start", post(local_start))
        .route("/api/ai/local/stop", post(local_stop))
        .route("/api/ai/local/use", post(local_use))
        .route("/api/ai/local/test", post(local_test))
        .route("/api/ai/local/models/{id}", delete(local_delete))
}
