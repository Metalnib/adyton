//! `suggest` and `fix` (S10/S11): the spec §2.1 pipelines — args → context →
//! adapter → event stream → exactly the command on stdout. All feedback rides
//! the stderr overlay; stdout stays pure for the shell glue to insert.

use std::io::Read as _;
use std::time::{Duration, Instant};

use crate::cli::{RunOpts, Shell};
use crate::config::{self, Config, TokenParam, WireKind};
use crate::context::session::FailureState;
use crate::context::{self, GatherLimits, live, redact, session};
use crate::error::{Error, Result};
use crate::overlay::Overlay;
use crate::wire::event::Event;
use crate::wire::http::{HttpRequest, Transport, UreqTransport};
use crate::{paths, prompt, wire};

/// Everything a request needs, resolved once and owned — `suggest` and `fix`
/// share this preamble.
struct Session {
    cfg: Config,
    profile_name: String,
    wire_kind: WireKind,
    base_url: String,
    model: String,
    extra_headers: Vec<String>,
    max_tokens: u32,
    token_param: TokenParam,
    temperature: Option<f64>,
    extra_body: Option<String>,
    key: config::ResolvedKey,
}

fn prepare(opts: &RunOpts) -> Result<Session> {
    let config_path = paths::config_file()?;
    let cfg = Config::load(&config_path)?;
    let (name, profile) = cfg.select_profile(opts.profile.as_deref())?;
    let missing = profile.missing_required();
    if !missing.is_empty() {
        return Err(Error::Config(format!(
            "profile \"{name}\": missing required {}",
            missing.join(", ")
        )));
    }
    let key = config::resolve_api_key(opts.api_key.as_deref(), name, profile, &|var| {
        std::env::var(var).ok()
    })?;
    Ok(Session {
        profile_name: name.to_owned(),
        wire_kind: profile.wire.expect("required checked"),
        base_url: profile.base_url.clone().expect("required checked"),
        model: profile.model.clone().expect("required checked"),
        extra_headers: profile.extra_headers.clone(),
        max_tokens: profile.max_tokens,
        token_param: profile.token_param(),
        temperature: profile.temperature,
        extra_body: profile.extra_body.clone(),
        key,
        cfg,
    })
}

fn thinking_enabled(opts: &RunOpts, cfg: &Config) -> bool {
    cfg.show_thinking && !opts.no_thinking
}

pub fn suggest(opts: &RunOpts, query: &str) -> Result<()> {
    let session = prepare(opts)?;
    let overlay = Overlay::start(opts.plain, thinking_enabled(opts, &session.cfg));
    let (system, user) = {
        overlay.phase("context");
        let bundle = collect_context(opts, &session.cfg);
        (
            prompt::system_prompt(Some(&shell_hint(opts)), bundle.as_ref()),
            prompt::user_suggest(query),
        )
    };
    let outcome = complete(&session, &overlay, &system, &user);
    drop(overlay); // clear the status line before anything hits stdout
    emit(&session, outcome?)
}

pub fn fix(opts: &RunOpts, rerun: bool) -> Result<()> {
    let session = prepare(opts)?;
    let state_path = paths::cache_dir()?.join("last");
    let failure = session::read_failure_state(&state_path).ok_or_else(|| {
        Error::StaleState(
            "no recorded failure — the shell glue records one when a command fails".to_owned(),
        )
    })?;
    if failure.is_stale(unix_now()) {
        return Err(Error::StaleState(format!(
            "the last recorded failure ({}) is older than 10 minutes",
            failure.cmd
        )));
    }

    let overlay = Overlay::start(opts.plain, thinking_enabled(opts, &session.cfg));
    let rerun_output = if rerun {
        overlay.phase("rerun");
        // The rerun executes the ORIGINAL command; only the prompt below
        // sees the redacted form.
        rerun_capture(
            &failure,
            Duration::from_secs((session.cfg.timeout_seconds / 4).max(1)),
        )
    } else {
        None
    };
    // §7.2 applies to the failed command line itself — it may embed a secret
    // (export X=…) or *be* a secret-tool invocation.
    let failure = FailureState {
        cmd: redact::redact_line(&failure.cmd)
            .unwrap_or_else(|| "«redacted secret-tool invocation»".to_owned()),
        ..failure
    };
    overlay.phase("context");
    let bundle = collect_context(opts, &session.cfg);
    let system = prompt::system_prompt(Some(&shell_hint(opts)), bundle.as_ref());
    let user = prompt::user_fix(&failure, rerun_output.as_deref());
    let outcome = complete(&session, &overlay, &system, &user);
    drop(overlay);
    emit(&session, outcome?)
}

/// Ambient context is gathered unless `--no-context`; piped stdin (§7.5) is
/// explicit, so it is attached either way (creating a bundle if needed).
fn collect_context(opts: &RunOpts, cfg: &Config) -> Option<context::ContextBundle> {
    let mut bundle =
        (!opts.no_context).then(|| context::gather_system(&GatherLimits::from_config(cfg)));
    if let Some(piped) = context::piped_stdin() {
        bundle.get_or_insert_default().piped = Some(piped);
    }
    bundle
}

fn shell_hint(opts: &RunOpts) -> String {
    shell_name(opts.shell, &|var| std::env::var(var).ok())
}

fn request_for(session: &Session, system: &str, user: &str) -> HttpRequest {
    wire::build_request(
        session.wire_kind,
        &session.base_url,
        session.key.secret.as_ref(),
        &session.extra_headers,
        &wire::ChatParams {
            model: &session.model,
            system,
            user,
            max_tokens: session.max_tokens,
            token_param: session.token_param,
            temperature: session.temperature,
            extra_body: session.extra_body.as_deref(),
        },
    )
}

fn complete(session: &Session, overlay: &Overlay, system: &str, user: &str) -> Result<Completion> {
    let request = request_for(session, system, user);
    overlay.phase("request");
    let transport = UreqTransport::new(Duration::from_secs(session.cfg.timeout_seconds));
    stream_command(overlay, session.wire_kind, &transport, &request)
}

/// `ask` (spec §2): prose Q&A over the same context. The answer streams to
/// stdout token by token — it is terminal output, never a buffer insertion.
pub fn ask(opts: &RunOpts, question: &str) -> Result<()> {
    use std::io::Write as _;

    let session = prepare(opts)?;
    let overlay = Overlay::start(opts.plain, thinking_enabled(opts, &session.cfg));
    overlay.phase("context");
    let bundle = collect_context(opts, &session.cfg);
    let system = prompt::ask_system(bundle.as_ref());

    let request = request_for(&session, &system, question);
    overlay.phase("request");
    let transport = UreqTransport::new(Duration::from_secs(session.cfg.timeout_seconds));
    let reader = match transport.post_stream(&request) {
        Ok(reader) => reader,
        Err(err) => {
            drop(overlay);
            return Err(err);
        }
    };
    overlay.phase("streaming");

    // The spinner owns the line until the first token; then stdout takes over.
    let mut overlay = Some(overlay);
    let mut stdout = std::io::stdout().lock();
    let mut printed = false;
    let mut stop_reason = None;
    let mut saw_reasoning = false;
    for event in wire::events(session.wire_kind, reader) {
        match event {
            Ok(Event::TextDelta(delta)) => {
                drop(overlay.take());
                printed = true;
                let _ = stdout.write_all(delta.as_bytes());
                let _ = stdout.flush();
            }
            Ok(Event::ReasoningDelta(delta)) => {
                saw_reasoning = true;
                if let Some(o) = &overlay {
                    o.reasoning(&delta);
                }
            }
            Ok(Event::ToolCallDelta { .. }) => {}
            Ok(Event::Done {
                stop_reason: reason,
                ..
            }) => {
                stop_reason = reason;
                break;
            }
            Err(err) => {
                drop(overlay.take());
                if printed {
                    let _ = stdout.write_all(b"\n");
                }
                return Err(err);
            }
        }
    }
    drop(overlay.take());
    if !printed {
        return Err(Error::Provider(budget_hint(
            &session,
            saw_reasoning,
            None,
            "the model returned no answer",
        )));
    }
    let _ = stdout.write_all(b"\n");
    // Streamed live, so the partial is already on screen — warn, can't suppress.
    if is_truncated(stop_reason.as_deref()) {
        eprintln!(
            "adyton: {}",
            budget_hint(
                &session,
                saw_reasoning,
                None,
                "the answer was cut off at the token limit"
            )
        );
    }
    Ok(())
}

struct Completion {
    text: String,
    output_tokens: Option<u64>,
    stop_reason: Option<String>,
    saw_reasoning: bool,
}

/// A hit token ceiling: `length` (openai) or `max_tokens` (anthropic).
fn is_truncated(stop: Option<&str>) -> bool {
    matches!(stop, Some("length" | "max_tokens"))
}

fn budget_hint(session: &Session, saw_reasoning: bool, spent: Option<u64>, what: &str) -> String {
    let model = if saw_reasoning {
        " — this looks like a reasoning model"
    } else {
        ""
    };
    let spent = spent.map_or(String::new(), |n| format!(" after {n} output tokens"));
    format!(
        "{what}{spent}{model}. Raise the budget: \
         adyton config set profile.{}.max_tokens 16384",
        session.profile_name
    )
}

fn emit(session: &Session, c: Completion) -> Result<()> {
    let Completion {
        text,
        output_tokens,
        stop_reason,
        saw_reasoning,
    } = c;
    if text.is_empty() {
        return Err(Error::Provider(budget_hint(
            session,
            saw_reasoning,
            output_tokens,
            "the model returned no command",
        )));
    }
    // A truncated command is unsafe to run — suppress it.
    if is_truncated(stop_reason.as_deref()) {
        return Err(Error::Provider(budget_hint(
            session,
            saw_reasoning,
            output_tokens,
            "the command was cut off at the token limit",
        )));
    }
    println!("{text}");
    Ok(())
}

fn stream_command(
    overlay: &Overlay,
    wire_kind: WireKind,
    transport: &dyn Transport,
    request: &HttpRequest,
) -> Result<Completion> {
    let reader = transport.post_stream(request)?;
    overlay.phase("streaming");
    let mut text = String::new();
    let mut output_tokens = None;
    let mut stop_reason = None;
    let mut saw_reasoning = false;
    for event in wire::events(wire_kind, reader) {
        match event? {
            Event::TextDelta(delta) => {
                overlay.delta(&delta);
                text.push_str(&delta);
            }
            Event::ReasoningDelta(delta) => {
                saw_reasoning = true;
                overlay.reasoning(&delta);
            }
            // Tool calls belong to the phase-2 agent loop; ignored in MVP.
            Event::ToolCallDelta { .. } => {}
            Event::Done {
                usage,
                stop_reason: reason,
            } => {
                output_tokens = usage.and_then(|u| u.output_tokens);
                stop_reason = reason;
                break;
            }
        }
    }
    Ok(Completion {
        text: clean_command(&text),
        output_tokens,
        stop_reason,
        saw_reasoning,
    })
}

/// §7.4 tier 3 — the `--rerun` consent path: re-execute the failed command
/// through its recorded shell, merged output redacted and tail-capped.
/// Best-effort: a spawn failure or timeout yields whatever was captured.
fn rerun_capture(failure: &FailureState, timeout: Duration) -> Option<String> {
    let shell = failure.shell.as_deref().unwrap_or("sh");
    let mut command = std::process::Command::new(shell);
    command
        .arg("-c")
        .arg(&failure.cmd)
        .current_dir(&failure.cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Own process group: a runaway command's *grandchildren* (e.g. `sh -c
    // "sleep 30"` where the shell forks sleep) hold the stdout pipe open, so
    // killing only the direct child leaves the reader threads blocked for the
    // command's full lifetime — defeating the deadline. Killing the group
    // reaps them too.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn().ok()?;

    let mut stdout = child.stdout.take()?;
    let mut stderr = child.stderr.take()?;
    let out_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stdout.read_to_string(&mut buffer);
        buffer
    });
    let err_reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stderr.read_to_string(&mut buffer);
        buffer
    });

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            // Any exit code is fine — it failed before; errors end the wait too.
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if Instant::now() >= deadline => {
                kill_process_group(&mut child);
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut combined = out_reader.join().unwrap_or_default();
    combined.push_str(&err_reader.join().unwrap_or_default());
    let clean = redact::redact_block(&combined);
    let capped = live::tail_bytes(&clean, live::SCROLLBACK_CAP);
    (!capped.is_empty()).then_some(capped)
}

/// Kill the whole process group (child is its leader via `process_group(0)`),
/// so grandchildren holding the pipes die and the reader joins unblock.
#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    // Negative pid = the group. `kill` is frequently only a shell builtin (no
    // `/bin/kill` on e.g. Debian slim), so route through `sh`; avoids a libc
    // dependency, and the pid is an integer we produced — no injection.
    let _ = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("kill -KILL -{}", child.id()))
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Dialect hint: `--shell` flag → `$SHELL` basename → generic.
fn shell_name(flag: Option<Shell>, env: &dyn Fn(&str) -> Option<String>) -> String {
    if let Some(shell) = flag {
        return shell.as_str().to_owned();
    }
    env("SHELL")
        .and_then(|path| path.rsplit('/').next().map(str::to_owned))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "shell".to_owned())
}

/// Defense against models that disobey the output rules: strip a wrapping
/// markdown fence and a `$ ` prompt prefix; trim.
fn clean_command(raw: &str) -> String {
    let mut text = raw.trim();
    if text.starts_with("```") {
        if let Some((_first_line, rest)) = text.split_once('\n') {
            text = rest.trim_end();
        }
        text = text.strip_suffix("```").unwrap_or(text).trim();
    }
    text.strip_prefix("$ ").unwrap_or(text).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{clean_command, is_truncated, rerun_capture, shell_name};
    use crate::cli::Shell;
    use crate::context::session::FailureState;
    use std::time::Duration;

    #[test]
    fn truncation_covers_both_wire_reasons() {
        assert!(is_truncated(Some("length")), "openai");
        assert!(is_truncated(Some("max_tokens")), "anthropic");
        assert!(!is_truncated(Some("stop")));
        assert!(!is_truncated(None));
    }

    #[test]
    fn clean_command_passes_a_plain_command_through() {
        assert_eq!(clean_command("  ls -la\n"), "ls -la");
    }

    #[test]
    fn clean_command_strips_markdown_fences_and_prompt_prefix() {
        assert_eq!(clean_command("```zsh\nls -la\n```"), "ls -la");
        assert_eq!(clean_command("```\nls -la\n```"), "ls -la");
        assert_eq!(clean_command("$ ls -la"), "ls -la");
    }

    #[test]
    fn clean_command_keeps_multiline_commands() {
        assert_eq!(
            clean_command("```sh\nfor f in *; do\n  echo $f\ndone\n```"),
            "for f in *; do\n  echo $f\ndone"
        );
    }

    #[test]
    fn shell_name_prefers_the_flag_then_env_basename() {
        let env = |k: &str| (k == "SHELL").then(|| "/opt/local/bin/fish".to_owned());
        assert_eq!(shell_name(Some(Shell::Zsh), &env), "zsh");
        assert_eq!(shell_name(None, &env), "fish");
        assert_eq!(shell_name(None, &|_| None), "shell");
    }

    fn failure(cmd: &str) -> FailureState {
        FailureState {
            exit: 1,
            cmd: cmd.to_owned(),
            cwd: std::env::temp_dir().display().to_string(),
            ts: 0,
            shell: Some("sh".to_owned()),
        }
    }

    #[test]
    fn rerun_captures_merged_output_even_on_nonzero_exit() {
        let out = rerun_capture(
            &failure("echo to-stdout; echo to-stderr >&2; exit 7"),
            Duration::from_secs(5),
        )
        .expect("output captured");
        assert!(out.contains("to-stdout"));
        assert!(out.contains("to-stderr"));
    }

    #[test]
    fn rerun_output_is_redacted() {
        let out = rerun_capture(
            &failure("echo TOKEN=sk-rerun-secret-000000; exit 1"),
            Duration::from_secs(5),
        )
        .expect("output captured");
        assert!(!out.contains("sk-rerun-secret"), "{out}");
    }

    #[test]
    fn rerun_is_killed_at_the_deadline() {
        let started = std::time::Instant::now();
        let _ = rerun_capture(&failure("sleep 30"), Duration::from_millis(200));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "runaway command must be killed at the deadline"
        );
    }

    #[test]
    fn rerun_of_an_unspawnable_shell_is_none() {
        let mut bad = failure("echo x");
        bad.shell = Some("/nonexistent-shell-xyz".to_owned());
        assert_eq!(rerun_capture(&bad, Duration::from_secs(1)), None);
    }
}
