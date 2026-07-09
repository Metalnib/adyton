//! Readers for the two files the shell glue writes (spec §5.3, §7.3).
//! adyton never writes these — glue hooks do — so this module is parse-only.
//!
//! Session log line format (one record per executed command, tab-separated —
//! trivial for glue to `printf`, unambiguous for commands containing spaces):
//! `ts<TAB>exit<TAB>duration_ms<TAB>cwd<TAB>cmd`

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::context::probe::EnvFn;

/// §5.3: `fix` refuses failure state older than this (exit 5).
pub const FAILURE_MAX_AGE: Duration = Duration::from_mins(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub ts: u64,
    pub exit: i32,
    pub duration_ms: u64,
    pub cwd: String,
    pub cmd: String,
}

/// Last `limit` records; missing file or malformed lines degrade silently —
/// context is best-effort, never a reason to fail the request.
pub fn read_session_tail(path: &Path, limit: usize) -> Vec<SessionRecord> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let records: Vec<SessionRecord> = text.lines().filter_map(parse_record).collect();
    let skip = records.len().saturating_sub(limit);
    records.into_iter().skip(skip).collect()
}

fn parse_record(line: &str) -> Option<SessionRecord> {
    let mut fields = line.splitn(5, '\t');
    Some(SessionRecord {
        ts: fields.next()?.parse().ok()?,
        exit: fields.next()?.parse().ok()?,
        duration_ms: fields.next()?.parse().ok()?,
        cwd: fields.next()?.to_owned(),
        cmd: fields.next()?.to_owned(),
    })
}

/// `${XDG_CACHE_HOME}/adyton/session-<tty>.log`. The tty id comes from the
/// glue via `ADYTON_TTY` (spec §7.3) — a subprocess cannot reliably learn its
/// controlling terminal's name portably, but the shell knows it for free.
pub fn session_log_path(cache_dir: &Path, env: &EnvFn) -> Option<PathBuf> {
    let tty = env("ADYTON_TTY")?;
    let sanitized: String = tty
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(cache_dir.join(format!("session-{sanitized}.log")))
}

// --- failure state (spec §5.3) --------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureState {
    pub exit: i32,
    pub cmd: String,
    pub cwd: String,
    pub ts: u64,
    pub shell: Option<String>,
}

impl FailureState {
    pub fn is_stale(&self, now_unix: u64) -> bool {
        now_unix.saturating_sub(self.ts) > FAILURE_MAX_AGE.as_secs()
    }
}

/// Flat `key=value` record at `${XDG_CACHE_HOME}/adyton/last`.
pub fn read_failure_state(path: &Path) -> Option<FailureState> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut exit = None;
    let mut cmd = None;
    let mut cwd = None;
    let mut ts = None;
    let mut shell = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "exit" => exit = value.parse().ok(),
            "cmd" => cmd = Some(value.to_owned()),
            "cwd" => cwd = Some(value.to_owned()),
            "ts" => ts = value.parse().ok(),
            "shell" => shell = Some(value.to_owned()),
            _ => {}
        }
    }
    Some(FailureState {
        exit: exit?,
        cmd: cmd?,
        cwd: cwd?,
        ts: ts?,
        shell,
    })
}

#[cfg(test)]
mod tests {
    use super::{FailureState, read_failure_state, read_session_tail, session_log_path};
    use std::path::PathBuf;

    struct TempFile(PathBuf);

    impl TempFile {
        fn with(tag: &str, contents: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("adyton-session-{tag}-{}", std::process::id()));
            std::fs::write(&path, contents).unwrap();
            TempFile(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn session_tail_returns_last_n_and_skips_malformed_lines() {
        let file = TempFile::with(
            "tail",
            "1751971000\t0\t42\t/home/u\tls -la\n\
             not a record at all\n\
             1751971010\t1\t100\t/home/u\tcmake ..\n\
             1751971020\t0\t9000\t/home/u/build\tmake -j18\n",
        );
        let tail = read_session_tail(&file.0, 2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].cmd, "cmake ..");
        assert_eq!(tail[0].exit, 1);
        assert_eq!(tail[1].cmd, "make -j18");
        assert_eq!(tail[1].duration_ms, 9000);
    }

    #[test]
    fn session_tail_of_missing_file_or_zero_limit_is_empty() {
        assert!(read_session_tail(std::path::Path::new("/nonexistent"), 10).is_empty());
        let file = TempFile::with("zero", "1\t0\t1\t/\tls\n");
        assert!(read_session_tail(&file.0, 0).is_empty());
    }

    #[test]
    fn commands_containing_tabs_keep_their_tail() {
        let file = TempFile::with("tabs", "1\t0\t1\t/\tprintf 'a\tb'\n");
        let tail = read_session_tail(&file.0, 5);
        assert_eq!(tail[0].cmd, "printf 'a\tb'");
    }

    #[test]
    fn session_log_path_requires_adyton_tty_and_sanitizes_it() {
        let cache = std::path::Path::new("/cache/adyton");
        let env_with_tty = |k: &str| (k == "ADYTON_TTY").then(|| "ttys012".to_owned());
        assert_eq!(
            session_log_path(cache, &env_with_tty).unwrap(),
            std::path::Path::new("/cache/adyton/session-ttys012.log")
        );
        let env_odd = |k: &str| (k == "ADYTON_TTY").then(|| "../evil/../p".to_owned());
        let path = session_log_path(cache, &env_odd).unwrap();
        assert_eq!(
            path,
            std::path::Path::new("/cache/adyton/session-___evil____p.log"),
            "path traversal characters are neutralized"
        );
        assert_eq!(session_log_path(cache, &|_| None), None);
    }

    #[test]
    fn failure_state_roundtrip_and_staleness() {
        let file = TempFile::with(
            "failure",
            "exit=127\ncmd=gti status\ncwd=/home/u/proj\nts=1751971000\nshell=zsh\n",
        );
        let state = read_failure_state(&file.0).unwrap();
        assert_eq!(
            state,
            FailureState {
                exit: 127,
                cmd: "gti status".to_owned(),
                cwd: "/home/u/proj".to_owned(),
                ts: 1_751_971_000,
                shell: Some("zsh".to_owned()),
            }
        );
        assert!(!state.is_stale(state.ts + 599));
        assert!(state.is_stale(state.ts + 601));
    }

    #[test]
    fn incomplete_or_missing_failure_state_is_none() {
        assert_eq!(
            read_failure_state(std::path::Path::new("/nonexistent")),
            None
        );
        let file = TempFile::with("partial", "exit=1\ncmd=x\n");
        assert_eq!(read_failure_state(&file.0), None, "missing cwd/ts");
    }
}
