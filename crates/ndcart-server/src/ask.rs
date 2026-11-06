//! "Ask your maps": a plain-language question answered from what is written in
//! the projects (room names, notes, markers, zones, regions, routes). The studio
//! builds a compact text digest of every map, hands it to the model together
//! with the question, and tells the model to answer only from that digest and
//! to say so when the maps do not hold the answer. Only text from the maps is
//! sent (no pictures, no coordinates beyond which floor something is on).

// Without the AI client nothing asks; the digest code stays so the tests keep covering it.
#![cfg_attr(not(feature = "bridge"), allow(dead_code))]

use crate::store::Store;

pub const MAX_DIGEST_CHARS: usize = 14_000;

pub const SYSTEM_PROMPT: &str =
    "You answer questions about a game master's maps using ONLY the digest below. \
Each project lists its floors with their rooms, notes, markers, zones, regions and routes. \
Name the project and floor when you cite something. If the digest does not contain the answer, say \
that the maps do not say, and do not invent places or details. Be brief.";

/// The text digest of every project, oldest content first dropped when too long.
pub fn digest(store: &Store) -> String {
    let mut docs: Vec<_> = store.docs.values().collect();
    docs.sort_by(|a, b| a.manifest.name.cmp(&b.manifest.name));
    let mut out = String::new();
    for d in docs {
        let mut block = format!(
            "PROJECT {} ({})\n",
            d.manifest.name,
            d.manifest.kind.as_str()
        );
        for f in &d.floors {
            block.push_str(&format!("  FLOOR {}\n", f.name));
            let rooms: Vec<&str> = f
                .grid
                .spaces
                .values()
                .map(|s| s.name.as_str())
                .take(80)
                .collect();
            if !rooms.is_empty() {
                block.push_str(&format!("    rooms: {}\n", rooms.join("; ")));
            }
            let notes: Vec<String> = f
                .labels
                .iter()
                .map(|l| l.text.replace('\n', " "))
                .take(40)
                .collect();
            if !notes.is_empty() {
                block.push_str(&format!("    notes: {}\n", notes.join(" | ")));
            }
            for m in f.markers.iter().take(40) {
                let note = if m.note.is_empty() {
                    String::new()
                } else {
                    format!(": {}", m.note.replace('\n', " "))
                };
                block.push_str(&format!("    marker {}{}\n", m.label, note));
            }
            let zones: Vec<&str> = f.zones.iter().map(|z| z.name.as_str()).collect();
            if !zones.is_empty() {
                block.push_str(&format!("    zones: {}\n", zones.join("; ")));
            }
            let named: Vec<&str> = f
                .regions
                .iter()
                .map(|r| r.name.as_str())
                .chain(f.routes.iter().map(|r| r.name.as_str()))
                .filter(|n| !n.is_empty())
                .collect();
            if !named.is_empty() {
                block.push_str(&format!("    regions and routes: {}\n", named.join("; ")));
            }
        }
        if out.len() + block.len() > MAX_DIGEST_CHARS {
            out.push_str("(more projects were left out to keep this short)\n");
            break;
        }
        out.push_str(&block);
    }
    if out.is_empty() {
        out.push_str("(there are no projects yet)\n");
    }
    out
}

/// The user turn: the digest, then the question.
pub fn user_turn(digest: &str, question: &str) -> String {
    format!(
        "DIGEST OF THE MAPS\n{digest}\nQUESTION: {}",
        question.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::StoredDoc;
    use ndcart_core::DocKind;

    fn store() -> Store {
        let mut st = Store::default();
        let mut d = StoredDoc::new("Harbour", DocKind::Battle);
        let fid = d.floors[0].id.clone();
        let ops: Vec<ndcart_core::journal::Op> = serde_json::from_value(serde_json::json!([
            {"t":"upsert_space","space":{"id":"a","name":"Smuggler's Cellar","color":"#7f5539","kind":"room"}},
            {"t":"upsert_label","label":{"id":"n","x":1.0,"y":1.0,"text":"Trapdoor behind the barrels","size":0.8,"color":"#222222","bold":false}},
            {"t":"put_overlay","item":{"kind":"marker","id":"m","x":2.0,"y":2.0,"label":"Hidden stash","note":"Holds the ledger"}}
        ]))
        .unwrap();
        d.apply_ops(&fid, &ops).unwrap();
        let id = d.manifest.id.clone();
        st.docs.insert(id, d);
        st.docs
            .insert("x".into(), StoredDoc::new("Aardvark Keep", DocKind::Region));
        st
    }

    #[test]
    fn the_digest_lists_every_project_with_its_text_and_nothing_else() {
        let d = digest(&store());
        assert!(
            d.starts_with("PROJECT Aardvark Keep"),
            "sorted by name: {d}"
        );
        for want in [
            "Smuggler's Cellar",
            "Trapdoor behind the barrels",
            "marker Hidden stash: Holds the ledger",
            "FLOOR Ground",
        ] {
            assert!(d.contains(want), "{want} in {d}");
        }
        assert_eq!(digest(&Store::default()), "(there are no projects yet)\n");
        let t = user_turn(&d, "  Where is the ledger? ");
        assert!(t.ends_with("QUESTION: Where is the ledger?") && t.contains("DIGEST OF THE MAPS"));
        assert!(SYSTEM_PROMPT.contains("ONLY") && SYSTEM_PROMPT.contains("do not invent"));
    }

    #[test]
    fn a_huge_collection_is_cut_with_a_note_instead_of_overflowing() {
        let mut st = Store::default();
        for i in 0..400 {
            let mut d = StoredDoc::new(
                &format!("Project {i:03} with a long enough name to matter"),
                DocKind::Battle,
            );
            let fid = d.floors[0].id.clone();
            let ops: Vec<ndcart_core::journal::Op> = serde_json::from_value(serde_json::json!([
                {"t":"upsert_label","label":{"id":"n","x":1.0,"y":1.0,"text":"a note that takes up some room in the digest","size":0.8,"color":"#222222","bold":false}}
            ]))
            .unwrap();
            d.apply_ops(&fid, &ops).unwrap();
            st.docs.insert(d.manifest.id.clone(), d);
        }
        let d = digest(&st);
        assert!(
            d.len() <= MAX_DIGEST_CHARS + 100 && d.contains("left out"),
            "{}",
            d.len()
        );
    }
}
