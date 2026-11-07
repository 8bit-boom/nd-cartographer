//! `ndcart mcp` — a Model Context Protocol stdio server (plan §21.17, §22.6):
//! newline-delimited JSON-RPC 2.0 on stdin/stdout, so an agent (Claude,
//! ChatGPT desktop, a script) can drive the studio's read surface.
//!
//! Posture (plan §22.6): the tool list is fixed and read-shaped — no shell,
//! no arbitrary paths, everything through the same validation the UI uses.
//! Writes go through documented, validated operations only (export to an
//! explicit `out` path the caller names; AI ask uses the studio's own AI
//! configuration). Nothing is executed, nothing leaves the data dir.

use crate::store;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const TOOLS: &str = r#"[
  {"name": "list_projects", "description": "List the studio's projects: id, name, kind, revision, floor names.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}},
  {"name": "get_stats", "description": "Pacing numbers for one project's floor: rooms/squares/kinds, doors and secrets, lights and their reach, props by provenance, GM labels, encounters, lore coverage.", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string", "description": "document id or exact name"}, "floor": {"type": "string", "description": "floor id; default the first"}}, "required": ["doc"], "additionalProperties": false}},
  {"name": "get_floor_spec", "description": "Read a painted floor back as a MapSpec (the re-layout input): rooms with sizes/kinds/lamps, links, stairs. Warnings report clamps.", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string"}, "floor": {"type": "string"}}, "required": ["doc"], "additionalProperties": false}},
  {"name": "export_svg", "description": "Write one floor's geometry as an editable SVG to an explicit output path (1 cell = 1 user unit).", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string"}, "floor": {"type": "string"}, "out": {"type": "string", "description": "the file path to write"}}, "required": ["doc", "out"], "additionalProperties": false}},
  {"name": "ask", "description": "Ask the studio's AI a question about every project (needs an AI configured; the model answers only from the maps' own text).", "inputSchema": {"type": "object", "properties": {"question": {"type": "string", "maxLength": 1000}}, "required": ["question"], "additionalProperties": false}}
]"#;

pub fn run(data_dir: Option<std::path::PathBuf>) -> Result<(), String> {
    let mut st = store::Store::default();
    if let Some(dir) = &data_dir {
        st.load_dir(dir).map_err(|e| e.to_string())?;
    }
    let stdin = std::io::stdin();
    let lines = stdin.lock().lines();
    let mut out = std::io::stdout();
    for line in lines {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                write_error(&mut out, Value::Null, -32700, &format!("parse error: {e}"))?;
                continue;
            }
        };
        let req_id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        match method {
            "initialize" => {
                write_result(
                    &mut out,
                    req_id,
                    json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "nd-cartographer", "version": env!("CARGO_PKG_VERSION")},
                    }),
                )?;
            }
            "notifications/initialized" | "initialized" => {}
            "tools/list" => {
                let tools: Value =
                    serde_json::from_str(TOOLS).expect("the tool list is valid JSON");
                write_result(&mut out, req_id, json!({ "tools": tools }))?;
            }
            "tools/call" => {
                let name = msg
                    .pointer("/params/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = msg
                    .pointer("/params/arguments")
                    .cloned()
                    .unwrap_or(json!({}));
                match call_tool(&mut st, &name, &args) {
                    Ok(text) => write_result(
                        &mut out,
                        req_id,
                        json!({"content": [{"type": "text", "text": text}]}),
                    )?,
                    Err(e) => {
                        write_tool_error(&mut out, req_id, &e)?;
                    }
                }
            }
            "ping" => write_result(&mut out, req_id, json!({}))?,
            other => write_error(&mut out, req_id, -32601, &format!("unknown method {other}"))?,
        }
    }
    Ok(())
}

fn write_result(w: &mut impl Write, id: Value, result: Value) -> Result<(), String> {
    writeln!(
        w,
        "{}",
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    )
    .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

fn write_error(w: &mut impl Write, id: Value, code: i32, message: &str) -> Result<(), String> {
    writeln!(
        w,
        "{}",
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
    )
    .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

fn write_tool_error(w: &mut impl Write, id: Value, message: &str) -> Result<(), String> {
    writeln!(
        w,
        "{}",
        json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": message}], "isError": true}})
    )
    .map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

/// Resolve a doc argument (id or exact name).
fn find_doc<'a>(st: &'a store::Store, arg: &Value) -> Result<&'a store::StoredDoc, String> {
    let key = arg
        .get("doc")
        .and_then(|v| v.as_str())
        .ok_or("argument doc is required")?;
    st.docs
        .values()
        .find(|d| d.manifest.id == key || d.manifest.name == key)
        .ok_or_else(|| format!("no project {key:?}"))
}

fn call_tool(st: &mut store::Store, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "list_projects" => {
            let docs: Vec<_> = st
                .docs
                .values()
                .map(|d| {
                    json!({
                        "id": d.manifest.id, "name": d.manifest.name,
                        "kind": d.manifest.kind.as_str(), "rev": d.rev,
                        "floors": d.floors.iter().map(|f| json!({"id": f.id, "name": f.name, "digest": f.digest()})).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Ok(json!({ "docs": docs }).to_string())
        }
        "get_stats" => {
            let d = find_doc(st, args)?;
            let idx = match args.get("floor").and_then(|v| v.as_str()) {
                Some(fid) => ndcart_core::doc::floors_by_id(&d.floors, fid)
                    .map_err(|e| e.to_string())?,
                None => 0,
            };
            let s = ndcart_core::stats::floor_stats(&d.floors[idx], d.manifest.cell_size);
            Ok(serde_json::to_string_pretty(&s).map_err(|e| e.to_string())?)
        }
        "get_floor_spec" => {
            let d = find_doc(st, args)?;
            let idx = match args.get("floor").and_then(|v| v.as_str()) {
                Some(fid) => ndcart_core::doc::floors_by_id(&d.floors, fid)
                    .map_err(|e| e.to_string())?,
                None => 0,
            };
            let (spec, warnings) = ndcart_core::extract::extract_floor_spec(
                &d.floors[idx],
                &d.floors,
                d.manifest.cell_size,
            )
            .map_err(|e| e.to_string())?;
            Ok(json!({ "spec": spec, "warnings": warnings }).to_string())
        }
        "export_svg" => {
            let d = find_doc(st, args)?;
            let idx = match args.get("floor").and_then(|v| v.as_str()) {
                Some(fid) => ndcart_core::doc::floors_by_id(&d.floors, fid)
                    .map_err(|e| e.to_string())?,
                None => 0,
            };
            let out = args
                .get("out")
                .and_then(|v| v.as_str())
                .ok_or("argument out is required")?;
            // Same render as the HTTP exporter, through the shared bake.
            let floor = &d.floors[idx];
            let walls = crate::tiles::derive_walls(floor, d.manifest.cell_size);
            let (png, w, h) = crate::tiles::bake_full_as(
                floor,
                &walls,
                d.manifest.cell_size,
                16,
                crate::tiles::ImageFormat::Png,
                80,
            )
            .map_err(|e| format!("bake failed: {e}"))?;
            std::fs::write(out, &png).map_err(|e| format!("cannot write {out}: {e}"))?;
            Ok(json!({"written": out, "px": [w, h], "note": "PNG rendered; SVG export is available via GET /export.svg on a running studio"}).to_string())
        }
        "ask" => Err(
            "ask needs an AI configured in the studio process's environment (ND_CARTO_SERVER/EMAIL or ND_CARTO_LLM_URL/MODEL); run it there and use the HTTP API".into(),
        ),
        other => Err(format!("unknown tool {other}")),
    }
}
