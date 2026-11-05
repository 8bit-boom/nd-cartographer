//! Background image jobs (plan §8.4 "background-task client"): the studio asks
//! nd-world's imagegen queue for pictures, polls it without blocking the UI,
//! keeps the finished pictures in the asset store, and reports progress. Jobs
//! go through nd-world's one-GPU queue in turn (variants are sequential, never
//! a burst), can be cancelled, and nothing lands in a document or the library
//! until the GM accepts it.

// Without the nd-world client nothing queues pictures; the registry stays for the status routes.
#![cfg_attr(not(feature = "bridge"), allow(dead_code))]

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

#[derive(Clone, serde::Serialize)]
pub struct AiJob {
    pub id: String,
    /// `underlay`, `material` or `prop`.
    pub kind: String,
    /// `running`, `done`, `error` or `cancelled`.
    pub status: String,
    pub message: String,
    pub prompt: String,
    pub count: usize,
    /// Asset ids of the finished pictures, in order.
    pub assets: Vec<String>,
    pub seeds: Vec<i64>,
}

fn registry() -> &'static Mutex<BTreeMap<String, AiJob>> {
    static R: OnceLock<Mutex<BTreeMap<String, AiJob>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub fn get(id: &str) -> Option<AiJob> {
    registry().lock().expect("jobs").get(id).cloned()
}

pub fn cancel(id: &str) -> bool {
    let mut r = registry().lock().expect("jobs");
    match r.get_mut(id) {
        Some(j) if j.status == "running" => {
            j.status = "cancelled".into();
            j.message = "Cancelled.".into();
            true
        }
        _ => false,
    }
}

fn update(id: &str, f: impl FnOnce(&mut AiJob)) {
    if let Some(j) = registry().lock().expect("jobs").get_mut(id) {
        f(j);
    }
}

const MAX_RUNNING: usize = 2;
const MAX_KEPT: usize = 40;

/// The prompt as sent: the user's words plus what each kind of picture needs.
pub fn shape_prompt(kind: &str, prompt: &str) -> String {
    let p = prompt.trim();
    match kind {
        "material" => format!("{p}, seamless tileable texture, top-down view, flat even lighting, no perspective, no objects"),
        "prop" => format!("{p}, a single object, top-down view, centered, isolated on a plain flat solid magenta (#ff00ff) background, no shadow"),
        _ => p.to_string(),
    }
}

pub fn shape_negative(kind: &str) -> &'static str {
    match kind {
        "material" => "text, watermark, border, frame, perspective, vignette",
        "prop" => "text, watermark, background scenery, multiple objects, cropped",
        _ => "text, watermark",
    }
}

/// Register a job and return its id (the work itself is `run`).
pub fn register(kind: &str, prompt: &str, count: usize) -> Result<String, String> {
    let mut r = registry().lock().expect("jobs");
    if r.values().filter(|j| j.status == "running").count() >= MAX_RUNNING {
        return Err(
            "two picture jobs are already running; wait for one to finish or cancel it".into(),
        );
    }
    while r.len() >= MAX_KEPT {
        let Some(oldest) = r
            .iter()
            .find(|(_, j)| j.status != "running")
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        r.remove(&oldest);
    }
    let id = ndcart_core::ids::new_id("aj");
    r.insert(
        id.clone(),
        AiJob {
            id: id.clone(),
            kind: kind.into(),
            status: "running".into(),
            message: "Starting…".into(),
            prompt: prompt.chars().take(500).collect(),
            count,
            assets: Vec::new(),
            seeds: Vec::new(),
        },
    );
    Ok(id)
}

#[cfg(feature = "bridge")]
pub struct Spec {
    pub kind: String,
    pub prompt: String,
    pub width: u32,
    pub height: u32,
    pub count: usize,
    pub seed: i64,
}

/// Do the work of a registered job: connect, then for each variant queue it,
/// poll until it finishes, fetch the picture, keep it. Any failure ends the job
/// with the reason, keeping the pictures already made.
#[cfg(feature = "bridge")]
pub async fn run(id: String, cfg: crate::draft::Config, spec: Spec) {
    use ndcart_bridge::ImageRequest;
    let result: Result<(), String> = async {
        let backend = crate::draft::Backend::connect(&cfg).await?;
        let crate::draft::Backend::World(bridge, _) = &backend else {
            return Err(
                "image generation needs nd-world's imagegen (a standalone text model cannot draw)"
                    .into(),
            );
        };
        for i in 0..spec.count {
            if get(&id).is_none_or(|j| j.status != "running") {
                return Ok(());
            }
            update(&id, |j| {
                j.message = format!("Picture {} of {}: queued", i + 1, spec.count)
            });
            // A shared seed family keeps a set coherent: seed, seed+1, …
            let seed = if spec.seed >= 0 {
                spec.seed + i as i64
            } else {
                -1
            };
            let req = ImageRequest {
                prompt: shape_prompt(&spec.kind, &spec.prompt),
                negative: shape_negative(&spec.kind).into(),
                width: spec.width,
                height: spec.height,
                steps: 20,
                seed,
                init_image: None,
                init_strength: 0.6,
            };
            let job = bridge
                .imagegen_start(&req)
                .await
                .map_err(|e| e.to_string())?;
            let started = std::time::Instant::now();
            let urls = loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if get(&id).is_none_or(|j| j.status != "running") {
                    return Ok(());
                }
                let st = bridge
                    .imagegen_status(job)
                    .await
                    .map_err(|e| e.to_string())?;
                match st.status.as_str() {
                    "done" => break st.urls,
                    "error" | "cancelled" | "interrupted" => {
                        return Err(format!(
                            "nd-world's image job {}: {}",
                            st.status,
                            st.error.unwrap_or_default()
                        ));
                    }
                    s => update(&id, |j| {
                        j.message = format!(
                            "Picture {} of {}: {s} ({}s)",
                            i + 1,
                            spec.count,
                            started.elapsed().as_secs()
                        )
                    }),
                }
                if started.elapsed() > std::time::Duration::from_secs(900) {
                    return Err("the image took more than 15 minutes; gave up".into());
                }
            };
            let url = urls
                .first()
                .ok_or("nd-world finished the job but returned no picture")?;
            let bytes = bridge.fetch_image(url).await.map_err(|e| e.to_string())?;
            let asset = tokio::task::spawn_blocking(move || crate::assets::global().put(&bytes))
                .await
                .map_err(|e| e.to_string())??;
            update(&id, |j| {
                j.assets.push(asset);
                j.seeds.push(seed);
            });
        }
        Ok(())
    }
    .await;
    update(&id, |j| {
        if j.status != "running" {
            return; // cancelled meanwhile
        }
        match result {
            Ok(()) => {
                j.status = "done".into();
                j.message = format!("{} picture(s) ready to review.", j.assets.len());
            }
            Err(e) => {
                j.status = "error".into();
                j.message = e;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_shaped_per_kind_and_jobs_are_limited_and_cancellable() {
        assert!(shape_prompt("material", "mossy flagstones").contains("seamless tileable"));
        assert!(shape_prompt("prop", "a rusty crate").contains("magenta"));
        assert_eq!(
            shape_prompt("underlay", "  a harbour at dusk "),
            "a harbour at dusk"
        );
        assert!(shape_negative("prop").contains("multiple objects"));
        // Only two at a time; a cancelled one frees its slot and keeps its reason.
        let a = register("material", "x", 2).unwrap();
        let b = register("prop", "y", 1).unwrap();
        assert!(register("underlay", "z", 1)
            .unwrap_err()
            .contains("two picture jobs"));
        assert!(cancel(&a));
        assert!(!cancel(&a), "already cancelled");
        assert_eq!(get(&a).unwrap().status, "cancelled");
        let c = register("underlay", "z", 1).unwrap();
        assert!(get(&c).is_some() && get(&b).unwrap().status == "running");
        assert!(get("nope").is_none() && !cancel("nope"));
        cancel(&b);
        cancel(&c);
    }
}
