//! Thin entry point: parse argv → dispatch → map errors to the
//! specification §2.1 exit codes. All logic lives in the modules.

mod cli;
mod config;
mod context;
mod error;
mod glue;
mod keychain;
mod overlay;
mod paths;
mod prompt;
mod run;
mod secret;
mod selfupdate;
mod wire;

use std::process::ExitCode;

use cli::{Command, ConfigAction};
use error::{Error, Result};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("adyton: {err}");
            ExitCode::from(err.exit_code())
        }
    }
}

fn run() -> Result<()> {
    match cli::parse(std::env::args_os())? {
        Command::Version => {
            println!("adyton {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Help => {
            print!("{}", cli::HELP);
            Ok(())
        }
        Command::Config(action) => run_config(&action),
        Command::Init { shell } => {
            print!("{}", glue::init(shell));
            Ok(())
        }
        Command::Suggest { opts, query } => run::suggest(&opts, &query),
        Command::Ask { opts, query } => run::ask(&opts, &query),
        Command::Fix { opts, rerun } => run::fix(&opts, rerun),
        Command::ContextRefresh => context::refresh_foreground(),
        Command::SelfUpdate { check, yes } => selfupdate::self_update(check, yes),
    }
}

fn run_config(action: &ConfigAction) -> Result<()> {
    let path = paths::config_file()?;
    match action {
        ConfigAction::Path => {
            println!("{}", path.display());
            Ok(())
        }
        ConfigAction::Get { key } => {
            let cfg = config::Config::load(&path)?;
            if let Some(value) = config::get(&cfg, key)? {
                println!("{value}");
            }
            Ok(())
        }
        ConfigAction::Set { key, value } => {
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
                Err(err) => {
                    return Err(Error::Config(format!("read {}: {err}", path.display())));
                }
            };
            let updated = config::set_in_text(&text, key, value)?;
            write_private(&path, &updated)
        }
        ConfigAction::SetKey { profile } => run_config_set_key(profile, &path),
        ConfigAction::Check { profile } => run_config_check(profile.as_deref(), &path),
    }
}

/// `config set-key`: key arrives on stdin (never argv), goes to the keychain,
/// and the profile's `api_key_cmd` is pointed at the lookup.
fn run_config_set_key(profile: &str, path: &std::path::Path) -> Result<()> {
    let cfg = config::Config::load(path)?;
    if !cfg.profiles.contains_key(profile) {
        return Err(Error::Config(format!(
            "unknown profile \"{profile}\" — create it first \
             (adyton config set profile.{profile}.wire openai)"
        )));
    }
    let key = read_key_from_stdin()?;
    keychain::store(profile, key.expose())?;

    let text = std::fs::read_to_string(path)
        .map_err(|err| Error::Config(format!("read {}: {err}", path.display())))?;
    let updated = config::set_in_text(
        &text,
        &format!("profile.{profile}.api_key_cmd"),
        &keychain::lookup_command(profile),
    )?;
    write_private(path, &updated)?;
    println!(
        "stored in keychain (service \"{}\", account \"{profile}\"); api_key_cmd updated",
        keychain::SERVICE
    );
    Ok(())
}

fn read_key_from_stdin() -> Result<secret::Secret> {
    use std::io::{IsTerminal as _, Read as _};

    let mut input = String::new();
    if std::io::stdin().is_terminal() {
        eprintln!("paste the api key, then press Enter:");
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|err| Error::Usage(format!("cannot read key: {err}")))?;
    } else {
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|err| Error::Usage(format!("cannot read key: {err}")))?;
    }
    let key = input.trim();
    if key.is_empty() {
        return Err(Error::Usage("no key provided on stdin".to_owned()));
    }
    Ok(secret::Secret::new(key.to_owned()))
}

/// `config check`: validate the file, select a profile, resolve the api key —
/// reporting the key's SOURCE, never the key itself.
fn run_config_check(want: Option<&str>, path: &std::path::Path) -> Result<()> {
    let cfg = config::Config::load(path)?;
    let (name, profile) = cfg.select_profile(want)?;
    let missing = profile.missing_required();
    if !missing.is_empty() {
        return Err(Error::Config(format!(
            "profile \"{name}\": missing required {}",
            missing.join(", ")
        )));
    }
    let resolved = config::resolve_api_key(None, name, profile, &|var| std::env::var(var).ok())?;

    println!("config    = {}", path.display());
    println!("profile   = {name}");
    println!(
        "wire      = {}",
        profile.wire.expect("required checked").as_str()
    );
    println!(
        "base_url  = {}",
        profile.base_url.as_deref().expect("required checked")
    );
    println!(
        "model     = {}",
        profile.model.as_deref().expect("required checked")
    );
    println!(
        "max_tokens = {} ({})",
        profile.max_tokens,
        profile.token_param().as_str()
    );
    for header in &profile.extra_headers {
        let shown = header.split_once(':').map_or(header.as_str(), |(n, _)| n);
        println!("header    = {shown}: …");
    }
    println!("api_key   = {}", resolved.source);
    drop(resolved.secret); // zeroized; check never uses the key material
    Ok(())
}

/// Config may hold a key; keep it out of other users' reach (spec §10).
fn write_private(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write as _;

    let dir = path.parent().expect("config path always has a parent");
    std::fs::create_dir_all(dir)
        .map_err(|err| Error::Config(format!("create {}: {err}", dir.display())))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|err| Error::Config(format!("write {}: {err}", path.display())))?;
    file.write_all(contents.as_bytes())
        .map_err(|err| Error::Config(format!("write {}: {err}", path.display())))
}
