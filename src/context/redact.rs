//! Redaction ruleset (specification §7.2): applied to every history line and
//! scrollback capture before bytes leave the process. One table, one engine,
//! a unit test per rule. Bias is over-redaction — losing a value the model
//! did not need is free; leaking one is not.
//!
//! Hand-rolled (no regex crate): the shapes are simple prefixes and charsets.
//! One deliberate deviation from a naive "base64 blob" rule: `/` is not a
//! token character, so long filesystem paths never classify as blobs.

const REDACTED: &str = "«redacted»";

/// Keywords which, followed by `=` or `:`, mask the rest of the line.
const KV_KEYWORDS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "authorization",
    "bearer",
    "passwd",
    "password",
    "secret",
    "token",
];

/// Commands whose whole invocation line is dropped — their arguments alone
/// can reveal what/where secrets are.
const SECRET_TOOLS: &[&str] = &["gpg", "op", "pass", "security"];

/// Redact one line. `None` means the line must be dropped entirely.
pub fn redact_line(line: &str) -> Option<String> {
    if invokes_secret_tool(line) {
        return None;
    }
    Some(mask_token_shapes(&mask_kv_value(line)))
}

/// Redact a multi-line block (scrollback, history), dropping dead lines.
pub fn redact_block(text: &str) -> String {
    text.lines()
        .filter_map(redact_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// A secret tool in any command position (start, after `|`/`&&`/`||`/`;`,
/// after `sudo`/`env`/`xargs`, after `VAR=x` prefixes) drops the line.
fn invokes_secret_tool(line: &str) -> bool {
    let mut command_position = true;
    for word in line.split_whitespace() {
        if command_position {
            let base = word.rsplit('/').next().unwrap_or(word);
            if SECRET_TOOLS.contains(&base) {
                return true;
            }
            // `VAR=x cmd` keeps the command position open.
            if !word.contains('=') {
                command_position = false;
            }
        }
        if matches!(word, "|" | "||" | "&&" | ";" | "sudo" | "env" | "xargs") {
            command_position = true;
        }
    }
    false
}

/// Masks to end-of-line — over-redacts multi-assignment lines, which is intended.
fn mask_kv_value(line: &str) -> String {
    // Ascii lowering preserves byte offsets, so indices map back to `line`.
    let lower = line.to_ascii_lowercase();
    for keyword in KV_KEYWORDS {
        let mut search_from = 0;
        while let Some(found) = lower[search_from..].find(keyword) {
            let after_keyword = search_from + found + keyword.len();
            let rest = &line[after_keyword..];
            let value = rest.trim_start();
            if value.starts_with(['=', ':']) && value.len() > 1 {
                let separator_end = after_keyword + (rest.len() - value.len()) + 1;
                return format!("{} {REDACTED}", &line[..separator_end]);
            }
            search_from = after_keyword;
        }
    }
    line.to_owned()
}

/// Replace tokens matching known key shapes or generic long blobs.
fn mask_token_shapes(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut token = String::new();
    for c in line.chars() {
        if is_token_char(c) {
            token.push(c);
        } else {
            flush_token(&mut out, &mut token);
            out.push(c);
        }
    }
    flush_token(&mut out, &mut token);
    out
}

/// `=` and `/` are deliberately excluded: `?key=sk-…` must split at `=` so
/// the shape check sees the bare token, and paths must not become blobs.
fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+')
}

fn flush_token(out: &mut String, token: &mut String) {
    if is_secret_shape(token) {
        out.push_str(REDACTED);
    } else {
        out.push_str(token);
    }
    token.clear();
}

/// The specification §7.2 shape table.
fn is_secret_shape(token: &str) -> bool {
    let aws = token.len() == 20
        && token.strip_prefix("AKIA").is_some_and(|rest| {
            rest.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        });
    (token.starts_with("sk-") && token.len() >= 23)
        || (token.starts_with("ghp_") && token.len() >= 20)
        || ((token.starts_with("xoxb-")
            || token.starts_with("xoxp-")
            || token.starts_with("xoxs-"))
            && token.len() >= 10)
        || aws
        || is_long_blob(token)
}

/// 40+ chars of pure hex, or of base64-ish charset containing both letters
/// and digits (pure alphabetic runs are hyphenated prose, not keys).
fn is_long_blob(token: &str) -> bool {
    if token.len() < 40 {
        return false;
    }
    let hex = token.chars().all(|c| c.is_ascii_hexdigit());
    let base64ish =
        token.chars().any(|c| c.is_ascii_digit()) && token.chars().any(char::is_alphabetic);
    hex || base64ish
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, redact_block, redact_line};

    fn kept(line: &str) -> String {
        redact_line(line).expect("line should be kept")
    }

    // one test per §7.2 rule row

    #[test]
    fn kv_keyword_masks_the_value_to_end_of_line() {
        for line in [
            "export OPENAI_API_KEY=sk-proj-abc123",
            "api-key: abc",
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9",
            "PASSWORD=hunter2 OTHER=also-gone",
            "token = tk_live_xyz",
        ] {
            let out = kept(line);
            assert!(out.ends_with(REDACTED), "{line:?} → {out:?}");
            assert!(
                !out.contains("hunter2")
                    && !out.contains("abc")
                    && !out.contains("eyJ")
                    && !out.contains("tk_live"),
                "{line:?} → {out:?}"
            );
        }
    }

    #[test]
    fn openai_style_sk_tokens_are_masked_even_inside_urls() {
        let out = kept("curl https://api.test/v1?key=sk-aaaaaaaaaaaaaaaaaaaaaaaa");
        assert!(!out.contains("sk-aaa"), "{out}");
        assert!(out.contains(REDACTED));
        assert!(
            out.contains("https://api.test/v1?key="),
            "context around token kept"
        );
    }

    #[test]
    fn github_slack_and_aws_shapes_are_masked() {
        for (line, needle) in [
            (
                "git push https://ghp_abcdefghijklmnop1234@github.com/x",
                "ghp_",
            ),
            ("slack --token xoxb-1234-abcd", "xoxb-"),
            (
                "aws configure set aws_access_key_id AKIAIOSFODNN7EXAMPLE",
                "AKIA",
            ),
        ] {
            let out = kept(line);
            assert!(!out.contains(needle), "{line:?} → {out:?}");
            assert!(out.contains(REDACTED), "{line:?} → {out:?}");
        }
    }

    #[test]
    fn long_hex_and_base64_blobs_are_masked() {
        let hex = "echo deadbeefdeadbeefdeadbeefdeadbeefdeadbeef42";
        let b64 = "attach QWxhZGRpbjpvcGVuIHNlc2FtZQfoo12345678901234567890";
        for line in [hex, b64] {
            let out = kept(line);
            assert!(out.contains(REDACTED), "{line:?} → {out:?}");
        }
    }

    #[test]
    fn secret_tool_invocations_drop_the_whole_line() {
        for line in [
            "security find-generic-password -w -s adyton -a claude",
            "gpg --decrypt vault.gpg",
            "sudo gpg --import key.asc",
            "pass show personal/email",
            "history | grep foo | op read op://vault/item",
            "VAULT=prod pass ls",
            "/usr/bin/security dump-keychain",
        ] {
            assert_eq!(redact_line(line), None, "{line:?} must be dropped");
        }
    }

    #[test]
    fn ordinary_commands_are_untouched() {
        for line in [
            "ls -la ~/Downloads",
            "git commit -m improve-the-secretive-naming",
            "man gpg",
            "grep -r password docs/", // keyword without =/: value
            "cargo build --release --target x86_64-unknown-linux-musl",
            "find /a/very/long/path/that/goes/on/and/on/and/on/forever -name x",
            "run --flag this-is-a-very-long-hyphenated-flag-name-here-yes",
            "echo skate-parks-are-cool", // sk… but not the sk- shape
        ] {
            assert_eq!(kept(line), line, "must pass unchanged");
        }
    }

    #[test]
    fn block_redaction_drops_and_masks_line_wise() {
        let block = "ls -la\nsecurity find-generic-password -s x\nexport TOKEN=abc\n";
        let out = redact_block(block);
        assert_eq!(out.lines().count(), 2, "tool line dropped");
        assert!(out.contains("ls -la"));
        assert!(out.contains(REDACTED));
        assert!(!out.contains("abc"));
    }
}
