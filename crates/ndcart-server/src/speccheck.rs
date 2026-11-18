//! Check a spec the way the AI loop does, with no model involved: read it, report fields the
//! spec does not have, validate, lay it out, and lint the result. The AI repair loop
//! (`draft::parse_reply`), `POST /api/spec/check` and the MCP `validate_spec` tool all use
//! this, so they give the same complaints.

// Without the `bridge` feature only the spec check routes use this module.
#![cfg_attr(not(feature = "bridge"), allow(dead_code))]

use crate::specshape::{unknown_fields, unknown_fields_complaint, SpecKind};
use crate::worldspec::WorldSpec;
use ndcart_core::mapspec::MapSpec;
use ndcart_core::speclint::SpecProblem;
use serde_json::Value;

/// The first JSON object in `text`, tolerating code fences and chatter around it.
pub fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let (mut in_str, mut esc) = (false, false);
    for (i, ch) in text[start..].char_indices() {
        if in_str {
            match (esc, ch) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// What a check found. `errors` are things that stop the spec being used (the first is what
/// the AI loop sends back to the model); `problems` are the floor linter's findings, of any
/// severity (an `error` one is also in `errors`).
#[derive(Debug)]
pub struct Checked<T> {
    pub spec: Option<T>,
    pub errors: Vec<String>,
    pub unknown_fields: Vec<String>,
    pub problems: Vec<SpecProblem>,
}

impl<T> Checked<T> {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Fields that only a WorldSpec has; a spec carrying them and no `floors` is a world.
pub fn guess_kind(v: &Value) -> SpecKind {
    let has = |k: &str| v.get(k).is_some();
    if !has("floors")
        && ["cols", "rows", "land", "biomes", "places", "regions"]
            .iter()
            .any(|k| has(k))
    {
        SpecKind::World
    } else {
        SpecKind::Map
    }
}

fn unknown<T>(json: &str, kind: SpecKind, out: &mut Checked<T>) {
    if let Ok(v) = serde_json::from_str::<Value>(json) {
        out.unknown_fields = unknown_fields(&v, kind);
        if let Some(m) = unknown_fields_complaint(&v, kind) {
            out.errors.push(m);
        }
    }
}

/// Check the JSON text of a MapSpec.
pub fn check_map(json: &str) -> Checked<MapSpec> {
    let mut out = Checked {
        spec: None,
        errors: Vec::new(),
        unknown_fields: Vec::new(),
        problems: Vec::new(),
    };
    unknown(json, SpecKind::Map, &mut out);
    let spec: MapSpec = match serde_json::from_str(json) {
        Ok(s) => s,
        Err(e) => {
            out.errors
                .push(format!("The JSON did not match the schema: {e}."));
            return out;
        }
    };
    if let Err(e) = spec.validate() {
        out.errors.push(format!("The spec broke a rule: {e}."));
        return out;
    }
    // Laying it out is the next check: it may not fit.
    let laid = match ndcart_core::mapspec::compile(&spec, 1) {
        Ok(l) => l,
        Err(e) => {
            out.errors
                .push(format!("The spec could not be laid out: {e}."));
            return out;
        }
    };
    // The floor linter runs on the result: an error here (a publish cap) is refused.
    match ndcart_core::speclint::lint_compiled(&spec, &laid) {
        Ok(problems) => {
            let limits: Vec<&str> = problems
                .iter()
                .filter(|p| p.severity == ndcart_core::Severity::Error)
                .map(|p| p.message.as_str())
                .collect();
            if !limits.is_empty() {
                out.errors.push(format!(
                    "The laid-out map breaks a limit: {} Make the map smaller.",
                    limits.join(" ")
                ));
            }
            out.problems = problems;
        }
        Err(e) => out
            .errors
            .push(format!("The spec could not be checked: {e}.")),
    }
    out.spec = Some(spec);
    out
}

/// Check the JSON text of a WorldSpec.
pub fn check_world(json: &str) -> Checked<WorldSpec> {
    let mut out = Checked {
        spec: None,
        errors: Vec::new(),
        unknown_fields: Vec::new(),
        problems: Vec::new(),
    };
    unknown(json, SpecKind::World, &mut out);
    let spec: WorldSpec = match serde_json::from_str(json) {
        Ok(s) => s,
        Err(e) => {
            out.errors
                .push(format!("The JSON did not match the schema: {e}."));
            return out;
        }
    };
    if let Err(e) = spec.validate() {
        out.errors.push(format!("The spec broke a rule: {e}."));
        return out;
    }
    if let Err(e) = crate::worldspec::compile(&spec, 1, &[]) {
        out.errors
            .push(format!("The spec could not be drawn: {e}."));
        return out;
    }
    out.spec = Some(spec);
    out
}

/// The answer of `POST /api/spec/check` and the MCP `validate_spec` tool.
/// `spec` is a JSON object, or a string holding one (code fences and chatter around it are
/// tolerated, as for a model's reply). `kind` is `map` or `world`; unset, it is guessed.
pub fn check_value(spec: &Value, kind: Option<&str>) -> Result<Value, String> {
    let text = match spec {
        Value::String(t) => extract_json(t)
            .ok_or("the spec text holds no JSON object")?
            .to_string(),
        Value::Object(_) => serde_json::to_string_pretty(spec).map_err(|e| e.to_string())?,
        _ => return Err("spec must be a JSON object (or a string holding one)".into()),
    };
    let parsed: Value = serde_json::from_str(&text)
        .map_err(|e| format!("The JSON did not match the schema: {e}."))?;
    let kind = match kind {
        None => guess_kind(&parsed),
        Some("map") => SpecKind::Map,
        Some("world") => SpecKind::World,
        Some(k) => return Err(format!("kind must be map or world, not '{k}'")),
    };
    let (ok, errors, unknown, problems) = match kind {
        SpecKind::Map => {
            let c = check_map(&text);
            (c.ok(), c.errors, c.unknown_fields, c.problems)
        }
        SpecKind::World => {
            let c = check_world(&text);
            (c.ok(), c.errors, c.unknown_fields, c.problems)
        }
    };
    Ok(serde_json::json!({
        "ok": ok,
        "kind": if kind == SpecKind::Map { "map" } else { "world" },
        "errors": errors,
        "unknown_fields": unknown,
        "problems": problems,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"name":"Crypt","floors":[{"name":"G","rooms":[{"id":"a","name":"Nave","w":8,"h":6},{"id":"b","name":"Ossuary","w":5,"h":5}],"links":[{"a":"a","b":"b"}]}]}"#;

    #[test]
    fn a_good_spec_is_ok_and_carries_its_problems() {
        let c = check_map(GOOD);
        assert!(c.ok() && c.spec.is_some(), "{:?}", c.errors);
        assert!(c.unknown_fields.is_empty());
        // The first error of a bad spec is what the AI loop sends back.
        let bad = GOOD.replace(r#","links":[{"a":"a","b":"b"}]"#, "");
        let c = check_map(&bad);
        assert!(!c.ok() && c.spec.is_none());
        assert!(c.errors[0].contains("not linked"), "{:?}", c.errors);
    }

    #[test]
    fn unknown_fields_come_first_but_the_rest_is_still_checked() {
        let bad = GOOD
            .replace(r#""w":8"#, r#""w":80,"props":[]"#)
            .replace(r#","links":[{"a":"a","b":"b"}]"#, "");
        let c = check_map(&bad);
        assert_eq!(
            c.unknown_fields,
            vec!["floors[0].rooms[0].props".to_string()]
        );
        assert!(
            c.errors[0].starts_with("Unknown field(s)"),
            "{:?}",
            c.errors
        );
        assert!(c.errors[1].contains("2-30"), "{:?}", c.errors);
    }

    #[test]
    fn the_kind_is_guessed_from_what_the_spec_carries() {
        let v = |t: &str| serde_json::from_str::<Value>(t).unwrap();
        assert_eq!(guess_kind(&v(GOOD)), SpecKind::Map);
        assert_eq!(
            guess_kind(&v(r#"{"name":"W","land":0.4}"#)),
            SpecKind::World
        );
        assert_eq!(guess_kind(&v(r#"{"name":"M"}"#)), SpecKind::Map);
    }

    #[test]
    fn a_world_is_checked_too() {
        let c = check_world(r#"{"name":"X","land":3}"#);
        assert!(c.errors[0].contains("land must be"), "{:?}", c.errors);
        let c = check_world(r#"{"name":"X","places":[{"name":"P","x":0.5,"y":0.5,"icon":"s"}]}"#);
        assert_eq!(c.unknown_fields, vec!["places[0].icon".to_string()]);
    }
}
