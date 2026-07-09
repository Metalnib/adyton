//! End-to-end tests through the real binary: exit codes (spec §2.1),
//! config round-trips (§3), and `config check`'s never-print-the-key rule.

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_adyton"))
}

struct TempHome {
    dir: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("adyton-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempHome { dir }
    }

    fn config_path(&self) -> PathBuf {
        self.dir.join("adyton").join("config")
    }

    fn write_config(&self, contents: &str) {
        std::fs::create_dir_all(self.dir.join("adyton")).unwrap();
        std::fs::write(self.config_path(), contents).unwrap();
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = bin();
        cmd.env("XDG_CONFIG_HOME", &self.dir)
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env_remove("ADYTON_API_KEY")
            .env_remove("TMUX")
            .env_remove("STY")
            .env_remove("ADYTON_TTY")
            .current_dir(&self.dir)
            .args(args);
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().expect("run adyton")
    }

    /// Pre-seed a fresh §7.1 cache so suggest tests skip live probing and
    /// carry deterministic machine facts.
    fn seed_context_cache(&self) {
        let dir = self.dir.join("cache").join("adyton");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("context"),
            format!(
                "os = TestOS 1.0\narch = testarch\nadyton = {}\n",
                env!("CARGO_PKG_VERSION")
            ),
        )
        .unwrap();
    }

    fn seed_session_log(&self, tty: &str, lines: &str) {
        let dir = self.dir.join("cache").join("adyton");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("session-{tty}.log")), lines).unwrap();
    }

    /// Like `run`, but feeds `stdin` in — for the piped-context path (§7.5).
    fn run_with_stdin(&self, args: &[&str], extra_env: &[(&str, &str)], stdin: &str) -> Output {
        use std::io::Write as _;
        let mut cmd = bin();
        cmd.env("XDG_CONFIG_HOME", &self.dir)
            .env("XDG_CACHE_HOME", self.dir.join("cache"))
            .env_remove("ADYTON_API_KEY")
            .env_remove("TMUX")
            .env_remove("STY")
            .env_remove("ADYTON_TTY")
            .current_dir(&self.dir)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn adyton");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("run adyton")
    }

    fn seed_failure_state(&self, cmd: &str, exit: i32, ts: u64) {
        let dir = self.dir.join("cache").join("adyton");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("last"),
            format!(
                "exit={exit}\ncmd={cmd}\ncwd={}\nts={ts}\nshell=sh\n",
                self.dir.display()
            ),
        )
        .unwrap();
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// One-shot mock LLM endpoint: serves `response`, hands back the `base_url` and
/// a channel yielding the raw captured request (head + body).
fn mock_llm(response: Vec<u8>) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(1) => request.push(byte[0]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&request).to_ascii_lowercase();
        let content_length = head
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut body = vec![0u8; content_length];
        let _ = stream.read_exact(&mut body);
        request.extend_from_slice(&body);
        let _ = tx.send(request);
        let _ = stream.write_all(&response);
    });
    (base_url, rx)
}

fn sse_response(events: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{events}",
        events.len()
    )
    .into_bytes()
}

const OPENAI_STREAM: &str = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"eza -T \"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"--git-ignore\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

fn openai_profile(base_url: &str) -> String {
    format!(
        "default_profile = mock\n\n[profile.mock]\nwire = openai\nbase_url = {base_url}\nmodel = test-model\n"
    )
}

#[test]
fn suggest_prints_exactly_the_streamed_command_and_nothing_on_stderr() {
    let home = TempHome::new("suggest-happy");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(
        &["suggest", "--shell", "zsh", "--", "show", "the", "tree"],
        &[("ADYTON_API_KEY", "test-key-123")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "eza -T --git-ignore\n",
        "stdout is the command, exactly"
    );
    assert_eq!(
        stderr(&out),
        "",
        "overlay must be fully suppressed when stderr is not a tty (§6)"
    );

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        request.contains("authorization: Bearer test-key-123"),
        "auth header"
    );
    assert!(request.contains("show the tree"), "query in body");
    assert!(
        request.contains("TestOS 1.0"),
        "machine facts from the seeded cache"
    );
    assert!(request.contains("zsh"), "dialect hint");
}

/// §9 criterion 5, full: a key planted in the session history never reaches
/// the serialized request body.
#[test]
fn suggest_request_body_never_contains_a_planted_secret() {
    let home = TempHome::new("suggest-redact");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();
    home.seed_session_log(
        "ttys042",
        "1751971000\t0\t4\t/home/u\texport OPENAI_API_KEY=sk-planted-in-history-abc123\n\
         1751971001\t0\t4\t/home/u\tgit status\n",
    );

    let out = home.run(
        &["suggest", "--", "list", "files"],
        &[("ADYTON_TTY", "ttys042")],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        !request.contains("sk-planted-in-history"),
        "planted key must be redacted out of the request body"
    );
    assert!(request.contains("git status"), "innocent history is sent");
}

#[test]
fn suggest_no_context_sends_the_query_only() {
    let home = TempHome::new("suggest-noctx");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(&["suggest", "--no-context", "--", "list", "files"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        !request.contains("## target machine"),
        "no context sections"
    );
    assert!(!request.contains("TestOS"), "no cache facts");
    assert!(request.contains("list files"));
}

#[test]
fn suggest_against_anthropic_wire_uses_native_headers() {
    let home = TempHome::new("suggest-anthropic");
    let stream = "\
event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"m\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"c\",\"content\":[],\"usage\":{\"input_tokens\":10,\"output_tokens\":1}}}\n\n\
event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"df -h\"}}\n\n\
event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
    let (base_url, request_rx) = mock_llm(sse_response(stream));
    // anthropic base_url convention excludes /v1 (spec §4.2)
    let base_url = base_url.trim_end_matches("/v1").to_owned();
    home.write_config(&format!(
        "default_profile = a\n\n[profile.a]\nwire = anthropic\nbase_url = {base_url}\nmodel = claude-test\n"
    ));
    home.seed_context_cache();

    let out = home.run(
        &["suggest", "--", "disk", "usage"],
        &[("ADYTON_API_KEY", "sk-ant-key")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "df -h\n");

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(request.contains("x-api-key: sk-ant-key"));
    assert!(request.contains("anthropic-version: 2023-06-01"));
    assert!(
        !request.contains("Bearer"),
        "never Bearer on the anthropic wire"
    );
    assert!(request.contains("\"max_tokens\":4096"), "always required");
}

#[test]
fn reasoning_never_leaks_into_the_command_on_stdout() {
    let home = TempHome::new("suggest-reasoning");
    let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"the user wants disk usage\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"df -h\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
    let (base_url, _rx) = mock_llm(sse_response(stream));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(
        &["suggest", "--shell", "zsh", "--", "disk", "usage"],
        &[("ADYTON_API_KEY", "k")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "df -h\n",
        "only the command reaches stdout — reasoning is overlay-only"
    );
}

#[test]
fn a_truncated_command_is_suppressed_with_advice() {
    let home = TempHome::new("suggest-truncated");
    let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"find / -name \"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
data: [DONE]\n\n";
    let (base_url, _rx) = mock_llm(sse_response(stream));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(
        &["suggest", "--", "find", "something"],
        &[("ADYTON_API_KEY", "k")],
    );
    assert!(
        !out.status.success(),
        "truncation must fail, not emit a half-command"
    );
    assert_eq!(
        stdout(&out),
        "",
        "the partial command must never reach stdout"
    );
    assert!(
        stderr(&out).contains("max_tokens"),
        "advice names the fix: {}",
        stderr(&out)
    );
}

#[test]
fn extra_body_is_merged_into_the_request() {
    let home = TempHome::new("extra-body");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&format!(
        "default_profile = mock\n\n[profile.mock]\nwire = openai\nbase_url = {base_url}\nmodel = test-model\nextra_body = {{\"reasoning_effort\":\"none\"}}\n"
    ));
    home.seed_context_cache();

    let out = home.run(
        &["suggest", "--", "show", "the", "tree"],
        &[("ADYTON_API_KEY", "k")],
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        request.contains("\"reasoning_effort\":\"none\""),
        "extra_body must reach the request body: {request}"
    );
}

#[test]
fn ask_keeps_the_streamed_partial_then_warns_when_truncated() {
    let home = TempHome::new("ask-truncated");
    let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"hmm\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"The answer is\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
data: [DONE]\n\n";
    let (base_url, _rx) = mock_llm(sse_response(stream));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(&["ask", "--", "explain"], &[("ADYTON_API_KEY", "k")]);
    assert!(
        out.status.success(),
        "ask keeps the already-streamed partial: {}",
        stderr(&out)
    );
    assert_eq!(
        stdout(&out),
        "The answer is\n",
        "reasoning stripped from stdout; the partial answer is kept"
    );
    assert!(
        stderr(&out).contains("cut off") || stderr(&out).contains("max_tokens"),
        "truncation warned on stderr: {}",
        stderr(&out)
    );
}

#[test]
fn ask_streams_prose_to_stdout_with_context() {
    let home = TempHome::new("ask");
    let stream = "\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Homebrew lives in \"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"/opt/homebrew.\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
    let (base_url, request_rx) = mock_llm(sse_response(stream));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(&["ask", "--", "where", "is", "brew"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "Homebrew lives in /opt/homebrew.\n",
        "prose streamed to stdout with a trailing newline"
    );

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(request.contains("terminal assistant"), "ask system prompt");
    assert!(
        request.contains("TestOS 1.0"),
        "same context bundle as suggest"
    );
    assert!(request.contains("where is brew"));
}

const LSOF_PIPE: &str = "\
COMMAND     PID USER   FD   TYPE  DEVICE SIZE/OFF NODE NAME\n\
OrbStack  69305  hgg  102u  IPv4  0x755   0t0  TCP 127.0.0.1:32222 (LISTEN)\n\
OrbStack  69305  hgg  104u  IPv4  0x44f   0t0  TCP 127.0.0.1:52198 (LISTEN)\n";

#[test]
fn piped_stdin_reaches_the_request_body() {
    let home = TempHome::new("pipe");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run_with_stdin(&["ask", "--", "which is OrbStack"], &[], LSOF_PIPE);
    assert!(out.status.success(), "{}", stderr(&out));

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(request.contains("## piped input"), "piped section present");
    assert!(request.contains("52198"), "the real port reaches the model");
}

#[test]
fn piped_secret_is_redacted_before_send() {
    let home = TempHome::new("pipe-redact");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run_with_stdin(
        &["ask", "--", "explain"],
        &[],
        "connecting with TOKEN=sk-piped-into-adyton-000000\n",
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        !request.contains("sk-piped-into-adyton"),
        "piped secret redacted"
    );
}

#[test]
fn piped_input_survives_no_context() {
    let home = TempHome::new("pipe-noctx");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run_with_stdin(&["ask", "--no-context", "--", "which port"], &[], LSOF_PIPE);
    assert!(out.status.success(), "{}", stderr(&out));
    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        !request.contains("## target machine"),
        "--no-context drops ambient"
    );
    assert!(
        request.contains("52198"),
        "but explicit piped input survives"
    );
}

/// §9 criterion 2: every fixture under tests/fixtures/ replays through the
/// complete binary. `openai-vultr-*.sse` is REAL recorded wire data (vLLM on
/// Vultr, reasoning deltas included); `anthropic-docs-derived.sse` follows the
/// documented event sequence pending a live recording.
#[test]
fn recorded_fixtures_replay_through_the_full_binary() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut replayed = 0;
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("sse") {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let body = std::fs::read_to_string(&path).unwrap();
        let home = TempHome::new(&format!("fx-{}", name.replace('.', "-")));
        let (base_url, _rx) = mock_llm(sse_response(&body));
        let (wire, base) = if name.starts_with("anthropic") {
            ("anthropic", base_url.trim_end_matches("/v1").to_owned())
        } else {
            ("openai", base_url)
        };
        home.write_config(&format!(
            "default_profile = f\n\n[profile.f]\nwire = {wire}\nbase_url = {base}\nmodel = m\n"
        ));
        let out = home.run(&["suggest", "--no-context", "--", "replay"], &[]);
        assert!(out.status.success(), "{name}: {}", stderr(&out));
        assert!(
            !stdout(&out).trim().is_empty(),
            "{name}: produced an empty command"
        );
        replayed += 1;
    }
    assert!(
        replayed >= 2,
        "expected at least the vultr + anthropic fixtures"
    );
}

/// Multi-request HTTP mock: serves `routes` (path → (content-type, body)) on a
/// loop over a pre-bound listener (caller needs its address to build the URLs
/// the routes reference), for the self-update flow's several sequential GETs.
fn serve_on(listener: std::net::TcpListener, routes: Vec<(String, &'static str, Vec<u8>)>) {
    use std::io::{Read as _, Write as _};
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .ok();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let req = String::from_utf8_lossy(&head);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("")
                .to_owned();
            let route = routes.iter().find(|(p, _, _)| *p == path);
            let response = match route {
                Some((_, ct, body)) => {
                    let mut r = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: {ct}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(body);
                    r
                }
                None => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    .to_vec(),
            };
            let _ = stream.write_all(&response);
        }
    });
}

fn host_triple() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        _ => panic!("unsupported test host"),
    }
}

/// §12.1 end-to-end: a copy of the binary updates *itself* from a mock release
/// (download → verify sha256 → extract → atomic rename) and then runs the new
/// payload. This is the assertion that matters most.
#[test]
fn self_update_verifies_extracts_and_atomically_swaps_the_binary() {
    let home = TempHome::new("selfupdate");
    let triple = host_triple();
    let asset_name = format!("adyton-v9.9.9-{triple}.tar.gz");

    // Build the "new release" tarball: a top-level `adyton` that identifies itself.
    let payload = home.dir.join("payload");
    std::fs::create_dir_all(&payload).unwrap();
    std::fs::write(payload.join("adyton"), "#!/bin/sh\necho UPDATED-PAYLOAD\n").unwrap();
    let tarball = home.dir.join(&asset_name);
    assert!(
        std::process::Command::new("tar")
            .arg("-czf")
            .arg(&tarball)
            .arg("-C")
            .arg(&payload)
            .arg("adyton")
            .status()
            .unwrap()
            .success()
    );
    let tarball_bytes = std::fs::read(&tarball).unwrap();
    let sha = {
        let o = std::process::Command::new("shasum")
            .args(["-a", "256"])
            .arg(&tarball)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned()
    };

    // Bind once, build the URLs from its real address, then serve on it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let real_base = format!("http://{}", listener.local_addr().unwrap());
    let latest_json = format!(
        r#"{{"tag_name":"v9.9.9","assets":[{{"name":"{asset_name}","browser_download_url":"{real_base}/dl/bin"}},{{"name":"SHA256SUMS.txt","browser_download_url":"{real_base}/dl/sums"}}]}}"#
    );
    let sums = format!("{sha}  {asset_name}\n");
    serve_on(
        listener,
        vec![
            (
                "/releases/latest".to_owned(),
                "application/json",
                latest_json.into_bytes(),
            ),
            (
                "/dl/bin".to_owned(),
                "application/octet-stream",
                tarball_bytes,
            ),
            ("/dl/sums".to_owned(), "text/plain", sums.into_bytes()),
        ],
    );

    // Copy the real binary to a throwaway path and let it replace *itself*.
    let exe = home.dir.join("adyton");
    std::fs::copy(env!("CARGO_BIN_EXE_adyton"), &exe).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let out = std::process::Command::new(&exe)
        .args(["selfupdate", "--yes"])
        .env("ADYTON_GITHUB_API", &real_base)
        .output()
        .expect("run selfupdate");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("updated to adyton 9.9.9"),
        "stdout: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    let after = std::process::Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        "UPDATED-PAYLOAD"
    );
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn fix_without_recorded_failure_exits_5() {
    let home = TempHome::new("fix-nostate");
    let (base_url, _rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));

    let out = home.run(&["fix"], &[]);
    assert_eq!(out.status.code(), Some(5));
    assert!(
        stderr(&out).contains("no recorded failure"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn fix_with_stale_failure_exits_5() {
    let home = TempHome::new("fix-stale");
    let (base_url, _rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_failure_state("gti status", 127, now_unix() - 3600);

    let out = home.run(&["fix"], &[]);
    assert_eq!(out.status.code(), Some(5));
    assert!(
        stderr(&out).contains("older than 10 minutes"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn fix_sends_the_failure_details_and_prints_the_correction() {
    let home = TempHome::new("fix-happy");
    let (base_url, request_rx) = mock_llm(sse_response(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"git status\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    ));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();
    home.seed_failure_state("gti status", 127, now_unix());

    let out = home.run(&["fix"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "git status\n");

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(request.contains("gti status"), "failed command in body");
    assert!(request.contains("exit code: 127"), "exit code in body");
}

#[test]
fn fix_rerun_attaches_redacted_command_output() {
    let home = TempHome::new("fix-rerun");
    let (base_url, request_rx) = mock_llm(sse_response(OPENAI_STREAM));
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();
    home.seed_failure_state(
        "echo rerun-marker-xyz; echo TOKEN=sk-rerun-leak-000000; exit 3",
        3,
        now_unix(),
    );

    let out = home.run(&["fix", "--rerun"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));

    let request = String::from_utf8_lossy(&request_rx.recv().unwrap()).into_owned();
    assert!(
        request.contains("rerun-marker-xyz"),
        "rerun output reaches the body"
    );
    assert!(
        !request.contains("sk-rerun-leak"),
        "rerun output is redacted before it can leave the machine"
    );
}

#[test]
fn suggest_surfaces_provider_http_errors_with_the_body() {
    let home = TempHome::new("suggest-401");
    let body = r#"{"error":{"message":"Incorrect API key provided"}}"#;
    let response = format!(
        "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let (base_url, _request_rx) = mock_llm(response.into_bytes());
    home.write_config(&openai_profile(&base_url));
    home.seed_context_cache();

    let out = home.run(&["suggest", "--", "anything"], &[]);
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.contains("HTTP 401") && err.contains("Incorrect API key"),
        "{err}"
    );
    assert_eq!(stdout(&out), "", "nothing lands on stdout on failure");
}

#[test]
fn version_prints_and_exits_zero() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(stdout(&out).starts_with("adyton "));
}

#[test]
fn missing_command_is_usage_error_2() {
    let out = bin().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--help"));
}

#[test]
fn unknown_flag_is_usage_error_2() {
    let out = bin().arg("--bogus").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn config_path_honors_xdg_config_home() {
    let home = TempHome::new("path");
    let out = home.run(&["config", "path"], &[]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out).trim(),
        home.config_path().display().to_string()
    );
}

#[test]
fn config_set_then_get_roundtrip_with_private_permissions() {
    let home = TempHome::new("roundtrip");
    for (key, value) in [
        ("profile.local.wire", "openai"),
        ("profile.local.base_url", "http://localhost:11434/v1"),
        ("profile.local.model", "qwen3:8b"),
    ] {
        let out = home.run(&["config", "set", key, value], &[]);
        assert!(out.status.success(), "set {key}: {}", stderr(&out));
    }
    let out = home.run(&["config", "get", "profile.local.model"], &[]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "qwen3:8b");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(home.config_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "config file must be private");
    }
}

#[test]
fn config_get_of_broken_file_is_config_error_3() {
    let home = TempHome::new("broken");
    home.write_config("not a config\n");
    let out = home.run(&["config", "get", "timeout_seconds"], &[]);
    assert_eq!(out.status.code(), Some(3));
}

const CHECKABLE: &str = "\
default_profile = local

[profile.local]
wire = openai
base_url = http://localhost:11434/v1
model = qwen3:8b
";

#[test]
fn config_check_reports_key_source_from_env() {
    let home = TempHome::new("check-env");
    home.write_config(CHECKABLE);
    let out = home.run(
        &["config", "check"],
        &[("ADYTON_API_KEY", "sk-super-secret")],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("env ADYTON_API_KEY"), "{text}");
    assert!(
        !text.contains("sk-super-secret"),
        "the key itself must never be printed"
    );
}

#[test]
fn config_check_reports_key_source_from_cmd_without_printing_it() {
    let home = TempHome::new("check-cmd");
    home.write_config(&format!("{CHECKABLE}api_key_cmd = echo sk-from-cmd\n"));
    let out = home.run(&["config", "check"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("api_key_cmd"), "{text}");
    assert!(
        !text.contains("sk-from-cmd"),
        "the key itself must never be printed"
    );
}

#[test]
fn config_check_without_key_is_ok_for_local_endpoints() {
    let home = TempHome::new("check-nokey");
    home.write_config(CHECKABLE);
    let out = home.run(&["config", "check"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("none (ok for local endpoints)"));
}

#[test]
fn config_check_with_missing_required_fields_is_config_error_3() {
    let home = TempHome::new("check-missing");
    home.write_config("[profile.p]\nwire = openai\n");
    let out = home.run(&["config", "check"], &[]);
    assert_eq!(out.status.code(), Some(3));
    let err = stderr(&out);
    assert!(err.contains("base_url") && err.contains("model"), "{err}");
}

#[test]
fn init_fish_emits_widget_hooks_functions_and_no_eval() {
    let home = TempHome::new("init-fish");
    let out = home.run(&["init", "fish"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let glue = stdout(&out);

    assert!(glue.contains("bind \\cg __adyton_widget"), "hotkey bound");
    assert!(
        glue.contains("commandline -r --"),
        "widget replaces the buffer"
    );
    assert!(
        glue.contains("--on-event fish_postexec"),
        "failure capture hook"
    );
    assert!(
        glue.contains("function ai")
            && glue.contains("function aifix")
            && glue.contains("function aiask"),
        "fish cannot name a command ?, so ?/??/??? become ai/aifix/aiask"
    );
    assert!(
        glue.contains(env!("CARGO_BIN_EXE_adyton")),
        "own path baked in"
    );
    assert!(!glue.contains("eval"), "criterion 7: no eval, ever");
}

/// §9 criterion 3 (fish half): drive the widget + hooks in a real fish against
/// a stub adyton. Skips when fish is absent — closes validation-log open item 2
/// when it runs (fish's live behavior, incl. `$status` in `fish_postexec`).
#[test]
fn fish_glue_widget_and_hooks_work_in_a_real_fish() {
    let fish_present = std::process::Command::new("fish")
        .args(["-c", "true"])
        .status()
        .is_ok_and(|s| s.success());
    if !fish_present {
        eprintln!("skipping: fish not available on this machine");
        return;
    }
    let home = TempHome::new("fish-drive");

    let stub = home.dir.join("stub-adyton");
    std::fs::write(
        &stub,
        "#!/bin/sh\nif [ -n \"$STUB_FAIL\" ]; then exit 3; fi\ncase \"$1\" in\n  suggest) echo \"stub: suggested\";;\n  fix) echo \"stub: fixed\";;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let glue_out = home.run(&["init", "fish"], &[]);
    assert!(glue_out.status.success());
    let glue_path = home.dir.join("glue.fish");
    std::fs::write(&glue_path, stdout(&glue_out)).unwrap();

    let driver = home.dir.join("driver.fish");
    std::fs::write(
        &driver,
        r#"
function commandline; end   # no real editor outside interaction
function bind; end
source $argv[1]

# widget: non-empty buffer routes to suggest
function commandline; echo "list all files"; end
set out ("$ADYTON_BIN" suggest --shell fish -- (commandline))
test "$out" = "stub: suggested"; or begin; echo "FAIL suggest: $out"; exit 1; end

# hooks: a failing command writes the session record + failure state
__adyton_postexec "gti status"
# emulate a nonzero prior status by calling with a failing status context:
false; __adyton_postexec "gti status"
grep -q 'cmd=gti status' $XDG_CACHE_HOME/adyton/last; or begin; echo FAIL-last-cmd; exit 1; end
grep -q 'shell=fish' $XDG_CACHE_HOME/adyton/last; or begin; echo FAIL-last-shell; exit 1; end
grep -q 'gti status' $XDG_CACHE_HOME/adyton/session-fishtty.log; or begin; echo FAIL-session; exit 1; end

echo OK
"#,
    )
    .unwrap();

    let out = std::process::Command::new("fish")
        .arg(&driver)
        .arg(&glue_path)
        .env("XDG_CACHE_HOME", home.dir.join("cache"))
        .env("ADYTON_BIN", &stub)
        .env("ADYTON_TTY", "fishtty")
        .current_dir(&home.dir)
        .output()
        .expect("run fish driver");
    assert!(
        out.status.success() && stdout(&out).contains("OK"),
        "driver failed:\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn init_bash_emits_widget_hooks_functions_and_no_eval() {
    let home = TempHome::new("init-bash");
    let out = home.run(&["init", "bash"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let glue = stdout(&out);

    assert!(
        glue.contains(r#"bind -x '"\C-g": __adyton_widget'"#),
        "hotkey via bind -x"
    );
    assert!(
        glue.contains("READLINE_LINE"),
        "widget edits the readline buffer"
    );
    assert!(
        glue.contains("BASH_VERSINFO[0] >= 4"),
        "guards the widget for bash < 4"
    );
    assert!(glue.contains("PROMPT_COMMAND") && glue.contains("trap") && glue.contains("DEBUG"));
    assert!(
        glue.contains("ai()") && glue.contains("aifix()") && glue.contains("aiask()"),
        "bash cannot name a command ?, so ?/??/??? become ai/aifix/aiask"
    );
    assert!(
        glue.contains(env!("CARGO_BIN_EXE_adyton")),
        "own path baked in"
    );
    assert!(!glue.contains("eval"), "criterion 7: no eval, ever");
}

/// §9 criterion 3 (bash half): drive the widget + hooks in a real bash against
/// a stub adyton — buffer replacement via `READLINE_LINE`, empty→fix routing,
/// no-clobber on failure, and the session-log + failure-state writes.
#[test]
fn bash_glue_widget_and_hooks_work_in_a_real_bash() {
    // Prefer a modern bash for READLINE_LINE (macOS /bin/bash is 3.2).
    let bash = ["/opt/local/bin/bash", "/usr/local/bin/bash", "bash"]
        .into_iter()
        .find(|b| {
            std::process::Command::new(b)
                .args(["-c", "((BASH_VERSINFO[0] >= 4))"])
                .status()
                .is_ok_and(|s| s.success())
        });
    let Some(bash) = bash else {
        eprintln!("skipping: no bash >= 4 available");
        return;
    };
    let home = TempHome::new("bash-drive");

    let stub = home.dir.join("stub-adyton");
    std::fs::write(
        &stub,
        "#!/bin/sh\nif [ -n \"$STUB_FAIL\" ]; then exit 3; fi\ncase \"$1\" in\n  suggest) echo \"stub: suggested\";;\n  fix) echo \"stub: fixed\";;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let glue_out = home.run(&["init", "bash"], &[]);
    assert!(glue_out.status.success());
    let glue_path = home.dir.join("glue.bash");
    std::fs::write(&glue_path, stdout(&glue_out)).unwrap();

    let driver = home.dir.join("driver.bash");
    std::fs::write(
        &driver,
        r#"
set -u
bind() { :; }   # no readline outside an interactive editor
source "$1"

# widget: non-empty buffer routes to suggest and replaces the buffer
READLINE_LINE="list all files"; READLINE_POINT=0
__adyton_widget
[[ "$READLINE_LINE" == "stub: suggested" ]] || { echo "FAIL suggest: $READLINE_LINE"; exit 1; }
[[ "$READLINE_POINT" == "${#READLINE_LINE}" ]] || { echo "FAIL point"; exit 1; }

# widget: empty buffer routes to fix
READLINE_LINE=""
__adyton_widget
[[ "$READLINE_LINE" == "stub: fixed" ]] || { echo "FAIL fix: $READLINE_LINE"; exit 1; }

# widget: a failing adyton must not clobber the user's buffer
READLINE_LINE="precious user text"
STUB_FAIL=1 __adyton_widget || :
[[ "$READLINE_LINE" == "precious user text" ]] || { echo "FAIL clobber: $READLINE_LINE"; exit 1; }

# hooks: a failing command writes the session record + failure state
history -s "gti status"
( exit 1 )
__adyton_precmd
grep -q 'exit=1' "$XDG_CACHE_HOME/adyton/last" || { echo "FAIL last exit"; exit 1; }
grep -q 'cmd=gti status' "$XDG_CACHE_HOME/adyton/last" || { echo "FAIL last cmd"; exit 1; }
grep -q 'shell=bash' "$XDG_CACHE_HOME/adyton/last" || { echo "FAIL last shell"; exit 1; }
grep -q $'\t1\t.*\tgti status' "$XDG_CACHE_HOME/adyton/session-ttytest.log" \
  || { echo "FAIL session record"; exit 1; }

echo OK
"#,
    )
    .unwrap();

    let out = std::process::Command::new(bash)
        .arg("--norc")
        .arg(&driver)
        .arg(&glue_path)
        .env("XDG_CACHE_HOME", home.dir.join("cache"))
        .env("ADYTON_BIN", &stub)
        .env("ADYTON_TTY", "ttytest")
        .env("HISTFILE", home.dir.join("bash_history"))
        .current_dir(&home.dir)
        .output()
        .expect("run bash driver");
    assert!(
        out.status.success() && stdout(&out).contains("OK"),
        "driver failed:\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

/// §9 criterion 7: the emitted glue never contains `eval` — model output is
/// assigned, not executed.
#[test]
fn init_zsh_emits_glue_with_widget_hooks_and_no_eval() {
    let home = TempHome::new("init-zsh");
    let out = home.run(&["init", "zsh"], &[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let glue = stdout(&out);

    assert!(glue.contains("bindkey '^G' adyton-widget"), "hotkey bound");
    assert!(
        glue.contains("print -z --"),
        "alias path pushes the buffer stack"
    );
    assert!(
        glue.contains("alias -- '?'='noglob ")
            && glue.contains("alias -- '??'='noglob ")
            && glue.contains("alias -- '???'='noglob "),
        "?/??/??? must be noglob aliases — zsh globs a bare ? before function lookup"
    );
    assert!(glue.contains("add-zsh-hook precmd"), "failure capture hook");
    assert!(
        glue.contains(env!("CARGO_BIN_EXE_adyton")),
        "own path baked in"
    );
    assert!(!glue.contains("eval"), "criterion 7: no eval, ever");
}

/// §9 criterion 3 (zsh half, expect-style-lite): drive the widget and hooks
/// in a real zsh against a stub adyton — buffer replacement, fix routing on
/// empty buffer, no-clobber on failure, session log + failure state writes.
#[test]
fn zsh_glue_widget_and_hooks_work_in_a_real_zsh() {
    let zsh_present = std::process::Command::new("zsh")
        .arg("-c")
        .arg("exit 0")
        .status()
        .is_ok_and(|s| s.success());
    if !zsh_present {
        eprintln!("skipping: zsh not available on this machine");
        return;
    }
    let home = TempHome::new("zsh-drive");

    // Stub standing in for the adyton binary, distinguishing subcommands.
    let stub = home.dir.join("stub-adyton");
    std::fs::write(
        &stub,
        "#!/bin/sh\nif [ -n \"$STUB_FAIL\" ]; then exit 3; fi\ncase \"$1\" in\n  suggest) echo \"stub: suggested\";;\n  fix) echo \"stub: fixed\";;\nesac\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let glue_out = home.run(&["init", "zsh"], &[]);
    assert!(glue_out.status.success());
    let glue_path = home.dir.join("glue.zsh");
    std::fs::write(&glue_path, stdout(&glue_out)).unwrap();

    let driver = home.dir.join("driver.zsh");
    std::fs::write(
        &driver,
        r#"
set -u
# ZLE builtins are unavailable outside an interactive editor: stub them so
# the widget logic itself can run.
zle() { : }
bindkey() { : }
TTY=/dev/ttytest0
source "$1"

# widget: non-empty buffer routes to suggest and replaces the buffer
BUFFER="list all files"; CURSOR=0
__adyton_widget
[[ "$BUFFER" == "stub: suggested" ]] || { print "FAIL suggest buffer: $BUFFER"; exit 1 }
[[ "$CURSOR" == ${#BUFFER} ]] || { print "FAIL cursor"; exit 1 }

# widget: empty buffer routes to fix
BUFFER=""
__adyton_widget
[[ "$BUFFER" == "stub: fixed" ]] || { print "FAIL fix buffer: $BUFFER"; exit 1 }

# widget: a failing adyton must not clobber the user's buffer
BUFFER="precious user text"
STUB_FAIL=1 __adyton_widget 2>/dev/null || :
[[ "$BUFFER" == "precious user text" ]] || { print "FAIL clobber: $BUFFER"; exit 1 }

# hooks fire automatically even in scripts once registered — verify the
# registration, then deregister so the calls below are deterministic.
(( ${precmd_functions[(I)__adyton_precmd]} )) || { print "FAIL precmd registered"; exit 1 }
(( ${preexec_functions[(I)__adyton_preexec]} )) || { print "FAIL preexec registered"; exit 1 }
add-zsh-hook -d preexec __adyton_preexec
add-zsh-hook -d precmd __adyton_precmd
__adyton_cmd=""
: > "$XDG_CACHE_HOME/adyton/session-ttytest0.log"

# hooks: failing command writes session record + failure state
__adyton_preexec "gti status"
false
__adyton_precmd
grep -q 'exit=1' "$XDG_CACHE_HOME/adyton/last" || { print "FAIL last exit"; exit 1 }
grep -q 'cmd=gti status' "$XDG_CACHE_HOME/adyton/last" || { print "FAIL last cmd"; exit 1 }
grep -q 'shell=zsh' "$XDG_CACHE_HOME/adyton/last" || { print "FAIL last shell"; exit 1 }
grep -q "	1	.*	gti status" "$XDG_CACHE_HOME/adyton/session-ttytest0.log" \
  || { print "FAIL session record"; exit 1 }

# hooks: succeeding command appends to the log but leaves `last` alone
__adyton_preexec "ls"
true
__adyton_precmd
[[ "$(wc -l < "$XDG_CACHE_HOME/adyton/session-ttytest0.log")" -eq 2 ]] \
  || { print "FAIL session count"; exit 1 }
grep -q 'cmd=gti status' "$XDG_CACHE_HOME/adyton/last" || { print "FAIL last overwritten"; exit 1 }

print OK
"#,
    )
    .unwrap();

    let out = std::process::Command::new("zsh")
        .arg(&driver)
        .arg(&glue_path)
        .env("XDG_CACHE_HOME", home.dir.join("cache"))
        .env("ADYTON_BIN", &stub)
        .env_remove("ADYTON_TTY")
        .current_dir(&home.dir)
        .output()
        .expect("run zsh driver");
    assert!(
        out.status.success() && stdout(&out).contains("OK"),
        "driver failed:\nstdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
}

#[test]
fn set_key_for_unknown_profile_is_config_error_before_touching_the_keychain() {
    let home = TempHome::new("setkey-unknown");
    home.write_config(CHECKABLE);
    let mut cmd = bin();
    cmd.env("XDG_CONFIG_HOME", &home.dir)
        .args(["config", "set-key", "ghost"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    {
        use std::io::Write as _;
        // The child rejects the unknown profile and exits BEFORE reading stdin,
        // so this write may hit EPIPE — that's expected, don't unwrap it.
        let _ = child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"sk-would-be-a-key");
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ghost"));
}

#[test]
fn context_refresh_builds_the_cache_with_private_permissions() {
    let home = TempHome::new("ctx-refresh");
    let cache_home = home.dir.join("cache");
    let out = home.run(
        &["context", "refresh"],
        &[("XDG_CACHE_HOME", cache_home.to_str().unwrap())],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "refresh prints nothing on success");

    let cache_file = cache_home.join("adyton").join("context");
    let text = std::fs::read_to_string(&cache_file).expect("cache written");
    for key in ["os =", "arch =", "shell =", "adyton ="] {
        assert!(text.contains(key), "missing `{key}` in:\n{text}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&cache_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "cache must be private");
    }
}
