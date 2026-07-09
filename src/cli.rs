//! Argument parsing for the specification §2 command tree. Pure function of
//! argv → `Command`, so every branch is unit-testable.

use std::ffi::OsString;

use lexopt::{Arg, Parser};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
    Fish,
}

impl Shell {
    pub fn as_str(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
            Shell::Fish => "fish",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "zsh" => Ok(Shell::Zsh),
            "bash" => Ok(Shell::Bash),
            "fish" => Ok(Shell::Fish),
            other => Err(Error::Usage(format!(
                "unsupported shell \"{other}\" (expected zsh, bash or fish)"
            ))),
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunOpts {
    pub shell: Option<Shell>,
    pub profile: Option<String>,
    pub no_context: bool,
    pub plain: bool,
    pub no_thinking: bool,
    pub api_key: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigAction {
    Get { key: String },
    Set { key: String, value: String },
    SetKey { profile: String },
    Check { profile: Option<String> },
    Path,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Init { shell: Shell },
    Suggest { opts: RunOpts, query: String },
    Ask { opts: RunOpts, query: String },
    Fix { opts: RunOpts, rerun: bool },
    ContextRefresh,
    Config(ConfigAction),
    SelfUpdate { check: bool, yes: bool },
    Version,
    Help,
}

pub const HELP: &str = "\
adyton — natural language → shell command (never auto-executed)

USAGE:
  adyton init <zsh|bash|fish>            print shell glue for eval
  adyton suggest [OPTIONS] -- <query>    natural language → command on stdout
  adyton ask [OPTIONS] -- <question>     answer a question (prose, streamed)
  adyton fix [OPTIONS] [--rerun]         fix the last failed command
  adyton context refresh                 rebuild the context cache
  adyton selfupdate [--check] [--yes]    update adyton to the latest release
  adyton config get <key>                print an effective config value
  adyton config set <key> <value>        update the config file
  adyton config set-key <profile>        store an api key in the macOS keychain
                                         (key read from stdin, never argv)
  adyton config check [--profile <p>]    validate config and key resolution
  adyton config path                     print the config file location
  adyton --version | --help

OPTIONS (suggest, fix):
  -s, --shell <zsh|bash|fish>   dialect hint for the generated command
  -p, --profile <name>          provider profile from the config file
      --no-context              send the query only
      --plain                   no spinner or streaming on stderr
      --no-thinking             hide the model's streamed reasoning (experimental)
      --api-key <key>           override the api key resolution chain
";

pub fn parse<I>(args: I) -> Result<Command>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = Parser::from_iter(args);
    let Some(first) = parser.next()? else {
        return Err(Error::Usage(
            "missing command (try `adyton --help`)".to_owned(),
        ));
    };
    match first {
        Arg::Long("version") | Arg::Short('V') => finish(&mut parser, Command::Version),
        Arg::Long("help") | Arg::Short('h') => finish(&mut parser, Command::Help),
        Arg::Value(v) => match into_string(v)?.as_str() {
            "init" => parse_init(&mut parser),
            "suggest" => parse_run(&mut parser, RunKind::Suggest),
            "ask" => parse_run(&mut parser, RunKind::Ask),
            "fix" => parse_run(&mut parser, RunKind::Fix),
            "context" => parse_context(&mut parser),
            "config" => parse_config(&mut parser),
            "selfupdate" => parse_selfupdate(&mut parser),
            other => Err(Error::Usage(format!(
                "unknown command \"{other}\" (try `adyton --help`)"
            ))),
        },
        arg => Err(unexpected_leading(arg)),
    }
}

/// A run-option (`--plain`, `-s`, …) belongs after its command, so a leading one
/// is a usage error — say where it goes instead of a bare "invalid option".
fn unexpected_leading(arg: Arg) -> Error {
    match arg {
        Arg::Long(name) => Error::Usage(format!(
            "option '--{name}' must come after the command, e.g. `adyton suggest --{name} …`"
        )),
        Arg::Short(c) => Error::Usage(format!(
            "option '-{c}' must come after the command, e.g. `adyton suggest -{c} …`"
        )),
        other @ Arg::Value(_) => other.unexpected().into(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RunKind {
    Suggest,
    Ask,
    Fix,
}

fn parse_run(parser: &mut Parser, kind: RunKind) -> Result<Command> {
    let mut opts = RunOpts::default();
    let mut rerun = false;
    let mut words: Vec<String> = Vec::new();
    while let Some(arg) = parser.next()? {
        match arg {
            Arg::Short('s') | Arg::Long("shell") => {
                opts.shell = Some(Shell::parse(&value_of(parser)?)?);
            }
            Arg::Short('p') | Arg::Long("profile") => opts.profile = Some(value_of(parser)?),
            Arg::Long("no-context") => opts.no_context = true,
            Arg::Long("plain") => opts.plain = true,
            Arg::Long("no-thinking") => opts.no_thinking = true,
            Arg::Long("api-key") => opts.api_key = Some(value_of(parser)?),
            Arg::Long("rerun") if kind == RunKind::Fix => rerun = true,
            Arg::Value(word) => words.push(into_string(word)?),
            arg => return Err(arg.unexpected().into()),
        }
    }
    match kind {
        RunKind::Suggest => {
            if words.is_empty() {
                Err(Error::Usage(
                    "suggest: missing query (adyton suggest -- <what you want>)".to_owned(),
                ))
            } else {
                Ok(Command::Suggest {
                    opts,
                    query: words.join(" "),
                })
            }
        }
        RunKind::Ask => {
            if words.is_empty() {
                Err(Error::Usage(
                    "ask: missing question (adyton ask -- <your question>)".to_owned(),
                ))
            } else {
                Ok(Command::Ask {
                    opts,
                    query: words.join(" "),
                })
            }
        }
        RunKind::Fix => {
            if words.is_empty() {
                Ok(Command::Fix { opts, rerun })
            } else {
                Err(Error::Usage(
                    "fix takes no arguments (it reads the last failure)".to_owned(),
                ))
            }
        }
    }
}

fn parse_init(parser: &mut Parser) -> Result<Command> {
    let shell = Shell::parse(&next_value(parser, "init: missing shell")?)?;
    finish(parser, Command::Init { shell })
}

fn parse_selfupdate(parser: &mut Parser) -> Result<Command> {
    let mut check = false;
    let mut yes = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Arg::Long("check") => check = true,
            Arg::Long("yes") | Arg::Short('y') => yes = true,
            arg => return Err(arg.unexpected().into()),
        }
    }
    Ok(Command::SelfUpdate { check, yes })
}

fn parse_context(parser: &mut Parser) -> Result<Command> {
    let action = next_value(parser, "context: missing action (expected `refresh`)")?;
    if action != "refresh" {
        return Err(Error::Usage(format!(
            "context: unknown action \"{action}\" (expected `refresh`)"
        )));
    }
    finish(parser, Command::ContextRefresh)
}

fn parse_config(parser: &mut Parser) -> Result<Command> {
    let action = next_value(parser, "config: missing action (get, set, check or path)")?;
    let command = match action.as_str() {
        "get" => Command::Config(ConfigAction::Get {
            key: next_value(parser, "config get: missing key")?,
        }),
        "set" => Command::Config(ConfigAction::Set {
            key: next_value(parser, "config set: missing key")?,
            value: next_value(parser, "config set: missing value")?,
        }),
        "set-key" => Command::Config(ConfigAction::SetKey {
            profile: next_value(parser, "config set-key: missing profile")?,
        }),
        "check" => {
            let mut profile = None;
            while let Some(arg) = parser.next()? {
                match arg {
                    Arg::Short('p') | Arg::Long("profile") => profile = Some(value_of(parser)?),
                    arg => return Err(arg.unexpected().into()),
                }
            }
            return Ok(Command::Config(ConfigAction::Check { profile }));
        }
        "path" => Command::Config(ConfigAction::Path),
        other => {
            return Err(Error::Usage(format!(
                "config: unknown action \"{other}\" (expected get, set, check or path)"
            )));
        }
    };
    finish(parser, command)
}

/// Reject trailing arguments after a fully-parsed command.
fn finish(parser: &mut Parser, command: Command) -> Result<Command> {
    match parser.next()? {
        None => Ok(command),
        Some(arg) => Err(arg.unexpected().into()),
    }
}

fn next_value(parser: &mut Parser, missing: &str) -> Result<String> {
    match parser.next()? {
        Some(Arg::Value(v)) => into_string(v),
        Some(arg) => Err(arg.unexpected().into()),
        None => Err(Error::Usage(missing.to_owned())),
    }
}

fn value_of(parser: &mut Parser) -> Result<String> {
    into_string(parser.value()?)
}

fn into_string(os: OsString) -> Result<String> {
    os.into_string()
        .map_err(|bad| Error::Usage(format!("invalid unicode in argument: {}", bad.display())))
}

#[cfg(test)]
mod tests {
    use super::{Command, ConfigAction, RunOpts, Shell, parse};
    use crate::error::Error;

    fn cmd(args: &[&str]) -> Result<Command, Error> {
        parse(std::iter::once("adyton").chain(args.iter().copied()))
    }

    fn usage_of(args: &[&str]) -> String {
        match cmd(args) {
            Err(Error::Usage(msg)) => msg,
            other => panic!("expected usage error, got {other:?}"),
        }
    }

    #[test]
    fn version_and_help() {
        assert_eq!(cmd(&["--version"]).unwrap(), Command::Version);
        assert_eq!(cmd(&["-V"]).unwrap(), Command::Version);
        assert_eq!(cmd(&["--help"]).unwrap(), Command::Help);
    }

    #[test]
    fn no_args_is_usage_error() {
        usage_of(&[]);
    }

    #[test]
    fn trailing_args_after_version_rejected() {
        usage_of(&["--version", "extra"]);
    }

    #[test]
    fn init_parses_each_shell() {
        for (name, shell) in [
            ("zsh", Shell::Zsh),
            ("bash", Shell::Bash),
            ("fish", Shell::Fish),
        ] {
            assert_eq!(cmd(&["init", name]).unwrap(), Command::Init { shell });
        }
        assert!(usage_of(&["init", "powershell"]).contains("unsupported shell"));
        usage_of(&["init"]);
    }

    #[test]
    fn suggest_joins_query_words_with_and_without_separator() {
        let expected = Command::Suggest {
            opts: RunOpts::default(),
            query: "find big files".to_owned(),
        };
        assert_eq!(
            cmd(&["suggest", "--", "find", "big", "files"]).unwrap(),
            expected
        );
        assert_eq!(cmd(&["suggest", "find", "big", "files"]).unwrap(), expected);
    }

    #[test]
    fn suggest_parses_all_options() {
        let got = cmd(&[
            "suggest",
            "-s",
            "fish",
            "--profile",
            "work",
            "--no-context",
            "--plain",
            "--no-thinking",
            "--api-key",
            "k",
            "--",
            "list",
            "ports",
        ])
        .unwrap();
        assert_eq!(
            got,
            Command::Suggest {
                opts: RunOpts {
                    shell: Some(Shell::Fish),
                    profile: Some("work".to_owned()),
                    no_context: true,
                    plain: true,
                    no_thinking: true,
                    api_key: Some("k".to_owned()),
                },
                query: "list ports".to_owned(),
            }
        );
    }

    #[test]
    fn suggest_requires_a_query() {
        assert!(usage_of(&["suggest"]).contains("missing query"));
    }

    #[test]
    fn ask_joins_the_question_and_requires_one() {
        assert_eq!(
            cmd(&["ask", "--", "what", "is", "this", "error"]).unwrap(),
            Command::Ask {
                opts: RunOpts::default(),
                query: "what is this error".to_owned(),
            }
        );
        assert!(usage_of(&["ask"]).contains("missing question"));
    }

    #[test]
    fn ask_parses_like_suggest_but_is_its_own_command() {
        assert_eq!(
            cmd(&["ask", "--", "why", "did", "that", "fail"]).unwrap(),
            Command::Ask {
                opts: RunOpts::default(),
                query: "why did that fail".to_owned(),
            }
        );
        assert!(usage_of(&["ask"]).contains("missing question"));
        assert!(
            usage_of(&["ask", "--rerun", "q"]).contains("--rerun"),
            "fix-only flag"
        );
    }

    #[test]
    fn fix_takes_rerun_but_no_positional_args() {
        assert_eq!(
            cmd(&["fix", "--rerun"]).unwrap(),
            Command::Fix {
                opts: RunOpts::default(),
                rerun: true
            }
        );
        assert!(usage_of(&["fix", "something"]).contains("no arguments"));
        assert!(usage_of(&["suggest", "--rerun", "q"]).contains("--rerun"));
    }

    #[test]
    fn selfupdate_flags() {
        assert_eq!(
            cmd(&["selfupdate"]).unwrap(),
            Command::SelfUpdate {
                check: false,
                yes: false
            }
        );
        assert_eq!(
            cmd(&["selfupdate", "--check", "--yes"]).unwrap(),
            Command::SelfUpdate {
                check: true,
                yes: true
            }
        );
        assert!(usage_of(&["selfupdate", "bogus"]).contains("bogus"));
    }

    #[test]
    fn context_refresh_only() {
        assert_eq!(
            cmd(&["context", "refresh"]).unwrap(),
            Command::ContextRefresh
        );
        usage_of(&["context"]);
        usage_of(&["context", "rebuild"]);
        usage_of(&["context", "refresh", "extra"]);
    }

    #[test]
    fn config_actions() {
        assert_eq!(
            cmd(&["config", "path"]).unwrap(),
            Command::Config(ConfigAction::Path)
        );
        assert_eq!(
            cmd(&["config", "get", "timeout_seconds"]).unwrap(),
            Command::Config(ConfigAction::Get {
                key: "timeout_seconds".to_owned()
            })
        );
        assert_eq!(
            cmd(&["config", "set", "profile.local.model", "qwen3:8b"]).unwrap(),
            Command::Config(ConfigAction::Set {
                key: "profile.local.model".to_owned(),
                value: "qwen3:8b".to_owned(),
            })
        );
        assert_eq!(
            cmd(&["config", "check", "--profile", "work"]).unwrap(),
            Command::Config(ConfigAction::Check {
                profile: Some("work".to_owned())
            })
        );
        assert_eq!(
            cmd(&["config", "set-key", "claude"]).unwrap(),
            Command::Config(ConfigAction::SetKey {
                profile: "claude".to_owned()
            })
        );
        usage_of(&["config", "set-key"]);
        usage_of(&["config"]);
        usage_of(&["config", "list"]);
        usage_of(&["config", "set", "key"]);
    }

    #[test]
    fn unknown_command_names_the_offender() {
        assert!(usage_of(&["sugest"]).contains("sugest"));
    }

    #[test]
    fn leading_option_hints_it_belongs_after_the_command() {
        let msg = usage_of(&["--plain", "suggest", "--", "x"]);
        assert!(msg.contains("--plain"), "{msg}");
        assert!(msg.contains("after the command"), "{msg}");
        assert!(usage_of(&["-s", "zsh", "suggest"]).contains("-s"));
    }
}
