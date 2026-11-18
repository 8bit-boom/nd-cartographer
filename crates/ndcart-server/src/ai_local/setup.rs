//! `ndcart ai setup`: from nothing to a tested local model in one command. Detect the
//! computer, find `llama-server` (it prints how to get one and stops if missing), choose a
//! model, download it (after the size is shown and confirmed), save the settings, start the
//! server and run the self-test. It never asks for a secret and never installs a program.

use super::catalog::{self, Entry};
use super::human_bytes;
use super::hw::{self, Fit, Hw};
use super::pull::{fetch, models_dir, FetchOpts};
use super::runtime;
use super::selftest;
use super::serve::{self, Launch};
use crate::ai_settings::{self, Managed};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

fn ask(question: &str) -> String {
    eprint!("{question} ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_string()
}

/// The models to offer: the recommended ones, best first (up to three).
pub fn offer<'a>(
    machine: &Hw,
    cat: &'a [Entry],
    tested: &std::collections::BTreeMap<String, catalog::Record>,
) -> Vec<&'a Entry> {
    hw::recommend_tested(machine, cat, tested)
        .into_iter()
        .filter(|r| r.recommended)
        .map(|r| r.entry)
        .collect()
}

#[derive(Default)]
struct Flags {
    model: Option<String>,
    yes: bool,
    port: Option<u16>,
    autostart: bool,
    no_start: bool,
    max_gb: Option<f64>,
}

fn parse(rest: Vec<String>) -> Result<Flags, String> {
    let mut f = Flags::default();
    let mut it = rest.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => f.model = Some(it.next().ok_or("--model needs a catalog id")?),
            "--yes" | "-y" => f.yes = true,
            "--autostart" => f.autostart = true,
            "--no-start" => f.no_start = true,
            "--port" => {
                f.port = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--port needs a number")?,
                )
            }
            "--max-gb" => {
                f.max_gb = Some(
                    it.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--max-gb needs a number")?,
                )
            }
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    Ok(f)
}

/// Returns the exit code (0 good or usable, 3 too weak).
pub async fn run_setup(args: Vec<String>) -> Result<i32, String> {
    let c = super::common(args)?;
    let f = parse(c.rest)?;
    let data: &Path = &c.data;
    let stored = ai_settings::load(Some(data));

    // 1. the runtime
    let Some(rt) = runtime::find(
        stored
            .managed
            .as_ref()
            .and_then(|m| m.runtime_path.as_deref()),
    ) else {
        let machine = hw::detect(None);
        eprintln!("This computer: {}\n", machine.summary());
        eprintln!(
            "llama-server (the program that runs the model) was not found.\n{}\n",
            runtime::install_hint_here()
        );
        return Err("install llama-server, then run `ndcart ai setup` again".into());
    };
    eprintln!(
        "Runtime: llama-server {} at {}",
        rt.version.as_deref().unwrap_or("(version unknown)"),
        rt.path.display()
    );

    // 2. the computer, and a model for it
    let machine = hw::detect(Some(&rt.path));
    eprintln!("Computer: {}", machine.summary());
    if machine.runtime_devices.is_empty() {
        eprintln!("llama.cpp lists no GPU: the model will run on the CPU (slower). A CUDA, Vulkan or Metal build of llama.cpp uses the GPU.");
    }
    let cat = catalog::bundled();
    let tested = catalog::load_records(data);
    let entry: Entry = match &f.model {
        Some(id) => catalog::find(&cat, id)
            .cloned()
            .ok_or_else(|| format!("'{id}' is not a catalog model (see: ndcart ai models)"))?,
        None => {
            let choices = offer(&machine, &cat, &tested);
            if choices.is_empty() {
                return Err("no catalog model is a good fit for this computer. See `ndcart ai models`, or use your own server in Settings.".into());
            }
            eprintln!("\nModels that suit this computer:");
            for (i, m) in choices.iter().enumerate() {
                eprintln!(
                    "  {}. {}: {} download, needs about {} of memory ({})",
                    i + 1,
                    m.id,
                    human_bytes(m.approx_bytes),
                    human_bytes(hw::need_bytes(m)),
                    m.label
                );
            }
            let pick = if f.yes {
                1
            } else {
                let a = ask(&format!("Choose 1-{} [1]:", choices.len()));
                if a.is_empty() {
                    1
                } else {
                    a.parse::<usize>()
                        .ok()
                        .filter(|n| (1..=choices.len()).contains(n))
                        .ok_or("not a choice")?
                }
            };
            choices[pick - 1].clone()
        }
    };
    if let Some(r) = hw::recommend_tested(&machine, &cat, &tested)
        .iter()
        .find(|r| r.entry.id == entry.id)
    {
        match r.fit {
            Fit::No => eprintln!(
                "Warning: {} is probably too big for this computer (it needs about {}).",
                entry.id,
                human_bytes(r.need_bytes)
            ),
            Fit::Partial => eprintln!(
                "Note: {} will run partly on the CPU, which is slower.",
                entry.id
            ),
            Fit::Full => {}
        }
    }
    eprintln!("Model: {} ({}).", entry.id, entry.hf_repo);

    // 3. the file
    let present = models_dir(data).join(&entry.hf_repo).join(&entry.file);
    let rel = if present.is_file() {
        eprintln!("Already downloaded: {}", present.display());
        format!("{}/{}", entry.hf_repo, entry.file)
    } else {
        fetch(
            data,
            &entry.id,
            &FetchOpts {
                yes: f.yes,
                max_gb: f.max_gb,
                ..Default::default()
            },
        )
        .await?
    };

    // 4. the settings
    let mut st = stored.clone();
    let replaced = st.direct_url.clone();
    if let Some(u) = &replaced {
        if !f.yes
            && !ask(&format!(
                "This replaces the saved endpoint {u} with the local model. Continue? [y/N]"
            ))
            .to_lowercase()
            .starts_with('y')
        {
            return Err("cancelled; nothing was changed".into());
        }
    }
    let key = serve::ensure_key(data)?;
    let old = stored.managed.clone().unwrap_or_default();
    let m = Managed {
        model_file: rel.clone(),
        model_id: Some(entry.id.clone()),
        ctx: Some(entry.ctx_default),
        port: f.port.or(old.port),
        gpu_layers: old.gpu_layers,
        threads: old.threads,
        autostart: f.autostart || old.autostart,
        runtime_path: old.runtime_path,
        key_file: Some(key.to_string_lossy().into_owned()),
    };
    st.managed = Some(m.clone());
    // The local model is used when nothing else is set; a saved endpoint (and the key that
    // belonged to it) would take precedence, so it is cleared.
    st.direct_url = None;
    st.direct_model = None;
    st.key = None;
    ai_settings::save(Some(data), &st)?;
    eprintln!(
        "Saved: the studio will use the local model{}.",
        if m.autostart {
            " and start it with `ndcart serve`"
        } else {
            ""
        }
    );
    if f.no_start {
        eprintln!("Start it with: ndcart ai serve");
        return Ok(0);
    }

    // 5. start it
    let launch = Launch::new(data, &m, rt.path.clone(), key)?;
    serve::stop(data, Duration::from_secs(10))?;
    eprintln!("Starting the server (loading the model can take a minute)...");
    let rec = serve::start(data, &launch, Duration::from_secs(300)).await?;
    eprintln!(
        "Running on http://127.0.0.1:{} (pid {}).",
        rec.port, rec.pid
    );

    // 6. test it
    eprintln!("\nRunning the self-test...");
    let cfg = crate::draft::Config::from_env_and(&st)?;
    let report = selftest::run(&cfg, &selftest::Opts { tools: false }).await?;
    eprint!("{}", report.render());
    if selftest::record(data, &entry.id, &report)? {
        eprintln!(
            "Recorded for {} in {}.",
            entry.id,
            data.join("ai-verified.json").display()
        );
    }
    if report.exit_code() == 3 {
        eprintln!("This model is too weak to be a good helper. Try a bigger one: ndcart ai setup --model <id>.");
    } else if report.exit_code() == 0 {
        eprintln!("Done. Open the studio and try \u{2728} From a description.");
    }
    Ok(report.exit_code())
}
