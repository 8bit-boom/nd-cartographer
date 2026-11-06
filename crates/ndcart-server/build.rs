//! The UI files are compiled into the binary with `include_str!`; without
//! this, editing only `ui/*` left the served page stale (cargo saw no Rust
//! change and skipped the rebuild).

fn main() {
    for f in ["index.html", "app.js", "app.css", "table.html"] {
        println!("cargo:rerun-if-changed=../../ui/{f}");
    }
}
