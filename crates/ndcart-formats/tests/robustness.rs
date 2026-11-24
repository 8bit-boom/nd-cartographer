//! Deterministic mutation robustness: the import parsers eat untrusted files,
//! so no input may make them panic — only return Ok or a clean Err. This runs
//! on stable in CI (the cargo-fuzz targets in /fuzz go deeper on nightly).
//! A panic here fails the test, so this is a real gate, not a smoke test.

const UVTT: &str = r#"{"resolution":{"map_size":{"x":4,"y":3},"pixels_per_grid":100,"map_origin":{"x":0,"y":0}},"line_of_sight":[[{"x":0,"y":0},{"x":1,"y":0},{"x":1,"y":1}]],"portals":[{"bounds":[{"x":1,"y":0},{"x":1,"y":1}],"closed":true}],"lights":[{"position":{"x":0.5,"y":0.5},"range":4,"color":"ffffaa00"}]}"#;
const FOUNDRY: &str = r##"{"name":"S","width":400,"height":300,"grid":{"size":100,"distance":5},"walls":[{"c":[0,0,100,0],"move":20,"sense":20},{"c":[100,0,100,100],"door":1,"ds":1,"move":20,"sense":20}],"lights":[{"x":50,"y":50,"config":{"dim":20,"bright":10,"color":"#ffaa00","alpha":0.8}}]}"##;

/// A tiny deterministic PRNG so the mutations are reproducible run to run.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % (n as u64)) as usize
    }
}

fn mutate(seed: &str, rng: &mut Lcg) -> Vec<u8> {
    let mut b = seed.as_bytes().to_vec();
    let steps = 1 + rng.below(6);
    for _ in 0..steps {
        if b.is_empty() {
            b.push(rng.below(256) as u8);
            continue;
        }
        let i = rng.below(b.len());
        match rng.below(5) {
            0 => b[i] = rng.below(256) as u8, // flip a byte
            1 => b.truncate(i),               // cut the tail
            2 => {
                b.remove(i);
            } // drop a byte
            3 => b.insert(i, rng.below(256) as u8), // insert a byte
            _ => {
                b[i] ^= 0x20;
            } // perturb a character
        }
    }
    b
}

#[test]
fn uvtt_reader_never_panics_on_mutated_input() {
    let mut rng = Lcg(0x5eed_1234);
    for _ in 0..4_000 {
        let bytes = mutate(UVTT, &mut rng);
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let _ = ndcart_formats::uvtt::from_uvtt(text);
        }
        // Also feed raw (possibly non-UTF-8) bytes through a lossy view.
        let lossy = String::from_utf8_lossy(&bytes);
        let _ = ndcart_formats::uvtt::from_uvtt(&lossy);
    }
}

#[test]
fn foundry_reader_never_panics_on_mutated_input() {
    let mut rng = Lcg(0xf00d_1234);
    for _ in 0..4_000 {
        let bytes = mutate(FOUNDRY, &mut rng);
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let _ = ndcart_formats::foundry::from_scene(text);
        }
        let lossy = String::from_utf8_lossy(&bytes);
        let _ = ndcart_formats::foundry::from_scene(&lossy);
    }
}
