//! Samsung SPI canvas-cache decoder (notes-preview `spi`), the format the
//! Cloud stores rendered page caches in. Input: an SPI file.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = notes_preview::spi::parse_header(data);
    let _ = notes_preview::spi::decode(data);
});
