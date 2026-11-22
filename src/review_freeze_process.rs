// New host-owned review verification transport. Owns bounded child observation;
// does not execute repository scripts, interpret shell text, or change policy.

use super::*;

/// The shared budget of every pinned Git call of one freeze (and of the
/// content revisions and source bases that reuse it). Doubled from 20 s on
/// Greg's decision (2026-09-28): under CPU load Git calls take seconds each,
/// in the running host as in the tests.
pub(crate) const REVIEW_FREEZE_TIMEOUT: Duration = Duration::from_secs(40);
/// How much longer than `REVIEW_FREEZE_TIMEOUT` the parent watches the
/// `termal review-freeze-check` child, so the child's own budget, not the
/// parent's wait, decides a slow freeze.
pub(crate) const REVIEW_FREEZE_OBSERVER_GRACE: Duration = Duration::from_secs(5);
pub(crate) const REVIEW_FREEZE_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviewFreezeObservation {
    pub(crate) status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) error: Option<String>,
    stdout_length: usize,
    pub(crate) stderr_length: usize,
    stdout_base64: String,
    pub(crate) stderr_base64: String,
    pub(crate) stdout_exact: bool,
}

pub(crate) fn review_freeze_observation(
    output: std::process::Output,
    expected: &str,
) -> ReviewFreezeObservation {
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        output.status.signal()
    };
    #[cfg(not(unix))]
    let signal = None;
    ReviewFreezeObservation {
        status: output.status.code(),
        signal,
        error: None,
        stdout_exact: output.status.success()
            && output.stdout == format!("{expected}\n").as_bytes(),
        stdout_length: output.stdout.len(),
        stderr_length: output.stderr.len(),
        stdout_base64: base64::engine::general_purpose::STANDARD.encode(output.stdout),
        stderr_base64: base64::engine::general_purpose::STANDARD.encode(output.stderr),
    }
}
