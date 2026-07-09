//! Cache file I/O (specification §7.1): flat kv at 0600, torn-write-proof via
//! `context.tmp` + atomic rename, single-flight via a `context.lock` created
//! `O_EXCL` and considered stale after 60 s.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};

pub(crate) const STALE_AFTER: Duration = Duration::from_hours(24);
pub(crate) const LOCK_STALE_AFTER: Duration = Duration::from_mins(1);

pub(crate) struct CacheDir {
    dir: PathBuf,
}

impl CacheDir {
    pub(crate) fn new(dir: PathBuf) -> Self {
        CacheDir { dir }
    }

    pub(crate) fn file(&self) -> PathBuf {
        self.dir.join("context")
    }

    fn tmp(&self) -> PathBuf {
        self.dir.join("context.tmp")
    }

    fn lock(&self) -> PathBuf {
        self.dir.join("context.lock")
    }

    /// Parsed cache, `None` when missing/unreadable. Lenient line parsing:
    /// we only ever read what `write_atomic` wrote, so anything malformed is
    /// simply skipped rather than failing the whole snapshot.
    pub(crate) fn read(&self) -> Option<Vec<(String, String)>> {
        let text = std::fs::read_to_string(self.file()).ok()?;
        let entries: Vec<(String, String)> = text
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once('=')?;
                Some((key.trim().to_owned(), value.trim().to_owned()))
            })
            .collect();
        Some(entries)
    }

    pub(crate) fn age(&self) -> Option<Duration> {
        let modified = std::fs::metadata(self.file()).ok()?.modified().ok()?;
        std::time::SystemTime::now().duration_since(modified).ok()
    }

    /// Write-then-rename so an abandoned refresh (process exit mid-write)
    /// can never tear the cache — the old file survives intact.
    pub(crate) fn write_atomic(&self, entries: &[(String, String)]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|err| Error::Config(format!("create {}: {err}", self.dir.display())))?;
        let tmp = self.tmp();
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|err| Error::Config(format!("write {}: {err}", tmp.display())))?;
        let mut text = String::new();
        for (key, value) in entries {
            text.push_str(key);
            text.push_str(" = ");
            text.push_str(value);
            text.push('\n');
        }
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|err| Error::Config(format!("write {}: {err}", tmp.display())))?;
        drop(file);
        std::fs::rename(&tmp, self.file())
            .map_err(|err| Error::Config(format!("rename {}: {err}", tmp.display())))
    }

    /// Single-flight refresh lock. `None` means another refresh is running
    /// (fresh lock). A lock older than 60 s is from a dead process — break it.
    pub(crate) fn try_lock(&self) -> Option<LockGuard> {
        std::fs::create_dir_all(&self.dir).ok()?;
        let path = self.lock();
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let _ = write!(file, "{}", std::process::id());
                    return Some(LockGuard { path });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists && attempt == 0 => {
                    if lock_is_stale(&path) {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    return None;
                }
                Err(_) => return None,
            }
        }
        None
    }
}

fn lock_is_stale(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        // Vanished between create_new and here: the holder just finished.
        return false;
    };
    metadata
        .modified()
        .ok()
        .and_then(|m| std::time::SystemTime::now().duration_since(m).ok())
        .is_some_and(|age| age >= LOCK_STALE_AFTER)
}

pub(crate) struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheDir, LOCK_STALE_AFTER};
    use std::time::{Duration, SystemTime};

    fn temp_cache(tag: &str) -> CacheDir {
        let dir = std::env::temp_dir().join(format!("adyton-cache-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        CacheDir::new(dir)
    }

    fn entries() -> Vec<(String, String)> {
        vec![
            ("os".to_owned(), "macOS 15.5".to_owned()),
            ("arch".to_owned(), "arm64".to_owned()),
        ]
    }

    #[test]
    fn write_atomic_then_read_roundtrip_with_private_permissions() {
        let cache = temp_cache("roundtrip");
        cache.write_atomic(&entries()).unwrap();
        assert_eq!(cache.read().unwrap(), entries());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(cache.file())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn read_is_none_when_missing_and_ignores_a_torn_tmp() {
        let cache = temp_cache("torn");
        assert!(cache.read().is_none());

        // A dead refresh left a torn tmp behind: reads ignore it, and the
        // next write_atomic simply replaces it.
        std::fs::create_dir_all(cache.file().parent().unwrap()).unwrap();
        std::fs::write(cache.file().with_extension("tmp"), "os = torn-mid-wr").unwrap();
        assert!(
            cache.read().is_none(),
            "tmp file must never be read as the cache"
        );

        cache.write_atomic(&entries()).unwrap();
        assert_eq!(cache.read().unwrap(), entries());
    }

    #[test]
    fn lock_is_single_flight_and_released_on_drop() {
        let cache = temp_cache("lock");
        let guard = cache.try_lock().expect("first lock");
        assert!(
            cache.try_lock().is_none(),
            "fresh lock must not be taken twice"
        );
        drop(guard);
        assert!(
            cache.try_lock().is_some(),
            "released lock is available again"
        );
    }

    #[test]
    fn stale_lock_is_broken() {
        let cache = temp_cache("stale-lock");
        let guard = cache.try_lock().expect("lock");
        // Forget the guard so the lock file survives, then backdate it past
        // the 60 s staleness horizon — a dead process's leftover.
        std::mem::forget(guard);
        let lock_path = cache.file().with_extension("lock");
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&lock_path)
            .unwrap();
        file.set_modified(SystemTime::now() - LOCK_STALE_AFTER - Duration::from_secs(5))
            .unwrap();

        assert!(
            cache.try_lock().is_some(),
            "stale lock must be broken and re-acquired"
        );
    }

    #[test]
    fn age_reflects_backdated_mtime() {
        let cache = temp_cache("age");
        cache.write_atomic(&entries()).unwrap();
        assert!(cache.age().unwrap() < Duration::from_secs(5));

        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(cache.file())
            .unwrap();
        file.set_modified(SystemTime::now() - Duration::from_hours(25))
            .unwrap();
        assert!(cache.age().unwrap() > Duration::from_hours(24));
    }
}
