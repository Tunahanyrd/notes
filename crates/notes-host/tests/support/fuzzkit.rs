//! Shared deterministic mutation fuzzer for hostile-input tests (no extra
//! dependencies). Included with `#[path]` by the hostile_fuzz tests of
//! several crates; seeds are synthetic fixtures only.
#![allow(dead_code)]
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, Once};

pub struct Rng(u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const INTERESTING: &[&str] = &[
    "0", "-1", "1", "-0", "2147483647", "2147483648", "-2147483649", "4294967295", "4294967296",
    "9223372036854775807", "9223372036854775808", "-9223372036854775808", "-9223372036854775809",
    "99999999999999999999999999", "1e308", "1e309", "-1e309", "NaN", "inf", "0.0000001", "",
    "18446744073709551615", "65535", "65536", "-65536", "3.5", "00", "+1", " 1",
];

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_decode(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() % 4 != 0 { return None; }
    let mut out = Vec::new();
    for chunk in s.chunks(4) {
        let mut v = 0u32;
        let mut pad = 0;
        for &c in chunk {
            v <<= 6;
            if c == b'=' { pad += 1; continue; }
            v |= B64.iter().position(|&b| b == c)? as u32;
        }
        let bytes = [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Some(out)
}

pub fn b64_encode(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in d.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let v = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() { out.push(B64[((v >> (18 - 6 * i)) & 63) as usize]); } else { out.push(b'='); }
        }
    }
    out
}

/// Spans of `[A-Za-z0-9+/=]{24,}` (base64 payloads embedded in the XML).
fn b64_runs(s: &[u8]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let start = i;
        while i < s.len() && (s[i].is_ascii_alphanumeric() || matches!(s[i], b'+' | b'/' | b'=')) { i += 1; }
        if i - start >= 24 && (i - start) % 4 == 0 { runs.push((start, i)); }
        i += 1;
    }
    runs
}

fn digit_runs(s: &[u8]) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i].is_ascii_digit() {
            let start = i;
            while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'.') { i += 1; }
            runs.push((start, i));
        } else {
            i += 1;
        }
    }
    runs
}

pub fn mutate_binary(rng: &mut Rng, d: &mut Vec<u8>) {
    if d.is_empty() { d.push(0); return; }
    match rng.below(7) {
        0 => { let i = rng.below(d.len()); d[i] ^= 1 << rng.below(8); }
        1 => { let i = rng.below(d.len()); d[i] = [0u8, 0xff, 0x7f, 0x80, 1][rng.below(5)]; }
        2 => { // 32-bit field
            let i = rng.below(d.len().saturating_sub(4).max(1));
            let v: u32 = [0, u32::MAX, 0x7fff_ffff, 0x8000_0000, 1 << 20, 0xffff][rng.below(6)];
            for (k, b) in v.to_le_bytes().iter().enumerate() { if i + k < d.len() { d[i + k] = *b; } }
        }
        3 => { let n = rng.below(d.len()); d.truncate(n); }
        4 => { let i = rng.below(d.len()); let n = rng.below(16); for _ in 0..n { d.insert(i, rng.next() as u8); } }
        5 => { // f32 field
            let i = rng.below(d.len().saturating_sub(4).max(1));
            let v: f32 = [f32::NAN, f32::INFINITY, -f32::INFINITY, -1.0, 1e30, -1e30, 0.0][rng.below(7)];
            for (k, b) in v.to_le_bytes().iter().enumerate() { if i + k < d.len() { d[i + k] = *b; } }
        }
        _ => { let a = rng.below(d.len()); let b = (a + rng.below(64)).min(d.len()); let seg = d[a..b].to_vec(); let at = rng.below(d.len()); d.splice(at..at, seg); }
    }
}

pub fn mutate(rng: &mut Rng, src: &[u8]) -> Vec<u8> {
    let mut d = src.to_vec();
    let rounds = 1 + rng.below(3);
    for _ in 0..rounds {
        match rng.below(10) {
            0..=2 => {
                let runs = digit_runs(&d);
                if runs.is_empty() { continue; }
                let (a, b) = runs[rng.below(runs.len())];
                let v = INTERESTING[rng.below(INTERESTING.len())].as_bytes().to_vec();
                d.splice(a..b, v);
            }
            3..=5 => {
                let runs = b64_runs(&d);
                if runs.is_empty() { continue; }
                let (a, b) = runs[rng.below(runs.len())];
                let Some(mut inner) = b64_decode(&d[a..b]) else { continue };
                // Nested base64 (rich text inside XML inside base64) gets a second level.
                let nested = b64_runs(&inner);
                if !nested.is_empty() && rng.below(2) == 0 {
                    let (x, y) = nested[rng.below(nested.len())];
                    if let Some(mut deep) = b64_decode(&inner[x..y]) {
                        mutate_binary(rng, &mut deep);
                        inner.splice(x..y, b64_encode(&deep));
                    }
                } else if rng.below(3) == 0 {
                    let runs = digit_runs(&inner);
                    if !runs.is_empty() {
                        let (x, y) = runs[rng.below(runs.len())];
                        inner.splice(x..y, INTERESTING[rng.below(INTERESTING.len())].as_bytes().to_vec());
                    }
                } else {
                    mutate_binary(rng, &mut inner);
                }
                d.splice(a..b, b64_encode(&inner));
            }
            6 => mutate_binary(rng, &mut d),
            7 => { // attribute value -> interesting
                let quotes: Vec<usize> = d.iter().enumerate().filter(|(_, &c)| c == b'"').map(|(i, _)| i).collect();
                if quotes.len() < 2 { continue; }
                let k = rng.below(quotes.len() / 2) * 2;
                let (a, b) = (quotes[k] + 1, quotes[k + 1]);
                d.splice(a..b, INTERESTING[rng.below(INTERESTING.len())].as_bytes().to_vec());
            }
            8 => { // duplicate an element-ish slice
                let lt: Vec<usize> = d.iter().enumerate().filter(|(_, &c)| c == b'<').map(|(i, _)| i).collect();
                if lt.len() < 2 { continue; }
                let a = lt[rng.below(lt.len())];
                let b = lt[rng.below(lt.len())];
                let (a, b) = (a.min(b), a.max(b));
                let seg = d[a..b].to_vec();
                d.splice(b..b, seg);
            }
            _ => { // delete a tag-to-tag slice
                let lt: Vec<usize> = d.iter().enumerate().filter(|(_, &c)| c == b'<').map(|(i, _)| i).collect();
                if lt.len() < 2 { continue; }
                let a = lt[rng.below(lt.len())];
                let b = lt[rng.below(lt.len())];
                d.drain(a.min(b)..a.max(b));
            }
        }
    }
    d
}


static HOOK: Once = Once::new();
static LAST: Mutex<Option<String>> = Mutex::new(None);

pub fn quiet_hook() {
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name().is_some_and(|n| n.starts_with("fuzz")) {
                let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
                *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(loc);
            } else {
                previous(info);
            }
        }));
    });
}

/// Mutate `seeds` `iters` times (env `NOTES_FUZZ_ITERS` overrides) and feed
/// each input to `target`; returns the distinct panic locations.
pub fn run(name: &'static str, seed: u64, default_iters: usize, seeds: Vec<(Vec<u8>, String)>, target: fn(&[u8], &str)) -> Vec<(String, usize)> {
    run_with_stack(name, seed, default_iters, seeds, target, 2 << 20)
}

/// [`run`] on a thread with `stack` bytes (2 MiB = std default, the sync worker).
pub fn run_with_stack(name: &'static str, seed: u64, default_iters: usize, seeds: Vec<(Vec<u8>, String)>, target: fn(&[u8], &str), stack: usize) -> Vec<(String, usize)> {
    quiet_hook();
    let iters = std::env::var("NOTES_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(default_iters);
    let handle = std::thread::Builder::new().name(format!("fuzz-{name}")).stack_size(stack).spawn(move || {
        assert!(!seeds.is_empty());
        let mut rng = Rng(seed);
        let mut failures: Vec<(String, usize)> = Vec::new();
        let dump = std::env::var("NOTES_FUZZ_DUMP").ok();
        for i in 0..iters {
            let (raw, ct) = &seeds[rng.below(seeds.len())];
            let input = mutate(&mut rng, raw);
            let ok = catch_unwind(AssertUnwindSafe(|| target(&input, ct))).is_ok();
            if !ok {
                let loc = LAST.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default();
                if !failures.iter().any(|(l, _)| *l == loc) {
                    if let Some(dir) = &dump {
                        let _ = std::fs::write(format!("{dir}/crash-{}.bin", failures.len()), &input);
                        let _ = std::fs::write(format!("{dir}/crash-{}.ct", failures.len()), ct);
                    }
                    failures.push((loc, i));
                }
            }
        }
        failures
    }).unwrap();
    let failures = handle.join().unwrap();
    eprintln!("fuzz {name}: {iters} inputs, {} distinct panics", failures.len());
    failures
}

