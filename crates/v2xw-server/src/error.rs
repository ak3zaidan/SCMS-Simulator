//! The crate error type, and its mapping onto the JSON-RPC error codes of vwp-v1 §6.4.
//!
//! Every failure a control call can produce is one of these, and every one of them has a
//! code. That is deliberate: §10.7 R3 asks for `-32602` with a `data` array of
//! `{path, message, hint}`, and a stringly-typed error cannot produce that shape. The
//! numbering lives here, once, so the HTTP path (§6.2) and the socket path (§6.1) cannot
//! disagree about what a failure is called.

use serde_json::{Value, json};

/// A control-surface or transport failure.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Invalid JSON on the text channel (§6.4 `-32700`).
    #[error("parse error: {0}")]
    Parse(String),
    /// Not a valid JSON-RPC 2.0 request object (§6.4 `-32600`). A batch array is one of
    /// these, per the §6.1 decision and conformance R10.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Unknown method (§6.4 `-32601`).
    #[error("method not found: {0}")]
    MethodNotFound(String),
    /// Invalid params (§6.4 `-32602`). Carries the `{path, message, hint}` rows of R3.
    #[error("invalid params")]
    InvalidParams(Vec<ParamError>),
    /// An internal failure (§6.4 `-32603`).
    #[error("internal error: {0}")]
    Internal(String),
    /// No such run id (§6.4 `-32000`).
    #[error("run not found: {0}")]
    RunNotFound(String),
    /// The run is already running (§6.4 `-32001`).
    #[error("run already running")]
    RunAlreadyRunning,
    /// Pause, step or stop with nothing to act on (§6.4 `-32002`).
    #[error("run not running: {0}")]
    RunNotRunning(String),
    /// Seek target outside the recorded range (§6.4 `-32003`).
    #[error("seek out of range")]
    SeekOutOfRange {
        /// Earliest seekable sim time, nanoseconds.
        min_ns: u64,
        /// Latest seekable sim time, nanoseconds.
        max_ns: u64,
    },
    /// The scenario does not validate (§6.4 `-32004`).
    #[error("scenario invalid")]
    ScenarioInvalid(Vec<ParamError>),
    /// Unknown world hash or unreadable source (§6.4 `-32005`).
    #[error("world not found: {0}")]
    WorldNotFound(String),
    /// Unknown node, link, entity, actor or lane id (§6.4 `-32006`).
    #[error("unknown {kind} id {id}")]
    UnknownId {
        /// What kind of id was not found.
        kind: &'static str,
        /// The id as the caller wrote it.
        id: String,
    },
    /// Unknown metric (§6.4 `-32007`).
    #[error("unknown metric: {metric}")]
    UnknownMetric {
        /// The metric the caller asked for.
        metric: String,
        /// Near-miss suggestions from the catalogue.
        did_you_mean: Vec<String>,
    },
    /// An export failed (§6.4 `-32008`).
    #[error("export failed at {stage}: {detail}")]
    ExportFailed {
        /// Which stage failed.
        stage: String,
        /// What went wrong.
        detail: String,
    },
    /// A replay-only, live-only or HTTP-only restriction (§6.4 `-32009`).
    #[error("not supported here: {0}")]
    NotSupportedHere(String),
    /// Another long operation holds the run (§6.4 `-32010`).
    #[error("busy: {operation}")]
    Busy {
        /// The operation holding the run.
        operation: String,
        /// Its job id.
        job_id: String,
    },
    /// Unknown experiment (§6.4 `-32011`).
    #[error("experiment not found: {0}")]
    ExperimentNotFound(String),
    /// A file-system or network failure (§6.4 `-32013`).
    #[error("io error at {path}: {errno}")]
    Io {
        /// The path involved.
        path: String,
        /// The operating-system error.
        errno: String,
    },
    /// The `node` profile forbids this (§6.4 `-32040`, §5.3).
    #[error("visibility denied: {field}")]
    VisibilityDenied {
        /// The field or channel that was withheld.
        field: String,
        /// Its visibility tag; always `"GT"` in v1.
        visibility: &'static str,
    },
    /// Missing or incorrect bearer token (§6.4 `-32041`).
    #[error("unauthorized")]
    Unauthorized,
    /// A protocol major version this server cannot serve (§6.4 `-32050`).
    #[error("unsupported version")]
    UnsupportedVersion,
}

/// One `{path, message, hint}` row of a `-32602` or `-32004` `data` array (§6.4).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ParamError {
    /// JSON Pointer into the params or scenario document.
    pub path: String,
    /// What is wrong.
    pub message: String,
    /// How to fix it, when there is a mechanical answer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// `error` or `warning`; absent means `error`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
}

impl ParamError {
    /// A row with a hint.
    pub fn new(
        path: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        ParamError {
            path: path.into(),
            message: message.into(),
            hint: Some(hint.into()),
            severity: None,
        }
    }

    /// A row with no hint, for a failure with no mechanical fix.
    pub fn bare(path: impl Into<String>, message: impl Into<String>) -> Self {
        ParamError {
            path: path.into(),
            message: message.into(),
            hint: None,
            severity: None,
        }
    }
}

impl ServerError {
    /// The JSON-RPC error code of §6.4.
    pub fn code(&self) -> i32 {
        match self {
            ServerError::Parse(_) => -32700,
            ServerError::InvalidRequest(_) => -32600,
            ServerError::MethodNotFound(_) => -32601,
            ServerError::InvalidParams(_) => -32602,
            ServerError::Internal(_) => -32603,
            ServerError::RunNotFound(_) => -32000,
            ServerError::RunAlreadyRunning => -32001,
            ServerError::RunNotRunning(_) => -32002,
            ServerError::SeekOutOfRange { .. } => -32003,
            ServerError::ScenarioInvalid(_) => -32004,
            ServerError::WorldNotFound(_) => -32005,
            ServerError::UnknownId { .. } => -32006,
            ServerError::UnknownMetric { .. } => -32007,
            ServerError::ExportFailed { .. } => -32008,
            ServerError::NotSupportedHere(_) => -32009,
            ServerError::Busy { .. } => -32010,
            ServerError::ExperimentNotFound(_) => -32011,
            ServerError::Io { .. } => -32013,
            ServerError::VisibilityDenied { .. } => -32040,
            ServerError::Unauthorized => -32041,
            ServerError::UnsupportedVersion => -32050,
        }
    }

    /// The `data` member of the JSON-RPC error object, when §6.4 gives the code one.
    pub fn data(&self) -> Option<Value> {
        match self {
            ServerError::InvalidParams(rows) | ServerError::ScenarioInvalid(rows) => {
                if matches!(self, ServerError::ScenarioInvalid(_)) {
                    Some(json!({"errors": rows}))
                } else {
                    Some(json!(rows))
                }
            }
            ServerError::SeekOutOfRange { min_ns, max_ns } => {
                Some(json!({"min_ns": min_ns, "max_ns": max_ns}))
            }
            ServerError::UnknownId { kind, id } => Some(json!({"kind": kind, "id": id})),
            ServerError::UnknownMetric {
                metric,
                did_you_mean,
            } => Some(json!({"metric": metric, "did_you_mean": did_you_mean})),
            ServerError::ExportFailed { stage, detail } => {
                Some(json!({"stage": stage, "detail": detail}))
            }
            ServerError::NotSupportedHere(why) => Some(json!({"why": why})),
            ServerError::Busy { operation, job_id } => {
                Some(json!({"operation": operation, "job_id": job_id}))
            }
            ServerError::Io { path, errno } => Some(json!({"path": path, "errno": errno})),
            ServerError::VisibilityDenied { field, visibility } => {
                Some(json!({"field": field, "visibility": visibility}))
            }
            ServerError::UnsupportedVersion => {
                Some(json!({"server": "1.0", "supported_major": [1]}))
            }
            _ => None,
        }
    }

    /// The JSON-RPC error object: `{code, message, data?}`.
    pub fn to_rpc_object(&self) -> Value {
        let mut obj = json!({"code": self.code(), "message": self.to_string()});
        if let Some(data) = self.data() {
            obj["data"] = data;
        }
        obj
    }

    /// A `-32602` for one missing or malformed parameter.
    pub fn param(path: &str, message: &str, hint: &str) -> Self {
        ServerError::InvalidParams(vec![ParamError::new(path, message, hint)])
    }
}

impl From<v2xw_record::RecordError> for ServerError {
    fn from(e: v2xw_record::RecordError) -> Self {
        ServerError::Internal(format!("recording: {e}"))
    }
}

impl From<v2xw_world::WorldError> for ServerError {
    fn from(e: v2xw_world::WorldError) -> Self {
        ServerError::Internal(format!("world: {e}"))
    }
}

/// The crate result alias.
pub type Result<T> = core::result::Result<T, ServerError>;
