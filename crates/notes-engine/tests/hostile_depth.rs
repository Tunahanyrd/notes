//! Deeply nested local API JSON bodies must not overflow the 64 MiB
//! connection thread (a stack overflow aborts the engine process).
use rs_engine::pyjson;

fn on_connection_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new().stack_size(64 << 20).spawn(f).unwrap().join().unwrap();
}

#[test]
fn deepest_accepted_body_survives_conversion_and_drop() {
    on_connection_stack(|| {
        let depth = pyjson::MAX_DEPTH - 1;
        let body = format!("{{\"k\":{}{}}}", "[".repeat(depth - 1), "]".repeat(depth - 1));
        let value = pyjson::loads(body.as_bytes()).expect("within the parser bound");
        let _ = pyjson::dumps(&value);
        let converted = notes_engine::util::from_json(&value);
        let _ = rs_store::val::dumps_utf8(&converted);
        drop(converted);
        drop(value);
    });
}

#[test]
fn deeper_bodies_are_invalid_json() {
    on_connection_stack(|| {
        for depth in [pyjson::MAX_DEPTH + 1, 60_000, 500_000] {
            let body = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
            assert!(pyjson::loads(body.as_bytes()).is_err());
        }
    });
}
