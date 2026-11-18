//! `ndcart ai test`: does this model work with this studio? The same steps for any
//! OpenAI-compatible endpoint (the managed llama-server, Ollama, Unsloth Studio, ...):
//! reachability, the model id, a ping, a JSON-schema probe, then three golden drafts through
//! the real validate, lint and repair loop, and an optional tool-call probe.
//!
//! Verdict, from the golden drafts only: **good** (all three valid on the first ask),
//! **usable** (all valid within the repairs) or **too weak** (any failure). The probes tell
//! you why; they do not change the verdict.

use crate::draft::{self, Config, Context};
use ndcart_bridge::{ChatOptions, DirectChat, Msg};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Single-floor requests, so "first try" means one ask. (Batch 5's evaluation suite holds the
/// full prompt set; these three are the quick check.)
pub const GOLDEN: [&str; 3] = [
    "A small shop: a sales floor and a stockroom, linked by a door.",
    "A crypt with a nave, an ossuary behind a secret door, and a side chapel.",
    "A riverside inn: common room, kitchen, cellar and three guest rooms off a corridor.",
];

#[derive(Serialize, Clone, Debug)]
pub struct Step {
    pub name: String,
    /// `pass`, `fail` or `skip`.
    pub status: &'static str,
    pub detail: String,
    pub seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Report {
    pub endpoint: String,
    pub model: String,
    pub steps: Vec<Step>,
    /// `good`, `usable`, `too weak`, or `error` (the endpoint did not answer the basics).
    pub verdict: &'static str,
    pub valid: usize,
    pub first_try: usize,
    pub total: usize,
    pub tokens_per_second: Option<f64>,
    pub seconds: f64,
}

impl Report {
    /// 0 for good and usable, 3 for too weak, 1 when the endpoint could not be tested.
    pub fn exit_code(&self) -> i32 {
        match self.verdict {
            "good" | "usable" => 0,
            "too weak" => 3,
            _ => 1,
        }
    }

    pub fn render(&self) -> String {
        let mut out = format!("Testing {} (model {})\n", self.endpoint, self.model);
        for s in &self.steps {
            let tag = match s.status {
                "pass" => "pass",
                "fail" => "FAIL",
                _ => "skip",
            };
            let extra = match (s.attempts, s.tokens_per_second) {
                (Some(a), _) => format!(", {a} ask(s)"),
                (_, Some(t)) => format!(", {t:.0} tokens/s"),
                _ => String::new(),
            };
            out.push_str(&format!(
                "[{tag}] {} ({:.1}s{extra}): {}\n",
                s.name, s.seconds, s.detail
            ));
        }
        let speed = self
            .tokens_per_second
            .map(|t| format!(", about {t:.0} tokens/s"))
            .unwrap_or_default();
        out.push_str(&match self.verdict {
            "good" => format!("\nVerdict: GOOD. All {} drafts were valid on the first ask{speed}.\n", self.total),
            "usable" => format!(
                "\nVerdict: USABLE. All {} drafts were valid, but {} needed repairs{speed}. A bigger model or a better quant would be steadier.\n",
                self.total,
                self.total - self.first_try
            ),
            "too weak" => format!(
                "\nVerdict: TOO WEAK. {} of {} drafts were valid even with repairs. Try a bigger model (ndcart ai models).\n",
                self.valid, self.total
            ),
            _ => "\nCould not test: the endpoint did not pass the basic checks above.\n".to_string(),
        });
        out
    }
}

pub struct Opts {
    pub tools: bool,
}

fn step(name: &str, status: &'static str, detail: impl Into<String>, t: Instant) -> Step {
    Step {
        name: name.into(),
        status,
        detail: detail.into(),
        seconds: t.elapsed().as_secs_f64(),
        attempts: None,
        tokens_per_second: None,
    }
}

/// Generation speed: llama.cpp's own `timings`, else completion tokens over wall time.
fn speed(reply: &ndcart_bridge::ChatReply, secs: f64) -> Option<f64> {
    if let Some(v) = reply
        .timings
        .as_ref()
        .and_then(|t| t.get("predicted_per_second"))
        .and_then(|v| v.as_f64())
    {
        return Some(v);
    }
    let n = reply.usage.as_ref()?.get("completion_tokens")?.as_f64()?;
    (n > 0.0 && secs > 0.0).then(|| n / secs)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("client")
}

fn short(s: &str) -> String {
    let t: String = s.chars().take(60).collect();
    t.replace('\n', " ")
}

pub async fn run(cfg: &Config, o: &Opts) -> Result<Report, String> {
    let (url, model) = cfg
        .direct
        .clone()
        .ok_or("the self-test needs a direct model endpoint (not nd-world)")?;
    let began = Instant::now();
    let base = url.trim_end_matches('/').to_string();
    let origin = ndcart_bridge::caps::origin_of(&base);
    let http = client();
    let auth = |r: reqwest::RequestBuilder| match &cfg.key {
        Some(k) => r.bearer_auth(k),
        None => r,
    };
    let mut steps: Vec<Step> = Vec::new();
    let mut basics_ok = true;

    // 1. health (llama.cpp: 503 while loading, 200 when ready; other servers may not have it)
    let t = Instant::now();
    steps.push(match http.get(format!("{origin}/health")).send().await {
        Ok(r) if r.status().as_u16() == 200 => step("health", "pass", "ready", t),
        Ok(r) if r.status().as_u16() == 503 => {
            basics_ok = false;
            step(
                "health",
                "fail",
                "the server is still loading the model; wait and run again",
                t,
            )
        }
        Ok(r) => step(
            "health",
            "skip",
            format!("no /health here ({}): not llama.cpp, fine", r.status()),
            t,
        ),
        Err(e) => {
            basics_ok = false;
            step("health", "fail", format!("cannot reach {origin}: {e}"), t)
        }
    });

    // 2. the model id is listed
    let t = Instant::now();
    steps.push(
        match auth(http.get(format!("{base}/models"))).send().await {
            Ok(r) if matches!(r.status().as_u16(), 401 | 403) => {
                basics_ok = false;
                step("model list", "fail", "the server refused the API key", t)
            }
            Ok(r) if r.status().is_success() => {
                let v: serde_json::Value = r.json().await.unwrap_or_default();
                let ids: Vec<String> = v["data"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if ids.is_empty() || ids.contains(&model) {
                    step("model list", "pass", format!("'{model}' is served"), t)
                } else {
                    basics_ok = false;
                    step(
                        "model list",
                        "fail",
                        format!("'{model}' is not served; served: {}", ids.join(", ")),
                        t,
                    )
                }
            }
            Ok(r) => {
                basics_ok = false;
                step(
                    "model list",
                    "fail",
                    format!("the model list answered {}", r.status()),
                    t,
                )
            }
            Err(e) => {
                basics_ok = false;
                step("model list", "fail", e.to_string(), t)
            }
        },
    );

    let chat = DirectChat::new(&url, &model, cfg.key.clone())
        .map_err(|e| e.to_string())?
        .with_thinking(cfg.thinking);
    let mut tps: Option<f64> = None;

    // 3. ping
    if basics_ok {
        let t = Instant::now();
        let msgs = [
            Msg::system("You are a connection test."),
            Msg::user("Reply with the single word OK."),
        ];
        steps.push(
            match chat.chat_messages(&msgs, &ChatOptions::default()).await {
                Ok(r) => {
                    let secs = t.elapsed().as_secs_f64();
                    let mut s = step(
                        "ping",
                        "pass",
                        format!("replied '{}'", short(&r.content)),
                        t,
                    );
                    s.tokens_per_second = speed(&r, secs);
                    tps = tps.or(s.tokens_per_second);
                    s
                }
                Err(e) => {
                    basics_ok = false;
                    step("ping", "fail", e.to_string(), t)
                }
            },
        );
    } else {
        steps.push(step(
            "ping",
            "skip",
            "the endpoint is not ready",
            Instant::now(),
        ));
    }
    if !basics_ok {
        for n in ["JSON schema", "golden 1", "golden 2", "golden 3"] {
            steps.push(step(
                n,
                "skip",
                "the endpoint did not pass the basic checks",
                Instant::now(),
            ));
        }
        return Ok(Report {
            endpoint: url,
            model,
            steps,
            verdict: "error",
            valid: 0,
            first_try: 0,
            total: GOLDEN.len(),
            tokens_per_second: None,
            seconds: began.elapsed().as_secs_f64(),
        });
    }

    // 4. a tiny schema, answered as JSON
    let t = Instant::now();
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"],
        "additionalProperties": false,
    });
    let msgs = [
        Msg::system("Reply with one JSON object only."),
        Msg::user("Reply with {\"ok\": true}."),
    ];
    steps.push(
        match chat
            .chat_structured(&msgs, Some(("probe", &schema)), &ChatOptions::default())
            .await
        {
            Ok(r) => {
                let ok = crate::speccheck::extract_json(&r.content)
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
                    .is_some_and(|v| v["ok"].is_boolean());
                let secs = t.elapsed().as_secs_f64();
                let mut s = if ok {
                    step("JSON schema", "pass", "answered with the JSON asked for", t)
                } else {
                    step(
                        "JSON schema",
                        "fail",
                        format!(
                            "not the JSON asked for: '{}'. Drafts will rely on repairs.",
                            short(&r.content)
                        ),
                        t,
                    )
                };
                s.tokens_per_second = speed(&r, secs);
                tps = tps.or(s.tokens_per_second);
                s
            }
            Err(e) => step("JSON schema", "fail", e.to_string(), t),
        },
    );

    // 5. golden drafts through the real loop
    let (mut valid, mut first_try) = (0usize, 0usize);
    for (i, prompt) in GOLDEN.iter().enumerate() {
        let t = Instant::now();
        let name = format!("golden {}", i + 1);
        match draft::draft(cfg, prompt, &Context::default()).await {
            Ok(d) => {
                valid += 1;
                if d.attempts == 1 {
                    first_try += 1;
                }
                let rooms: usize = d.spec.floors.iter().map(|f| f.rooms.len()).sum();
                let mut s = step(
                    &name,
                    "pass",
                    format!(
                        "a valid spec with {rooms} rooms and {} note(s)",
                        d.warnings.len()
                    ),
                    t,
                );
                s.attempts = Some(d.attempts);
                steps.push(s);
            }
            Err(e) => steps.push(step(&name, "fail", short(&e), t)),
        }
    }

    // 6. optional: tool calling
    if o.tools {
        let t = Instant::now();
        let body = serde_json::json!({
            "model": model, "stream": false,
            "messages": [{"role": "user", "content": "What is the weather in Paris? Use the tool."}],
            "tools": [{"type": "function", "function": {
                "name": "get_weather", "description": "Current weather for a city",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"}}, "required": ["city"]},
            }}],
        });
        let r = auth(http.post(format!("{base}/chat/completions")).json(&body))
            .send()
            .await;
        steps.push(match r {
            Ok(r) if r.status().is_success() => {
                let v: serde_json::Value = r.json().await.unwrap_or_default();
                let calls = v["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .map_or(0, Vec::len);
                if calls > 0 {
                    step(
                        "tool call",
                        "pass",
                        format!("{calls} tool call(s) returned"),
                        t,
                    )
                } else {
                    step(
                        "tool call",
                        "fail",
                        "answered in text, not a tool call (optional: the studio does not need it)",
                        t,
                    )
                }
            }
            Ok(r) => step(
                "tool call",
                "fail",
                format!("answered {} (optional)", r.status()),
                t,
            ),
            Err(e) => step("tool call", "fail", format!("{e} (optional)"), t),
        });
    }

    let verdict = if valid == GOLDEN.len() && first_try == GOLDEN.len() {
        "good"
    } else if valid == GOLDEN.len() {
        "usable"
    } else {
        "too weak"
    };
    Ok(Report {
        endpoint: url,
        model,
        steps,
        verdict,
        valid,
        first_try,
        total: GOLDEN.len(),
        tokens_per_second: tps,
        seconds: began.elapsed().as_secs_f64(),
    })
}

/// Remember the verdict for a catalog model, so `ndcart ai models` can say it was tested here.
pub fn record(data: &std::path::Path, id: &str, r: &Report) -> Result<bool, String> {
    if !matches!(r.verdict, "good" | "usable" | "too weak")
        || super::catalog::find(&super::catalog::bundled(), id).is_none()
    {
        return Ok(false);
    }
    super::catalog::record(
        data,
        id,
        super::catalog::Record {
            verdict: r.verdict.to_string(),
            at: super::iso_date(super::now_secs()),
            tokens_per_second: r.tokens_per_second,
        },
    )?;
    Ok(true)
}

/// `ndcart ai test [--url U --model M] [--id CATALOG_ID] [--tools] [--json] [--data-dir D]`.
/// Exit code 0 (good, usable), 3 (too weak), 1 (could not test).
pub async fn run_cli(args: Vec<String>) -> Result<i32, String> {
    let c = super::common(args)?;
    let (mut url, mut model, mut id, mut tools) = (None, None, None, false);
    let mut it = c.rest.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().ok_or(format!("{n} needs a value"));
        match a.as_str() {
            "--url" => url = Some(val("--url")?),
            "--model" => model = Some(val("--model")?),
            "--id" => id = Some(val("--id")?),
            "--tools" => tools = true,
            o => return Err(format!("unknown argument: {o}")),
        }
    }
    let mut stored = crate::ai_settings::load(Some(&c.data));
    if url.is_some() || model.is_some() {
        stored.direct_url = url.or(stored.direct_url);
        stored.direct_model = model.or(stored.direct_model);
    }
    let id = id.or_else(|| {
        // Only the managed model has a catalog id to remember a result against.
        let direct_set = stored.direct_url.is_some();
        stored
            .managed
            .as_ref()
            .filter(|_| !direct_set)
            .and_then(|m| m.model_id.clone())
    });
    let cfg = Config::from_env_and(&stored)?;
    let report = run(&cfg, &Opts { tools }).await?;
    if c.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", report.render());
    }
    match id {
        Some(id) => match record(&c.data, &id, &report) {
            Ok(true) if !c.json => eprintln!(
                "Recorded for '{id}' in {}.",
                c.data.join("ai-verified.json").display()
            ),
            Ok(false) if !c.json && report.verdict != "error" => {
                eprintln!("Not recorded: '{id}' is not a catalog model.")
            }
            Err(e) => eprintln!("could not record the result: {e}"),
            _ => {}
        },
        None if !c.json && report.verdict != "error" => {
            eprintln!("Not recorded (pass --id <catalog id> to remember this result for a model).")
        }
        None => {}
    }
    Ok(report.exit_code())
}
