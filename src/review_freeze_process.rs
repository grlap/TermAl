// New host-owned review verification transport. Owns bounded child observation;
// does not execute repository scripts, interpret shell text, or change policy.

const REVIEW_FREEZE_TIMEOUT: Duration = Duration::from_secs(20);
const REVIEW_FREEZE_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewFreezeObservation {
    status: Option<i32>,
    signal: Option<i32>,
    error: Option<String>,
    stdout_length: usize,
    stderr_length: usize,
    stdout_base64: String,
    stderr_base64: String,
    stdout_exact: bool,
}

fn review_freeze_observation(
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
