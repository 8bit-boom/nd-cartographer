#![no_main]
use libfuzzer_sys::fuzz_target;

// The Foundry scene reader eats untrusted JSON; it must never panic.
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ndcart_formats::foundry::from_scene(text);
    }
});