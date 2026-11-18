//! Every relative link in the Markdown docs points at a file that exists, so a moved or
//! renamed document cannot leave a dead link behind.

use std::path::{Path, PathBuf};

fn markdown_files(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join("README.md")];
    for e in std::fs::read_dir(root.join("docs")).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("md") {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// The targets of `[text](target)` links in `text`, skipping fenced code blocks.
fn links(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let mut rest = line;
        while let Some(i) = rest.find("](") {
            let after = &rest[i + 2..];
            if let Some(end) = after.find(')') {
                out.push(after[..end].trim().to_string());
                rest = &after[end..];
            } else {
                break;
            }
        }
    }
    out
}

#[test]
fn every_relative_link_in_the_docs_points_at_a_file_that_exists() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut checked = 0;
    for file in markdown_files(&root) {
        let text = std::fs::read_to_string(&file).unwrap();
        let dir = file.parent().unwrap();
        for target in links(&text) {
            if target.is_empty()
                || target.starts_with('#')
                || target.contains("://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            let path = target.split('#').next().unwrap();
            let resolved = dir.join(path);
            assert!(
                resolved.exists(),
                "{} links to '{target}', which does not exist",
                file.strip_prefix(&root).unwrap().display()
            );
            checked += 1;
        }
    }
    assert!(checked > 10, "only {checked} links were checked");
}

#[test]
fn the_ai_instruction_docs_exist_and_name_the_pack() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let text = std::fs::read_to_string(root.join("docs/AI_INSTRUCTIONS.md")).unwrap();
    assert!(text.contains("crates/ndcart-server/assets/ai/"));
    assert!(root
        .join("crates/ndcart-server/assets/ai/pack.toml")
        .exists());
}
