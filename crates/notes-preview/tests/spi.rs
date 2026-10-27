//! Canvas-cache (SPI) decoder tests on synthetic fixtures.
//!
//! `tests/fixtures/spi/*.spi` are built by `spi_support` from the format
//! description (regenerate with `cargo test -p notes-preview --test spi --
//! --ignored regenerate_fixtures`). `manifest.tsv` records the SHA-256 of
//! each fixture's decoded pixel bytes; those digests were checked against an
//! independent reference decoder during development (see docs/CANVAS_CACHE.md).
mod spi_support;

use notes_preview::spi::{self, SpiError, Stats};
use sha2::{Digest, Sha256};
use spi_support::{build, Plan, Rng};
use std::path::PathBuf;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/spi")
}

fn hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

const ALL_COLOR: [u64; 6] = [15, 10, 15, 25, 15, 20];
const ALL_ALPHA: [u64; 4] = [20, 10, 55, 15];
const QS: [u8; 13] = [1, 6, 21, 22, 24, 26, 27, 31, 32, 36, 37, 50, 51];

fn plans() -> Vec<(String, Plan)> {
    let base = |w: usize, h: usize, g: usize, seed: u64| Plan {
        width: w,
        height: h,
        row_group: g,
        seed,
        color_weights: ALL_COLOR,
        alpha_weights: ALL_ALPHA,
        quantizers: vec![0],
        modes: vec![],
        reduced: true,
    };
    let mut out = Vec::new();
    let sizes: [(usize, usize); 8] = [(1, 1), (1, 33), (17, 33), (33, 49), (48, 48), (65, 65), (129, 40), (200, 23)];
    for (i, &(w, h)) in sizes.iter().enumerate() {
        let rows = h.div_ceil(16);
        for g in 1..=rows.min(3) {
            let mut mixed = base(w, h, g, 100 + i as u64 * 10 + g as u64);
            mixed.quantizers = vec![0, 0, 24, 23, 51, 1];
            out.push((format!("mixed-{w}x{h}-g{g}"), mixed));
        }
        let mut lossless = base(w, h, 1, 200 + i as u64);
        lossless.color_weights = [5, 5, 5, 70, 5, 10];
        out.push((format!("intra-q0-{w}x{h}"), lossless));
        let mut quantized = base(w, h, rows.min(2), 300 + i as u64);
        quantized.color_weights = [5, 5, 5, 70, 5, 10];
        quantized.quantizers = QS.to_vec();
        out.push((format!("intra-q-{w}x{h}"), quantized));
        let mut differential = base(w, h, 1, 400 + i as u64);
        differential.color_weights = [10, 5, 60, 5, 10, 10];
        out.push((format!("differential-{w}x{h}"), differential));
        let mut palette = base(w, h, rows.min(2), 500 + i as u64);
        palette.color_weights = [10, 5, 5, 5, 70, 5];
        out.push((format!("palette-{w}x{h}"), palette));
        let mut literal = base(w, h, 1, 600 + i as u64);
        literal.alpha_weights = [10, 10, 30, 50];
        out.push((format!("alpha-literal-{w}x{h}"), literal));
    }
    // Every prediction mode with each partition side, lossless and quantized.
    for mode in 0u8..18 {
        let mut p = base(48, 32, 1, 700 + mode as u64);
        p.color_weights = [0, 0, 0, 90, 0, 10];
        p.alpha_weights = [0, 0, 100, 0];
        p.modes = vec![mode, mode, mode, (mode + 1) % 18];
        out.push((format!("mode{mode:02}-q0"), p.clone()));
        p.quantizers = QS.to_vec();
        p.seed += 1000;
        out.push((format!("mode{mode:02}-q"), p));
    }
    out
}

#[test]
#[ignore]
fn regenerate_fixtures() {
    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).unwrap();
    for (name, plan) in plans() {
        std::fs::write(dir.join(format!("{name}.spi")), build(&plan)).unwrap();
    }
}

fn manifest() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixture_dir().join("manifest.tsv")).expect("manifest.tsv");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let mut parts = l.split('\t');
            (parts.next().unwrap().to_string(), parts.next().unwrap().to_string())
        })
        .collect()
}

fn add(total: &mut Stats, s: &Stats) {
    for i in 0..6 {
        total.color_modes[i] += s.color_modes[i];
        total.alpha_modes[i] += s.alpha_modes[i];
    }
    for i in 0..18 {
        total.prediction_modes[i] += s.prediction_modes[i];
    }
    for i in 0..3 {
        total.partition_sides[i] += s.partition_sides[i];
    }
    total.color_intra_q0 += s.color_intra_q0;
    total.color_intra_quantized += s.color_intra_quantized;
    total.differential_reduced += s.differential_reduced;
    total.packets += s.packets;
}

#[test]
fn fixtures_are_reproducible() {
    for (name, plan) in plans() {
        let stored = std::fs::read(fixture_dir().join(format!("{name}.spi"))).expect("fixture");
        assert_eq!(stored, build(&plan), "{name} differs from its plan");
    }
}

#[test]
fn fixtures_decode_to_recorded_pixels() {
    let entries = manifest();
    assert_eq!(entries.len(), plans().len());
    let mut total = Stats::default();
    for (name, expected) in entries {
        let data = std::fs::read(fixture_dir().join(format!("{name}.spi"))).unwrap();
        let (image, stats) = spi::decode_with_stats(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(image.pixels.len(), image.width * image.height * 4);
        assert_eq!(hex(&image.pixels), expected, "{name}");
        add(&mut total, &stats);
    }
    // Coverage: every block mode in both passes, every prediction mode and
    // partition side, both quantizer paths, reduced differential planes and
    // multiple packets.
    assert!(total.color_modes.iter().all(|&n| n > 0), "{total:?}");
    for mode in [0, 1, 3, 5] {
        assert!(total.alpha_modes[mode] > 0, "{total:?}");
    }
    assert!(total.prediction_modes.iter().all(|&n| n > 0), "{total:?}");
    assert!(total.partition_sides.iter().all(|&n| n > 0), "{total:?}");
    assert!(total.color_intra_q0 > 0 && total.color_intra_quantized > 0);
    assert!(total.differential_reduced > 0);
    assert!(total.packets > plans().len());
}

/// Smallest valid image: one literal colour block and one literal alpha block.
fn literal_1x1(color: [u8; 3], alpha: u8) -> Vec<u8> {
    let mut packet = vec![0xaa, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    packet.push(0b0011_0000);
    for c in color {
        packet.extend(std::iter::repeat_n(c, 256));
    }
    packet.push(0b0011_0000);
    packet.extend(std::iter::repeat_n(alpha, 256));
    let length = packet.len() as u32;
    packet[2..6].copy_from_slice(&length.to_be_bytes());
    let mut out = 20u32.to_le_bytes().to_vec();
    out.extend_from_slice(&[0xaa, 1, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 1, 0, 1, 4, 0, 1, 0, 0xe0]);
    out.extend_from_slice(&(packet.len() as u32).to_le_bytes());
    out.extend_from_slice(&packet);
    out
}

#[test]
fn literal_pixel_byte_order() {
    let image = spi::decode(&literal_1x1([0x11, 0x22, 0x33], 0x44)).unwrap();
    // Output order: plane 2, plane 0, plane 1, alpha.
    assert_eq!(image.pixels, vec![0x33, 0x11, 0x22, 0x44]);
}

#[test]
fn rejects_unsupported_configurations() {
    let good = literal_1x1([1, 2, 3], 4);
    let mut flags = good.clone();
    flags[23] = 0xf0;
    assert_eq!(spi::decode(&flags), Err(SpiError::Unsupported("header flags")));
    let mut color = good.clone();
    color[19] = 5;
    assert_eq!(spi::decode(&color), Err(SpiError::Unsupported("colour index")));
    let mut cache = good.clone();
    cache[22] = 1;
    assert_eq!(spi::decode(&cache), Err(SpiError::Unsupported("reference images")));
    let mut byte_b = good.clone();
    byte_b[28 + 9] = 1;
    assert_eq!(spi::decode(&byte_b), Err(SpiError::Unsupported("packet configuration")));
    let mut shortcut = good.clone();
    shortcut[28 + 10] = 0x80;
    assert_eq!(spi::decode(&shortcut), Err(SpiError::Unsupported("packet configuration")));
}

#[test]
fn rejects_malformed_framing() {
    let good = literal_1x1([1, 2, 3], 4);
    assert!(spi::decode(&[]).is_err());
    assert!(spi::decode(&good[..27]).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(spi::decode(&trailing).is_err());
    let mut zero_size = good.clone();
    zero_size[15] = 0;
    zero_size[16] = 0;
    assert!(spi::decode(&zero_size).is_err());
    let mut group = good.clone();
    group[20] = 0;
    group[21] = 2;
    assert!(spi::decode(&group).is_err());
    let mut reserved = good.clone();
    reserved[23] = 0xe1;
    assert!(spi::decode(&reserved).is_err());
    let mut alpha_mode = good.clone();
    let alpha_at = 28 + 14 + 1 + 768;
    alpha_mode[alpha_at] = 0b0010_0000;
    assert!(spi::decode(&alpha_mode).is_err());
}

#[test]
fn rejects_oversized_images_before_allocating() {
    let mut big = literal_1x1([0, 0, 0], 0);
    big[15..17].copy_from_slice(&4096u16.to_be_bytes());
    big[17..19].copy_from_slice(&2048u16.to_be_bytes());
    assert_eq!(spi::decode(&big), Err(SpiError::TooLarge));
    let mut huge = literal_1x1([0, 0, 0], 0);
    huge[15..17].copy_from_slice(&40000u16.to_be_bytes());
    assert!(spi::decode(&huge).is_err());
}

/// Truncation at every length and random corruption never panic and never
/// return an image of the wrong size.
#[test]
fn hostile_input_never_panics() {
    let names = ["mixed-33x49-g2", "intra-q-65x65", "differential-48x48", "palette-129x40", "alpha-literal-129x40"];
    let mut rng = Rng::new(42);
    for name in names {
        let data = std::fs::read(fixture_dir().join(format!("{name}.spi"))).unwrap();
        let step = (data.len() / 400).max(1);
        for cut in (0..data.len()).step_by(step) {
            assert!(spi::decode(&data[..cut]).is_err(), "{name} truncated at {cut}");
        }
        for _ in 0..300 {
            let mut corrupt = data.clone();
            for _ in 0..1 + rng.below(4) {
                let at = 28 + rng.below((corrupt.len() - 28) as u64) as usize;
                corrupt[at] ^= 1 << rng.below(8);
            }
            if let Ok(image) = spi::decode(&corrupt) {
                assert_eq!(image.pixels.len(), image.width * image.height * 4);
            }
        }
        for _ in 0..50 {
            let mut garbage = data[..28].to_vec();
            garbage.extend((0..data.len() - 28).map(|_| rng.byte()));
            let _ = spi::decode(&garbage);
        }
    }
    for len in 0..64 {
        let noise: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        assert!(spi::decode(&noise).is_err());
    }
}

/// Longer corruption run over every fixture (`SPI_FUZZ_ROUNDS`, default 2000).
#[test]
#[ignore]
fn fuzz_all_fixtures() {
    let rounds: u64 = std::env::var("SPI_FUZZ_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(2000);
    let mut rng = Rng::new(7);
    let mut accepted = 0usize;
    for (name, _) in plans() {
        let data = std::fs::read(fixture_dir().join(format!("{name}.spi"))).unwrap();
        for _ in 0..rounds {
            let mut corrupt = data.clone();
            for _ in 0..1 + rng.below(8) {
                let at = rng.below(corrupt.len() as u64) as usize;
                corrupt[at] = if rng.chance(50) { corrupt[at] ^ (1 << rng.below(8)) } else { rng.byte() };
            }
            if let Ok(image) = spi::decode(&corrupt) {
                assert_eq!(image.pixels.len(), image.width * image.height * 4);
                accepted += 1;
            }
        }
    }
    eprintln!("corrupted inputs accepted: {accepted}");
}

#[test]
fn codec_returns_png_of_decoded_pixels() {
    use notes_preview::codec;
    let root = std::path::Path::new("/");
    assert!(codec::provider_available(root));
    for name in ["mixed-33x49-g2", "intra-q-17x33", "alpha-literal-129x40"] {
        let data = std::fs::read(fixture_dir().join(format!("{name}.spi"))).unwrap();
        let image = spi::decode(&data).unwrap();
        let (w, h) = (image.width as i64, image.height as i64);
        let png = codec::decode_spi(root, &data, Some((w, h))).unwrap();
        assert!(notes_preview::png_valid(&png, w, h));
        assert_eq!(rs_render_rgba(&png, w, h), image.pixels);
        assert!(codec::decode(root, &data, (w, h)).is_ok());
        assert_eq!(codec::decode_spi(root, &data, Some((w + 1, h))).unwrap_err().code, "size_mismatch");
        let mut broken = data.clone();
        let last = broken.len() - 1;
        broken[last] ^= 0xff;
        broken.truncate(last);
        assert_eq!(codec::decode_spi(root, &broken, None).unwrap_err().code, "unsupported");
    }
    let mut flags = literal_1x1([0, 0, 0], 0);
    flags[23] = 0xa0;
    assert_eq!(codec::decode_spi(root, &flags, None).unwrap_err().code, "unsupported");
    let mut bad = literal_1x1([0, 0, 0], 0);
    bad[28 + 14] = 0b0001_0000; // colour intra block with no payload
    bad[28 + 14 + 1..].fill(0xff);
    assert_eq!(codec::decode_spi(root, &bad, Some((1, 1))).unwrap_err().code, "decode_failed");
}

fn rs_render_rgba(png: &[u8], w: i64, h: i64) -> Vec<u8> {
    notes_preview::pdf::rgba(png, w, h).expect("rgba")
}
