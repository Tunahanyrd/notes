//! Deeply nested sign-in JSON (account/token answers, bound inputs) must be
//! rejected without overflowing a 2 MiB thread (an abort, not a panic).
use notes_login::json;

#[test]
fn deep_json_is_bounded_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(|| {
            for depth in [899, 900, 901, 100_000] {
                let text = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
                let parsed = json::loads(text.as_bytes());
                if let Ok(value) = &parsed {
                    let _ = json::dumps(value, &json::INDENT2);
                }
                if depth > 901 {
                    assert!(parsed.is_err());
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
