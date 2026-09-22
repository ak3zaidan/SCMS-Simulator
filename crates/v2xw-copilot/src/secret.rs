//! A value that must not be printed.
//!
//! The API key is read once from the environment and lives in a [`Secret`] from then on.
//! [`Secret`] has no `Display`, no `Serialize` and a `Debug` that prints nothing useful,
//! so the ordinary ways a value escapes — a `{:?}` in a log line, an error rendered into
//! a reply, a struct serialised into a tool result — cannot carry it. Reading it is one
//! named method, [`Secret::expose`], which is greppable.
//!
//! The one place it goes is the `Authorization` header of the API request, written to the
//! HTTP client's standard input rather than into an argument vector, so it does not appear
//! in the process table either (see [`crate::http`]).

use crate::error::{CopilotError, Result};

/// A secret string.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Reads one environment variable, trimming surrounding whitespace.
    ///
    /// # Errors
    /// [`CopilotError::MissingApiKey`] when the variable is unset or empty. The error
    /// names the variable and never its value.
    pub fn from_env(variable: &str) -> Result<Self> {
        let raw = std::env::var(variable).unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(CopilotError::MissingApiKey {
                variable: variable.to_string(),
            });
        }
        Ok(Secret(trimmed.to_string()))
    }

    /// The value itself. Every call site is a place the secret leaves this type.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the secret is empty, which it never is when it came from
    /// [`Secret::from_env`].
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `text` with every occurrence of the secret replaced.
    ///
    /// Applied to anything from outside this process before it is surfaced: a transport's
    /// diagnostics, an error body echoed back by the API.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        if self.0.is_empty() {
            return text.to_string();
        }
        text.replace(&self.0, "<redacted>")
    }
}

impl core::fmt::Debug for Secret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_does_not_print_the_value() {
        let s = Secret::new("sk-not-a-real-key");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        assert!(!format!("{s:?}").contains("sk-"));
    }

    #[test]
    fn redaction_removes_every_occurrence() {
        let s = Secret::new("sk-abc");
        let out = s.redact("curl: header 'Bearer sk-abc' rejected; retry with sk-abc");
        assert!(!out.contains("sk-abc"));
        assert_eq!(out.matches("<redacted>").count(), 2);
    }

    #[test]
    fn an_unset_variable_is_an_error_that_names_only_the_variable() {
        let name = "V2XW_COPILOT_TEST_KEY_THAT_IS_NOT_SET";
        // SAFETY of intent, not of memory: the variable is this test's own name and is
        // never read by anything else.
        let err = Secret::from_env(name).expect_err("unset");
        let text = err.to_string();
        assert!(text.contains(name));
    }
}
