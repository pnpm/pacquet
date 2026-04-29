//! Environment-variable substitution in `.npmrc` values.
//!
//! Ports pnpm's [`@pnpm/config.env-replace`](https://github.com/pnpm/components/blob/9c2bd17/config/env-replace/env-replace.ts):
//! occurrences of `${VAR}` (with optional `${VAR:-default}` fallback) are
//! replaced with the value from `env`. Backslashes immediately preceding
//! the `$` escape the placeholder so it is left as-is.
//!
//! The mirrored behaviours are:
//! * pattern: `${IDENT}` or `${IDENT:-default}`. `IDENT` is any non-empty
//!   sequence that does not contain `$`, `{`, or `}`.
//! * even-number-of-backslashes prefix: the placeholder is expanded and
//!   half of the backslashes are kept (one literal `\\` per pair).
//! * odd-number-of-backslashes prefix: the placeholder is left literal
//!   and one backslash is consumed.
//! * unset variable + no default: the call returns
//!   [`EnvReplaceError::Missing`] rather than substituting an empty
//!   string. Pacquet surfaces the same condition as a warning, matching
//!   `loadNpmrcFiles.ts`'s `substituteEnv`.
//! * empty variable + default present: the default wins; this is
//!   pnpm's behaviour even though plain shell `${VAR:-default}` would
//!   also use the default for the empty case.

use std::fmt;

/// A single missing variable surfaced from [`env_replace`].
///
/// Mirrors the `Failed to replace env in config: ${...}` message pnpm
/// produces in `loadNpmrcFiles.ts`'s `substituteEnv`. Callers typically
/// downgrade this to a warning and keep the original value verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvReplaceError {
    /// The placeholder that could not be resolved, including its
    /// surrounding `${...}` so the message lines up with pnpm's.
    pub placeholder: String,
}

impl fmt::Display for EnvReplaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Failed to replace env in config: {}", self.placeholder)
    }
}

impl std::error::Error for EnvReplaceError {}

/// Replace every `${VAR}` (or `${VAR:-default}`) placeholder in `text`
/// with its value from `env`. Returns an error on the first
/// unresolvable placeholder so the caller can warn and skip the line,
/// matching pnpm's `substituteEnv`.
pub fn env_replace<Env>(text: &str, env: Env) -> Result<String, EnvReplaceError>
where
    Env: Fn(&str) -> Option<String>,
{
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let char = bytes[index];
        if char != b'$' {
            output.push(char as char);
            index += 1;
            continue;
        }

        // Count backslashes immediately before this `$`.
        let mut backslashes = 0;
        while backslashes < output.len()
            && output.as_bytes()[output.len() - 1 - backslashes] == b'\\'
        {
            backslashes += 1;
        }

        let Some(end) = find_placeholder_end(bytes, index) else {
            output.push('$');
            index += 1;
            continue;
        };

        // Each pair of backslashes collapses to one literal backslash,
        // matching `(\\*)\$\{...\}` in the JS regex with the escape
        // semantics from `replaceEnvMatch`.
        output.truncate(output.len() - backslashes);
        for _ in 0..(backslashes / 2) {
            output.push('\\');
        }

        let placeholder = &text[index..=end];
        if backslashes % 2 == 1 {
            // Odd backslashes: the placeholder is escaped, leave it literal.
            output.push_str(placeholder);
        } else {
            let inside = &text[index + 2..end];
            let (var_name, default) = match inside.find(":-") {
                Some(separator) => (&inside[..separator], Some(&inside[separator + 2..])),
                None => (inside, None),
            };
            let value = env(var_name).filter(|value| !value.is_empty());
            match (value, default) {
                (Some(value), _) => output.push_str(&value),
                (None, Some(default)) => output.push_str(default),
                (None, None) => {
                    return Err(EnvReplaceError { placeholder: placeholder.to_owned() });
                }
            }
        }
        index = end + 1;
    }
    Ok(output)
}

/// Return the index of the closing `}` for a `${...}` starting at `start`.
/// Returns `None` if `text[start..]` is not a well-formed placeholder
/// (no opening `{` immediately after `$`, an empty body, or a stray `$`
/// or `{` inside the body).
fn find_placeholder_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start + 1)? != &b'{' {
        return None;
    }
    let body_start = start + 2;
    let mut cursor = body_start;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'}' if cursor > body_start => return Some(cursor),
            b'$' | b'{' | b'}' => return None,
            _ => cursor += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        move |key| pairs.iter().find(|(name, _)| *name == key).map(|(_, value)| value.to_string())
    }

    #[test]
    fn substitutes_simple_placeholder() {
        let env = env_of(&[("TOKEN", "abc123")]);
        assert_eq!(env_replace("Bearer ${TOKEN}", &env).unwrap(), "Bearer abc123");
    }

    #[test]
    fn returns_error_on_missing_variable() {
        let env = env_of(&[]);
        let result = env_replace("${MISSING}", env).unwrap_err();
        assert_eq!(result.placeholder, "${MISSING}");
        assert_eq!(result.to_string(), "Failed to replace env in config: ${MISSING}");
    }

    #[test]
    fn uses_default_when_variable_unset() {
        let env = env_of(&[]);
        assert_eq!(env_replace("${MISSING:-fallback}", env).unwrap(), "fallback");
    }

    #[test]
    fn uses_default_when_variable_empty() {
        let env = env_of(&[("EMPTY", "")]);
        assert_eq!(env_replace("${EMPTY:-fallback}", env).unwrap(), "fallback");
    }

    #[test]
    fn variable_wins_over_default_when_set() {
        let env = env_of(&[("PORT", "8080")]);
        assert_eq!(env_replace("${PORT:-3000}", env).unwrap(), "8080");
    }

    #[test]
    fn passthrough_when_no_placeholder() {
        assert_eq!(env_replace("plain string", env_of(&[])).unwrap(), "plain string");
    }

    #[test]
    fn lone_dollar_is_left_alone() {
        assert_eq!(env_replace("$ price", env_of(&[])).unwrap(), "$ price");
    }

    #[test]
    fn malformed_placeholder_is_left_alone() {
        // No closing brace, no nested `$`, etc.
        assert_eq!(env_replace("${OPEN", env_of(&[])).unwrap(), "${OPEN");
        assert_eq!(env_replace("${A$B}", env_of(&[("A$B", "x")])).unwrap(), "${A$B}");
    }

    #[test]
    fn odd_backslash_count_escapes_placeholder() {
        // One literal backslash => placeholder treated as literal text.
        let env = env_of(&[("X", "y")]);
        assert_eq!(env_replace(r"\${X}", &env).unwrap(), "${X}");
    }

    #[test]
    fn even_backslash_count_keeps_half_and_substitutes() {
        // Two literal backslashes => one literal `\` plus expanded value.
        let env = env_of(&[("X", "y")]);
        assert_eq!(env_replace(r"\\${X}", &env).unwrap(), r"\y");
    }

    #[test]
    fn handles_multiple_placeholders() {
        let env = env_of(&[("A", "1"), ("B", "2")]);
        assert_eq!(env_replace("${A}-${B}-${A}", env).unwrap(), "1-2-1");
    }

    #[test]
    fn placeholder_inside_url() {
        // The actual .npmrc shape pnpm users hit.
        let env = env_of(&[("NPM_TOKEN", "secret")]);
        assert_eq!(
            env_replace("//registry.npmjs.org/:_authToken=${NPM_TOKEN}", env).unwrap(),
            "//registry.npmjs.org/:_authToken=secret",
        );
    }
}
