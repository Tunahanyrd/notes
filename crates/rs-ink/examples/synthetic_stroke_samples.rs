//! Deterministic synthetic stroke bodies for the offline storage benchmark.
use _ink::core::binary::{decode_stroke_resource, replace_stroke_channels_offline, Samples};
use std::{env, fs};

fn main() {
    let path = env::args_os().nth(1).expect("synthetic template path");
    let record = decode_stroke_resource(&fs::read(path).expect("synthetic template")).expect("valid stroke");
    let mut bodies = Vec::new();
    for variant in 0..87 {
        let count = 64 + variant;
        let mut samples = Samples::default();
        for i in 0..count {
            // Fixed-point paths stay within the template's 9..13 x 18..22 bounds.
            let x = 10.0 + ((i * (variant % 7 + 1) + variant * 3) % 65) as f64 / 32.0;
            let y = 19.0 + ((i * (variant % 11 + 1) + variant * 5) % 65) as f64 / 32.0;
            samples.points.push((x, y));
            samples.pressures.push((1024 + (i * 31 + variant * 23) % 2049) as f64 / 4096.0);
            samples.timestamps.push((i * 8) as i64);
            if record.stylus { samples.tilts.push(0.5); samples.orientations.push(0.0); }
        }
        let body = replace_stroke_channels_offline(&record, &samples).expect("encode synthetic stroke");
        assert_eq!(decode_stroke_resource(&body).expect("decode synthetic stroke").samples, samples);
        bodies.push(rs_doc::util::b64encode(&body));
    }
    println!("{}", serde_json::to_string(&bodies).unwrap());
}
