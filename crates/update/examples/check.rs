//! Asks GitHub for an update as if clipr were `<version>` and, with `--download <dir>`, downloads and
//! verifies the portable zip (or installer) of what it finds.
//!
//! `cargo run -p clipr-update --example check -- 0.0.1 --beta --download target/upd`

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let version = args.first().map_or(clipr_core::VERSION, String::as_str);
    let beta = args.iter().any(|a| a == "--beta");
    let Some(release) = clipr_update::check(version, beta)? else {
        println!("{version} is up to date");
        return Ok(());
    };
    println!(
        "{} ({}) at {}",
        release.version,
        if release.prerelease { "beta" } else { "stable" },
        release.page
    );
    for asset in [&release.setup, &release.portable].into_iter().flatten() {
        println!("  {} {} bytes, sha256 {:?}", asset.name, asset.size, asset.sha256);
    }
    if let Some(i) = args.iter().position(|a| a == "--download") {
        let dir = std::path::PathBuf::from(args.get(i + 1).map_or("target/upd", String::as_str));
        let asset = release
            .setup
            .as_ref()
            .or(release.portable.as_ref())
            .expect("no asset");
        let path = clipr_update::download(asset, &dir, |done, total| {
            if done == total {
                println!("  {done}/{total}");
            }
        })?;
        println!("verified {}", path.display());
    }
    Ok(())
}
