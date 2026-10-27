//! Ink stroke binaries: the resource wrapper, the frame chain, point channel
//! decoding (compressed / stylus profiles), the stroke record decoder, the
//! style reader, the offline channel replacement and the SPI header check.
//! Input: one selector byte, then the binary.
#![no_main]
use libfuzzer_sys::fuzz_target;
use rs_ink::core::binary as b;

fuzz_target!(|data: &[u8]| {
    let Some((sel, raw)) = notes_fuzz::selector(data) else { return };
    let _ = b::unwrap_stroke_resource(raw);
    let _ = b::inspect_spi(raw);
    let _ = b::parse_frames(raw);
    if let Ok(record) = b::decode_stroke_resource(raw) {
        let _ = b::replace_stroke_channels_offline(&record, &record.samples);
        let count = record.samples.points.len();
        if let Ok(channels) = b::encode_point_channels(&record.samples, record.compressed, record.stylus) {
            let _ = b::rebuild_with_channels(&record, count, &channels);
        }
    }
    // Raw channel decode with a fuzzer-chosen profile and point count.
    let compressed = sel & 1 != 0;
    let stylus = sel & 2 != 0;
    let count = (sel >> 2) as usize * 7 + raw.len() % 7;
    let _ = b::decode_point_channels(raw, count, compressed, stylus);
});
