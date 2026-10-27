//! The four JSON readers: rs-store `val::loads` (library, snapshots, cloud
//! receipts), notes-cloud `json::loads` (bytes + BOM), rs-engine `pyjson`
//! (local API bodies, strict and lenient) and notes-login `json::loads`
//! (sign-in responses), each followed by its serializer. Input: JSON bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        if let Ok(v) = rs_store::val::loads(text) {
            let _ = rs_store::val::dumps(&v, true, true);
            let _ = rs_store::val::dumps(&v, false, false);
            let _ = notes_cloud::json::dumps(&v, &notes_cloud::json::INDENT2);
        }
    }
    let _ = notes_cloud::json::loads(data);
    if let Ok(v) = rs_engine::pyjson::loads(data) {
        let _ = rs_engine::pyjson::dumps(&v);
        let val = notes_engine::util::from_json(&v);
        let _ = notes_engine::util::dumps(&val);
    }
    if let Ok(v) = rs_engine::pyjson::loads_lenient(data) {
        let _ = rs_engine::pyjson::dumps(&v);
    }
    if let Ok(v) = notes_login::json::loads(data) {
        let _ = notes_login::json::dumps(&v, &notes_login::json::INDENT2);
    }
    let _ = notes_protocol::entrypoint::EntryPointMetadata::parse(data);
});
