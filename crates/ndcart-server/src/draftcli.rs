//! `ndcart draft`: the command line for drafting a spec, one at a time or from a file of
//! prompts, with no studio running. Scheduling stays the operating system's job (cron, a
//! systemd timer, Task Scheduler: docs/AI_RECIPES.md); this command is what they run.
//!
//! ```text
//! ndcart draft PROMPT [--kind map|world] [--seed N] [--template] [--out FILE]
//! ndcart draft --batch prompts.txt|prompts.jsonl --out-dir DIR [same options]
//! ndcart draft --check FILE [--kind map|world]
//!        where the model is:  --llm-url U --llm-model M   (a model you run)
//!                        or:  --server URL --email E [--world SLUG] [--model NAME]  (nd-world)
//!                        or:  neither, and ND_CARTO_LLM_URL / ND_CARTO_LLM_MODEL (and the key)
//!                             from the environment, or the studio's saved AI settings in
//!                             --data-dir
//!        and, to keep the result:  --accept-into PROJECT [--create] --data-dir D
//! ```
//!
//! `--template` drafts without any model (the deterministic template draft). `--check` runs
//! the studio's spec check on a file with no model. `--accept-into` adds the drafted floors to
//! a project folder directly; it refuses while a studio is running on the same data folder
//! (see `datalock`).

use crate::draft::{self, Config, Context};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SpecKind {
    Map,
    World,
}

impl SpecKind {
    fn name(self) -> &'static str {
        match self {
            SpecKind::Map => "map",
            SpecKind::World => "world",
        }
    }
}

#[derive(Default, Debug)]
struct Args {
    prompt: Option<String>,
    kind: Option<String>,
    seed: Option<u64>,
    template: bool,
    out: Option<String>,
    batch: Option<String>,
    out_dir: Option<String>,
    check: Option<String>,
    accept_into: Option<String>,
    create: bool,
    data_dir: Option<String>,
    server: Option<String>,
    nd_world: Option<String>,
    email: Option<String>,
    model: Option<String>,
    llm_url: Option<String>,
    llm_model: Option<String>,
}

fn parse(args: Vec<String>) -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = args.into_iter();
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match x.as_str() {
            "--kind" => a.kind = Some(val("--kind")?),
            "--seed" => {
                a.seed = Some(
                    val("--seed")?
                        .parse()
                        .map_err(|_| "--seed must be a whole number".to_string())?,
                )
            }
            "--template" => a.template = true,
            "--out" => a.out = Some(val("--out")?),
            "--batch" => a.batch = Some(val("--batch")?),
            "--out-dir" => a.out_dir = Some(val("--out-dir")?),
            "--check" => a.check = Some(val("--check")?),
            "--accept-into" => a.accept_into = Some(val("--accept-into")?),
            "--create" => a.create = true,
            "--data-dir" => a.data_dir = Some(val("--data-dir")?),
            "--server" => a.server = Some(val("--server")?),
            // The nd-world slug, as before; `--kind world` asks for a world map spec.
            "--world" => a.nd_world = Some(val("--world")?),
            "--email" => a.email = Some(val("--email")?),
            "--model" => a.model = Some(val("--model")?),
            "--llm-url" => a.llm_url = Some(val("--llm-url")?),
            "--llm-model" => a.llm_model = Some(val("--llm-model")?),
            o if !o.starts_with("--") && a.prompt.is_none() => a.prompt = Some(o.to_string()),
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    Ok(a)
}

fn kind_of(s: Option<&str>) -> Result<SpecKind, String> {
    match s {
        None | Some("map") => Ok(SpecKind::Map),
        Some("world") => Ok(SpecKind::World),
        Some(o) => Err(format!("--kind must be map or world, not '{o}'")),
    }
}

/// The model to ask: flags first, else the environment and the saved AI settings.
fn config(a: &Args) -> Result<Config, String> {
    let direct = match (&a.llm_url, &a.llm_model) {
        (Some(u), Some(m)) => Some((u.clone(), m.clone())),
        (None, None) => None,
        _ => return Err("--llm-url and --llm-model go together".into()),
    };
    if direct.is_none() && a.server.is_none() {
        let stored = crate::ai_settings::load(a.data_dir.as_deref().map(Path::new));
        return Config::from_env_and(&stored).map_err(|e| {
            format!(
                "{e} (or pass --llm-url and --llm-model, or --server and --email; --template needs no model)"
            )
        });
    }
    if direct.is_none() && a.email.is_none() {
        return Err("--server needs --email".into());
    }
    let key = std::env::var("ND_CARTO_LLM_KEY").ok().or_else(|| {
        std::env::var("ND_CARTO_LLM_KEY_FILE")
            .ok()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|k| k.trim().to_string())
    });
    Ok(Config {
        server: a.server.clone(),
        world: a.nd_world.clone(),
        email: a.email.clone(),
        model: a.model.clone(),
        direct,
        key,
        thinking: false,
        cache: Default::default(),
        stepwise: false,
    })
}

/// One thing to draft.
#[derive(Debug, PartialEq)]
struct Item {
    prompt: String,
    /// A file-name hint (`.jsonl` only).
    name: Option<String>,
    kind: Option<SpecKind>,
}

/// Read a batch file: one prompt per line in a text file (blank lines and `#` comments are
/// skipped); in a `.jsonl` file each line is a JSON string or `{"prompt", "name"?, "kind"?}`.
fn read_batch(path: &str) -> Result<Vec<Item>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let jsonl = path.to_lowercase().ends_with(".jsonl");
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || (!jsonl && line.starts_with('#')) {
            continue;
        }
        if !jsonl {
            out.push(Item {
                prompt: line.to_string(),
                name: None,
                kind: None,
            });
            continue;
        }
        let at = |m: String| format!("{path} line {}: {m}", i + 1);
        let v: Value = serde_json::from_str(line).map_err(|e| at(e.to_string()))?;
        out.push(match v {
            Value::String(p) => Item {
                prompt: p,
                name: None,
                kind: None,
            },
            Value::Object(o) => Item {
                prompt: o
                    .get("prompt")
                    .and_then(Value::as_str)
                    .ok_or_else(|| at("an object needs a \"prompt\"".into()))?
                    .to_string(),
                name: o.get("name").and_then(Value::as_str).map(str::to_string),
                kind: match o.get("kind").and_then(Value::as_str) {
                    None => None,
                    Some(k) => Some(kind_of(Some(k)).map_err(at)?),
                },
            },
            _ => return Err(at("expected a string or an object".into())),
        });
    }
    if out.is_empty() {
        return Err(format!("{path} holds no prompts"));
    }
    Ok(out)
}

/// `Crypt of the Drowned King!` -> `crypt-of-the-drowned-king` (at most 40 characters).
fn slug(s: &str) -> String {
    let mut out = String::new();
    for w in s
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(w);
        if out.len() >= 40 {
            out.truncate(40);
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "map".into()
    } else {
        out
    }
}

/// A drafted spec and what came with it.
struct Made {
    spec: Value,
    notes: Vec<String>,
    attempts: usize,
}

async fn make(
    cfg: Option<&Config>,
    prompt: &str,
    kind: SpecKind,
    seed: u64,
) -> Result<Made, String> {
    let ctx = Context::default();
    match (cfg, kind) {
        (None, SpecKind::Map) => {
            let spec = ndcart_core::template::template_from_prompt(prompt, seed);
            let notes = notes_of(&spec);
            Ok(Made {
                spec: json(&spec),
                notes,
                attempts: 0,
            })
        }
        (None, SpecKind::World) => {
            Err("--template makes map specs only; a world map needs a model".into())
        }
        (Some(cfg), SpecKind::Map) => {
            let d = draft::draft(cfg, prompt, &ctx).await?;
            let notes = d
                .warnings
                .iter()
                .map(|w| format!("{}: {}", w.floor, w.message))
                .collect();
            Ok(Made {
                spec: json(&d.spec),
                notes,
                attempts: d.attempts,
            })
        }
        (Some(cfg), SpecKind::World) => {
            let (spec, attempts) = draft::draft_world_with(cfg, prompt, &ctx, &|_| {}).await?;
            Ok(Made {
                spec: json(&spec),
                notes: Vec::new(),
                attempts,
            })
        }
    }
}

fn json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).expect("a spec serialises")
}

fn notes_of(spec: &ndcart_core::mapspec::MapSpec) -> Vec<String> {
    draft::warnings_of(spec)
        .iter()
        .map(|w| format!("{}: {}", w.floor, w.message))
        .collect()
}

// ---- --accept-into ------------------------------------------------------------------------

/// The data folder, held exclusively, with the projects loaded: what `--accept-into` writes to.
struct Target {
    dir: PathBuf,
    store: crate::store::Store,
    doc_id: String,
    // Dropped last: the lock outlives the writes.
    _lock: crate::datalock::Held,
}

impl Target {
    fn open(a: &Args, kind: SpecKind, project: &str) -> Result<Target, String> {
        let dir = a
            .data_dir
            .clone()
            .or_else(|| {
                std::env::var("ND_CARTO_DATA_DIR")
                    .ok()
                    .filter(|v| !v.is_empty())
            })
            .map(PathBuf::from)
            .ok_or("--accept-into needs --data-dir (or ND_CARTO_DATA_DIR)")?;
        let lock = match crate::datalock::take(&dir) {
            crate::datalock::Taken::Held(h) => h,
            crate::datalock::Taken::Busy => {
                return Err(format!(
                "a studio is running on {} (or another ndcart is writing to it). Stop it first, \
                     or draft to a file and use the studio's From a spec dialog",
                dir.display()
            ))
            }
            crate::datalock::Taken::Unavailable(why) => return Err(why),
        };
        let mut store = crate::store::Store::default();
        store
            .load_dir(&dir)
            .map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let by_id = store
            .docs
            .contains_key(project)
            .then(|| project.to_string());
        let named: Vec<String> = store
            .docs
            .values()
            .filter(|d| d.manifest.name.eq_ignore_ascii_case(project))
            .map(|d| d.manifest.id.clone())
            .collect();
        let doc_id = match (by_id, named.as_slice()) {
            (Some(id), _) => id,
            (None, [one]) => one.clone(),
            (None, []) if a.create => {
                let k = match kind {
                    SpecKind::Map => ndcart_core::DocKind::Battle,
                    SpecKind::World => ndcart_core::DocKind::Region,
                };
                store.create(project, k).id.clone()
            }
            (None, []) => {
                return Err(format!(
                    "no project '{project}' in {} (give its id or exact name, or add --create)",
                    dir.display()
                ))
            }
            _ => {
                return Err(format!(
                    "{} projects are named '{project}'; use the id of one",
                    named.len()
                ))
            }
        };
        Ok(Target {
            dir,
            store,
            doc_id,
            _lock: lock,
        })
    }

    /// Add one drafted spec to the project the way the studio's Accept does. Returns the new
    /// floor ids (a world map has none: it redraws the first floor).
    fn accept(&mut self, spec: &Value, kind: SpecKind, seed: u64) -> Result<Vec<String>, String> {
        let doc = self
            .store
            .docs
            .get_mut(&self.doc_id)
            .ok_or("the project went missing")?;
        let ids = match kind {
            SpecKind::Map => {
                if doc.manifest.kind == ndcart_core::DocKind::Region {
                    return Err("a map spec goes in a battle-map project, not a world map".into());
                }
                let spec: ndcart_core::mapspec::MapSpec =
                    serde_json::from_value(spec.clone()).map_err(|e| e.to_string())?;
                let pid = doc
                    .propose(spec, seed)
                    .map_err(|e| e.to_string())?
                    .id
                    .clone();
                doc.accept_proposal(&pid).map_err(|e| e.to_string())?
            }
            SpecKind::World => {
                if doc.manifest.kind != ndcart_core::DocKind::Region {
                    return Err("a world map goes in a world-map project".into());
                }
                let spec: crate::worldspec::WorldSpec =
                    serde_json::from_value(spec.clone()).map_err(|e| e.to_string())?;
                let old: Vec<String> = doc.floors[0].rivers.iter().map(|r| r.id.clone()).collect();
                let ops = crate::worldspec::compile(&spec, seed as u32, &old)
                    .map_err(|e| e.to_string())?;
                let pid = ndcart_core::ids::new_id("pw");
                doc.world_proposals.push(crate::store::WorldProposal {
                    id: pid.clone(),
                    ops,
                });
                doc.accept_world(&pid).map_err(|e| e.to_string())?;
                Vec::new()
            }
        };
        crate::store::save_doc(Some(&self.dir), &self.store.docs[&self.doc_id]);
        Ok(ids)
    }

    fn describe(&self) -> String {
        let m = &self.store.docs[&self.doc_id].manifest;
        format!("'{}' ({})", m.name, m.id)
    }
}

// ---- the command ---------------------------------------------------------------------------

fn print_check(v: &Value) -> bool {
    for e in v["errors"].as_array().into_iter().flatten() {
        println!("error: {}", e.as_str().unwrap_or_default());
    }
    for p in v["problems"].as_array().into_iter().flatten() {
        println!(
            "{} ({}): {}",
            p["severity"].as_str().unwrap_or("note").to_lowercase(),
            p["floor"].as_str().unwrap_or(""),
            p["message"].as_str().unwrap_or_default()
        );
    }
    let ok = v["ok"].as_bool().unwrap_or(false);
    if ok {
        println!("ok: a valid {} spec", v["kind"].as_str().unwrap_or("map"));
    }
    ok
}

pub async fn run(args: Vec<String>) -> Result<(), String> {
    let a = parse(args)?;
    let kind = kind_of(a.kind.as_deref())?;
    let seed = a.seed.unwrap_or(1);

    // No model: check a file the way the studio does.
    if let Some(f) = &a.check {
        let text = std::fs::read_to_string(f).map_err(|e| format!("cannot read {f}: {e}"))?;
        let v = crate::speccheck::check_value(&Value::String(text), a.kind.as_deref())?;
        return if print_check(&v) {
            Ok(())
        } else {
            Err(format!("{f} is not a usable spec"))
        };
    }

    let items = match (&a.batch, &a.prompt) {
        (Some(_), Some(_)) => return Err("give either a prompt or --batch, not both".into()),
        (Some(f), None) => read_batch(f)?,
        (None, Some(p)) => vec![Item {
            prompt: p.clone(),
            name: None,
            kind: None,
        }],
        (None, None) => {
            return Err("describe the map, in quotes (or use --batch FILE or --check FILE)".into())
        }
    };
    if a.batch.is_some() && a.out_dir.is_none() {
        return Err("--batch needs --out-dir DIR".into());
    }
    if a.batch.is_some() && a.out.is_some() {
        return Err("--batch writes into --out-dir; --out is for a single prompt".into());
    }
    let cfg = if a.template { None } else { Some(config(&a)?) };
    // Take the data folder before asking the model anything: a busy studio is known at once.
    let mut target = match &a.accept_into {
        Some(p) => Some(Target::open(&a, kind, p)?),
        None => None,
    };

    let mut report: Vec<Value> = Vec::new();
    let (mut ok, mut failed) = (0usize, 0usize);
    let total = items.len();
    for (i, item) in items.iter().enumerate() {
        let k = item.kind.unwrap_or(kind);
        let n = i + 1;
        let file_name = format!(
            "{n:03}-{}.json",
            slug(item.name.as_deref().unwrap_or(&item.prompt))
        );
        let mut entry = json!({"index": n, "prompt": item.prompt, "kind": k.name()});
        let result = async {
            let made = make(cfg.as_ref(), &item.prompt, k, seed).await?;
            let floors = match target.as_mut() {
                Some(t) => Some(t.accept(&made.spec, k, seed)?),
                None => None,
            };
            Ok::<_, String>((made, floors))
        }
        .await;
        match result {
            Ok((made, floors)) => {
                ok += 1;
                entry["status"] = json!("ok");
                entry["attempts"] = json!(made.attempts);
                entry["notes"] = json!(made.notes);
                let text = serde_json::to_string_pretty(&made.spec).map_err(|e| e.to_string())?;
                if a.batch.is_some() {
                    let dir = a.out_dir.as_deref().expect("checked above");
                    std::fs::create_dir_all(dir)
                        .map_err(|e| format!("cannot create {dir}: {e}"))?;
                    let path = Path::new(dir).join(&file_name);
                    std::fs::write(&path, &text)
                        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                    entry["file"] = json!(file_name);
                    eprintln!("{n}/{total} ok: {file_name}");
                } else {
                    for w in &made.notes {
                        eprintln!("note ({w})");
                    }
                    match &a.out {
                        Some(f) => {
                            std::fs::write(f, &text)
                                .map_err(|e| format!("cannot write {f}: {e}"))?;
                            println!(
                                "wrote {f}; paste it into the studio's \u{2728} From a spec dialog"
                            );
                        }
                        None => println!("{text}"),
                    }
                }
                if let Some(ids) = floors {
                    entry["accepted_floors"] = json!(ids);
                    eprintln!(
                        "added to {}{}",
                        target.as_ref().map(Target::describe).unwrap_or_default(),
                        if ids.is_empty() {
                            String::new()
                        } else {
                            format!(": {} floor(s)", ids.len())
                        }
                    );
                }
            }
            Err(e) => {
                failed += 1;
                entry["status"] = json!("failed");
                entry["error"] = json!(e);
                eprintln!("{n}/{total} FAILED: {e}");
            }
        }
        report.push(entry);
    }

    if a.batch.is_some() {
        let dir = a.out_dir.as_deref().expect("checked above");
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {dir}: {e}"))?;
        let path = Path::new(dir).join("report.json");
        let body = serde_json::to_string_pretty(&json!({
            "total": total, "ok": ok, "failed": failed,
            "accepted_into": target.as_ref().map(Target::describe),
            "items": report,
        }))
        .map_err(|e| e.to_string())?;
        std::fs::write(&path, body).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        println!("{ok} of {total} drafted; report in {}", path.display());
        if failed > 0 {
            return Err(format!(
                "{failed} of {total} prompts failed (see the report)"
            ));
        }
    } else if failed > 0 {
        return Err(report[0]["error"].as_str().unwrap_or("failed").to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ndcart-draftcli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_become_short_file_names() {
        assert_eq!(
            slug("Crypt of the Drowned King!"),
            "crypt-of-the-drowned-king"
        );
        assert_eq!(slug("  ***  "), "map");
        assert!(slug(&"word ".repeat(30)).len() <= 40);
        assert!(!slug(&"word ".repeat(30)).ends_with('-'));
    }

    #[test]
    fn a_batch_file_is_text_lines_or_json_lines() {
        let d = tmp("batch");
        let t = d.join("p.txt");
        std::fs::write(&t, "# nightly\n\na crypt\n  a tavern  \n").unwrap();
        let got = read_batch(t.to_str().unwrap()).unwrap();
        assert_eq!(
            got.iter().map(|i| i.prompt.as_str()).collect::<Vec<_>>(),
            ["a crypt", "a tavern"]
        );
        let j = d.join("p.jsonl");
        std::fs::write(
            &j,
            "\"a keep\"\n{\"prompt\":\"an isle\",\"name\":\"Isle\",\"kind\":\"world\"}\n",
        )
        .unwrap();
        let got = read_batch(j.to_str().unwrap()).unwrap();
        assert_eq!(got[1].kind, Some(SpecKind::World));
        assert_eq!(got[1].name.as_deref(), Some("Isle"));
        std::fs::write(&j, "{\"nope\":1}\n").unwrap();
        assert!(read_batch(j.to_str().unwrap())
            .unwrap_err()
            .contains("line 1"));
        std::fs::write(&t, "# only a comment\n").unwrap();
        assert!(read_batch(t.to_str().unwrap())
            .unwrap_err()
            .contains("no prompts"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn flags_are_parsed_and_mistakes_named() {
        let a = parse(
            ["a crypt", "--seed", "7", "--template", "--kind", "world"]
                .map(String::from)
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            (a.seed, a.template, a.prompt.as_deref()),
            (Some(7), true, Some("a crypt"))
        );
        assert!(parse(vec!["--seed".into(), "x".into()]).is_err());
        assert!(parse(vec!["--bogus".into()])
            .unwrap_err()
            .contains("--bogus"));
        assert!(kind_of(Some("dungeon")).is_err());
        assert!(config(&Args {
            llm_url: Some("u".into()),
            ..Default::default()
        })
        .err()
        .unwrap()
        .contains("go together"));
    }

    #[tokio::test]
    async fn accepting_adds_floors_to_a_saved_project_and_a_second_taker_is_refused() {
        let d = tmp("accept");
        // A project on disk, as the studio would have saved it.
        let mut store = crate::store::Store::default();
        let id = store
            .create("Nightly", ndcart_core::DocKind::Battle)
            .id
            .clone();
        crate::store::save_doc(Some(&d), &store.docs[&id]);
        let args = |extra: &[&str]| {
            let mut v = vec![
                "a crypt with a secret door".to_string(),
                "--template".into(),
            ];
            v.extend(["--data-dir".into(), d.to_str().unwrap().to_string()]);
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };
        run(args(&["--accept-into", "nightly"])).await.unwrap();
        run(args(&["--accept-into", &id, "--seed", "2"]))
            .await
            .unwrap();
        let mut back = crate::store::Store::default();
        back.load_dir(&d).unwrap();
        let before = back.docs[&id].floors.len();
        assert!(
            before >= 3,
            "two drafts of one floor each on top of the first: {before}"
        );
        // A studio on the folder: refused, nothing changes.
        let held = crate::datalock::take(&d);
        assert!(matches!(held, crate::datalock::Taken::Held(_)));
        let e = run(args(&["--accept-into", "nightly"])).await.unwrap_err();
        assert!(e.contains("a studio is running"), "{e}");
        drop(held);
        // An unknown project is refused unless --create is given.
        let e = run(args(&["--accept-into", "Other"])).await.unwrap_err();
        assert!(e.contains("no project 'Other'"), "{e}");
        run(args(&["--accept-into", "Other", "--create"]))
            .await
            .unwrap();
        let mut back = crate::store::Store::default();
        back.load_dir(&d).unwrap();
        assert_eq!(back.docs.len(), 2);
        assert_eq!(back.docs[&id].floors.len(), before);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn a_template_batch_writes_files_and_a_report_and_a_world_template_fails_alone() {
        let d = tmp("tpl");
        let prompts = d.join("p.jsonl");
        std::fs::write(
            &prompts,
            "\"a crypt\"\n{\"prompt\":\"an isle\",\"kind\":\"world\"}\n\"a tavern, 2 floors\"\n",
        )
        .unwrap();
        let out = d.join("out");
        let e = run([
            "--batch",
            prompts.to_str().unwrap(),
            "--out-dir",
            out.to_str().unwrap(),
            "--template",
        ]
        .map(String::from)
        .to_vec())
        .await
        .unwrap_err();
        assert!(e.contains("1 of 3 prompts failed"), "{e}");
        let rep: Value =
            serde_json::from_str(&std::fs::read_to_string(out.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(
            (rep["ok"].clone(), rep["failed"].clone()),
            (json!(2), json!(1))
        );
        assert_eq!(rep["items"][1]["status"], "failed");
        assert!(out.join("001-a-crypt.json").is_file());
        assert!(out.join("003-a-tavern-2-floors.json").is_file());
        assert!(!out.join("002-an-isle.json").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
