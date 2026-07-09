//! Configuration per specification §3: a flat `key = value` file with
//! `[profile.<name>]` sections. Deliberately not TOML — quoted values are
//! literals and stay that way; if richer syntax is ever needed we adopt the
//! `toml` crate instead of growing this parser.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::error::{Error, Result};
use crate::secret::Secret;

pub const ROOT_KEYS: &[&str] = &[
    "default_profile",
    "timeout_seconds",
    "session_log_commands",
    "scrollback_lines",
    "git_timeout_ms",
    "show_thinking",
];

pub const PROFILE_KEYS: &[&str] = &[
    "wire",
    "base_url",
    "model",
    "api_key",
    "api_key_cmd",
    "extra_headers",
    "max_tokens",
    "token_param",
    "temperature",
    "extra_body",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireKind {
    Openai,
    Anthropic,
}

impl WireKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WireKind::Openai => "openai",
            WireKind::Anthropic => "anthropic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenParam {
    MaxTokens,
    MaxCompletionTokens,
}

impl TokenParam {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenParam::MaxTokens => "max_tokens",
            TokenParam::MaxCompletionTokens => "max_completion_tokens",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub wire: Option<WireKind>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub api_key_cmd: Option<String>,
    pub extra_headers: Vec<String>,
    pub max_tokens: u32,
    pub token_param: Option<TokenParam>,
    pub temperature: Option<f64>,
    pub extra_body: Option<String>,
}

impl Default for Profile {
    fn default() -> Self {
        Profile {
            wire: None,
            base_url: None,
            model: None,
            api_key: None,
            api_key_cmd: None,
            extra_headers: Vec::new(),
            max_tokens: 4096,
            token_param: None,
            temperature: None,
            extra_body: None,
        }
    }
}

impl Profile {
    /// Wire default per spec §3: both wires default to `max_tokens`; the knob
    /// exists for reasoning models that require `max_completion_tokens`.
    pub fn token_param(&self) -> TokenParam {
        self.token_param.unwrap_or(TokenParam::MaxTokens)
    }

    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.wire.is_none() {
            missing.push("wire");
        }
        if self.base_url.is_none() {
            missing.push("base_url");
        }
        if self.model.is_none() {
            missing.push("model");
        }
        missing
    }
}

#[derive(Debug, PartialEq)]
pub struct Config {
    pub default_profile: Option<String>,
    pub timeout_seconds: u64,
    pub session_log_commands: usize,
    pub scrollback_lines: usize,
    pub git_timeout_ms: u64,
    pub show_thinking: bool,
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            default_profile: None,
            timeout_seconds: 60,
            session_log_commands: 20,
            scrollback_lines: 120,
            git_timeout_ms: 200,
            show_thinking: true,
            profiles: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(err) => Err(Error::Config(format!("read {}: {err}", path.display()))),
        }
    }

    /// Selection order: explicit flag → `default_profile` → the only profile.
    pub fn select_profile<'a>(&'a self, want: Option<&'a str>) -> Result<(&'a str, &'a Profile)> {
        let name = if let Some(name) = want {
            name
        } else if let Some(name) = self.default_profile.as_deref() {
            name
        } else if self.profiles.len() == 1 {
            self.profiles.keys().next().expect("len checked").as_str()
        } else if self.profiles.is_empty() {
            return Err(Error::Config(
                "no profiles configured (see `adyton config path`)".to_owned(),
            ));
        } else {
            return Err(Error::Config(
                "several profiles but no selection: set default_profile or pass --profile"
                    .to_owned(),
            ));
        };
        let profile = self.profiles.get(name).ok_or_else(|| {
            Error::Config(format!(
                "unknown profile \"{name}\" (configured: {})",
                self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
        })?;
        Ok((name, profile))
    }
}

fn err_at(line: usize, msg: impl std::fmt::Display) -> Error {
    Error::Config(format!("line {line}: {msg}"))
}

/// `[profile.<name>]` → name. Any other section is the caller's error to report.
fn section_name(trimmed: &str) -> Option<&str> {
    trimmed.strip_prefix("[profile.")?.strip_suffix(']')
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

pub fn parse(text: &str) -> Result<Config> {
    let mut config = Config::default();
    let mut current: Option<String> = None;
    let mut seen_root: Vec<String> = Vec::new();

    for (idx, raw) in text.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let name = section_name(line).ok_or_else(|| {
                err_at(
                    line_no,
                    format!("unknown section {line} (expected [profile.<name>])"),
                )
            })?;
            if !is_valid_name(name) {
                return Err(err_at(line_no, format!("invalid profile name \"{name}\"")));
            }
            if config.profiles.contains_key(name) {
                return Err(err_at(line_no, format!("duplicate profile \"{name}\"")));
            }
            config.profiles.insert(name.to_owned(), Profile::default());
            current = Some(name.to_owned());
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(err_at(
                line_no,
                format!("expected `key = value`, got \"{line}\""),
            ));
        };
        let (key, value) = (key.trim(), value.trim());
        match &current {
            None => {
                if seen_root.iter().any(|k| k == key) {
                    return Err(err_at(line_no, format!("duplicate key \"{key}\"")));
                }
                seen_root.push(key.to_owned());
                apply_root(&mut config, key, value, line_no)?;
            }
            Some(name) => {
                let profile = config.profiles.get_mut(name).expect("section inserted");
                apply_profile(profile, key, value, line_no)?;
            }
        }
    }
    Ok(config)
}

fn apply_root(config: &mut Config, key: &str, value: &str, line: usize) -> Result<()> {
    match key {
        "default_profile" => config.default_profile = Some(value.to_owned()),
        "timeout_seconds" => config.timeout_seconds = parse_num(key, value, line)?,
        "session_log_commands" => config.session_log_commands = parse_num(key, value, line)?,
        "scrollback_lines" => config.scrollback_lines = parse_num(key, value, line)?,
        "git_timeout_ms" => config.git_timeout_ms = parse_num(key, value, line)?,
        "show_thinking" => config.show_thinking = parse_bool(key, value, line)?,
        other => {
            return Err(err_at(
                line,
                format!(
                    "unknown key \"{other}\" (allowed: {})",
                    ROOT_KEYS.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn apply_profile(profile: &mut Profile, key: &str, value: &str, line: usize) -> Result<()> {
    match key {
        "wire" => {
            profile.wire = Some(match value {
                "openai" => WireKind::Openai,
                "anthropic" => WireKind::Anthropic,
                other => {
                    return Err(err_at(
                        line,
                        format!("unknown wire \"{other}\" (expected openai or anthropic)"),
                    ));
                }
            });
        }
        "base_url" => profile.base_url = Some(value.trim_end_matches('/').to_owned()),
        "model" => profile.model = Some(value.to_owned()),
        "api_key" => profile.api_key = Some(value.to_owned()),
        "api_key_cmd" => profile.api_key_cmd = Some(value.to_owned()),
        "extra_headers" => {
            if !value.contains(':') {
                return Err(err_at(
                    line,
                    format!("extra_headers must be \"Name: value\", got \"{value}\""),
                ));
            }
            profile.extra_headers.push(value.to_owned());
        }
        "max_tokens" => profile.max_tokens = parse_num(key, value, line)?,
        "token_param" => {
            profile.token_param = Some(match value {
                "max_tokens" => TokenParam::MaxTokens,
                "max_completion_tokens" => TokenParam::MaxCompletionTokens,
                other => {
                    return Err(err_at(line, format!("unknown token_param \"{other}\"")));
                }
            });
        }
        "temperature" => {
            profile.temperature = Some(value.parse().map_err(|_| {
                err_at(line, format!("invalid number for temperature: \"{value}\""))
            })?);
        }
        // Raw JSON, validated by `config check` (JSON parsing stays in wire/, D3).
        "extra_body" => profile.extra_body = Some(value.to_owned()),
        other => {
            return Err(err_at(
                line,
                format!(
                    "unknown profile key \"{other}\" (allowed: {})",
                    PROFILE_KEYS.join(", ")
                ),
            ));
        }
    }
    Ok(())
}

fn parse_num<T: std::str::FromStr>(key: &str, value: &str, line: usize) -> Result<T> {
    value
        .parse()
        .map_err(|_| err_at(line, format!("invalid number for {key}: \"{value}\"")))
}

fn parse_bool(key: &str, value: &str, line: usize) -> Result<bool> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(err_at(
            line,
            format!("{key} must be true or false, got \"{other}\""),
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum KeyRef<'a> {
    Root(&'a str),
    Profile { name: &'a str, key: &'a str },
}

fn parse_key(key: &str) -> Result<KeyRef<'_>> {
    if let Some(rest) = key.strip_prefix("profile.") {
        let (name, k) = rest.split_once('.').ok_or_else(|| {
            Error::Usage(format!("bad key \"{key}\" (expected profile.<name>.<key>)"))
        })?;
        if !is_valid_name(name) {
            return Err(Error::Usage(format!("invalid profile name \"{name}\"")));
        }
        if !PROFILE_KEYS.contains(&k) {
            return Err(Error::Usage(format!(
                "unknown profile key \"{k}\" (allowed: {})",
                PROFILE_KEYS.join(", ")
            )));
        }
        Ok(KeyRef::Profile { name, key: k })
    } else if ROOT_KEYS.contains(&key) {
        Ok(KeyRef::Root(key))
    } else {
        Err(Error::Usage(format!(
            "unknown key \"{key}\" (allowed: {} or profile.<name>.<key>)",
            ROOT_KEYS.join(", ")
        )))
    }
}

/// Effective value (defaults included), matching `git config` semantics.
pub fn get(config: &Config, key: &str) -> Result<Option<String>> {
    match parse_key(key)? {
        KeyRef::Root(k) => Ok(match k {
            "default_profile" => config.default_profile.clone(),
            "timeout_seconds" => Some(config.timeout_seconds.to_string()),
            "session_log_commands" => Some(config.session_log_commands.to_string()),
            "scrollback_lines" => Some(config.scrollback_lines.to_string()),
            "git_timeout_ms" => Some(config.git_timeout_ms.to_string()),
            "show_thinking" => Some(config.show_thinking.to_string()),
            _ => unreachable!("validated by parse_key"),
        }),
        KeyRef::Profile { name, key: k } => {
            let Some(p) = config.profiles.get(name) else {
                return Ok(None);
            };
            Ok(match k {
                "wire" => p.wire.map(|w| w.as_str().to_owned()),
                "base_url" => p.base_url.clone(),
                "model" => p.model.clone(),
                "api_key" => p.api_key.clone(),
                "api_key_cmd" => p.api_key_cmd.clone(),
                "extra_headers" => {
                    if p.extra_headers.is_empty() {
                        None
                    } else {
                        Some(p.extra_headers.join("\n"))
                    }
                }
                "max_tokens" => Some(p.max_tokens.to_string()),
                "token_param" => Some(p.token_param().as_str().to_owned()),
                "temperature" => p.temperature.map(|t| t.to_string()),
                "extra_body" => p.extra_body.clone(),
                _ => unreachable!("validated by parse_key"),
            })
        }
    }
}

/// Comment-preserving single-key update.
pub fn set_in_text(text: &str, key: &str, value: &str) -> Result<String> {
    parse(text)?; // never edit a file we cannot read back
    let kref = parse_key(key)?;
    let lines: Vec<&str> = text.lines().collect();

    let (region, key_name) = match kref {
        KeyRef::Root(k) => (root_region(&lines), k),
        KeyRef::Profile { name, key: k } => {
            if let Some(span) = profile_region(&lines, name) {
                (span, k)
            } else {
                let mut out = text.to_owned();
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if !out.is_empty() {
                    out.push('\n');
                }
                let _ = writeln!(out, "[profile.{name}]");
                let _ = writeln!(out, "{k} = {value}");
                parse(&out)?;
                return Ok(out);
            }
        }
    };

    let matches: Vec<usize> = (region.0..region.1)
        .filter(|&i| line_assigns(lines[i], key_name))
        .collect();

    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 1);
    let new_line = format!("{key_name} = {value}");
    if matches.is_empty() {
        let insert_at = (region.0..region.1)
            .rev()
            .find(|&i| !lines[i].trim().is_empty())
            .map_or(region.0, |i| i + 1);
        for (i, line) in lines.iter().enumerate() {
            if i == insert_at {
                out.push(new_line.clone());
            }
            out.push((*line).to_owned());
        }
        if insert_at == lines.len() {
            out.push(new_line);
        }
    } else {
        for (i, line) in lines.iter().enumerate() {
            if i == matches[0] {
                out.push(new_line.clone());
            } else if matches.contains(&i) {
                // collapse duplicates
            } else {
                out.push((*line).to_owned());
            }
        }
    }
    let mut result = out.join("\n");
    result.push('\n');
    parse(&result)?;
    Ok(result)
}

fn line_assigns(line: &str, key: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') {
        return false;
    }
    trimmed
        .split_once('=')
        .is_some_and(|(k, _)| k.trim() == key)
}

/// `[start, end)` of the root region: everything before the first section.
fn root_region(lines: &[&str]) -> (usize, usize) {
    let end = lines
        .iter()
        .position(|l| l.trim().starts_with('['))
        .unwrap_or(lines.len());
    (0, end)
}

/// `[start, end)` of the body of `[profile.<name>]`, excluding the header.
fn profile_region(lines: &[&str], name: &str) -> Option<(usize, usize)> {
    let header = lines
        .iter()
        .position(|l| section_name(l.trim()) == Some(name))?;
    let end = lines[header + 1..]
        .iter()
        .position(|l| l.trim().starts_with('['))
        .map_or(lines.len(), |off| header + 1 + off);
    Some((header + 1, end))
}

// API key resolution (spec §3): flag → env → profile env → api_key_cmd → file.

#[derive(Debug, PartialEq, Eq)]
pub enum KeySource {
    Flag,
    Env(String),
    Cmd,
    File,
    None,
}

impl std::fmt::Display for KeySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeySource::Flag => f.write_str("--api-key flag"),
            KeySource::Env(var) => write!(f, "env {var}"),
            KeySource::Cmd => f.write_str("api_key_cmd"),
            KeySource::File => f.write_str("config file (consider api_key_cmd + a keychain)"),
            KeySource::None => f.write_str("none (ok for local endpoints)"),
        }
    }
}

#[derive(Debug)]
pub struct ResolvedKey {
    pub secret: Option<Secret>,
    pub source: KeySource,
}

pub fn resolve_api_key(
    flag: Option<&str>,
    profile_name: &str,
    profile: &Profile,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedKey> {
    if let Some(key) = flag {
        return Ok(found(key, KeySource::Flag));
    }
    if let Some(key) = env("ADYTON_API_KEY").filter(|v| !v.is_empty()) {
        return Ok(found(&key, KeySource::Env("ADYTON_API_KEY".to_owned())));
    }
    let profile_var = profile_env_var(profile_name);
    if let Some(key) = env(&profile_var).filter(|v| !v.is_empty()) {
        return Ok(found(&key, KeySource::Env(profile_var)));
    }
    if let Some(cmd) = &profile.api_key_cmd {
        return Ok(ResolvedKey {
            secret: Some(run_key_cmd(cmd)?),
            source: KeySource::Cmd,
        });
    }
    if let Some(key) = &profile.api_key {
        return Ok(found(key, KeySource::File));
    }
    Ok(ResolvedKey {
        secret: None,
        source: KeySource::None,
    })
}

fn found(key: &str, source: KeySource) -> ResolvedKey {
    ResolvedKey {
        secret: Some(Secret::new(key.to_owned())),
        source,
    }
}

pub fn profile_env_var(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("ADYTON_{sanitized}_API_KEY")
}

/// Argv-style execution — whitespace-split, no shell involved (spec §11).
fn run_key_cmd(cmd: &str) -> Result<Secret> {
    let mut parts = cmd.split_whitespace();
    let program = parts
        .next()
        .ok_or_else(|| Error::ApiKey("api_key_cmd is empty".to_owned()))?;
    let output = std::process::Command::new(program)
        .args(parts)
        .output()
        .map_err(|err| Error::ApiKey(format!("api_key_cmd failed to start: {err}")))?;
    if !output.status.success() {
        return Err(Error::ApiKey(format!(
            "api_key_cmd exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let key = String::from_utf8(output.stdout)
        .map_err(|_| Error::ApiKey("api_key_cmd produced invalid utf-8".to_owned()))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(Error::ApiKey("api_key_cmd produced no output".to_owned()));
    }
    Ok(Secret::new(key.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# adyton config
default_profile = local
timeout_seconds = 30

[profile.local]
wire   = openai
base_url = http://localhost:11434/v1
model  = qwen3:8b

[profile.claude]
wire     = anthropic
base_url = https://api.anthropic.com
model    = claude-sonnet-4-20250514
extra_headers = anthropic-beta: token-efficient-tools
extra_headers = x-team: infra
";

    #[test]
    fn parses_the_spec_sample() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.default_profile.as_deref(), Some("local"));
        assert_eq!(cfg.timeout_seconds, 30);
        assert_eq!(cfg.session_log_commands, 20, "default when unset");
        let local = &cfg.profiles["local"];
        assert_eq!(local.wire, Some(WireKind::Openai));
        assert_eq!(local.base_url.as_deref(), Some("http://localhost:11434/v1"));
        let claude = &cfg.profiles["claude"];
        assert_eq!(claude.extra_headers.len(), 2, "extra_headers is repeatable");
    }

    #[test]
    fn empty_text_yields_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
    }

    #[test]
    fn quoted_values_stay_literal() {
        let cfg = parse("[profile.p]\nmodel = \"gpt\"\n").unwrap();
        assert_eq!(cfg.profiles["p"].model.as_deref(), Some("\"gpt\""));
    }

    #[test]
    fn base_url_trailing_slash_is_normalized() {
        let cfg = parse("[profile.p]\nbase_url = http://x/v1/\n").unwrap();
        assert_eq!(cfg.profiles["p"].base_url.as_deref(), Some("http://x/v1"));
    }

    #[test]
    fn errors_carry_line_numbers() {
        for (text, needle) in [
            ("bogus_key = 1\n", "line 1"),
            ("timeout_seconds = fast\n", "invalid number"),
            ("[section]\n", "unknown section"),
            ("[profile.p]\nwire = grpc\n", "unknown wire"),
            ("[profile.p]\nnope = 1\n", "unknown profile key"),
            (
                "[profile.p]\nextra_headers = no-colon-here\n",
                "Name: value",
            ),
            (
                "default_profile = a\ndefault_profile = b\n",
                "duplicate key",
            ),
            ("[profile.p]\n[profile.p]\n", "duplicate profile"),
            ("just some text\n", "key = value"),
        ] {
            let err = parse(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?} → {err}");
        }
    }

    #[test]
    fn select_profile_precedence() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(cfg.select_profile(Some("claude")).unwrap().0, "claude");
        assert_eq!(cfg.select_profile(None).unwrap().0, "local");
        assert!(cfg.select_profile(Some("nope")).is_err());

        let single = parse("[profile.only]\nwire = openai\n").unwrap();
        assert_eq!(single.select_profile(None).unwrap().0, "only");

        assert!(Config::default().select_profile(None).is_err());
    }

    #[test]
    fn missing_required_lists_gaps() {
        let cfg = parse("[profile.p]\nwire = openai\n").unwrap();
        assert_eq!(
            cfg.profiles["p"].missing_required(),
            vec!["base_url", "model"]
        );
    }

    #[test]
    fn get_returns_effective_values() {
        let cfg = parse(SAMPLE).unwrap();
        assert_eq!(get(&cfg, "timeout_seconds").unwrap().unwrap(), "30");
        assert_eq!(get(&cfg, "session_log_commands").unwrap().unwrap(), "20");
        assert_eq!(
            get(&cfg, "profile.local.model").unwrap().unwrap(),
            "qwen3:8b"
        );
        assert_eq!(
            get(&cfg, "profile.local.max_tokens").unwrap().unwrap(),
            "4096"
        );
        assert_eq!(get(&cfg, "profile.local.temperature").unwrap(), None);
        assert_eq!(get(&cfg, "profile.ghost.model").unwrap(), None);
        assert!(get(&cfg, "no_such_key").is_err());
        assert_eq!(get(&cfg, "show_thinking").unwrap().unwrap(), "true");
        assert!(get(&cfg, "profile.local.bogus").is_err());
    }

    #[test]
    fn extra_body_is_stored_verbatim() {
        let cfg = parse(
            "[profile.p]\nwire = openai\nbase_url = http://x/v1\nmodel = m\nextra_body = {\"reasoning_effort\":\"none\"}\n",
        )
        .unwrap();
        assert_eq!(
            get(&cfg, "profile.p.extra_body").unwrap().unwrap(),
            "{\"reasoning_effort\":\"none\"}"
        );
    }

    #[test]
    fn set_replaces_root_key_preserving_comments() {
        let out = set_in_text(SAMPLE, "timeout_seconds", "90").unwrap();
        assert!(out.contains("# adyton config"), "comment preserved");
        assert!(out.contains("timeout_seconds = 90"));
        assert!(!out.contains("timeout_seconds = 30"));
    }

    #[test]
    fn set_adds_missing_key_to_existing_profile() {
        let out = set_in_text(SAMPLE, "profile.local.temperature", "0.2").unwrap();
        let cfg = parse(&out).unwrap();
        assert_eq!(cfg.profiles["local"].temperature, Some(0.2));
        assert_eq!(
            cfg.profiles["claude"].temperature, None,
            "other section untouched"
        );
    }

    #[test]
    fn set_creates_missing_profile() {
        let out = set_in_text("", "profile.work.model", "gpt-4o").unwrap();
        assert!(out.contains("[profile.work]"));
        assert_eq!(
            parse(&out).unwrap().profiles["work"].model.as_deref(),
            Some("gpt-4o")
        );
    }

    #[test]
    fn set_collapses_duplicate_assignments() {
        let text = "[profile.p]\nextra_headers = a: 1\nextra_headers = b: 2\n";
        let out = set_in_text(text, "profile.p.extra_headers", "c: 3").unwrap();
        assert_eq!(
            parse(&out).unwrap().profiles["p"].extra_headers,
            vec!["c: 3"]
        );
    }

    #[test]
    fn set_rejects_unknown_keys_and_broken_files() {
        assert!(set_in_text("", "bogus", "1").is_err());
        assert!(set_in_text("garbage line\n", "timeout_seconds", "1").is_err());
    }

    #[test]
    fn key_resolution_precedence() {
        let profile = Profile {
            api_key: Some("from-file".to_owned()),
            api_key_cmd: Some("echo from-cmd".to_owned()),
            ..Profile::default()
        };
        let env_all = |k: &str| match k {
            "ADYTON_API_KEY" => Some("from-env".to_owned()),
            "ADYTON_WORK_API_KEY" => Some("from-profile-env".to_owned()),
            _ => None,
        };
        let flag = resolve_api_key(Some("from-flag"), "work", &profile, &env_all).unwrap();
        assert_eq!(flag.source, KeySource::Flag);
        assert_eq!(flag.secret.unwrap().expose(), "from-flag");

        let env = resolve_api_key(None, "work", &profile, &env_all).unwrap();
        assert_eq!(env.source, KeySource::Env("ADYTON_API_KEY".to_owned()));

        let env_profile_only =
            |k: &str| (k == "ADYTON_WORK_API_KEY").then(|| "from-profile-env".to_owned());
        let penv = resolve_api_key(None, "work", &profile, &env_profile_only).unwrap();
        assert_eq!(
            penv.source,
            KeySource::Env("ADYTON_WORK_API_KEY".to_owned())
        );

        let no_env = |_: &str| None;
        let cmd = resolve_api_key(None, "work", &profile, &no_env).unwrap();
        assert_eq!(cmd.source, KeySource::Cmd);
        assert_eq!(cmd.secret.unwrap().expose(), "from-cmd");

        let file_only = Profile {
            api_key: Some("from-file".to_owned()),
            ..Profile::default()
        };
        let file = resolve_api_key(None, "work", &file_only, &no_env).unwrap();
        assert_eq!(file.source, KeySource::File);

        let bare = resolve_api_key(None, "work", &Profile::default(), &no_env).unwrap();
        assert_eq!(bare.source, KeySource::None);
        assert!(bare.secret.is_none());
    }

    #[test]
    fn profile_env_var_sanitizes_names() {
        assert_eq!(profile_env_var("my-prof.1"), "ADYTON_MY_PROF_1_API_KEY");
    }

    #[test]
    fn failing_key_cmd_is_an_api_key_error() {
        let profile = Profile {
            api_key_cmd: Some("false".to_owned()),
            ..Profile::default()
        };
        let err = resolve_api_key(None, "p", &profile, &|_| None).unwrap_err();
        assert_eq!(err.exit_code(), 4);
    }
}
