#![no_main]
use libfuzzer_sys::fuzz_target;

// The Universal VTT reader eats untrusted files; it must never panic, only
// return Ok or a clean Err.
fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = ndcart_formats::uvtt::from_uvtt(text);
    }
});