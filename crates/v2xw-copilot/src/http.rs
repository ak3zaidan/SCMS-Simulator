//! The HTTP seam, and the one implementation that needs no dependency.
//!
//! This crate deliberately links no HTTP client and no TLS stack. Pulling one in would add
//! a large dependency tree to a workspace whose build is already the long pole, for a
//! feature that is not on any simulation path. So the transport is a trait, [`HttpPost`],
//! and the shipped implementation, [`CurlPost`], runs `curl`.
//!
//! # Why the request goes in on standard input
//!
//! `curl --config -` reads its whole configuration — url, headers and body — from standard
//! input. That is what this uses, and the reason is the key: an `Authorization` header
//! passed as `-H "Authorization: Bearer …"` is visible in the process table to every user
//! on the machine for as long as the request runs. On standard input it is visible to
//! nobody, and it is never written to a file either, so there is nothing to clean up and
//! nothing to commit.
//!
//! A separate thread writes that configuration while the parent waits on the child's
//! output, because a request larger than a pipe buffer would otherwise deadlock against a
//! child nobody is reading.
//!
//! # Determinism
//!
//! Nothing here reads a clock, draws a random number or touches a simulation artefact.
//! `max-time` is a wall-clock bound passed to the child process, which is the child's
//! business; this crate never observes one.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::error::{CopilotError, Result};
use crate::secret::Secret;

/// One request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// The absolute URL.
    pub url: String,
    /// The JSON body, already encoded.
    pub body: String,
    /// The bearer token, sent as `Authorization: Bearer …`.
    pub bearer: Option<Secret>,
    /// Any further headers, as `(name, value)`.
    pub headers: Vec<(String, String)>,
}

impl HttpRequest {
    /// A JSON POST with a bearer token.
    #[must_use]
    pub fn json(url: impl Into<String>, body: impl Into<String>, bearer: Secret) -> Self {
        HttpRequest {
            url: url.into(),
            body: body.into(),
            bearer: Some(bearer),
            headers: Vec::new(),
        }
    }

    /// A JSON POST with no token.
    #[must_use]
    pub fn json_unauthenticated(url: impl Into<String>, body: impl Into<String>) -> Self {
        HttpRequest {
            url: url.into(),
            body: body.into(),
            bearer: None,
            headers: Vec::new(),
        }
    }

    /// `text` with this request's token removed, for anything that is about to be shown.
    #[must_use]
    pub fn redact(&self, text: &str) -> String {
        match &self.bearer {
            Some(s) => s.redact(text),
            None => text.to_string(),
        }
    }
}

/// One response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// The HTTP status code, or `0` when the request never reached a server.
    pub status: u16,
    /// The response body.
    pub body: String,
}

impl HttpResponse {
    /// A response with this status and body.
    #[must_use]
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        HttpResponse {
            status,
            body: body.into(),
        }
    }

    /// Whether the status is a success.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Posting JSON somewhere.
///
/// Implement this to put the copilot behind a different client, a proxy, or a recorded
/// fixture. Nothing above this trait knows how bytes leave the process.
pub trait HttpPost {
    /// Sends one request.
    ///
    /// # Errors
    /// [`CopilotError::Transport`] when the request could not be sent. A request that
    /// reached a server and came back with an error status is **not** an error here: it is
    /// a [`HttpResponse`] with that status, so the caller can read the body.
    fn post_json(&mut self, request: &HttpRequest) -> Result<HttpResponse>;
}

/// The marker `curl` appends so the status code can be read off the end of the body.
const STATUS_MARKER: &str = "\\n%{http_code}";

/// Posts through the `curl` program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurlPost {
    /// The program to run. `curl` by default; a path for an unusual installation.
    program: String,
    /// The whole-request timeout handed to `curl`, in seconds.
    timeout_s: u64,
}

impl CurlPost {
    /// `curl`, with a 120-second timeout.
    #[must_use]
    pub fn new() -> Self {
        CurlPost {
            program: "curl".to_string(),
            timeout_s: 120,
        }
    }

    /// The same, running a named program.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// The same, with another timeout.
    #[must_use]
    pub fn with_timeout_s(mut self, timeout_s: u64) -> Self {
        self.timeout_s = timeout_s;
        self
    }
}

impl Default for CurlPost {
    fn default() -> Self {
        CurlPost::new()
    }
}

impl HttpPost for CurlPost {
    fn post_json(&mut self, request: &HttpRequest) -> Result<HttpResponse> {
        let config = curl_config(request, self.timeout_s)?;
        let mut child = Command::new(&self.program)
            .arg("--config")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                CopilotError::Transport(format!(
                    "cannot run {:?}: {e}. This crate has no built-in HTTP client; install \
                     curl or supply another `HttpPost`.",
                    self.program
                ))
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            CopilotError::Transport("the HTTP client has no standard input".to_string())
        })?;
        let writer = std::thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = stdin;
            stdin.write_all(config.as_bytes())?;
            stdin.flush()?;
            drop(stdin);
            Ok(())
        });
        let output = child.wait_with_output().map_err(|e| {
            CopilotError::Transport(format!("the HTTP client could not be waited on: {e}"))
        })?;
        match writer.join() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Err(CopilotError::Transport(format!(
                    "the request could not be written to the HTTP client: {e}"
                )));
            }
            Err(_) => {
                return Err(CopilotError::Transport(
                    "the thread writing the request stopped unexpectedly".to_string(),
                ));
            }
        }

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let (body, status) = split_status(&stdout);
        if status == 0 {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            let code = output
                .status
                .code()
                .map_or_else(|| "signal".to_string(), |c| c.to_string());
            return Err(CopilotError::Transport(format!(
                "the request did not reach a server (exit {code}): {}",
                request.redact(stderr.trim())
            )));
        }
        Ok(HttpResponse { status, body })
    }
}

/// Splits `curl`'s output into the body and the status code it was told to append.
///
/// A missing or unreadable trailer gives status `0`, which the caller reports as "never
/// reached a server" rather than guessing.
fn split_status(stdout: &str) -> (String, u16) {
    match stdout.rsplit_once('\n') {
        Some((body, tail)) => match tail.trim().parse::<u16>() {
            Ok(status) => (body.to_string(), status),
            Err(_) => (stdout.to_string(), 0),
        },
        None => (stdout.to_string(), 0),
    }
}

/// The configuration handed to `curl` on standard input.
///
/// Private on purpose: the returned string holds the bearer token in the clear, and the
/// only correct thing to do with it is to write it to the child and drop it.
fn curl_config(request: &HttpRequest, timeout_s: u64) -> Result<String> {
    let mut out = String::new();
    out.push_str("silent\n");
    out.push_str("show-error\n");
    out.push_str("request = \"POST\"\n");
    out.push_str(&format!("url = \"{}\"\n", quote(&request.url)?));
    out.push_str(&format!("max-time = {timeout_s}\n"));
    out.push_str(&format!("write-out = \"{STATUS_MARKER}\"\n"));
    out.push_str("header = \"Content-Type: application/json\"\n");
    out.push_str("header = \"Accept: application/json\"\n");
    if let Some(bearer) = &request.bearer {
        out.push_str(&format!(
            "header = \"Authorization: Bearer {}\"\n",
            quote(bearer.expose())?
        ));
    }
    for (name, value) in &request.headers {
        out.push_str(&format!(
            "header = \"{}: {}\"\n",
            quote(name)?,
            quote(value)?
        ));
    }
    // `data-binary` reads a file when its value starts with `@`. A JSON body starts with
    // `{`, and a body that does not is refused below rather than turned into a file read.
    if !request.body.starts_with('{') && !request.body.starts_with('[') {
        return Err(CopilotError::Transport(
            "the request body is not a JSON object or array".to_string(),
        ));
    }
    out.push_str(&format!("data-binary = \"{}\"\n", quote(&request.body)?));
    Ok(out)
}

/// Escapes a value for a double-quoted `curl` configuration value.
///
/// `curl` unescapes `\\`, `\"`, `\t`, `\n`, `\r` and `\v` inside quotes, so those are the
/// sequences this produces. Any other control character is refused rather than emitted:
/// a raw newline would end the line and turn the rest of the value into `curl` options,
/// which is an injection, and no legitimate header or JSON body contains one.
fn quote(value: &str) -> Result<String> {
    let mut out = String::with_capacity(value.len() + 8);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                return Err(CopilotError::Transport(format!(
                    "a request field holds the control character U+{:04X}, which cannot be \
                     sent safely",
                    c as u32
                )));
            }
            c => out.push(c),
        }
    }
    Ok(out)
}

/// An [`HttpPost`] that replays canned responses and keeps what it was asked to send.
///
/// The whole point of the seam: the OpenAI request builder and reply reader are tested
/// without a network, a key or a process.
#[derive(Debug, Default)]
pub struct ScriptedPost {
    /// The responses to hand back, in order.
    replies: Vec<HttpResponse>,
    /// How many have been used.
    used: usize,
    /// Every request that was made.
    pub seen: Vec<HttpRequest>,
}

impl ScriptedPost {
    /// A double that will answer with these, in order.
    #[must_use]
    pub fn new(replies: Vec<HttpResponse>) -> Self {
        ScriptedPost {
            replies,
            used: 0,
            seen: Vec::new(),
        }
    }

    /// How many requests it has answered.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.used
    }
}

impl HttpPost for ScriptedPost {
    fn post_json(&mut self, request: &HttpRequest) -> Result<HttpResponse> {
        self.seen.push(request.clone());
        let reply = self.replies.get(self.used).cloned().ok_or_else(|| {
            CopilotError::Transport(format!(
                "the scripted transport has no reply {} (it was given {})",
                self.used + 1,
                self.replies.len()
            ))
        })?;
        self.used += 1;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_in_the_configuration_and_never_in_an_argument() {
        let request = HttpRequest::json(
            "https://example.invalid/v1/chat",
            "{\"a\":1}",
            Secret::new("sk-test-key"),
        );
        let config = curl_config(&request, 30).expect("a plain request configures");
        assert!(config.contains("header = \"Authorization: Bearer sk-test-key\""));
        assert!(config.contains("url = \"https://example.invalid/v1/chat\""));
        assert!(config.contains("data-binary = \"{\\\"a\\\":1}\""));
        assert!(config.contains("max-time = 30"));
        // The program is invoked as `curl --config -`; nothing else is passed.
        assert_eq!(CurlPost::new().program, "curl");
    }

    #[test]
    fn a_newline_in_a_header_cannot_inject_an_option() {
        let mut request = HttpRequest::json_unauthenticated("https://example.invalid/", "{}");
        request.headers.push((
            "X-Test".to_string(),
            "a\nupload-file = /etc/passwd".to_string(),
        ));
        let config = curl_config(&request, 10).expect("the newline is escaped, not refused");
        assert!(config.contains("a\\nupload-file"));
        assert!(!config.contains("\nupload-file = "));
    }

    #[test]
    fn a_control_character_is_refused() {
        let request =
            HttpRequest::json_unauthenticated("https://example.invalid/", "{\"a\":\"\u{7}\"}");
        assert!(curl_config(&request, 10).is_err());
    }

    #[test]
    fn a_body_that_is_not_json_is_refused_rather_than_read_as_a_file() {
        let request = HttpRequest::json_unauthenticated("https://example.invalid/", "@/etc/passwd");
        assert!(curl_config(&request, 10).is_err());
    }

    #[test]
    fn the_status_trailer_is_split_off() {
        assert_eq!(
            split_status("{\"ok\":true}\n200"),
            ("{\"ok\":true}".to_string(), 200)
        );
        assert_eq!(split_status("").1, 0);
        assert_eq!(split_status("no trailer").1, 0);
    }

    #[test]
    fn the_scripted_double_records_and_replays() {
        let mut post = ScriptedPost::new(vec![HttpResponse::new(200, "{}")]);
        let request = HttpRequest::json_unauthenticated("https://example.invalid/", "{}");
        let reply = post.post_json(&request).expect("one reply was scripted");
        assert!(reply.is_success());
        assert_eq!(post.calls(), 1);
        assert_eq!(post.seen.len(), 1);
        assert!(
            post.post_json(&request).is_err(),
            "it must not invent a second reply"
        );
    }
}
