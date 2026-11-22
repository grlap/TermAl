// Owns bounded SSH stderr classification and exit observations for the owned
// child. Does not expose raw diagnostics, grant authority or replace cleanup.

use super::*;
pub(crate) const FEDERATION_SSH_DIAGNOSTIC_BYTES: usize = 4096;

#[derive(Default)]
pub(crate) struct FederationSshDiagnostic {
    pub(crate) class: Option<&'static str>,
    pub(crate) truncated: bool,
    pub(crate) stderr_complete: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) exited: bool,
    pub(crate) cleanup_requested: bool,
}

pub(crate) type FederationSshDiagnostics = Arc<Mutex<FederationSshDiagnostic>>;

impl FederationSshDiagnostic {
    pub(crate) fn project(&self) -> Value {
        json!({"class":self.class.unwrap_or("unknown"),
            "stderrComplete":self.stderr_complete, "truncated":self.truncated,
            "exited":self.exited, "exitCode":self.exit_code,
            "cleanupRequested":self.cleanup_requested})
    }

    pub(crate) fn exit(&mut self, status: &std::process::ExitStatus) {
        self.exited = true;
        self.exit_code = status.code();
    }
}

fn federation_ssh_failure_class(bytes: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.contains("host key verification failed")
        || text.contains("remote host identification has changed")
    {
        "hostKey"
    } else if text.contains("permission denied") || text.contains("authentication failed") {
        "authentication"
    } else if text.contains("could not resolve hostname")
        || text.contains("name or service not known")
    {
        "nameResolution"
    } else if text.contains("forwarding failed")
        || text.contains("cannot listen to port")
        || text.contains("stdio forwarding failed")
        || text.contains("administratively prohibited")
    {
        "forward"
    } else if text.contains("connection refused")
        || text.contains("connection timed out")
        || text.contains("no route to host")
        || text.contains("network is unreachable")
        || text.contains("connection reset")
        || text.contains("connection closed")
    {
        "connect"
    } else {
        "unknown"
    }
}

pub(crate) fn federation_read_ssh_diagnostics(
    mut stderr: impl std::io::Read,
    diagnostic: &FederationSshDiagnostics,
) {
    let mut retained = Vec::with_capacity(FEDERATION_SSH_DIAGNOSTIC_BYTES);
    let mut buffer = [0; 1024];
    loop {
        match stderr.read(&mut buffer) {
            Ok(0) => {
                diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .stderr_complete = true;
                break;
            }
            Ok(count) => {
                let keep = count.min(FEDERATION_SSH_DIAGNOSTIC_BYTES - retained.len());
                retained.extend_from_slice(&buffer[..keep]);
                let mut observed = diagnostic
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                observed.truncated |= keep < count;
                if keep > 0 {
                    observed.class = Some(federation_ssh_failure_class(&retained));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    // Only fixed classes escape this reader. Continue draining after the
    // bounded prefix so diagnostic output cannot block the SSH process.
}
