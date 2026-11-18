//! Bring-your-own cloud storage: clips are copied to the user's own bucket on Cloudflare R2, Amazon S3,
//! Backblaze B2 or another S3-compatible service. There's no server of ours: clipr talks to the bucket
//! directly with the user's key. Share pages and the online gallery come in a later phase.

pub mod queue;
pub mod s3;

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clipr_core::config::CloudConfig;

pub use queue::{CloudStatus, Job, JobState, Uploader};

/// The bucket the settings point at.
pub fn bucket(config: &CloudConfig) -> s3::Bucket {
    s3::Bucket {
        endpoint: config.endpoint.trim().to_string(),
        region: match config.region.trim() {
            "" => "auto".to_string(),
            r => r.to_string(),
        },
        bucket: config.bucket.trim().to_string(),
        key_id: config.access_key_id.trim().to_string(),
        secret: config.secret_access_key.trim().to_string(),
    }
}

/// The folder in the bucket: no leading `/`, one trailing `/` (or nothing).
pub fn prefix(raw: &str) -> String {
    let p = raw.trim().trim_matches('/');
    if p.is_empty() {
        String::new()
    } else {
        format!("{p}/")
    }
}

/// The public link to `key`, if the bucket has a public address.
pub fn public_url(config: &CloudConfig, key: &str) -> Option<String> {
    let base = config.public_url.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/{}", s3::encode_path(key)))
}

/// The cloud settings' ranges and formats.
pub fn check(c: &CloudConfig) -> Result<()> {
    if !c.endpoint.trim().is_empty() {
        s3::split_endpoint(&c.endpoint).context("Cloud endpoint")?;
    }
    let bucket = c.bucket.trim();
    if !bucket.is_empty()
        && (bucket.len() < 3
            || bucket.len() > 63
            || !bucket
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.'))
    {
        bail!("A bucket name is 3 to 63 lowercase letters, digits, dots and dashes");
    }
    if c.region.len() > 40
        || !c
            .region
            .trim()
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        bail!("The region is letters, digits and dashes, like auto or us-east-1");
    }
    if c.access_key_id.len() > 200 || c.secret_access_key.len() > 200 {
        bail!("That access key is too long");
    }
    let public = c.public_url.trim();
    if !public.is_empty() {
        let host = match public.strip_prefix("https://") {
            Some(h) => h,
            // A local test bucket only.
            None => public
                .strip_prefix("http://")
                .filter(|h| {
                    let name = h.split('/').next().unwrap_or("");
                    let name = name.rsplit_once(':').map_or(name, |(n, _)| n);
                    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
                })
                .context("The public address must start with https://")?,
        };
        if host.trim_end_matches('/').is_empty() || host.contains(['@', '?', '#', ' ']) {
            bail!(
                "The public address must be a link like https://pub-abc.r2.dev or https://clips.example.com"
            );
        }
    }
    let p = prefix(&c.prefix);
    if p.len() > 100 || p.split('/').any(|part| part == "." || part == "..") || p.contains(['\\', '?', '#']) {
        bail!("The bucket folder can't contain .., \\, ? or #, and is at most 100 characters");
    }
    if c.max_mbps > 10_000 {
        bail!("The upload speed limit is at most 10000 Mbit/s");
    }
    if c.enabled && !c.ready() {
        bail!("To turn on cloud storage, fill in the endpoint, the bucket, the access key and the secret");
    }
    Ok(())
}

/// Whether `config` is complete and well-formed enough to [`test`].
pub fn testable(config: &CloudConfig) -> Result<()> {
    check(&CloudConfig {
        enabled: false,
        ..config.clone()
    })?;
    if config.endpoint.trim().is_empty()
        || config.bucket.trim().is_empty()
        || config.access_key_id.trim().is_empty()
        || config.secret_access_key.trim().is_empty()
    {
        bail!("Fill in the endpoint, the bucket, the access key and the secret first");
    }
    Ok(())
}

/// Tries the settings: writes a small file, reads it back (and through the public address, if set), then
/// deletes it. Returns what worked, for the user.
pub fn test(config: &CloudConfig) -> Result<String> {
    testable(config)?;
    let b = bucket(config);
    let key = format!("{}clipr-test.txt", prefix(&config.prefix));
    let text = format!("clipr can write here ({})", clipr_core::VERSION);
    b.put_text(&key, &text).context("Uploading a test file failed")?;
    let result = (|| {
        let back = b.get_text(&key).context("Reading the test file back failed")?;
        if back != text {
            bail!("The bucket gave back something else than clipr wrote");
        }
        let Some(url) = public_url(config, &key) else {
            return Ok("Connected: clipr can upload to the bucket. Without a public address, Discord gets a smaller copy instead of a link.".to_string());
        };
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        let mut r = agent
            .get(&url)
            .call()
            .with_context(|| format!("can't reach {url}"))?;
        let status = r.status().as_u16();
        let got = r
            .body_mut()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        if status != 200 || got != text {
            bail!(
                "Connected, but the public address doesn't show the bucket ({status} for {url}). Check that public access is on and that the address points at this bucket{}",
                if prefix(&config.prefix).is_empty() {
                    ""
                } else {
                    " (without the folder)"
                }
            );
        }
        Ok("Connected: clipr can upload, and the public address works.".to_string())
    })();
    let _ = b.delete(&key);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> CloudConfig {
        CloudConfig {
            enabled: true,
            endpoint: "https://acc.r2.cloudflarestorage.com".into(),
            bucket: "my-clips".into(),
            access_key_id: "k".into(),
            secret_access_key: "s".into(),
            public_url: "https://pub-1.r2.dev/".into(),
            ..CloudConfig::default()
        }
    }

    #[test]
    fn settings_are_checked() {
        assert!(check(&cfg()).is_ok());
        assert!(check(&CloudConfig::default()).is_ok(), "off and empty is fine");
        let bad = [
            CloudConfig {
                endpoint: "http://acc.r2.cloudflarestorage.com".into(),
                ..cfg()
            },
            CloudConfig {
                endpoint: "https://evil.com/x".into(),
                ..cfg()
            },
            CloudConfig {
                bucket: "My_Clips".into(),
                ..cfg()
            },
            CloudConfig {
                region: "us east".into(),
                ..cfg()
            },
            CloudConfig {
                public_url: "javascript:alert(1)".into(),
                ..cfg()
            },
            CloudConfig {
                prefix: "../up".into(),
                ..cfg()
            },
            CloudConfig {
                max_mbps: 20_000,
                ..cfg()
            },
            CloudConfig {
                secret_access_key: String::new(),
                ..cfg()
            },
        ];
        for c in bad {
            assert!(check(&c).is_err(), "{c:?}");
        }
    }

    #[test]
    fn keys_and_links() {
        assert_eq!(prefix(" /clipr// "), "clipr/");
        assert_eq!(prefix(""), "");
        assert_eq!(
            queue::key_for(&cfg(), "Rocket League/a b.mp4"),
            "clipr/Rocket League/a b.mp4"
        );
        assert_eq!(
            public_url(&cfg(), "clipr/Rocket League/a b.mp4").as_deref(),
            Some("https://pub-1.r2.dev/clipr/Rocket%20League/a%20b.mp4")
        );
        assert_eq!(
            public_url(
                &CloudConfig {
                    public_url: String::new(),
                    ..cfg()
                },
                "k"
            ),
            None
        );
        assert_eq!(
            bucket(&CloudConfig {
                region: String::new(),
                ..cfg()
            })
            .region,
            "auto"
        );
    }
}
