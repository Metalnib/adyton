//! XDG paths (specification §3, §5.3, §7.1). Lookup goes through an injected
//! environment so path logic is testable without mutating process state.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::error::{Error, Result};

pub fn config_file() -> Result<PathBuf> {
    let base = base_dir("XDG_CONFIG_HOME", ".config", &real_env)?;
    Ok(base.join("adyton").join("config"))
}

pub fn cache_dir() -> Result<PathBuf> {
    let base = base_dir("XDG_CACHE_HOME", ".cache", &real_env)?;
    Ok(base.join("adyton"))
}

fn real_env(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

/// `$var` when set and non-empty, else `$HOME/<fallback>`.
fn base_dir(var: &str, fallback: &str, env: &dyn Fn(&str) -> Option<OsString>) -> Result<PathBuf> {
    if let Some(dir) = env(var)
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = env("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| Error::Config("HOME is not set".to_owned()))?;
    Ok(PathBuf::from(home).join(fallback))
}

#[cfg(test)]
mod tests {
    use super::base_dir;
    use std::ffi::OsString;

    fn env_with(pairs: &'static [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn xdg_var_wins_when_set() {
        let env = env_with(&[("XDG_CONFIG_HOME", "/xdg"), ("HOME", "/home/u")]);
        assert_eq!(
            base_dir("XDG_CONFIG_HOME", ".config", &env).unwrap(),
            std::path::PathBuf::from("/xdg")
        );
    }

    #[test]
    fn empty_xdg_var_falls_back_to_home() {
        let env = env_with(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/u")]);
        assert_eq!(
            base_dir("XDG_CONFIG_HOME", ".config", &env).unwrap(),
            std::path::PathBuf::from("/home/u/.config")
        );
    }

    #[test]
    fn missing_home_is_a_config_error() {
        let env = env_with(&[]);
        assert!(base_dir("XDG_CONFIG_HOME", ".config", &env).is_err());
    }
}
