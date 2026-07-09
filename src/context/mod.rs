//! Context collection (specification §7). S7 ships the machine-facts cache
//! (§7.1); live context — session log, scrollback (§7.3/§7.4) — arrives with
//! S9.
//!
//! Refresh never blocks a request: a stale cache is served as-is while a
//! detached thread rebuilds it during the network call. Version mismatch is
//! treated as *missing* (schema may have changed), so it rebuilds in the
//! foreground once per upgrade.

mod cache;
pub mod live;
mod probe;
pub mod redact;
pub mod session;

use std::time::Instant;

use cache::CacheDir;
use probe::{EnvFn, REBUILD_BUDGET, Runner};

use crate::error::Result;
use crate::paths;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Machine facts for prompt assembly, in schema order (spec §7.1).
#[derive(Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub entries: Vec<(String, String)>,
}

impl Snapshot {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "consumed by S10 prompt assembly")
    )]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// One invocation's collected context (spec §7 table), ready for prompt
/// assembly. All parts optional — `--no-context` yields no bundle at all.
#[derive(Debug, Default)]
pub struct ContextBundle {
    /// Machine facts from the §7.1 cache, schema order.
    pub machine: Vec<(String, String)>,
    pub cwd: Option<String>,
    /// Preformatted git one-liner (`GitSummary::to_line`).
    pub git: Option<String>,
    /// Oldest-first session tail, commands already redacted.
    pub recent: Vec<session::SessionRecord>,
    /// Redacted, tail-capped terminal scrollback.
    pub scrollback: Option<String>,
    /// Data the user piped into adyton (§7.5), redacted and tail-capped.
    /// Explicit input — survives `--no-context`, unlike the ambient fields.
    pub piped: Option<String>,
}

/// Per-invocation bounds, straight from config (spec §3 keys).
#[derive(Debug, Clone, Copy)]
pub struct GatherLimits {
    pub session_log_commands: usize,
    pub scrollback_lines: usize,
    pub git_timeout: std::time::Duration,
}

impl GatherLimits {
    pub fn from_config(config: &crate::config::Config) -> Self {
        GatherLimits {
            session_log_commands: config.session_log_commands,
            scrollback_lines: config.scrollback_lines,
            git_timeout: std::time::Duration::from_millis(config.git_timeout_ms),
        }
    }
}

/// [`gather`] against the real environment and probe runner.
pub fn gather_system(limits: &GatherLimits) -> ContextBundle {
    gather(limits, &real_env, &|argv, timeout| {
        probe::run_probe_with(argv, timeout)
    })
}

/// The §7 table, executed: cache + cwd + git + session tail + scrollback.
/// Everything best-effort; every user-generated line passes §7.2 redaction.
pub fn gather(
    limits: &GatherLimits,
    env: &EnvFn<'_>,
    run: &live::TimedRunner<'_>,
) -> ContextBundle {
    let machine = snapshot().map(|s| s.entries).unwrap_or_default();
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string());
    let git = live::git_summary(run, limits.git_timeout).map(|g| g.to_line());
    let recent = paths::cache_dir()
        .ok()
        .and_then(|dir| session::session_log_path(&dir, env))
        .map(|path| session::read_session_tail(&path, limits.session_log_commands))
        .unwrap_or_default();
    ContextBundle {
        machine,
        cwd,
        git,
        recent: redact_records(recent),
        scrollback: live::scrollback(env, run, limits.scrollback_lines),
        // Attached by the run layer (owns process stdin), not ambient gather.
        piped: None,
    }
}

/// Bound on the stdin read so an accidental `yes | adyton …` can neither spin
/// nor exhaust memory before the tail-cap trims it (§7.5).
const PIPED_READ_CAP: u64 = 256 * 1024;

/// Read piped context from stdin (§7.5). `None` when stdin is a terminal
/// (interactive use — reading would block on EOF) or the pipe is empty.
/// Redacted (§7.2) and tail-capped (§7.4) like scrollback.
pub fn piped_stdin() -> Option<String> {
    use std::io::{IsTerminal as _, Read as _};

    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut buf = Vec::new();
    std::io::stdin()
        .take(PIPED_READ_CAP)
        .read_to_end(&mut buf)
        .ok()?;
    clean_piped(&buf)
}

fn clean_piped(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let clean = redact::redact_block(&text);
    let capped = live::tail_bytes(&clean, live::SCROLLBACK_CAP);
    (!capped.trim().is_empty()).then_some(capped)
}

/// §7.2 over the session tail: secret-tool invocations drop the record,
/// values in surviving commands are masked.
pub fn redact_records(records: Vec<session::SessionRecord>) -> Vec<session::SessionRecord> {
    records
        .into_iter()
        .filter_map(|mut record| {
            record.cmd = redact::redact_line(&record.cmd)?;
            Some(record)
        })
        .collect()
}

/// Cached machine facts, refreshed in the background when stale (spec §7.1).
/// First run builds synchronously (2 s budget → degraded on overrun); the
/// spawned refresh is detached — an early process exit is safe because
/// writes are atomic.
pub fn snapshot() -> Result<Snapshot> {
    let dir = CacheDir::new(paths::cache_dir()?);
    let (snapshot, needs_refresh) = load(&dir, &probe::run_probe, &real_env, VERSION);
    if needs_refresh {
        let dir_path = paths::cache_dir()?;
        std::thread::spawn(move || {
            let _ = rebuild_locked(
                &CacheDir::new(dir_path),
                &probe::run_probe,
                &real_env,
                VERSION,
            );
        });
    }
    Ok(snapshot)
}

/// `adyton context refresh` (spec §2): foreground rebuild through the same
/// single-flight lock; a refresh already in flight makes this a no-op.
pub fn refresh_foreground() -> Result<()> {
    let dir = CacheDir::new(paths::cache_dir()?);
    rebuild_locked(&dir, &probe::run_probe, &real_env, VERSION).map(|_| ())
}

fn real_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Core load: serve fresh, serve stale + flag for background refresh, or
/// build now (missing/unreadable/version-mismatch).
fn load(dir: &CacheDir, run: &Runner<'_>, env: &EnvFn<'_>, version: &str) -> (Snapshot, bool) {
    match dir.read() {
        Some(entries) if version_of(&entries) == Some(version) => {
            let stale = dir.age().is_none_or(|age| age >= cache::STALE_AFTER);
            (Snapshot { entries }, stale)
        }
        _ => {
            let entries = probe::gather(run, env, version, Instant::now() + REBUILD_BUDGET);
            // Best-effort: a snapshot is useful even if the disk write fails.
            let _ = dir.write_atomic(&entries);
            (Snapshot { entries }, false)
        }
    }
}

/// Rebuild under the single-flight lock; `Ok(false)` means another refresh
/// holds it.
fn rebuild_locked(
    dir: &CacheDir,
    run: &Runner<'_>,
    env: &EnvFn<'_>,
    version: &str,
) -> Result<bool> {
    let Some(_guard) = dir.try_lock() else {
        return Ok(false);
    };
    let entries = probe::gather(run, env, version, Instant::now() + REBUILD_BUDGET);
    dir.write_atomic(&entries)?;
    Ok(true)
}

fn version_of(entries: &[(String, String)]) -> Option<&str> {
    entries
        .iter()
        .find(|(k, _)| k == "adyton")
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::{cache::CacheDir, clean_piped, load, rebuild_locked};
    use std::time::{Duration, SystemTime};

    #[test]
    fn clean_piped_redacts_and_drops_empty() {
        assert_eq!(clean_piped(b""), None);
        assert_eq!(
            clean_piped(b"   \n\n"),
            None,
            "whitespace-only pipe is nothing"
        );
        let out = clean_piped(b"listening on 127.0.0.1:52198\nTOKEN=sk-piped-secret-000000\n")
            .expect("non-empty");
        assert!(out.contains("52198"), "useful content kept");
        assert!(
            !out.contains("sk-piped-secret"),
            "secrets redacted before send"
        );
    }

    #[test]
    fn clean_piped_tail_caps_large_input() {
        let big = "x".repeat(20_000);
        let out = clean_piped(big.as_bytes()).expect("non-empty");
        assert!(out.len() <= super::live::SCROLLBACK_CAP + 32, "tail-capped");
    }

    fn temp_cache(tag: &str) -> CacheDir {
        let dir = std::env::temp_dir().join(format!("adyton-ctx-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        CacheDir::new(dir)
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_shell(key: &str) -> Option<String> {
        (key == "SHELL").then(|| "/bin/zsh".to_owned())
    }

    fn fake_runner(argv: &[&str]) -> Option<String> {
        (argv == ["uname", "-sr"]).then(|| "Darwin 25.5.0".to_owned())
    }

    fn backdate(cache: &CacheDir, by: Duration) {
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(cache.file())
            .unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    #[test]
    fn first_run_builds_synchronously_and_writes_the_cache() {
        let cache = temp_cache("first-run");
        let (snapshot, needs_refresh) = load(&cache, &fake_runner, &env_shell, "0.1.0");

        assert!(!needs_refresh, "a just-built cache is fresh");
        assert_eq!(snapshot.get("kernel"), Some("Darwin 25.5.0"));
        assert_eq!(snapshot.get("adyton"), Some("0.1.0"));
        assert!(cache.file().exists(), "first run persists the cache");
    }

    #[test]
    fn degraded_first_run_still_carries_os_arch_shell() {
        // Every probe fails (missing binaries / timeouts) — the env/const
        // floor must survive.
        let cache = temp_cache("degraded");
        let (snapshot, _) = load(&cache, &|_| None, &env_shell, "0.1.0");

        // os survives probe-less (env const on macOS; /etc/os-release on Linux) —
        // presence is the contract, not a specific string.
        assert!(snapshot.get("os").is_some_and(|v| !v.is_empty()));
        assert_eq!(snapshot.get("arch"), Some(std::env::consts::ARCH));
        assert_eq!(snapshot.get("shell"), Some("zsh"));
        assert_eq!(snapshot.get("kernel"), None);
    }

    #[test]
    fn fresh_cache_is_served_without_refresh() {
        let cache = temp_cache("fresh");
        cache
            .write_atomic(&[
                ("os".to_owned(), "cached-os".to_owned()),
                ("adyton".to_owned(), "0.1.0".to_owned()),
            ])
            .unwrap();

        let (snapshot, needs_refresh) = load(
            &cache,
            &|_| panic!("fresh cache must not probe"),
            &no_env,
            "0.1.0",
        );
        assert_eq!(snapshot.get("os"), Some("cached-os"));
        assert!(!needs_refresh);
    }

    #[test]
    fn stale_cache_is_served_and_flagged_for_background_refresh() {
        let cache = temp_cache("stale");
        cache
            .write_atomic(&[
                ("os".to_owned(), "old-os".to_owned()),
                ("adyton".to_owned(), "0.1.0".to_owned()),
            ])
            .unwrap();
        backdate(&cache, Duration::from_hours(25));

        let (snapshot, needs_refresh) = load(
            &cache,
            &|_| panic!("stale serve must not probe"),
            &no_env,
            "0.1.0",
        );
        assert_eq!(
            snapshot.get("os"),
            Some("old-os"),
            "stale data is served as-is"
        );
        assert!(needs_refresh, "caller must spawn the background refresh");
    }

    #[test]
    fn version_mismatch_rebuilds_in_the_foreground() {
        let cache = temp_cache("version");
        cache
            .write_atomic(&[
                ("os".to_owned(), "ancient".to_owned()),
                ("adyton".to_owned(), "0.0.1".to_owned()),
            ])
            .unwrap();

        let (snapshot, needs_refresh) = load(&cache, &fake_runner, &env_shell, "0.1.0");
        assert_eq!(
            snapshot.get("adyton"),
            Some("0.1.0"),
            "rebuilt with current version"
        );
        assert_ne!(snapshot.get("os"), Some("ancient"));
        assert!(!needs_refresh);
        assert_eq!(
            cache
                .read()
                .unwrap()
                .iter()
                .find(|(k, _)| k == "adyton")
                .map(|(_, v)| v.as_str()),
            Some("0.1.0"),
            "cache file rewritten"
        );
    }

    #[test]
    fn rebuild_is_a_no_op_while_the_lock_is_held() {
        let cache = temp_cache("locked");
        cache
            .write_atomic(&[("os".to_owned(), "kept".to_owned())])
            .unwrap();
        let _guard = cache.try_lock().expect("hold the lock");

        let did_work = rebuild_locked(&cache, &fake_runner, &env_shell, "0.1.0").unwrap();
        assert!(!did_work);
        assert_eq!(
            cache.read().unwrap(),
            vec![("os".to_owned(), "kept".to_owned())],
            "cache untouched while another refresh runs"
        );
    }

    #[test]
    fn rebuild_replaces_the_cache_when_it_gets_the_lock() {
        let cache = temp_cache("rebuild");
        cache
            .write_atomic(&[("os".to_owned(), "old".to_owned())])
            .unwrap();

        let did_work = rebuild_locked(&cache, &fake_runner, &env_shell, "0.1.0").unwrap();
        assert!(did_work);
        let rebuilt = cache.read().unwrap();
        assert!(
            rebuilt
                .iter()
                .any(|(k, v)| k == "kernel" && v == "Darwin 25.5.0")
        );
        assert!(cache.try_lock().is_some(), "lock released after rebuild");
    }
}
