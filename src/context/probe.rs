//! External probes (specification §7.1): explicit argv, never a shell string
//! (§11); each probe capped at 300 ms, a whole gather at 2 s. Cheap
//! env/const sources are collected first so the deadline only ever cuts the
//! expensive tail (`packages.*`).

use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_millis(300);
pub(crate) const REBUILD_BUDGET: Duration = Duration::from_secs(2);
/// Per-key cap so one package manager cannot bloat the cache (spec §7.1).
const VALUE_CAP: usize = 2048;

/// Probe execution, injected into [`gather`] so tests never depend on what
/// is installed on the machine running them.
pub(crate) type Runner<'a> = dyn Fn(&[&str]) -> Option<String> + 'a;
pub(crate) type EnvFn<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Run argv with the standard [`PROBE_TIMEOUT`]; trimmed stdout on success,
/// `None` on missing binary, non-zero exit, timeout, or empty output.
pub(crate) fn run_probe(argv: &[&str]) -> Option<String> {
    run_probe_with(argv, PROBE_TIMEOUT)
}

/// [`run_probe`] with an explicit timeout — live-context probes (git, tmux)
/// carry their own budgets (spec §7).
pub(crate) fn run_probe_with(argv: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Reader thread avoids a pipe-buffer deadlock with chatty probes.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        let _ = stdout.read_to_string(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    };
    let output = reader.join().ok()?;
    if !status.success() {
        return None;
    }
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Package managers: `pkg.<name>` detection probe (spec §7.1 detect list).
const MANAGERS: &[(&str, &[&str])] = &[
    ("macports", &["port", "version"]),
    ("brew", &["brew", "--version"]),
    ("apt", &["apt", "--version"]),
    ("dnf", &["dnf", "--version"]),
    ("yum", &["yum", "--version"]),
    ("pacman", &["pacman", "--version"]),
    ("zypper", &["zypper", "--version"]),
    ("apk", &["apk", "--version"]),
    ("nix", &["nix", "--version"]),
    ("flatpak", &["flatpak", "--version"]),
    ("snap", &["snap", "--version"]),
    ("cargo", &["cargo", "--version"]),
    ("rustup", &["rustup", "--version"]),
    ("uv", &["uv", "--version"]),
    ("pip", &["pip", "--version"]),
    ("pipx", &["pipx", "--version"]),
    ("npm", &["npm", "--version"]),
    ("pnpm", &["pnpm", "--version"]),
    ("yarn", &["yarn", "--version"]),
    ("gem", &["gem", "--version"]),
    ("go", &["go", "version"]),
];

/// How to pull top-level package names out of a manager's listing.
#[derive(Clone, Copy)]
enum Extract {
    /// First whitespace token of every non-empty line.
    FirstToken,
    /// First token of lines that start at column zero (cargo/uv style roots).
    Roots,
    /// Whole line is the name (brew leaves style).
    WholeLine,
}

/// `packages.<name>` listings, only probed for detected managers.
const PACKAGE_LISTS: &[(&str, &[&str], Extract)] = &[
    (
        "macports",
        &["port", "installed", "requested"],
        Extract::FirstToken,
    ),
    ("brew", &["brew", "leaves"], Extract::WholeLine),
    ("cargo", &["cargo", "install", "--list"], Extract::Roots),
    (
        "npm",
        &["npm", "-g", "ls", "--depth=0"],
        Extract::FirstToken,
    ),
    ("uv", &["uv", "tool", "list"], Extract::Roots),
    ("pipx", &["pipx", "list", "--short"], Extract::FirstToken),
];

/// Common CLI tooling worth telling the model about (spec §7.1 `tools`).
const TOOLS: &[&str] = &[
    "rg",
    "eza",
    "fd",
    "jq",
    "yq",
    "delta",
    "fzf",
    "gsed",
    "gawk",
    "git",
    "gh",
    "tmux",
    "screen",
    "python3",
    "node",
    "deno",
    "bun",
    "docker",
    "podman",
    "kubectl",
    "terraform",
    "uv",
    "cargo",
    "rustc",
    "go",
    "gcc",
    "clang",
    "make",
    "cmake",
    "ninja",
    "just",
    "bat",
    "rsync",
    "curl",
    "wget",
    "ssh",
    "nvim",
    "vim",
    "psql",
    "sqlite3",
    "redis-cli",
    "ffmpeg",
    "pandoc",
    "zoxide",
    "atuin",
    "starship",
    "direnv",
];

/// Build the full machine-facts kv (spec §7.1 schema, in schema order).
/// Past `deadline`, remaining probes are skipped — never the env/const keys,
/// so a fully-degraded gather still carries `os`/`arch`/`shell`.
pub(crate) fn gather(
    run: &Runner<'_>,
    env: &EnvFn<'_>,
    version: &str,
    deadline: Instant,
) -> Vec<(String, String)> {
    let probe = |argv: &[&str]| {
        if Instant::now() >= deadline {
            None
        } else {
            run(argv)
        }
    };
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut push = |key: &str, value: String| entries.push((key.to_owned(), value));

    push("os", os_name(&probe));
    if let Some(kernel) = probe(&["uname", "-sr"]) {
        push("kernel", first_line_capped(&kernel, 64));
    }
    push("arch", std::env::consts::ARCH.to_owned());
    push("shell", shell_description(&probe, env));
    push("adyton", version.to_owned());
    push("hw", hw_description(&probe));

    let mut detected: Vec<&str> = Vec::new();
    for (name, argv) in MANAGERS {
        if let Some(banner) = probe(argv) {
            detected.push(name);
            push(&format!("pkg.{name}"), first_line_capped(&banner, 64));
        }
    }

    let tools: Vec<&str> = TOOLS
        .iter()
        .copied()
        .filter(|tool| find_in_path(tool, env("PATH").as_deref()))
        .collect();
    if !tools.is_empty() {
        push("tools", tools.join(" "));
    }

    for (name, argv, extract) in PACKAGE_LISTS {
        if !detected.contains(name) {
            continue;
        }
        if let Some(listing) = probe(argv) {
            let names = extract_names(&listing, *extract);
            if !names.is_empty() {
                push(
                    &format!("packages.{name}"),
                    capped(names.join(" "), VALUE_CAP),
                );
            }
        }
    }
    entries
}

fn os_name(probe: &impl Fn(&[&str]) -> Option<String>) -> String {
    if cfg!(target_os = "macos") {
        // `sw_vers` prints ProductName/ProductVersion/BuildVersion lines.
        if let Some(out) = probe(&["sw_vers"]) {
            let field = |name: &str| {
                out.lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|v| v.trim_start_matches(':').trim().to_owned())
            };
            if let (Some(name), Some(version)) = (field("ProductName"), field("ProductVersion")) {
                return format!("{name} {version}");
            }
        }
    } else if let Ok(release) = std::fs::read_to_string("/etc/os-release")
        && let Some(pretty) = release.lines().find_map(|l| l.strip_prefix("PRETTY_NAME="))
    {
        return pretty.trim_matches('"').to_owned();
    }
    std::env::consts::OS.to_owned()
}

fn shell_description(probe: &impl Fn(&[&str]) -> Option<String>, env: &EnvFn<'_>) -> String {
    let Some(shell_path) = env("SHELL").filter(|s| !s.is_empty()) else {
        return "unknown".to_owned();
    };
    let name = shell_path
        .rsplit('/')
        .next()
        .unwrap_or(&shell_path)
        .to_owned();
    match probe(&[&shell_path, "--version"]) {
        Some(banner) => first_line_capped(&banner, 48),
        None => name,
    }
}

fn hw_description(probe: &impl Fn(&[&str]) -> Option<String>) -> String {
    let cores = std::thread::available_parallelism().map_or(0, std::num::NonZero::get);
    let mem_bytes: Option<u64> = if cfg!(target_os = "macos") {
        probe(&["sysctl", "-n", "hw.memsize"]).and_then(|v| v.trim().parse().ok())
    } else {
        // /proc/meminfo: "MemTotal:       32796552 kB"
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|info| {
                info.lines()
                    .find_map(|l| l.strip_prefix("MemTotal:"))
                    .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            })
            .map(|kb| kb * 1024)
    };
    match mem_bytes {
        Some(bytes) => format!("{cores} cores, {} GiB", bytes / (1024 * 1024 * 1024)),
        None => format!("{cores} cores"),
    }
}

/// In-process PATH lookup — cheaper than spawning `which` forty times.
fn find_in_path(name: &str, path_var: Option<&str>) -> bool {
    let Some(path_var) = path_var else {
        return false;
    };
    path_var.split(':').any(|dir| {
        if dir.is_empty() {
            return false;
        }
        let candidate = std::path::Path::new(dir).join(name);
        is_executable(&candidate)
    })
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

fn extract_names(listing: &str, extract: Extract) -> Vec<&str> {
    listing
        .lines()
        .filter_map(|line| match extract {
            Extract::FirstToken => line.split_whitespace().next(),
            Extract::WholeLine => {
                let t = line.trim();
                (!t.is_empty()).then_some(t)
            }
            Extract::Roots => {
                if line.starts_with(char::is_whitespace) {
                    None
                } else {
                    line.split_whitespace().next()
                }
            }
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn first_line_capped(text: &str, max: usize) -> String {
    capped(
        text.lines().next().unwrap_or_default().trim().to_owned(),
        max,
    )
}

fn capped(mut value: String, max: usize) -> String {
    if value.len() > max {
        let mut end = max;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value.truncate(end);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{Extract, extract_names, find_in_path, gather, run_probe};
    use std::time::{Duration, Instant};

    #[test]
    fn run_probe_captures_stdout_of_a_real_command() {
        assert_eq!(run_probe(&["echo", "hello"]).as_deref(), Some("hello"));
    }

    #[test]
    fn run_probe_kills_a_hanging_command_at_the_timeout() {
        let started = Instant::now();
        assert_eq!(run_probe(&["sleep", "5"]), None);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "probe must be killed at ~300 ms, waited {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn run_probe_missing_binary_and_failing_command_are_none() {
        assert_eq!(run_probe(&["adyton-definitely-not-installed"]), None);
        assert_eq!(run_probe(&["false"]), None);
    }

    #[test]
    fn find_in_path_checks_executable_bit() {
        let dir = std::env::temp_dir().join(format!("adyton-probe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("mytool");
        std::fs::write(&exe, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let path_var = dir.display().to_string();
            assert!(
                !find_in_path("mytool", Some(&path_var)),
                "not yet executable"
            );
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(find_in_path("mytool", Some(&path_var)));
        }
        assert!(!find_in_path("mytool", None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_names_variants() {
        assert_eq!(
            extract_names(
                "  ripgrep @14.1.1 (active)\n  eza @0.18.0\n",
                Extract::FirstToken
            ),
            vec!["ripgrep", "eza"]
        );
        assert_eq!(
            extract_names("ripgrep\n\neza\n", Extract::WholeLine),
            vec!["ripgrep", "eza"]
        );
        assert_eq!(
            extract_names(
                "ripgrep v14.1.1:\n    rg\nbat v0.24.0:\n    bat\n",
                Extract::Roots
            ),
            vec!["ripgrep", "bat"]
        );
    }

    #[test]
    fn gather_with_expired_deadline_still_yields_env_and_const_keys() {
        let calls = std::cell::Cell::new(0);
        let run = |_argv: &[&str]| {
            calls.set(calls.get() + 1);
            Some("should never be called".to_owned())
        };
        let env = |key: &str| (key == "SHELL").then(|| "/bin/zsh".to_owned());
        let entries = gather(
            &run,
            &env,
            "0.1.0",
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("clock is past epoch"),
        );

        assert_eq!(calls.get(), 0, "expired deadline must launch no probes");
        let get = |k: &str| {
            entries
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("os"), Some(std::env::consts::OS), "probe-less fallback");
        assert_eq!(get("arch"), Some(std::env::consts::ARCH));
        assert_eq!(get("shell"), Some("zsh"), "basename without version probe");
        assert_eq!(get("adyton"), Some("0.1.0"));
        assert!(get("kernel").is_none(), "probe-backed key must be absent");
    }

    #[test]
    fn gather_with_fake_runner_collects_managers_tools_and_packages() {
        let run = |argv: &[&str]| match argv {
            ["uname", "-sr"] => Some("Darwin 25.5.0".to_owned()),
            ["port", "version"] => Some("Version: 2.10.5".to_owned()),
            ["port", "installed", "requested"] => {
                Some("  ripgrep @14.1.1 (active)\n  fzf @0.54.0 (active)".to_owned())
            }
            ["cargo", "--version"] => Some("cargo 1.96.1".to_owned()),
            ["cargo", "install", "--list"] => {
                Some("cargo-bloat v0.12.1:\n    cargo-bloat".to_owned())
            }
            _ => None,
        };
        let env = |key: &str| match key {
            "SHELL" => Some("/bin/zsh".to_owned()),
            "PATH" => Some(String::new()), // no tools found on an empty PATH
            _ => None,
        };
        let entries = gather(&run, &env, "0.1.0", Instant::now() + Duration::from_secs(2));
        let get = |k: &str| {
            entries
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("kernel"), Some("Darwin 25.5.0"));
        assert_eq!(get("pkg.macports"), Some("Version: 2.10.5"));
        assert_eq!(get("pkg.cargo"), Some("cargo 1.96.1"));
        assert_eq!(get("packages.macports"), Some("ripgrep fzf"));
        assert_eq!(get("packages.cargo"), Some("cargo-bloat"));
        assert_eq!(get("pkg.npm"), None, "undetected manager stays absent");
        assert_eq!(get("tools"), None, "empty PATH finds no tools");
    }
}
