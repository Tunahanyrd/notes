//! Decode SPI canvas-cache files and print the SHA-256 of their pixel bytes
//! (or the error). With `--raw DIR`, also write `<name>.rgba` files.
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let raw_dir = match args.iter().position(|a| a == "--raw") {
        Some(i) => {
            let dir = PathBuf::from(args.remove(i + 1));
            args.remove(i);
            Some(dir)
        }
        None => None,
    };
    let mut total = notes_preview::spi::Stats::default();
    for path in args {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}\tread-error\t{e}");
                continue;
            }
        };
        match notes_preview::spi::decode_with_stats(&data) {
            Ok((image, stats)) => {
                for i in 0..6 {
                    total.color_modes[i] += stats.color_modes[i];
                    total.alpha_modes[i] += stats.alpha_modes[i];
                }
                for i in 0..18 {
                    total.prediction_modes[i] += stats.prediction_modes[i];
                }
                for i in 0..3 {
                    total.partition_sides[i] += stats.partition_sides[i];
                }
                total.color_intra_q0 += stats.color_intra_q0;
                total.color_intra_quantized += stats.color_intra_quantized;
                total.differential_reduced += stats.differential_reduced;
                total.packets += stats.packets;
                let digest = Sha256::digest(&image.pixels);
                let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
                println!("{path}\tok\t{hex}");
                if let Some(dir) = &raw_dir {
                    let name = PathBuf::from(&path).file_name().unwrap().to_string_lossy().into_owned();
                    std::fs::write(dir.join(format!("{name}.rgba")), &image.pixels).unwrap();
                }
            }
            Err(e) => println!("{path}\terror\t{e}"),
        }
    }
    eprintln!("{total:?}");
}
