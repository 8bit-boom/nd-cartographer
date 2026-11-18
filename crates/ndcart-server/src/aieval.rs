//! `ndcart ai eval`: a scorecard for a model. A fixed set of golden prompts (single-floor and
//! multi-floor maps, world maps, floor revisions and typed edits) goes through the same repair
//! loop the studio uses; each result is scored on whether it was valid, whether it was right
//! on the first ask, what the linter said, and how much of what the request asked for the
//! spec actually holds. Layout is seeded, so the same spec always lays out the same way; the
//! model's own replies are not reproducible, which is why `--repeat` exists.
//!
//! The scoring is pure and tested on canned specs. Running needs an endpoint: in `cargo test`
//! that is the stand-in server (`ndcart ai stand-in`), never a real model.
//!
//! Cases live in `assets/ai/evals/*.toml` (one file per set): see those files for the format.

use crate::draft::{self, Config, Context};
use ndcart_core::editplan::{Edit, EditPlan};
use ndcart_core::lint::Severity;
use ndcart_core::mapspec::{FloorSpec, LinkKind, MapSpec};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// The sets, in the order `--set all` runs them.
const SET_FILES: [(&str, &str); 4] = [
    ("map", include_str!("../assets/ai/evals/map.toml")),
    ("multi", include_str!("../assets/ai/evals/multi.toml")),
    ("world", include_str!("../assets/ai/evals/world.toml")),
    ("edits", include_str!("../assets/ai/evals/edits.toml")),
];

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Map,
    World,
    Refine,
    Edit,
}

/// What a request asked for. Every field is one check; a field left out is not checked.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// The exact number of floors.
    pub floors: Option<usize>,
    /// Rooms over all floors.
    pub rooms_min: Option<usize>,
    pub rooms_max: Option<usize>,
    /// At least one room (map), place or region (world) name contains one of these words.
    #[serde(default)]
    pub must_have_names_any: Vec<String>,
    pub secret_links_min: Option<usize>,
    /// Whether the map has stairs at all.
    pub stairs: Option<bool>,
    pub places_min: Option<usize>,
    pub regions_min: Option<usize>,
    /// Refine only: every pinned room came back unchanged.
    pub pinned_kept: Option<bool>,
    /// Edit only: at least this many edits, and at least one of these ops.
    pub edits_min: Option<usize>,
    #[serde(default)]
    pub edit_ops_any: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub kind: Kind,
    /// The request (for `refine` and `edit`: the change request).
    pub prompt: String,
    /// `refine` and `edit`: the pack example whose first floor is the current floor.
    pub base: Option<String>,
    /// `refine`: room ids that must come back unchanged.
    #[serde(default)]
    pub pins: Vec<String>,
    #[serde(default)]
    pub expect: Expect,
}

#[derive(Deserialize)]
struct SetFile {
    #[serde(default, rename = "case")]
    cases: Vec<Case>,
}

/// The names of the bundled sets.
pub fn set_names() -> Vec<&'static str> {
    SET_FILES.iter().map(|(n, _)| *n).collect()
}

/// The cases of one set, or of every set for `all`.
pub fn cases(set: &str) -> Result<Vec<Case>, String> {
    let mut out = Vec::new();
    for (name, text) in SET_FILES {
        if set == "all" || set == name {
            let f: SetFile = toml::from_str(text).map_err(|e| format!("evals/{name}.toml: {e}"))?;
            out.extend(f.cases);
        }
    }
    if out.is_empty() {
        return Err(format!(
            "unknown set '{set}' (use all, {})",
            set_names().join(", ")
        ));
    }
    Ok(out)
}

// ---- scoring (pure) -------------------------------------------------------------------------

/// One thing a request asked for, and whether the result has it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub passed: bool,
    pub detail: String,
}

fn check(name: &str, passed: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        passed,
        detail: detail.into(),
    }
}

fn has_name(names: &[String], any: &[String]) -> bool {
    names.iter().any(|n| {
        let n = n.to_lowercase();
        any.iter().any(|w| n.contains(&w.to_lowercase()))
    })
}

fn name_check(names: &[String], any: &[String], out: &mut Vec<Check>) {
    if !any.is_empty() {
        out.push(check(
            "names",
            has_name(names, any),
            format!("a name containing one of: {}", any.join(", ")),
        ));
    }
}

fn count_check(
    name: &str,
    have: usize,
    min: Option<usize>,
    max: Option<usize>,
    out: &mut Vec<Check>,
) {
    if let Some(m) = min {
        out.push(check(
            name,
            have >= m,
            format!("{have} (want at least {m})"),
        ));
    }
    if let Some(m) = max {
        out.push(check(
            &format!("{name}_max"),
            have <= m,
            format!("{have} (want at most {m})"),
        ));
    }
}

/// The checks for a drafted map.
pub fn score_map(spec: &MapSpec, e: &Expect) -> Vec<Check> {
    let mut out = Vec::new();
    if let Some(f) = e.floors {
        out.push(check(
            "floors",
            spec.floors.len() == f,
            format!("{} (want {f})", spec.floors.len()),
        ));
    }
    let rooms: usize = spec.floors.iter().map(|f| f.rooms.len()).sum();
    count_check("rooms", rooms, e.rooms_min, e.rooms_max, &mut out);
    let mut names: Vec<String> = spec
        .floors
        .iter()
        .flat_map(|f| f.rooms.iter().map(|r| r.name.clone()))
        .collect();
    names.extend(spec.floors.iter().map(|f| f.name.clone()));
    name_check(&names, &e.must_have_names_any, &mut out);
    if let Some(m) = e.secret_links_min {
        let n = spec
            .floors
            .iter()
            .flat_map(|f| &f.links)
            .filter(|l| l.kind == LinkKind::Secret)
            .count();
        out.push(check(
            "secret_links",
            n >= m,
            format!("{n} (want at least {m})"),
        ));
    }
    if let Some(want) = e.stairs {
        let has = spec.floors.iter().any(|f| !f.stairs.is_empty());
        out.push(check(
            "stairs",
            has == want,
            if has { "has stairs" } else { "no stairs" },
        ));
    }
    out
}

/// The checks for a drafted world.
pub fn score_world(spec: &crate::worldspec::WorldSpec, e: &Expect) -> Vec<Check> {
    let mut out = Vec::new();
    count_check("places", spec.places.len(), e.places_min, None, &mut out);
    count_check("regions", spec.regions.len(), e.regions_min, None, &mut out);
    let names: Vec<String> = spec
        .places
        .iter()
        .map(|p| p.name.clone())
        .chain(spec.regions.iter().map(|r| r.name.clone()))
        .collect();
    name_check(&names, &e.must_have_names_any, &mut out);
    out
}

/// A pinned room is kept when it is still there with the same id, name, size and kind.
pub fn pins_kept(before: &FloorSpec, after: &FloorSpec, pins: &[String]) -> bool {
    pins.iter().all(|id| {
        match (
            before.rooms.iter().find(|r| &r.id == id),
            after.rooms.iter().find(|r| &r.id == id),
        ) {
            (Some(b), Some(a)) => a.name == b.name && a.w == b.w && a.h == b.h && a.kind == b.kind,
            _ => false,
        }
    })
}

/// The checks for a revised floor.
pub fn score_refine(
    before: &FloorSpec,
    pins: &[String],
    after: &MapSpec,
    e: &Expect,
) -> Vec<Check> {
    let mut out = score_map(
        after,
        &Expect {
            floors: None,
            ..e.clone()
        },
    );
    // A pinned room that moved is a failed request, whether or not the case lists the check.
    if e.pinned_kept.is_some() || !pins.is_empty() {
        let kept = after
            .floors
            .first()
            .is_some_and(|f| pins_kept(before, f, pins));
        out.push(check(
            "pinned_kept",
            kept == e.pinned_kept.unwrap_or(true),
            if kept {
                "every pinned room came back unchanged"
            } else {
                "a pinned room changed or went missing"
            },
        ));
    }
    out
}

fn op_name(e: &Edit) -> &'static str {
    match e {
        Edit::RenameRoom { .. } => "rename_room",
        Edit::SetLit { .. } => "set_lit",
        Edit::SetLink { .. } => "set_link",
        Edit::SetMaterial { .. } => "set_material",
        Edit::Furnish { .. } => "furnish",
        Edit::Note { .. } => "note",
    }
}

/// The checks for an edit plan.
pub fn score_edit(plan: &EditPlan, e: &Expect) -> Vec<Check> {
    let mut out = Vec::new();
    count_check("edits", plan.edits.len(), e.edits_min, None, &mut out);
    if !e.edit_ops_any.is_empty() {
        out.push(check(
            "edit_ops",
            plan.edits
                .iter()
                .any(|x| e.edit_ops_any.iter().any(|o| o == op_name(x))),
            format!("one of: {}", e.edit_ops_any.join(", ")),
        ));
    }
    out
}

/// The result of one run of one case.
#[derive(Serialize, Clone, Debug)]
pub struct CaseScore {
    pub id: String,
    pub kind: Kind,
    /// Which repeat this was (1-based).
    pub run: usize,
    /// The reply became a spec the studio accepts (after any repairs).
    pub valid: bool,
    /// Valid without a repair round.
    pub first_try: bool,
    pub checks_passed: usize,
    pub checks_total: usize,
    pub failed_checks: Vec<String>,
    /// Linter findings that are not just information.
    pub lint_warnings: usize,
    /// Asks to the model (None when the loop gave up before reporting).
    pub attempts: Option<usize>,
    pub secs: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Assemble a score from the checks (a run that never became valid scores no checks).
pub fn case_score(
    case: &Case,
    run: usize,
    outcome: Result<(Vec<Check>, usize, usize, bool), String>,
    secs: f64,
) -> CaseScore {
    match outcome {
        Ok((checks, lint_warnings, attempts, first_try)) => CaseScore {
            id: case.id.clone(),
            kind: case.kind,
            run,
            valid: true,
            first_try,
            checks_passed: checks.iter().filter(|c| c.passed).count(),
            checks_total: checks.len(),
            failed_checks: checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| format!("{}: {}", c.name, c.detail))
                .collect(),
            lint_warnings,
            attempts: Some(attempts),
            secs,
            error: None,
        },
        Err(e) => CaseScore {
            id: case.id.clone(),
            kind: case.kind,
            run,
            valid: false,
            first_try: false,
            checks_passed: 0,
            checks_total: expected_checks(&case.expect, case.kind, &case.pins),
            failed_checks: Vec::new(),
            lint_warnings: 0,
            attempts: None,
            secs,
            error: Some(e),
        },
    }
}

/// How many checks a case holds (what a run that never became valid failed to meet).
fn expected_checks(e: &Expect, kind: Kind, pins: &[String]) -> usize {
    let mut n = 0;
    n += usize::from(e.floors.is_some() && kind == Kind::Map);
    n += usize::from(e.rooms_min.is_some()) + usize::from(e.rooms_max.is_some());
    n += usize::from(!e.must_have_names_any.is_empty());
    n += usize::from(e.secret_links_min.is_some());
    n += usize::from(e.stairs.is_some());
    n += usize::from(e.places_min.is_some()) + usize::from(e.regions_min.is_some());
    n += usize::from(kind == Kind::Refine && (e.pinned_kept.is_some() || !pins.is_empty()));
    n += usize::from(e.edits_min.is_some());
    n += usize::from(!e.edit_ops_any.is_empty());
    n
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Summary {
    pub runs: usize,
    pub valid_pct: f64,
    pub first_try_pct: f64,
    /// Mean share of the requested features present; a run that was not valid counts as 0.
    pub mean_checks_pct: f64,
    pub median_secs: f64,
    pub mean_lint_warnings: f64,
    pub mean_attempts: f64,
    /// From one short probe request before the run (the cases do not report tokens).
    pub tokens_per_second: Option<f64>,
}

fn pct(n: usize, of: usize) -> f64 {
    if of == 0 {
        0.0
    } else {
        (n as f64 / of as f64 * 1000.0).round() / 10.0
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

pub fn summarize(scores: &[CaseScore], tokens_per_second: Option<f64>) -> Summary {
    let runs = scores.len();
    let valid: Vec<&CaseScore> = scores.iter().filter(|s| s.valid).collect();
    let mean_checks = if runs == 0 {
        0.0
    } else {
        scores
            .iter()
            .map(|s| {
                if !s.valid {
                    0.0
                } else if s.checks_total == 0 {
                    1.0
                } else {
                    s.checks_passed as f64 / s.checks_total as f64
                }
            })
            .sum::<f64>()
            / runs as f64
            * 100.0
    };
    let mut secs: Vec<f64> = scores.iter().map(|s| s.secs).collect();
    secs.sort_by(|a, b| a.total_cmp(b));
    let median = match secs.len() {
        0 => 0.0,
        n if n % 2 == 1 => secs[n / 2],
        n => (secs[n / 2 - 1] + secs[n / 2]) / 2.0,
    };
    let mean = |f: &dyn Fn(&CaseScore) -> f64| {
        if valid.is_empty() {
            0.0
        } else {
            valid.iter().map(|s| f(s)).sum::<f64>() / valid.len() as f64
        }
    };
    Summary {
        runs,
        valid_pct: pct(valid.len(), runs),
        first_try_pct: pct(scores.iter().filter(|s| s.first_try).count(), runs),
        mean_checks_pct: (mean_checks * 10.0).round() / 10.0,
        median_secs: round2(median),
        mean_lint_warnings: round2(mean(&|s| s.lint_warnings as f64)),
        mean_attempts: round2(mean(&|s| s.attempts.unwrap_or(0) as f64)),
        tokens_per_second: tokens_per_second.map(round2),
    }
}

/// `good`, `usable` or `too weak`, the same words `ndcart ai test` uses. Good: every run valid
/// and at least 80% right on the first ask. Usable: at least 80% valid. Anything less is too
/// weak.
pub fn verdict(s: &Summary) -> &'static str {
    if s.runs == 0 {
        "error"
    } else if s.valid_pct >= 100.0 && s.first_try_pct >= 80.0 {
        "good"
    } else if s.valid_pct >= 80.0 {
        "usable"
    } else {
        "too weak"
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Report {
    pub ndcart: &'static str,
    pub pack: &'static str,
    pub date: String,
    pub endpoint: String,
    pub model: String,
    pub set: String,
    pub repeat: usize,
    pub verdict: &'static str,
    pub summary: Summary,
    pub cases: Vec<CaseScore>,
}

impl Report {
    /// 0 for good and usable, 3 for too weak, 1 when nothing ran.
    pub fn exit_code(&self) -> i32 {
        match self.verdict {
            "good" | "usable" => 0,
            "too weak" => 3,
            _ => 1,
        }
    }

    fn headline(&self) -> String {
        let s = &self.summary;
        let speed = s
            .tokens_per_second
            .map(|t| format!(", about {t:.0} tokens/s"))
            .unwrap_or_default();
        format!(
            "Valid {:.0}%, first try {:.0}%, requested features present {:.0}%, median {:.1}s, \
             {:.1} lint warning(s) and {:.1} ask(s) per valid draft{speed}.",
            s.valid_pct,
            s.first_try_pct,
            s.mean_checks_pct,
            s.median_secs,
            s.mean_lint_warnings,
            s.mean_attempts
        )
    }

    pub fn render(&self) -> String {
        let mut out = format!(
            "Evaluating {} (model {}): set {}, {} run(s) of each case\n\n",
            self.endpoint, self.model, self.set, self.repeat
        );
        out.push_str(&format!(
            "{:<18} {:<7} {:<6} {:<6} {:<7} {:<5} {:<5} {:>6}\n",
            "case", "kind", "valid", "first", "checks", "lint", "asks", "secs"
        ));
        for c in &self.cases {
            out.push_str(&format!(
                "{:<18} {:<7} {:<6} {:<6} {:<7} {:<5} {:<5} {:>6.1}\n",
                c.id,
                format!("{:?}", c.kind).to_lowercase(),
                if c.valid { "yes" } else { "NO" },
                if c.first_try { "yes" } else { "no" },
                if c.valid {
                    format!("{}/{}", c.checks_passed, c.checks_total)
                } else {
                    "-".into()
                },
                c.lint_warnings,
                c.attempts.map_or("-".into(), |a| a.to_string()),
                c.secs
            ));
            for f in &c.failed_checks {
                out.push_str(&format!("{:<18}   missing {f}\n", ""));
            }
            if let Some(e) = &c.error {
                let e: String = e.chars().take(110).collect();
                out.push_str(&format!("{:<18}   {}\n", "", e.replace('\n', " ")));
            }
        }
        out.push_str(&format!("\n{}\n", self.headline()));
        out.push_str(match self.verdict {
            "good" => "Verdict: GOOD.\n",
            "usable" => {
                "Verdict: USABLE. Some drafts needed repairs or missed part of the request.\n"
            }
            "too weak" => "Verdict: TOO WEAK. Try a bigger model (ndcart ai models).\n",
            _ => "Nothing was run.\n",
        });
        out
    }

    pub fn markdown(&self) -> String {
        let mut out = format!(
            "# ndcart ai eval\n\n{} · model `{}` · endpoint `{}` · set `{}` · {} run(s) of each case · ndcart {} · pack {}\n\n",
            self.date, self.model, self.endpoint, self.set, self.repeat, self.ndcart, self.pack
        );
        out.push_str("| case | kind | valid | first try | checks | lint | asks | secs |\n|---|---|---|---|---|---|---|---|\n");
        for c in &self.cases {
            out.push_str(&format!(
                "| {} | {:?} | {} | {} | {} | {} | {} | {:.1} |\n",
                c.id,
                c.kind,
                if c.valid { "yes" } else { "no" },
                if c.first_try { "yes" } else { "no" },
                if c.valid {
                    format!("{}/{}", c.checks_passed, c.checks_total)
                } else {
                    "-".into()
                },
                c.lint_warnings,
                c.attempts.map_or("-".into(), |a| a.to_string()),
                c.secs
            ));
        }
        out.push_str(&format!(
            "\n{}\n\n**Verdict: {}**\n",
            self.headline(),
            self.verdict.to_uppercase()
        ));
        out
    }
}

// ---- running ---------------------------------------------------------------------------------

pub struct Opts {
    pub set: String,
    /// Run only the cases with these ids (empty: all in the set).
    pub only: Vec<String>,
    pub repeat: usize,
}

fn base_floor(name: &str) -> Result<FloorSpec, String> {
    let file = format!("examples/{name}.json");
    let e = crate::aipack::examples()
        .into_iter()
        .find(|e| e.file == file)
        .ok_or_else(|| format!("no pack example '{name}'"))?;
    let spec: MapSpec = serde_json::from_value(e.spec).map_err(|e| e.to_string())?;
    spec.floors
        .into_iter()
        .next()
        .ok_or_else(|| format!("example '{name}' has no floor"))
}

/// Lay a floor spec out as a painted floor (what an edit plan is asked about).
fn painted(floor: &FloorSpec) -> Result<ndcart_core::doc::FloorDoc, String> {
    let c = ndcart_core::mapspec::compile_floor(floor, 1).map_err(|e| e.to_string())?;
    let mut doc = ndcart_core::doc::FloorDoc::new("eval".into(), c.name.clone());
    for op in &c.ops {
        ndcart_core::apply_op(&mut doc, op).map_err(|e| e.to_string())?;
    }
    Ok(doc)
}

fn warnings(spec: &MapSpec) -> usize {
    draft::warnings_of(spec)
        .iter()
        .filter(|p| p.severity != Severity::Info)
        .count()
}

type Done = (Vec<Check>, usize, usize, bool);

/// Run one case through the real loop.
async fn run_case(cfg: &Config, c: &Case) -> Result<Done, String> {
    let ctx = Context::default();
    match c.kind {
        Kind::Map => {
            let d = draft::draft(cfg, &c.prompt, &ctx).await?;
            let stepped = cfg.stepwise || ndcart_core::template::floors_asked(&c.prompt) > 1;
            let min_asks = if stepped { 1 + d.spec.floors.len() } else { 1 };
            Ok((
                score_map(&d.spec, &c.expect),
                d.warnings
                    .iter()
                    .filter(|p| p.severity != Severity::Info)
                    .count(),
                d.attempts,
                d.attempts <= min_asks,
            ))
        }
        Kind::World => {
            let (spec, attempts) = draft::draft_world_with(cfg, &c.prompt, &ctx, &|_| {}).await?;
            Ok((score_world(&spec, &c.expect), 0, attempts, attempts == 1))
        }
        Kind::Refine => {
            let base = base_floor(c.base.as_deref().ok_or("a refine case needs a base")?)?;
            let (spec, attempts) = draft::refine_with(cfg, &base, &c.pins, &c.prompt, &ctx).await?;
            Ok((
                score_refine(&base, &c.pins, &spec, &c.expect),
                warnings(&spec),
                attempts,
                attempts == 1,
            ))
        }
        Kind::Edit => {
            let base = base_floor(c.base.as_deref().ok_or("an edit case needs a base")?)?;
            let floor = painted(&base)?;
            let (plan, _op, attempts) = draft::edit_plan(cfg, &floor, 32.0, &c.prompt).await?;
            Ok((score_edit(&plan, &c.expect), 0, attempts, attempts == 1))
        }
    }
}

/// One short request to learn the endpoint's speed; an error means it cannot be reached.
async fn probe(cfg: &Config) -> Result<Option<f64>, String> {
    use ndcart_bridge::{ChatOptions, DirectChat, Msg};
    let (url, model) = cfg
        .direct
        .clone()
        .ok_or("the evaluation needs a direct model endpoint (not nd-world)")?;
    let chat = DirectChat::new(&url, &model, cfg.key.clone())
        .map_err(|e| e.to_string())?
        .with_thinking(cfg.thinking);
    let t = Instant::now();
    let r = chat
        .chat_messages(
            &[
                Msg::system("You are a connection test."),
                Msg::user("Reply with the single word OK."),
            ],
            &ChatOptions::default(),
        )
        .await
        .map_err(|e| format!("the model endpoint did not answer: {e}"))?;
    Ok(crate::ai_local::selftest::speed(
        &r,
        t.elapsed().as_secs_f64(),
    ))
}

/// Run the set and score it. `progress` is told about each finished run.
pub async fn run(
    cfg: &Config,
    o: &Opts,
    progress: &(dyn Fn(&CaseScore) + Sync),
) -> Result<Report, String> {
    let (endpoint, model) = cfg
        .direct
        .clone()
        .ok_or("the evaluation needs a direct model endpoint (not nd-world)")?;
    let mut list = cases(&o.set)?;
    if !o.only.is_empty() {
        for id in &o.only {
            if !list.iter().any(|c| &c.id == id) {
                return Err(format!("no case '{id}' in set '{}'", o.set));
            }
        }
        list.retain(|c| o.only.contains(&c.id));
    }
    let tps = probe(cfg).await?;
    let mut scores = Vec::new();
    for run in 1..=o.repeat.max(1) {
        for c in &list {
            let t = Instant::now();
            let out = run_case(cfg, c).await;
            let s = case_score(c, run, out, t.elapsed().as_secs_f64());
            progress(&s);
            scores.push(s);
        }
    }
    let summary = summarize(&scores, tps);
    Ok(Report {
        ndcart: env!("CARGO_PKG_VERSION"),
        pack: crate::aipack::version(),
        date: crate::ai_local::iso_date(crate::ai_local::now_secs()),
        endpoint,
        model,
        set: o.set.clone(),
        repeat: o.repeat.max(1),
        verdict: verdict(&summary),
        summary,
        cases: scores,
    })
}

/// Remember the verdict for a catalog model (the same per-machine record `ndcart ai test`
/// writes), so `ndcart ai models` can say it was evaluated here. Only a full run counts.
pub fn record(data: &std::path::Path, id: &str, r: &Report) -> Result<bool, String> {
    if r.set != "all"
        || !matches!(r.verdict, "good" | "usable" | "too weak")
        || crate::ai_local::catalog::find(&crate::ai_local::catalog::bundled(), id).is_none()
    {
        return Ok(false);
    }
    crate::ai_local::catalog::record(
        data,
        id,
        crate::ai_local::catalog::Record {
            verdict: r.verdict.to_string(),
            at: r.date.clone(),
            tokens_per_second: r.summary.tokens_per_second,
        },
    )?;
    Ok(true)
}

/// `ndcart ai eval [--url U --model M] [--set NAME] [--case ID]... [--repeat N] [--json]
/// [--markdown] [--out FILE] [--record --id CATALOG_ID] [--data-dir D]`.
/// Exit code 0 (good, usable), 3 (too weak), 1 (could not run).
pub async fn run_cli(args: Vec<String>) -> Result<i32, String> {
    let c = crate::ai_local::common(args)?;
    let (mut url, mut model, mut id, mut out_file) = (None, None, None, None);
    let (mut set, mut repeat, mut only) = ("all".to_string(), 1usize, Vec::new());
    let (mut markdown, mut rec) = (false, false);
    let mut it = c.rest.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--url" => url = Some(val("--url")?),
            "--model" => model = Some(val("--model")?),
            "--id" => id = Some(val("--id")?),
            "--set" => set = val("--set")?,
            "--case" => only.push(val("--case")?),
            "--repeat" => {
                repeat = val("--repeat")?
                    .parse()
                    .ok()
                    .filter(|n| (1..=20).contains(n))
                    .ok_or("--repeat must be 1-20")?
            }
            "--out" => out_file = Some(val("--out")?),
            "--markdown" => markdown = true,
            "--record" => rec = true,
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let mut stored = crate::ai_settings::load(Some(&c.data));
    if url.is_some() || model.is_some() {
        stored.direct_url = url.or(stored.direct_url);
        stored.direct_model = model.or(stored.direct_model);
    }
    if rec && id.is_none() {
        // The managed model is the one a catalog id can be remembered against.
        id = stored
            .managed
            .as_ref()
            .filter(|_| stored.direct_url.is_none())
            .and_then(|m| m.model_id.clone());
    }
    let cfg = Config::from_env_and(&stored)?;
    let quiet = c.json;
    let report = run(&cfg, &Opts { set, only, repeat }, &|s| {
        if !quiet {
            eprintln!(
                "{} #{}: {}",
                s.id,
                s.run,
                if s.valid { "valid" } else { "not valid" }
            );
        }
    })
    .await?;
    if let Some(f) = &out_file {
        let body = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
        std::fs::write(f, body).map_err(|e| format!("cannot write {f}: {e}"))?;
    }
    if c.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else if markdown {
        print!("{}", report.markdown());
    } else {
        print!("{}", report.render());
    }
    if rec {
        match id {
            Some(id) => match record(&c.data, &id, &report) {
                Ok(true) if !c.json => eprintln!(
                    "Recorded for '{id}' in {}.",
                    c.data.join("ai-verified.json").display()
                ),
                Ok(false) => eprintln!(
                    "Not recorded: only a full run (--set all) of a catalog model counts ('{id}', set {}).",
                    report.set
                ),
                Err(e) => eprintln!("could not record the result: {e}"),
                _ => {}
            },
            None => eprintln!("Not recorded: pass --id <catalog id> with --record."),
        }
    }
    Ok(report.exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndcart_core::mapspec::{LinkSpec, RoomSpec, StairSpec};

    fn room(id: &str, name: &str) -> RoomSpec {
        RoomSpec {
            id: id.into(),
            name: name.into(),
            kind: ndcart_core::SpaceKind::Room,
            w: 5,
            h: 5,
            lit: true,
        }
    }

    fn crypt() -> MapSpec {
        MapSpec {
            name: "Crypt".into(),
            floors: vec![FloorSpec {
                name: "Ground".into(),
                rooms: vec![room("a", "Nave"), room("b", "Ossuary"), room("c", "Chapel")],
                links: vec![
                    LinkSpec {
                        a: "a".into(),
                        b: "b".into(),
                        kind: LinkKind::Secret,
                    },
                    LinkSpec {
                        a: "a".into(),
                        b: "c".into(),
                        kind: LinkKind::Door,
                    },
                ],
                stairs: vec![],
            }],
        }
    }

    fn all_pass(c: &[Check]) -> bool {
        c.iter().all(|c| c.passed)
    }

    #[test]
    fn the_bundled_sets_parse_have_unique_ids_and_the_self_test_prompts() {
        let all = cases("all").unwrap();
        assert!(all.len() >= 20, "{}", all.len());
        let mut ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "case ids must be unique");
        for kind in [Kind::Map, Kind::World, Kind::Refine, Kind::Edit] {
            assert!(all.iter().any(|c| c.kind == kind), "{kind:?}");
        }
        assert!(all
            .iter()
            .any(|c| c.kind == Kind::Map && c.expect.floors.is_some_and(|f| f > 1)));
        // The quick self-test is the head of this set: same words, same order.
        let singles = cases("map").unwrap();
        for (i, p) in crate::ai_local::selftest::GOLDEN.iter().enumerate() {
            assert_eq!(&singles[i].prompt, p);
        }
        for c in &all {
            assert!(c.prompt.len() > 10 && c.prompt.len() < 400, "{}", c.id);
            if matches!(c.kind, Kind::Refine | Kind::Edit) {
                let base = c.base.as_deref().expect("edits name a base");
                base_floor(base).unwrap();
            }
            // Every case checks something, so a 100% score means something.
            assert!(
                expected_checks(&c.expect, c.kind, &c.pins) > 0,
                "{} has no checks",
                c.id
            );
        }
        assert!(cases("nope").unwrap_err().contains("unknown set"));
    }

    #[test]
    fn a_map_is_scored_on_what_the_request_asked_for() {
        let e = Expect {
            floors: Some(1),
            rooms_min: Some(3),
            rooms_max: Some(6),
            must_have_names_any: vec!["ossuary".into()],
            secret_links_min: Some(1),
            stairs: Some(false),
            ..Default::default()
        };
        let c = score_map(&crypt(), &e);
        assert_eq!(c.len(), 6);
        assert!(all_pass(&c), "{c:?}");
        let e = Expect {
            floors: Some(2),
            rooms_min: Some(9),
            must_have_names_any: vec!["kitchen".into(), "cellar".into()],
            secret_links_min: Some(2),
            stairs: Some(true),
            ..Default::default()
        };
        let c = score_map(&crypt(), &e);
        assert_eq!(c.iter().filter(|c| c.passed).count(), 0, "{c:?}");
        let mut with_stairs = crypt();
        with_stairs.floors[0].stairs.push(StairSpec {
            room: "a".into(),
            to_floor: "Down".into(),
            to_room: None,
            label: None,
        });
        assert!(
            score_map(
                &with_stairs,
                &Expect {
                    stairs: Some(true),
                    ..Default::default()
                }
            )[0]
            .passed
        );
        // Nothing asked, nothing to check.
        assert!(score_map(&crypt(), &Expect::default()).is_empty());
    }

    #[test]
    fn a_world_is_scored_on_places_regions_and_names() {
        let w: crate::worldspec::WorldSpec = serde_json::from_str(
            r#"{"name":"Isles","places":[{"name":"Port Aldan","x":0.4,"y":0.4},{"name":"Lighthouse","x":0.2,"y":0.3}],
                "regions":[{"name":"The Reach","points":[[0.1,0.1],[0.5,0.1],[0.3,0.5]]}]}"#,
        )
        .unwrap();
        let e = Expect {
            places_min: Some(2),
            regions_min: Some(1),
            must_have_names_any: vec!["port".into()],
            ..Default::default()
        };
        assert!(all_pass(&score_world(&w, &e)));
        let e = Expect {
            places_min: Some(3),
            must_have_names_any: vec!["oasis".into()],
            ..Default::default()
        };
        assert_eq!(score_world(&w, &e).iter().filter(|c| c.passed).count(), 0);
    }

    #[test]
    fn a_pinned_room_that_changed_fails_the_refine_even_when_the_case_lists_no_check() {
        let before = crypt().floors.remove(0);
        let mut after = crypt();
        let e = Expect {
            rooms_min: Some(3),
            ..Default::default()
        };
        let pins = vec!["a".to_string()];
        assert!(all_pass(&score_refine(&before, &pins, &after, &e)));
        after.floors[0].rooms[0].w = 9;
        let c = score_refine(&before, &pins, &after, &e);
        assert_eq!(c.last().unwrap().name, "pinned_kept");
        assert!(!c.last().unwrap().passed);
        // No pins and no check: no pin check at all.
        assert_eq!(score_refine(&before, &[], &after, &e).len(), 1);
        assert!(!pins_kept(&before, &after.floors[0], &["zzz".into()]));
    }

    #[test]
    fn an_edit_plan_is_scored_on_count_and_ops() {
        let plan = EditPlan {
            edits: vec![
                Edit::SetLit {
                    room: "a".into(),
                    lit: false,
                },
                Edit::Note {
                    room: "a".into(),
                    text: "t".into(),
                },
            ],
        };
        let e = Expect {
            edits_min: Some(2),
            edit_ops_any: vec!["note".into()],
            ..Default::default()
        };
        assert!(all_pass(&score_edit(&plan, &e)));
        let e = Expect {
            edits_min: Some(3),
            edit_ops_any: vec!["furnish".into()],
            ..Default::default()
        };
        assert_eq!(score_edit(&plan, &e).iter().filter(|c| c.passed).count(), 0);
    }

    fn case(id: &str) -> Case {
        Case {
            id: id.into(),
            kind: Kind::Map,
            prompt: "x".into(),
            base: None,
            pins: vec![],
            expect: Expect {
                rooms_min: Some(1),
                floors: Some(1),
                ..Default::default()
            },
        }
    }

    #[test]
    fn the_summary_counts_a_failed_run_as_nothing_and_the_verdict_follows_the_rates() {
        let ok = |id, checks: Vec<Check>, attempts, first| {
            case_score(&case(id), 1, Ok((checks, 1, attempts, first)), 2.0)
        };
        let pass = || vec![check("a", true, ""), check("b", true, "")];
        let half = || vec![check("a", true, ""), check("b", false, "x")];
        let bad = case_score(&case("c"), 1, Err("no JSON".into()), 4.0);
        assert!(!bad.valid && bad.checks_total == 2 && bad.checks_passed == 0);
        let scores = vec![
            ok("a", pass(), 1, true),
            ok("b", half(), 2, false),
            bad,
            ok("d", pass(), 1, true),
        ];
        let s = summarize(&scores, Some(41.234));
        assert_eq!(s.runs, 4);
        assert_eq!(s.valid_pct, 75.0);
        assert_eq!(s.first_try_pct, 50.0);
        // (1 + 0.5 + 0 + 1) / 4
        assert_eq!(s.mean_checks_pct, 62.5);
        assert_eq!(s.median_secs, 2.0);
        assert_eq!(s.mean_attempts, 1.33);
        assert_eq!(s.mean_lint_warnings, 1.0);
        assert_eq!(s.tokens_per_second, Some(41.23));
        assert_eq!(verdict(&s), "too weak");
        let mk = |valid, first| Summary {
            valid_pct: valid,
            first_try_pct: first,
            ..s.clone()
        };
        assert_eq!(verdict(&mk(100.0, 80.0)), "good");
        assert_eq!(verdict(&mk(100.0, 79.0)), "usable");
        assert_eq!(verdict(&mk(80.0, 0.0)), "usable");
        assert_eq!(verdict(&mk(79.0, 79.0)), "too weak");
        assert_eq!(verdict(&summarize(&[], None)), "error");
        // A run with nothing to check counts as fully covered.
        let none = case_score(&case("z"), 1, Ok((vec![], 0, 1, true)), 1.0);
        assert_eq!(summarize(&[none], None).mean_checks_pct, 100.0);
    }

    /// The JSON report's keys and value types, not its values.
    fn shape(v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value::*;
        match v {
            Object(m) => Object(m.iter().map(|(k, v)| (k.clone(), shape(v))).collect()),
            Array(a) => Array(a.first().map(shape).into_iter().collect()),
            Null => String("null".into()),
            Bool(_) => String("bool".into()),
            Number(_) => String("number".into()),
            String(_) => String("string".into()),
        }
    }

    #[test]
    fn the_json_report_has_the_documented_shape() {
        let good = case_score(
            &case("a"),
            1,
            Ok((vec![check("rooms", true, "3")], 0, 1, true)),
            1.5,
        );
        let summary = summarize(std::slice::from_ref(&good), Some(30.0));
        let r = Report {
            ndcart: "0.0.0",
            pack: "1.0.0",
            date: "2026-11-18".into(),
            endpoint: "http://127.0.0.1:1/v1".into(),
            model: "m".into(),
            set: "all".into(),
            repeat: 1,
            verdict: verdict(&summary),
            summary,
            cases: vec![good],
        };
        let got = shape(&serde_json::to_value(&r).unwrap());
        let want: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/eval-report.json")).unwrap();
        assert_eq!(
            got,
            shape(&want),
            "the report changed: update tests/fixtures/eval-report.json and docs/LOCAL_AI.md"
        );
        let text = r.render();
        assert!(
            text.contains("Verdict: GOOD") && text.contains("a  "),
            "{text}"
        );
        assert!(r.markdown().contains("| a | Map | yes | yes | 1/1 |"));
    }

    #[test]
    fn an_edit_base_lays_out_as_a_painted_floor_with_the_rooms_the_plan_can_name() {
        let floor = painted(&base_floor("map-shop").unwrap()).unwrap();
        let d = ndcart_core::editplan::describe(&floor, 32.0);
        assert!(d["rooms"].as_array().unwrap().len() >= 2, "{d}");
    }

    // ---- end to end against the stand-in (no model, loopback only) ----

    use crate::ai_standin::{self, Behaviour, Options};

    fn cfg_for(h: &ai_standin::Handle) -> Config {
        Config {
            server: None,
            world: None,
            email: None,
            model: None,
            direct: Some((h.base.clone(), "ndcart-local".into())),
            key: None,
            thinking: false,
            cache: Default::default(),
            stepwise: false,
        }
    }

    async fn eval_with(behaviour: Behaviour, set: &str, only: &[&str]) -> Report {
        let h = ai_standin::spawn(Options {
            behaviour,
            ..Options::default()
        })
        .await;
        run(
            &cfg_for(&h),
            &Opts {
                set: set.into(),
                only: only.iter().map(|s| s.to_string()).collect(),
                repeat: 1,
            },
            &|_| {},
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn every_bundled_case_runs_against_a_good_stand_in_and_comes_back_valid() {
        let r = eval_with(Behaviour::Good, "all", &[]).await;
        let failed: Vec<_> = r
            .cases
            .iter()
            .filter(|c| !c.valid)
            .map(|c| (&c.id, &c.error))
            .collect();
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(r.cases.len(), cases("all").unwrap().len());
        assert_eq!(r.summary.valid_pct, 100.0);
        assert_eq!(r.summary.first_try_pct, 100.0);
        assert_eq!(r.verdict, "good");
        assert_eq!(r.exit_code(), 0);
        // The stand-in replays worked examples, so it cannot meet every request (a "kitchen"
        // was asked for and a crypt comes back); the score says so.
        assert!(r.summary.mean_checks_pct < 100.0, "{:?}", r.summary);
        assert!(r.cases.iter().any(|c| !c.failed_checks.is_empty()));
        // Multi-floor drafts take more than one ask and still count as first try.
        let multi = r.cases.iter().find(|c| c.id == "tower-two").unwrap();
        assert!(multi.attempts.unwrap() >= 3 && multi.first_try, "{multi:?}");
    }

    #[tokio::test]
    async fn a_model_that_needs_a_repair_is_valid_but_not_first_try() {
        let r = eval_with(Behaviour::FixOnce, "map", &["shop", "crypt"]).await;
        assert_eq!(r.summary.valid_pct, 100.0);
        assert_eq!(r.summary.first_try_pct, 0.0);
        assert!(r.cases.iter().all(|c| c.attempts == Some(2)));
        assert_eq!(r.verdict, "usable");
        assert_eq!(r.exit_code(), 0);
    }

    #[tokio::test]
    async fn a_model_that_never_answers_with_a_spec_is_too_weak_and_exits_3() {
        let r = eval_with(Behaviour::Garbage, "map", &["shop"]).await;
        assert_eq!(r.summary.valid_pct, 0.0);
        assert_eq!(r.verdict, "too weak");
        assert_eq!(r.exit_code(), 3);
        assert!(r.cases[0].error.is_some() && r.cases[0].attempts.is_none());
    }

    #[tokio::test]
    async fn an_unknown_case_or_an_unreachable_endpoint_is_an_error_not_a_score() {
        let h = ai_standin::spawn(Options::default()).await;
        let e = run(
            &cfg_for(&h),
            &Opts {
                set: "map".into(),
                only: vec!["nope".into()],
                repeat: 1,
            },
            &|_| {},
        )
        .await
        .unwrap_err();
        assert!(e.contains("no case 'nope'"), "{e}");
        let mut cfg = cfg_for(&h);
        cfg.direct = Some(("http://127.0.0.1:9/v1".into(), "m".into()));
        let e = run(
            &cfg,
            &Opts {
                set: "map".into(),
                only: vec![],
                repeat: 1,
            },
            &|_| {},
        )
        .await
        .unwrap_err();
        assert!(e.contains("did not answer"), "{e}");
    }

    #[tokio::test]
    async fn only_a_full_run_of_a_catalog_model_is_recorded_for_this_machine() {
        let dir = std::env::temp_dir().join(format!("ndcart-eval-rec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let id = crate::ai_local::catalog::bundled()[0].id.clone();
        let partial = eval_with(Behaviour::Good, "map", &["shop"]).await;
        assert!(!record(&dir, &id, &partial).unwrap());
        let full = eval_with(Behaviour::Good, "all", &[]).await;
        assert!(!record(&dir, "not-a-model", &full).unwrap());
        assert!(record(&dir, &id, &full).unwrap());
        let rec = crate::ai_local::catalog::load_records(&dir);
        assert_eq!(rec[&id].verdict, "good");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
