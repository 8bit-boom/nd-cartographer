//! `ndcart mcp` — a Model Context Protocol stdio server (plan §21.17, §22.6):
//! newline-delimited JSON-RPC 2.0 on stdin/stdout, so an agent (Claude,
//! ChatGPT desktop, a script) can drive the studio's read surface.
//!
//! Posture (plan §22.6): the tool list is fixed and read-shaped — no shell,
//! no arbitrary paths, everything through the same validation the UI uses.
//! Writes go through documented, validated operations only (PNG export to an
//! explicit `.png` path the caller names, never over an existing file unless
//! asked, and the players' copy unless the GM copy is requested). Nothing is
//! executed, nothing leaves the data dir.

use crate::store;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

const TOOLS: &str = r#"[
  {"name": "list_projects", "description": "List the studio's projects: id, name, kind, revision, floor names.", "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}},
  {"name": "get_stats", "description": "Pacing numbers for one project's floor: rooms/squares/kinds, doors and secrets, lights and their reach, props by provenance, GM labels, encounters, lore coverage.", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string", "description": "document id or exact name"}, "floor": {"type": "string", "description": "floor id; default the first"}}, "required": ["doc"], "additionalProperties": false}},
  {"name": "get_floor_spec", "description": "Read a painted floor back as a MapSpec (the re-layout input): rooms with sizes/kinds/lamps, links, stairs. Warnings report clamps.", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string"}, "floor": {"type": "string"}}, "required": ["doc"], "additionalProperties": false}},
  {"name": "get_instructions", "description": "The AI instruction pack for writing a spec: system prompt, full reference, repair template, JSON Schema and worked examples. Read this first, then write a spec, then call validate_spec.", "inputSchema": {"type": "object", "properties": {"kind": {"type": "string", "enum": ["map", "world", "refine"], "description": "default map"}}, "additionalProperties": false}},
  {"name": "get_spec_schema", "description": "The JSON Schema (draft 2020-12) of a MapSpec or WorldSpec.", "inputSchema": {"type": "object", "properties": {"kind": {"type": "string", "enum": ["map", "world"], "description": "default map"}}, "additionalProperties": false}},
  {"name": "validate_spec", "description": "Check a spec without changing anything: unknown fields, validation, layout and the floor linter. Returns {ok, kind, errors, unknown_fields, problems}; fix the errors and call again. A spec that passes can be previewed by the game master through the HTTP propose route.", "inputSchema": {"type": "object", "properties": {"spec": {"description": "the spec object (or a string holding it)"}, "kind": {"type": "string", "enum": ["map", "world"], "description": "guessed when omitted"}}, "required": ["spec"], "additionalProperties": false}},
  {"name": "lint_floor", "description": "Run the project linter on one painted floor (read-only): unreachable rooms, doors into nothing, dead ends, repeated names, caps. Returns the problems.", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string", "description": "document id or exact name"}, "floor": {"type": "string", "description": "floor id; default the first"}}, "required": ["doc"], "additionalProperties": false}},
  {"name": "export_png", "description": "Render one floor to a PNG at an explicit output path (a .png path; refuses to overwrite an existing file unless overwrite is true). The picture is the players' copy by default (secret doors and GM-only labels stripped); pass audience \"gm\" for the GM copy. (export_svg is a deprecated alias for this tool.)", "inputSchema": {"type": "object", "properties": {"doc": {"type": "string"}, "floor": {"type": "string"}, "out": {"type": "string", "description": "the file path to write; must end in .png"}, "audience": {"type": "string", "enum": ["player", "gm"], "description": "default player"}, "overwrite": {"type": "boolean", "description": "replace an existing file (default false)"}}, "required": ["doc", "out"], "additionalProperties": false}}
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
                        "capabilities": {"tools": {}, "resources": {}},
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
            "resources/list" => {
                write_result(&mut out, req_id, json!({ "resources": resource_list() }))?
            }
            "resources/read" => {
                let uri = msg
                    .pointer("/params/uri")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                match resource_read(uri) {
                    Some(text) => write_result(
                        &mut out,
                        req_id,
                        json!({"contents": [{"uri": uri, "mimeType": mime_of(uri), "text": text}]}),
                    )?,
                    None => write_error(
                        &mut out,
                        req_id,
                        -32002,
                        &format!("no resource {uri:?}; see resources/list"),
                    )?,
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

const PACK_URI: &str = "ndcart://ai/pack/";

fn mime_of(uri: &str) -> &'static str {
    if uri.ends_with(".json") {
        "application/json"
    } else if uri.ends_with(".md") {
        "text/markdown"
    } else {
        "text/plain"
    }
}

/// The pack files as MCP resources (`ndcart://ai/pack/<file>`).
fn resource_list() -> Vec<Value> {
    std::iter::once("pack.toml")
        .chain(crate::aipack::FILES.iter().map(|(p, _)| *p))
        .map(|p| {
            let uri = format!("{PACK_URI}{p}");
            json!({"uri": uri, "name": p, "mimeType": mime_of(p),
                   "description": format!("nd-cartographer AI instruction pack {}", crate::aipack::version())})
        })
        .collect()
}

/// One pack file. Markdown is rendered (limits, ids and the version filled in); JSON is as written.
fn resource_read(uri: &str) -> Option<String> {
    let path = uri.strip_prefix(PACK_URI)?;
    if path == "pack.toml" {
        return Some(crate::aipack::MANIFEST.to_string());
    }
    let raw = crate::aipack::raw(path)?;
    Some(if path.ends_with(".md") {
        crate::aipack::render(raw)
    } else {
        raw.to_string()
    })
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
        "get_instructions" => {
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("map");
            Ok(crate::aipack::instructions(kind)?.to_string())
        }
        "get_spec_schema" => {
            match args.get("kind").and_then(|v| v.as_str()).unwrap_or("map") {
                "map" => Ok(crate::aipack::MAPSPEC_SCHEMA.to_string()),
                "world" => Ok(crate::aipack::WORLDSPEC_SCHEMA.to_string()),
                k => Err(format!("kind must be map or world, not {k:?}")),
            }
        }
        "validate_spec" => {
            let spec = args.get("spec").ok_or("argument spec is required")?;
            let kind = args.get("kind").and_then(|v| v.as_str());
            Ok(crate::speccheck::check_value(spec, kind)?.to_string())
        }
        "lint_floor" => {
            let d = find_doc(st, args)?;
            let idx = match args.get("floor").and_then(|v| v.as_str()) {
                Some(fid) => ndcart_core::doc::floors_by_id(&d.floors, fid)
                    .map_err(|e| e.to_string())?,
                None => 0,
            };
            let f = &d.floors[idx];
            let mut problems = ndcart_core::lint(f, d.manifest.cell_size);
            problems.extend(ndcart_core::lint::lint_links(f, &d.floors));
            Ok(json!({"floor": f.name, "problems": problems}).to_string())
        }
        "export_png" | "export_svg" => {
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
            if !out.to_ascii_lowercase().ends_with(".png") {
                return Err(format!("out must be a .png path, got {out:?}"));
            }
            let overwrite = args
                .get("overwrite")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let audience = ndcart_core::Audience::parse(
                args.get("audience")
                    .and_then(|v| v.as_str())
                    .unwrap_or("player"),
            )
            .ok_or("audience must be \"player\" or \"gm\"")?;
            // Same render as the HTTP exporter, through the shared bake, from
            // the audience's copy of the floor (the one secrets filter).
            let floor = ndcart_core::for_audience(&d.floors[idx], audience);
            let walls = crate::tiles::derive_walls(&floor, d.manifest.cell_size);
            let (png, w, h) = crate::tiles::bake_full_as(
                &floor,
                &walls,
                d.manifest.cell_size,
                16,
                crate::tiles::ImageFormat::Png,
                80,
            )
            .map_err(|e| format!("bake failed: {e}"))?;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true);
            if overwrite {
                opts.create(true).truncate(true);
            } else {
                opts.create_new(true);
            }
            let mut file = opts.open(out).map_err(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    format!("{out} already exists; pass \"overwrite\": true to replace it")
                } else {
                    format!("cannot write {out}: {e}")
                }
            })?;
            file.write_all(&png)
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            Ok(json!({"written": out, "px": [w, h], "audience": if audience == ndcart_core::Audience::Gm { "gm" } else { "player" }}).to_string())
        }
        // Not listed by tools/list: the AI lives in the studio process.
        "ask" => Err(
            "ask is not available in MCP mode; run it in the studio (ND_CARTO_SERVER/EMAIL or ND_CARTO_LLM_URL/MODEL) and use the HTTP API".into(),
        ),
        other => Err(format!("unknown tool {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_secret_door() -> store::Store {
        let mut st = store::Store::default();
        let m = st.create("Vault", ndcart_core::DocKind::Battle).clone();
        let d = st.docs.get_mut(&m.id).unwrap();
        let fid = d.floors[0].id.clone();
        let ops: Vec<ndcart_core::Op> = serde_json::from_value(json!([
            {"t":"upsert_space","space":{"id":"a","name":"Hall","color":"#7f5539","kind":"room"}},
            {"t":"upsert_space","space":{"id":"b","name":"Vault","color":"#5a7d9a","kind":"room"}},
            {"t":"paint_cells","cells":[{"x":0,"y":0},{"x":1,"y":0},{"x":2,"y":0}],"space":"a"},
            {"t":"paint_cells","cells":[{"x":3,"y":0}],"space":"b"},
            {"t":"set_mark","edge":{"cell":{"x":2,"y":0},"edge":"east"},"mark":{"kind":"secret","state":"closed"}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        st
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ndcart-mcp-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn has_secret_purple(png: &[u8]) -> bool {
        let pm = tiny_skia::Pixmap::decode_png(png).unwrap();
        pm.pixels().iter().any(|p| {
            let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
            d(p.red(), 0xb0) < 14 && d(p.green(), 0x6a) < 14 && d(p.blue(), 0xd8) < 14
        })
    }

    #[test]
    fn export_refuses_to_overwrite_and_non_png_paths() {
        let mut st = store_with_secret_door();
        let out = tmp("exists.png");
        std::fs::write(&out, b"precious").unwrap();
        let o = out.to_string_lossy().to_string();
        let err = call_tool(&mut st, "export_png", &json!({"doc": "Vault", "out": o})).unwrap_err();
        assert!(err.contains("already exists"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), b"precious");
        let ok = call_tool(
            &mut st,
            "export_png",
            &json!({"doc": "Vault", "out": o, "overwrite": true}),
        );
        assert!(ok.is_ok(), "{ok:?}");
        let txt = tmp("notes.txt").to_string_lossy().to_string();
        let err =
            call_tool(&mut st, "export_png", &json!({"doc": "Vault", "out": txt})).unwrap_err();
        assert!(err.contains(".png"), "{err}");
        assert!(!std::path::Path::new(&txt).exists());
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn export_defaults_to_the_player_copy_and_keeps_secret_doors_out() {
        let mut st = store_with_secret_door();
        let (p, g) = (tmp("player.png"), tmp("gm.png"));
        for f in [&p, &g] {
            let _ = std::fs::remove_file(f);
        }
        let ps = p.to_string_lossy().to_string();
        let gs = g.to_string_lossy().to_string();
        call_tool(&mut st, "export_png", &json!({"doc": "Vault", "out": ps})).unwrap();
        // The deprecated alias still works.
        call_tool(
            &mut st,
            "export_svg",
            &json!({"doc": "Vault", "out": gs, "audience": "gm"}),
        )
        .unwrap();
        assert!(!has_secret_purple(&std::fs::read(&p).unwrap()));
        assert!(
            has_secret_purple(&std::fs::read(&g).unwrap()),
            "the GM copy shows it"
        );
        let _ = (std::fs::remove_file(&p), std::fs::remove_file(&g));
    }

    #[test]
    fn the_spec_tools_work_without_a_project() {
        let mut st = store::Store::default();
        let ins = call_tool(&mut st, "get_instructions", &json!({"kind": "world"})).unwrap();
        let v: Value = serde_json::from_str(&ins).unwrap();
        assert_eq!(v["version"], crate::aipack::version());
        assert_eq!(v["schema"]["title"], "WorldSpec");
        assert!(call_tool(&mut st, "get_instructions", &json!({"kind": "x"})).is_err());
        let s = call_tool(&mut st, "get_spec_schema", &json!({})).unwrap();
        assert!(s.contains("\"MapSpec\""));
        assert!(call_tool(&mut st, "get_spec_schema", &json!({"kind": "q"})).is_err());
        let r = call_tool(
            &mut st,
            "validate_spec",
            &json!({"spec": {"name": "M", "floors": [{"name": "G", "rooms": [
                {"id": "a", "name": "A", "w": 4, "h": 4}, {"id": "lost", "name": "B", "w": 4, "h": 4}]}]}}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&r).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["errors"][0].as_str().unwrap().contains("'lost'"), "{v}");
        assert!(call_tool(&mut st, "validate_spec", &json!({})).is_err());
    }

    #[test]
    fn lint_floor_reads_a_painted_floor() {
        let mut st = store_with_secret_door();
        let r = call_tool(&mut st, "lint_floor", &json!({"doc": "Vault"})).unwrap();
        let v: Value = serde_json::from_str(&r).unwrap();
        let codes: Vec<&str> = v["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["code"].as_str().unwrap())
            .collect();
        assert!(codes.contains(&"secret_only_room"), "{codes:?}");
        assert!(call_tool(&mut st, "lint_floor", &json!({"doc": "Nope"})).is_err());
    }

    #[test]
    fn resources_are_the_pack_files_and_markdown_is_rendered() {
        let list = resource_list();
        assert!(list
            .iter()
            .any(|r| r["uri"] == "ndcart://ai/pack/reference.md"));
        assert!(list
            .iter()
            .any(|r| r["uri"] == "ndcart://ai/pack/pack.toml"));
        let text = resource_read("ndcart://ai/pack/reference.md").unwrap();
        assert!(text.contains(crate::aipack::version()) && !text.contains("{{"));
        assert!(resource_read("ndcart://ai/pack/mapspec.schema.json")
            .unwrap()
            .contains("MapSpec"));
        assert!(resource_read("ndcart://ai/pack/missing.md").is_none());
        assert!(resource_read("file:///etc/passwd").is_none());
        assert!(resource_read("ndcart://ai/pack/../../etc/passwd").is_none());
    }

    #[test]
    fn ask_is_not_listed() {
        let tools: Value = serde_json::from_str(TOOLS).unwrap();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(!names.contains(&"ask") && names.contains(&"export_png"));
    }
}
