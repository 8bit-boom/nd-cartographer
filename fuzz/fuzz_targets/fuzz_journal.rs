#![no_main]
use libfuzzer_sys::fuzz_target;

// The journal loader replays a project's history from disk; a corrupt or
// hostile journal must be refused, never panic.
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ndcart_core::journal::Journal::from_ndjson(text);
    }
});