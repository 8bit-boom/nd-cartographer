//! `ndcart ai status [--json]`: what is installed and running, in one place: the runtime, the
//! computer, the model files (and whether they were tested here), the managed server, and the
//! endpoint the studio would use, with what it can do (constrained decoding, context size).

use super::catalog::{self, Record};
use super::hw::Hw;
use super::pull::{models_dir, sidecar_path, Sidecar};
use super::runtime::{self, Runtime};
use super::serve::{self, Health};
use crate::ai_settings::{self, StoredAiSettings};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize, Debug)]
pub struct RuntimeView {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub source: Option<&'static str>,
    pub hint: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct InstalledModel {
    /// Relative to `<data>/models`.
    pub file: String,
    pub size_bytes: u64,
    pub sha256: Option<String>,
    pub catalog_id: Option<String>,
    pub tested: Option<Record>,
}

#[derive(Serialize, Debug)]
pub struct ManagedView {
    pub model_file: String,
    pub model_id: Option<String>,
    pub port: u16,
    pub ctx: u32,
    pub autostart: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub health: Health,
}

#[derive(Serialize, Debug)]
pub struct EndpointView {
    pub configured: bool,
    pub url: Option<String>,
    pub model: Option<String>,
    /// `environment`, `saved`, `managed` or `none`.
    pub source: &'static str,
    pub key_set: bool,
}

#[derive(Serialize, Debug)]
pub struct CapsView {
    pub kind: String,
    pub json_schema: Option<bool>,
    pub ctx: Option<u32>,
    pub models: Vec<String>,
}

#[derive(Serialize, Debug)]
pub struct StatusReport {
    pub data_dir: String,
    pub hardware: Hw,
    pub runtime: RuntimeView,
    pub models: Vec<InstalledModel>,
    pub managed: Option<ManagedView>,
    pub endpoint: EndpointView,
    pub caps: Option<CapsView>,
}

/// Every `.gguf` under `<data>/models` (not `.part`), with its checksum record.
pub fn installed(data: &Path) -> Vec<InstalledModel> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<std::path::PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() && depth < 5 {
                walk(&p, depth + 1, out);
            } else if p.extension().is_some_and(|x| x == "gguf") {
                out.push(p);
            }
        }
    }
    let root = models_dir(data);
    let mut files = Vec::new();
    walk(&root, 0, &mut files);
    files.sort();
    let cat = catalog::bundled();
    let records = catalog::load_records(data);
    files
        .into_iter()
        .filter_map(|p| {
            let rel = p
                .strip_prefix(&root)
                .ok()?
                .to_string_lossy()
                .replace('\\', "/");
            let sc: Option<Sidecar> = std::fs::read_to_string(sidecar_path(&p))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            let id = cat
                .iter()
                .find(|m| rel == format!("{}/{}", m.hf_repo, m.file))
                .map(|m| m.id.clone());
            Some(InstalledModel {
                size_bytes: std::fs::metadata(&p).ok()?.len(),
                sha256: sc.map(|s| s.sha256),
                tested: id.as_ref().and_then(|i| records.get(i).cloned()),
                catalog_id: id,
                file: rel,
            })
        })
        .collect()
}

pub async fn collect(
    data: &Path,
    stored: &StoredAiSettings,
    rt: Option<Runtime>,
    hw: Hw,
) -> StatusReport {
    let runtime = match &rt {
        Some(r) => RuntimeView {
            found: true,
            path: Some(r.path.to_string_lossy().into_owned()),
            version: r.version.clone(),
            source: Some(r.source),
            hint: None,
        },
        None => RuntimeView {
            found: false,
            path: None,
            version: None,
            source: None,
            hint: Some(runtime::install_hint_here()),
        },
    };
    let managed = match &stored.managed {
        Some(m) => {
            let rec = serve::read_rec(data).filter(|r| serve::is_alive(r.pid));
            let health = serve::health(m.port()).await;
            Some(ManagedView {
                model_file: m.model_file.clone(),
                model_id: m.model_id.clone(),
                port: m.port(),
                ctx: m.ctx(),
                autostart: m.autostart,
                running: rec.is_some() || health != Health::Down,
                pid: rec.map(|r| r.pid),
                health,
            })
        }
        None => None,
    };
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let direct =
        ai_settings::resolve_direct(env("ND_CARTO_LLM_URL"), env("ND_CARTO_LLM_MODEL"), stored);
    let key = direct.as_ref().and_then(|(u, _)| {
        ai_settings::resolve_key_for(
            u,
            env("ND_CARTO_LLM_KEY"),
            env("ND_CARTO_LLM_KEY_FILE"),
            stored,
        )
    });
    let caps = match &direct {
        Some((u, _)) => {
            let c = ndcart_bridge::caps::probe(u, key.as_deref()).await;
            Some(CapsView {
                kind: format!("{:?}", c.kind).to_lowercase(),
                json_schema: c.json_schema,
                ctx: c.ctx,
                models: c.model_ids,
            })
        }
        None => None,
    };
    StatusReport {
        data_dir: data.to_string_lossy().into_owned(),
        hardware: hw,
        runtime,
        models: installed(data),
        managed,
        endpoint: EndpointView {
            configured: direct.is_some(),
            url: direct.as_ref().map(|d| d.0.clone()),
            model: direct.as_ref().map(|d| d.1.clone()),
            source: ai_settings::view(stored).source,
            key_set: key.is_some(),
        },
        caps,
    }
}

impl StatusReport {
    pub fn render(&self) -> String {
        let mut o = String::new();
        match (&self.runtime.found, &self.runtime.path) {
            (true, Some(p)) => o.push_str(&format!(
                "Runtime:   llama-server {} ({p}, found via {})\n",
                self.runtime.version.as_deref().unwrap_or("version unknown"),
                self.runtime.source.unwrap_or("?")
            )),
            _ => o.push_str(&format!(
                "Runtime:   llama-server not found.\n{}\n",
                self.runtime.hint.as_deref().unwrap_or("")
            )),
        }
        o.push_str(&format!("Computer:  {}\n", self.hardware.summary()));
        if !self.hardware.runtime_devices.is_empty() {
            o.push_str(&format!(
                "           llama.cpp sees: {}\n",
                self.hardware.runtime_devices.join("; ")
            ));
        }
        o.push_str(&format!(
            "Models:    {} file(s) in {}/models\n",
            self.models.len(),
            self.data_dir
        ));
        for m in &self.models {
            o.push_str(&format!(
                "           {} ({}, {})\n",
                m.file,
                super::human_bytes(m.size_bytes),
                catalog::status_label(m.tested.as_ref())
            ));
        }
        match &self.managed {
            Some(m) => o.push_str(&format!(
                "Local server: {} on port {} ({:?}){}, model {}, context {}{}\n",
                if m.running { "running" } else { "not running" },
                m.port,
                m.health,
                m.pid.map(|p| format!(", pid {p}")).unwrap_or_default(),
                m.model_file,
                m.ctx,
                if m.autostart {
                    ", starts with the studio"
                } else {
                    ""
                }
            )),
            None => o.push_str("Local server: not set up (ndcart ai setup)\n"),
        }
        match (&self.endpoint.url, &self.endpoint.model) {
            (Some(u), Some(m)) => o.push_str(&format!(
                "Endpoint:  {u} (model {m}), from {}{}\n",
                self.endpoint.source,
                if self.endpoint.key_set {
                    ", with a key"
                } else {
                    ""
                }
            )),
            _ => o.push_str("Endpoint:  none. The studio uses its keyword template drafts.\n"),
        }
        if let Some(c) = &self.caps {
            o.push_str(&format!(
                "Server:    {}; constrained JSON {}; context {}\n",
                c.kind,
                match c.json_schema {
                    Some(true) => "works",
                    Some(false) => "refused",
                    None => "not known yet",
                },
                c.ctx
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".into())
            ));
        }
        o
    }
}

/// `ndcart ai status [--json] [--data-dir D]`
pub async fn run_status(args: Vec<String>) -> Result<(), String> {
    let c = super::common(args)?;
    if let Some(o) = c.rest.first() {
        return Err(format!("unknown argument: {o}"));
    }
    let stored = ai_settings::load(Some(&c.data));
    let rt = runtime::find(
        stored
            .managed
            .as_ref()
            .and_then(|m| m.runtime_path.as_deref()),
    );
    let hw = super::hw::detect(rt.as_ref().map(|r| r.path.as_path()));
    let report = collect(&c.data, &stored, rt, hw).await;
    if c.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.render());
    }
    Ok(())
}
