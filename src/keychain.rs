//! macOS Keychain write-side for `adyton config set-key` (spec §3). Read-side
//! needs nothing special: `api_key_cmd = security find-generic-password …`.
//!
//! The key travels stdin → `security -i` stdin. It is never an argv — neither
//! ours nor security's — so the process table never sees it.

use crate::error::{Error, Result};

pub const SERVICE: &str = "adyton";

pub fn store(profile: &str, key: &str) -> Result<()> {
    run_security(&security_command(profile, key)?)
}

/// The lookup command written into the profile's `api_key_cmd`.
pub fn lookup_command(profile: &str) -> String {
    format!("security find-generic-password -w -s {SERVICE} -a {profile}")
}

/// One `security -i` interactive-mode line. `-U` updates an existing item in
/// place, so re-running set-key rotates the key.
fn security_command(profile: &str, key: &str) -> Result<String> {
    if key.chars().any(char::is_control) {
        return Err(Error::Usage("key contains control characters".to_owned()));
    }
    let escaped = key.replace('\\', "\\\\").replace('"', "\\\"");
    Ok(format!(
        "add-generic-password -U -a \"{profile}\" -s {SERVICE} -w \"{escaped}\"\n"
    ))
}

#[cfg(target_os = "macos")]
fn run_security(command: &str) -> Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new("/usr/bin/security")
        .arg("-i")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| Error::Config(format!("cannot run security: {err}")))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(command.as_bytes())
        .map_err(|err| Error::Config(format!("cannot write to security: {err}")))?;
    let output = child
        .wait_with_output()
        .map_err(|err| Error::Config(format!("security did not finish: {err}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "security failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[cfg(not(target_os = "macos"))]
fn run_security(_command: &str) -> Result<()> {
    Err(Error::Config(
        "keychain storage is macOS-only; set api_key_cmd manually \
         (e.g. `secret-tool lookup service adyton profile <name>` on Linux)"
            .to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{lookup_command, security_command};

    #[test]
    fn command_quotes_and_escapes_the_key() {
        let cmd = security_command("claude", "pa\"ss\\word").unwrap();
        assert_eq!(
            cmd,
            "add-generic-password -U -a \"claude\" -s adyton -w \"pa\\\"ss\\\\word\"\n"
        );
    }

    #[test]
    fn control_characters_are_rejected() {
        assert!(security_command("p", "line1\nline2").is_err());
        assert!(security_command("p", "tab\there").is_err());
    }

    #[test]
    fn lookup_command_matches_what_store_writes() {
        assert_eq!(
            lookup_command("claude"),
            "security find-generic-password -w -s adyton -a claude"
        );
    }
}
