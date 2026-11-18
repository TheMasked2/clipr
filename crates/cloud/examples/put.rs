//! Uploads a file with the cloud settings from the environment, for trying a bucket by hand:
//! `CLIPR_S3_ENDPOINT`, `CLIPR_S3_REGION`, `CLIPR_S3_BUCKET`, `CLIPR_S3_KEY_ID`, `CLIPR_S3_SECRET`.
//!
//! `cargo run -p clipr-cloud --example put -- <file> <key> [max bytes per second]`

fn main() -> anyhow::Result<()> {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(file), Some(key)) = (args.first(), args.get(1)) else {
        anyhow::bail!("usage: put <file> <key> [max bytes per second]");
    };
    let rate = args.get(2).and_then(|r| r.parse().ok()).unwrap_or(0);
    let bucket = clipr_cloud::s3::Bucket {
        endpoint: env("CLIPR_S3_ENDPOINT"),
        region: env("CLIPR_S3_REGION"),
        bucket: env("CLIPR_S3_BUCKET"),
        key_id: env("CLIPR_S3_KEY_ID"),
        secret: env("CLIPR_S3_SECRET"),
    };
    let start = std::time::Instant::now();
    let mut progress = |n: u64| eprint!("\r{n} bytes");
    bucket.put_file(
        key,
        std::path::Path::new(file),
        "video/mp4",
        rate,
        &mut progress,
        &|| false,
    )?;
    eprintln!("\nput in {:.1} s", start.elapsed().as_secs_f64());
    println!("{}", bucket.get_text(key).map(|t| t.len()).unwrap_or(0));
    bucket.delete(key)?;
    println!("deleted");
    Ok(())
}
