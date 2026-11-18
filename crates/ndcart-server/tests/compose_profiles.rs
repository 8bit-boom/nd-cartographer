//! The compose file keeps the local-AI services opt-in: `ndcart` alone by default, the model
//! servers only under their profiles, with the safety flags in place. (CI also runs
//! `docker compose config` for each profile; this test needs no Docker.)

fn compose() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docker-compose.yml"
    ))
    .unwrap()
}

/// The text of one service: from its `  name:` line to the next service or the end.
fn service<'a>(text: &'a str, name: &str) -> &'a str {
    let head = format!("\n  {name}:\n");
    let start = text
        .find(&head)
        .unwrap_or_else(|| panic!("no service {name}"))
        + 1;
    let rest = &text[start..];
    let end = rest
        .match_indices("\n  ")
        .filter_map(|(i, _)| {
            let line = &rest[i + 1..];
            let key = line.strip_prefix("  ")?;
            (!key.starts_with(' ') && !key.starts_with('#') && key.contains(':')).then_some(i)
        })
        .find(|&i| i > 0)
        .unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_studio_has_no_profile_and_the_model_servers_each_have_one() {
    let c = compose();
    let studio = service(&c, "ndcart");
    assert!(
        !studio.contains("profiles:"),
        "the studio must always start"
    );
    let cpu = service(&c, "llm");
    let gpu = service(&c, "llm-gpu");
    assert!(cpu.contains(r#"profiles: ["ai-cpu"]"#));
    assert!(gpu.contains(r#"profiles: ["ai-gpu"]"#));
    assert!(gpu.contains("driver: nvidia") && gpu.contains("\"-ngl\""));
    assert!(!cpu.contains("driver: nvidia"));
}

#[test]
fn the_model_servers_are_locked_down() {
    let c = compose();
    for name in ["llm", "llm-gpu"] {
        let s = service(&c, name);
        assert!(
            s.contains("\"--api-key-file\""),
            "{name}: the key is a file"
        );
        assert!(
            !s.contains("\"--api-key\""),
            "{name}: never the key as an argument"
        );
        assert!(s.contains("\"--alias\"") && s.contains("\"ndcart-local\""));
        assert!(
            s.contains("./data:/data:ro"),
            "{name}: the data folder is read-only"
        );
        assert!(!s.contains("ports:"), "{name}: no published port");
        assert!(
            s.contains("\"--port\"") && s.contains("\"8080\""),
            "{name}: an explicit port"
        );
    }
}

#[test]
fn the_studio_is_pointed_at_the_model_through_commented_examples_only() {
    let c = compose();
    let studio = service(&c, "ndcart");
    for var in [
        "ND_CARTO_LLM_URL",
        "ND_CARTO_LLM_MODEL",
        "ND_CARTO_LLM_KEY_FILE",
    ] {
        assert!(
            studio.contains(&format!("# {var}:")),
            "{var} should be a commented example"
        );
        assert!(!studio
            .lines()
            .any(|l| l.trim_start().starts_with(&format!("{var}:"))));
    }
}
