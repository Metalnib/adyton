//! `adyton selfupdate` (specification §12.1): replace this binary with the
//! latest GitHub release. Hand-rolled on ureq + miniserde (no `self_update`
//! crate); integrity + extraction shell out to the system `sha256`/`tar`
//! tools (same pattern as keychain-via-`security`) — zero new dependencies.
//!
//! Order that matters: download → **verify sha256** → **extract** → atomic
//! rename over `current_exe()`. All temp work lives in the exe's own directory
//! so the final rename is same-filesystem (atomic; no `EXDEV`).

use std::io::{IsTerminal as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use miniserde::Deserialize;

use crate::error::{Error, Result};
use crate::wire::json;

const DEFAULT_API: &str = "https://api.github.com/repos/Metalnib/adyton";
const USER_AGENT: &str = concat!("adyton/", env!("CARGO_PKG_VERSION"));
/// Package-manager prefixes: those installs update through their manager.
const MANAGED_PREFIXES: &[&str] = &["/opt/homebrew", "/opt/local", "/nix/store"];
const DOWNLOAD_CAP: u64 = 64 * 1024 * 1024;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub fn self_update(check_only: bool, assume_yes: bool) -> Result<()> {
    let api_base = std::env::var("ADYTON_GITHUB_API").unwrap_or_else(|_| DEFAULT_API.to_owned());
    let current = env!("CARGO_PKG_VERSION");

    let release = fetch_latest(&api_base)?;
    let latest = release.tag_name.trim_start_matches('v');
    match compare(current, latest) {
        Some(std::cmp::Ordering::Equal) => {
            println!("adyton {current} is already the latest release.");
            return Ok(());
        }
        Some(std::cmp::Ordering::Greater) => {
            println!("adyton {current} is ahead of the latest release ({latest}); nothing to do.");
            return Ok(());
        }
        Some(std::cmp::Ordering::Less) => {}
        None => {
            return Err(Error::Provider(format!(
                "cannot compare versions ({current} vs {latest})"
            )));
        }
    }

    println!("update available: adyton {current} → {latest}");
    if check_only {
        return Ok(());
    }

    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| Error::Provider(format!("cannot locate own binary: {e}")))?;
    if let Some(prefix) = MANAGED_PREFIXES.iter().find(|p| exe.starts_with(p)) {
        return Err(Error::Config(format!(
            "adyton is installed under {prefix} — update it through that package manager"
        )));
    }
    if !confirm(current, latest, assume_yes) {
        println!("cancelled.");
        return Ok(());
    }

    let triple = host_triple()?;
    let asset_name = format!("adyton-{}-{triple}.tar.gz", release.tag_name);
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| Error::Provider(format!("release has no asset {asset_name}")))?;
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .ok_or_else(|| Error::Provider("release has no SHA256SUMS.txt".to_owned()))?;

    let dir = exe.parent().unwrap_or_else(|| Path::new("."));
    install_update(
        dir,
        &exe,
        &asset_name,
        &asset.browser_download_url,
        &sums.browser_download_url,
    )?;
    println!("updated to adyton {latest}.");
    Ok(())
}

/// Download → verify → extract → atomic swap, all inside `dir`. The temp
/// subdir is removed on every exit path.
fn install_update(
    dir: &Path,
    exe: &Path,
    asset_name: &str,
    asset_url: &str,
    sums_url: &str,
) -> Result<()> {
    let work = dir.join(format!(".adyton-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir(&work)
        .map_err(|e| Error::Provider(format!("cannot write to {}: {e}", dir.display())))?;
    let result = swap_in(&work, exe, asset_name, asset_url, sums_url);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn swap_in(
    work: &Path,
    exe: &Path,
    asset_name: &str,
    asset_url: &str,
    sums_url: &str,
) -> Result<()> {
    let tarball = work.join("pkg.tar.gz");
    download_to_file(asset_url, &tarball)?;

    let expected = expected_hash(&http_get_string(sums_url)?, asset_name)
        .ok_or_else(|| Error::Provider(format!("{asset_name} missing from SHA256SUMS.txt")))?;
    verify_sha256(&tarball, &expected)?;

    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(work)
        .arg("adyton")
        .status()
        .map_err(|e| Error::Provider(format!("tar failed to start: {e}")))?;
    let extracted = work.join("adyton");
    if !status.success() || !extracted.exists() {
        return Err(Error::Provider(
            "tarball did not contain an adyton binary".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&extracted, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::Provider(format!("chmod failed: {e}")))?;
    }
    // Atomic, same-filesystem (both under the exe's dir).
    std::fs::rename(&extracted, exe).map_err(|e| {
        Error::Config(format!(
            "couldn't replace {} ({e}) — reinstall, or update via your package manager",
            exe.display()
        ))
    })
}

fn confirm(current: &str, latest: &str, assume_yes: bool) -> bool {
    if assume_yes || !std::io::stdin().is_terminal() {
        return true;
    }
    print!("Update adyton {current} → {latest}? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim(), "y" | "Y" | "yes")
}

fn host_triple() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        (os, arch) => Err(Error::Provider(format!(
            "no prebuilt binary for {os}/{arch} — build from source"
        ))),
    }
}

/// 3-part numeric compare; `None` if either side is unparseable.
fn compare(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    Some(parse_version(a)?.cmp(&parse_version(b)?))
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches('v');
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    // Drop any -pre / +build suffix on the patch component.
    let patch = parts
        .next()
        .unwrap_or("0")
        .split(['-', '+'])
        .next()?
        .parse()
        .ok()?;
    Some((major, minor, patch))
}

fn expected_hash(sums: &str, asset_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?;
        (name == asset_name).then(|| hash.to_owned())
    })
}

fn verify_sha256(file: &Path, expected_hex: &str) -> Result<()> {
    let output = sha256_of(file)?;
    let got = output.split_whitespace().next().unwrap_or_default();
    if got.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(Error::Provider(
            "checksum mismatch — refusing to install".to_owned(),
        ))
    }
}

/// Shell out to the system sha256 tool (`shasum -a 256`, else `sha256sum`);
/// returns its `<hex>  <file>` line.
fn sha256_of(file: &Path) -> Result<String> {
    let attempts: [&[&str]; 2] = [&["shasum", "-a", "256"], &["sha256sum"]];
    for argv in attempts {
        let out = std::process::Command::new(argv[0])
            .args(&argv[1..])
            .arg(file)
            .output();
        if let Ok(out) = out
            && out.status.success()
        {
            return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
        }
    }
    Err(Error::Provider(
        "no sha256 tool found (shasum / sha256sum)".to_owned(),
    ))
}

// --- HTTP (a dedicated GET agent; the wire transport is POST-only) ----------

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_mins(2)))
        .build()
        .into()
}

fn fetch_latest(api_base: &str) -> Result<Release> {
    let url = format!("{api_base}/releases/latest");
    let body = agent()
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| Error::Provider(format!("cannot reach {url}: {e}")))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Provider(format!("reading release info: {e}")))?;
    json::from_line(&body)
}

fn http_get_string(url: &str) -> Result<String> {
    agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| Error::Provider(format!("download failed ({url}): {e}")))?
        .body_mut()
        .read_to_string()
        .map_err(|e| Error::Provider(format!("reading {url}: {e}")))
}

fn download_to_file(url: &str, path: &PathBuf) -> Result<()> {
    let mut resp = agent()
        .get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| Error::Provider(format!("download failed ({url}): {e}")))?;
    let mut file = std::fs::File::create(path)
        .map_err(|e| Error::Provider(format!("cannot write {}: {e}", path.display())))?;
    std::io::copy(
        &mut resp.body_mut().as_reader().take(DOWNLOAD_CAP),
        &mut file,
    )
    .map_err(|e| Error::Provider(format!("download interrupted: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{compare, expected_hash, parse_version};

    #[test]
    fn version_ordering() {
        use std::cmp::Ordering;
        assert_eq!(compare("0.1.1", "0.1.1"), Some(Ordering::Equal));
        assert_eq!(compare("0.1.1", "0.1.2"), Some(Ordering::Less));
        assert_eq!(compare("0.2.0", "0.1.9"), Some(Ordering::Greater));
        assert_eq!(
            compare("v0.1.0", "0.1.0"),
            Some(Ordering::Equal),
            "v prefix stripped"
        );
        assert_eq!(
            compare("1.0", "1.0.0"),
            Some(Ordering::Equal),
            "missing patch = 0"
        );
        assert_eq!(
            compare("0.1.2-rc1", "0.1.2"),
            Some(Ordering::Equal),
            "pre-release suffix dropped"
        );
        assert_eq!(
            compare("nightly", "0.1.0"),
            None,
            "unparseable → None, no panic"
        );
    }

    #[test]
    fn parse_version_forms() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("10.20.30"), Some((10, 20, 30)));
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn expected_hash_matches_by_filename() {
        let sums = "aaaa  adyton-v0.1.2-aarch64-apple-darwin.tar.gz\n\
                    bbbb  adyton-v0.1.2-x86_64-unknown-linux-musl.tar.gz\n";
        assert_eq!(
            expected_hash(sums, "adyton-v0.1.2-x86_64-unknown-linux-musl.tar.gz").as_deref(),
            Some("bbbb")
        );
        assert_eq!(expected_hash(sums, "nonexistent.tar.gz"), None);
    }
}
