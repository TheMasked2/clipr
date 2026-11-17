//! Makes a smaller copy of a clip: `cargo run -p clipr-mux --example shrink -- <src.mp4> <dst.mp4> <max MB> <seconds>`

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        a.len() == 4,
        "usage: shrink <src.mp4> <dst.mp4> <max MB> <seconds>"
    );
    let max = a[2].parse::<u64>()? * 1_000_000;
    let t = std::time::Instant::now();
    clipr_mux::shrink(
        std::path::Path::new(&a[0]),
        std::path::Path::new(&a[1]),
        max,
        a[3].parse()?,
    )?;
    let size = std::fs::metadata(&a[1])?.len();
    println!(
        "{} bytes ({:.1} MB of {} MB) in {:.1} s",
        size,
        size as f64 / 1e6,
        a[2],
        t.elapsed().as_secs_f64()
    );
    Ok(())
}
