//! Per-invocation live context (spec §7): git summary under `git_timeout_ms`,
//! and terminal scrollback via each terminal's own permission-free CLI
//! (tmux/screen/Zellij/WezTerm/kitty). Everything here is best-effort — a miss
//! degrades to absence, never to an error.

use std::time::{Duration, Instant};

use crate::context::probe::EnvFn;
use crate::context::redact;

/// Probe execution with a per-call budget, injected for testability.
pub type TimedRunner<'a> = dyn Fn(&[&str], Duration) -> Option<String> + 'a;

/// §7.4: redacted scrollback is tail-truncated to this many bytes — errors
/// print last, so the tail is the valuable end.
pub const SCROLLBACK_CAP: usize = 8 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct GitSummary {
    pub branch: String,
    pub dirty: bool,
    pub last_subject: Option<String>,
}

impl GitSummary {
    /// One prompt line: `main (dirty) — last: "fix tests"`.
    pub fn to_line(&self) -> String {
        use std::fmt::Write as _;

        let mut line = self.branch.clone();
        if self.dirty {
            line.push_str(" (dirty)");
        }
        if let Some(subject) = &self.last_subject {
            let _ = write!(line, " — last: \"{subject}\"");
        }
        line
    }
}

/// Branch + dirty + last subject inside one cumulative `budget`; whatever
/// misses the deadline is simply absent. Not-a-repo → `None` immediately.
pub fn git_summary(run: &TimedRunner, budget: Duration) -> Option<GitSummary> {
    let deadline = Instant::now() + budget;
    let remaining = |deadline: Instant| deadline.saturating_duration_since(Instant::now());

    let left = remaining(deadline);
    if left.is_zero() {
        return None;
    }
    let branch = run(&["git", "rev-parse", "--abbrev-ref", "HEAD"], left)?;

    // `--quiet` exit-code tricks are ambiguous through an Option runner;
    // `-uno` avoids the untracked-file explosion on big trees.
    let dirty = match remaining(deadline) {
        left if left.is_zero() => false,
        left => {
            run(&["git", "status", "--porcelain", "-uno"], left).is_some_and(|out| !out.is_empty())
        }
    };
    let last_subject = match remaining(deadline) {
        left if left.is_zero() => None,
        left => run(&["git", "log", "-1", "--format=%s"], left),
    };
    Some(GitSummary {
        branch,
        dirty,
        last_subject,
    })
}

/// Scrollback capture (spec §7.4): the current terminal's own permission-free
/// CLI, detected by the env var it sets. Multiplexers take precedence over the
/// host terminal (a tmux pane inside wezterm is what the user sees). Output is
/// redacted (§7.2) then tail-capped before it can reach any prompt. Any miss
/// (not present, remote-control disabled, timeout) degrades to `None`.
pub fn scrollback(env: &EnvFn, run: &TimedRunner, lines: usize) -> Option<String> {
    if lines == 0 {
        return None;
    }
    let timeout = crate::context::probe::PROBE_TIMEOUT;
    let raw = if env("TMUX").is_some() {
        run(
            &["tmux", "capture-pane", "-p", "-S", &format!("-{lines}")],
            timeout,
        )?
    } else if env("STY").is_some() {
        screen_hardcopy(run)?
    } else if env("ZELLIJ").is_some() {
        // `--full` = whole scrollback; no `--path` = print to stdout.
        run(&["zellij", "action", "dump-screen", "--full"], timeout)?
    } else if env("WEZTERM_PANE").is_some() {
        // Defaults to $WEZTERM_PANE; negative start-line reaches into scrollback.
        run(
            &[
                "wezterm",
                "cli",
                "get-text",
                &format!("--start-line=-{lines}"),
            ],
            timeout,
        )?
    } else if env("KITTY_WINDOW_ID").is_some() {
        // Needs `allow_remote_control` in kitty.conf; fails cleanly to None if off.
        run(&["kitty", "@", "get-text", "--extent", "all"], timeout)?
    } else {
        return None;
    };
    let clean = redact::redact_block(&raw);
    Some(tail_bytes(&clean, SCROLLBACK_CAP))
}

/// `screen -X hardcopy -h <file>` writes asynchronously; poll briefly.
fn screen_hardcopy(run: &TimedRunner) -> Option<String> {
    let path = std::env::temp_dir().join(format!("adyton-hardcopy-{}", std::process::id()));
    let path_str = path.to_str()?.to_owned();
    let _ = std::fs::remove_file(&path);
    run(
        &["screen", "-X", "hardcopy", "-h", &path_str],
        crate::context::probe::PROBE_TIMEOUT,
    )?;
    let mut text = None;
    for _ in 0..10 {
        if let Ok(contents) = std::fs::read_to_string(&path)
            && !contents.is_empty()
        {
            text = Some(contents);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_file(&path);
    text
}

/// Keep the last `cap` bytes, aligned to a line start, with a truncation mark.
pub fn tail_bytes(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let start = text.len() - cap;
    let aligned = text[start..]
        .find('\n')
        .map_or(start, |offset| start + offset + 1);
    format!("…(truncated)\n{}", &text[aligned..])
}

#[cfg(test)]
mod tests {
    use super::{GitSummary, SCROLLBACK_CAP, git_summary, scrollback, tail_bytes};
    use std::time::Duration;

    const BUDGET: Duration = Duration::from_millis(200);

    /// Table-driven fake: argv[0..2] match → canned output.
    fn fake_git(
        branch: Option<&'static str>,
        status: &'static str,
        subject: &'static str,
    ) -> impl Fn(&[&str], Duration) -> Option<String> {
        move |argv, _timeout| match argv {
            ["git", "rev-parse", ..] => branch.map(str::to_owned),
            ["git", "status", ..] => Some(status.to_owned()),
            ["git", "log", ..] => Some(subject.to_owned()),
            _ => None,
        }
    }

    #[test]
    fn git_summary_collects_branch_dirty_and_subject() {
        let run = fake_git(Some("main"), " M src/lib.rs", "fix tests");
        assert_eq!(
            git_summary(&run, BUDGET),
            Some(GitSummary {
                branch: "main".to_owned(),
                dirty: true,
                last_subject: Some("fix tests".to_owned()),
            })
        );
    }

    #[test]
    fn clean_tree_is_not_dirty_and_non_repo_is_none() {
        let clean = fake_git(Some("main"), "", "subject");
        assert!(!git_summary(&clean, BUDGET).unwrap().dirty);
        let no_repo = fake_git(None, "", "");
        assert_eq!(git_summary(&no_repo, BUDGET), None);
    }

    #[test]
    fn zero_budget_skips_git_entirely() {
        let run = fake_git(Some("main"), "", "subject");
        assert_eq!(git_summary(&run, Duration::ZERO), None);
    }

    #[test]
    fn git_line_formatting() {
        let summary = GitSummary {
            branch: "main".to_owned(),
            dirty: true,
            last_subject: Some("fix tests".to_owned()),
        };
        assert_eq!(summary.to_line(), "main (dirty) — last: \"fix tests\"");
    }

    #[test]
    fn scrollback_uses_tmux_when_present_and_respects_line_count() {
        let env = |k: &str| (k == "TMUX").then(|| "/tmp/tmux-1".to_owned());
        let run = |argv: &[&str], _: Duration| {
            assert_eq!(
                argv,
                ["tmux", "capture-pane", "-p", "-S", "-120"],
                "line count must ride the -S flag"
            );
            Some("$ make\nerror: boom\n".to_owned())
        };
        let out = scrollback(&env, &run, 120).unwrap();
        assert!(out.contains("error: boom"));
    }

    #[test]
    fn scrollback_uses_zellij_dump_screen_to_stdout() {
        let env = |k: &str| (k == "ZELLIJ").then(|| "0".to_owned());
        let run = |argv: &[&str], _: Duration| {
            assert_eq!(argv, ["zellij", "action", "dump-screen", "--full"]);
            Some("zellij pane text\n".to_owned())
        };
        assert_eq!(
            scrollback(&env, &run, 120).unwrap().trim(),
            "zellij pane text"
        );
    }

    #[test]
    fn scrollback_uses_wezterm_and_kitty_clis_when_detected() {
        let wez_env = |k: &str| (k == "WEZTERM_PANE").then(|| "3".to_owned());
        let wez_run = |argv: &[&str], _: Duration| {
            assert_eq!(argv, ["wezterm", "cli", "get-text", "--start-line=-120"]);
            Some("wez pane text\n".to_owned())
        };
        assert_eq!(
            scrollback(&wez_env, &wez_run, 120).unwrap().trim(),
            "wez pane text"
        );

        let kitty_env = |k: &str| (k == "KITTY_WINDOW_ID").then(|| "1".to_owned());
        let kitty_run = |argv: &[&str], _: Duration| {
            assert_eq!(argv, ["kitty", "@", "get-text", "--extent", "all"]);
            Some("kitty window text\n".to_owned())
        };
        assert_eq!(
            scrollback(&kitty_env, &kitty_run, 120).unwrap().trim(),
            "kitty window text"
        );
    }

    #[test]
    fn multiplexer_takes_precedence_over_the_host_terminal() {
        // Inside tmux inside wezterm, the tmux pane is what the user sees.
        let env = |k: &str| match k {
            "TMUX" | "WEZTERM_PANE" => Some("x".to_owned()),
            _ => None,
        };
        let run = |argv: &[&str], _: Duration| {
            assert_eq!(argv[0], "tmux", "tmux must win over wezterm");
            Some("tmux pane\n".to_owned())
        };
        assert!(scrollback(&env, &run, 120).is_some());
    }

    #[test]
    fn scrollback_absent_without_a_known_terminal_or_when_disabled() {
        let no_env = |_: &str| None;
        let run = |_: &[&str], _: Duration| panic!("must not probe without a known terminal");
        assert_eq!(scrollback(&no_env, &run, 120), None);

        let env = |k: &str| (k == "TMUX").then(|| "x".to_owned());
        assert_eq!(
            scrollback(&env, &run, 0),
            None,
            "scrollback_lines = 0 disables"
        );
    }

    #[test]
    fn scrollback_is_redacted_and_tail_capped() {
        let env = |k: &str| (k == "TMUX").then(|| "x".to_owned());
        let big = format!(
            "export OPENAI_API_KEY=sk-secret-value-123456\n{}last line wins\n",
            "filler line of some length to overflow the cap\n".repeat(300)
        );
        let run = move |_: &[&str], _: Duration| Some(big.clone());
        let out = scrollback(&env, &run, 120).unwrap();
        assert!(!out.contains("sk-secret-value"), "redaction before capping");
        assert!(out.len() <= SCROLLBACK_CAP + 32, "capped (plus marker)");
        assert!(out.starts_with("…(truncated)\n"));
        assert!(out.ends_with("last line wins"), "tail is kept, head is cut");
    }

    #[test]
    fn tail_bytes_keeps_short_text_and_aligns_to_lines() {
        assert_eq!(tail_bytes("short", 100), "short");
        let out = tail_bytes("aaaa\nbbbb\ncccc", 7);
        assert_eq!(out, "…(truncated)\ncccc");
    }
}
