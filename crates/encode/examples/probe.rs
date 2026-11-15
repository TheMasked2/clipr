//! Prints the GPUs and video encoders found by `clipr_encode::probe`.
//!
//! Run with `cargo run -p clipr-encode --example probe`.

use clipr_encode::Codec;
use clipr_encode::probe::{list_adapters, list_encoders};

fn main() -> anyhow::Result<()> {
    let adapters = list_adapters()?;
    println!("Adapters ({}):", adapters.len());
    for a in &adapters {
        println!(
            "  {} [{:?}, vendor 0x{:04X}]{} - {} MB dedicated",
            a.name,
            a.vendor,
            a.vendor_id,
            if a.software { " (software)" } else { "" },
            a.dedicated_memory / (1024 * 1024),
        );
    }

    let encoders = list_encoders()?;
    println!("Encoders ({}):", encoders.len());
    for codec in [Codec::H264, Codec::Hevc, Codec::Av1] {
        let mut group: Vec<_> = encoders.iter().filter(|e| e.codec == codec).collect();
        group.sort_by_key(|e| !e.hardware);
        println!("  {codec:?}:");
        if group.is_empty() {
            println!("    (none)");
        }
        for e in group {
            match (e.hardware, &e.vendor_id) {
                (true, Some(vendor)) => println!("    [hw] {} (vendor {vendor})", e.name),
                (true, None) => println!("    [hw] {}", e.name),
                (false, _) => println!("    [sw] {}", e.name),
            }
        }
    }
    Ok(())
}
