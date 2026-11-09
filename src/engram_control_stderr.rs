// Private bounded control-child stderr capture. Only fixed classes and scalar
// metadata escape; it owns no protocol, authority, retry or cleanup decision.
const ENGRAM_CONTROL_STDERR_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
struct EngramControlStderrObservation {
    class: &'static str,
    observed_bytes: u64,
    retained_bytes: usize,
    complete: bool,
    read_failed: bool,
}

impl Default for EngramControlStderrObservation {
    fn default() -> Self {
        Self {
            class: "unknown",
            observed_bytes: 0,
            retained_bytes: 0,
            complete: false,
            read_failed: false,
        }
    }
}

type EngramControlStderr = Arc<Mutex<EngramControlStderrObservation>>;

impl EngramControlStderrObservation {
    fn summary(&self) -> String {
        format!(
            "stderr class={}; observedBytes={}; retainedBytes={}; truncated={}; complete={}; readFailed={}",
            self.class,
            self.observed_bytes,
            self.retained_bytes,
            self.observed_bytes > self.retained_bytes as u64,
            self.complete,
            self.read_failed
        )
    }
}

fn engram_control_stderr_class(bytes: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.contains("panicked at") || text.contains("fatal runtime error") {
        "panic"
    } else if text.contains("database is locked") || text.contains("sqlite_busy") {
        "storeBusy"
    } else if text.contains("database disk image is malformed") || text.contains("sqlite_corrupt") {
        "storeCorrupt"
    } else if text.contains("permission denied") || text.contains("access is denied") {
        "permission"
    } else if text.contains("broken pipe") || text.contains("unexpected eof") {
        "io"
    } else if text.contains("unrecognized subcommand") || text.contains("unexpected argument") {
        "usage"
    } else {
        "unknown"
    }
}

fn read_engram_control_stderr(mut pipe: impl std::io::Read, observation: &EngramControlStderr) {
    // Raw text is private to this reader, never Debug/serialized or inherited.
    // Keep draining after the bound so stderr cannot block the control child.
    let mut tail = Vec::with_capacity(ENGRAM_CONTROL_STDERR_BYTES);
    let mut chunk = [0; 1024];
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) => {
                observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .complete = true;
                return;
            }
            Ok(count) => {
                let discard = (tail.len() + count).saturating_sub(ENGRAM_CONTROL_STDERR_BYTES);
                tail.drain(..discard);
                tail.extend_from_slice(&chunk[..count]);
                let class = engram_control_stderr_class(&tail);
                let mut seen = observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                seen.observed_bytes = seen.observed_bytes.saturating_add(count as u64);
                seen.retained_bytes = tail.len();
                if class != "unknown" {
                    seen.class = class;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                observation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .read_failed = true;
                return;
            }
        }
    }
}

impl EngramTransportError {
    fn with_control_stderr(mut self, observation: &EngramControlStderr) -> Self {
        // Snapshot before cleanup, without waiting for stderr EOF: stdout can
        // close while the same child (and its stderr pipe) is still alive.
        let seen = observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        self.message.push_str("; ");
        self.message.push_str(&seen.summary());
        self
    }
}

#[cfg(test)]
mod engram_control_stderr_tests {
    use super::*;

    #[test]
    fn control_stderr_drains_past_the_bound_without_exposing_private_text() {
        let mut bytes = vec![b'x'; ENGRAM_CONTROL_STDERR_BYTES * 4];
        bytes.extend_from_slice(
            b"thread 'private' panicked at PRIVATE_PATH token=PRIVATE_TOKEN PRIVATE_REQUEST",
        );
        let count = bytes.len();
        let mut pipe = std::io::Cursor::new(bytes);
        let seen = Arc::new(Mutex::new(EngramControlStderrObservation::default()));
        read_engram_control_stderr(&mut pipe, &seen);
        assert_eq!(
            pipe.position(),
            count as u64,
            "keep draining beyond retained storage"
        );
        let observation = seen.lock().unwrap().clone();
        assert_eq!(observation.observed_bytes, count as u64);
        assert_eq!(observation.retained_bytes, ENGRAM_CONTROL_STDERR_BYTES);
        assert_eq!(
            observation.class, "panic",
            "recognize a diagnostic arriving after the bound"
        );
        assert!(observation.complete);
        assert!(!observation.read_failed);
        let error = EngramTransportError::transport("Engram control reached EOF before replying")
            .with_control_stderr(&seen);
        assert!(error.message.contains("truncated=true"));
        for text in [error.message.clone(), format!("{error:?}")] {
            for secret in ["PRIVATE_PATH", "PRIVATE_TOKEN", "PRIVATE_REQUEST"] {
                assert!(!text.contains(secret));
            }
        }
    }

    #[test]
    fn control_stderr_distinguishes_empty_completion_from_a_read_failure() {
        let seen = Arc::new(Mutex::new(EngramControlStderrObservation::default()));
        read_engram_control_stderr(std::io::empty(), &seen);
        let empty = seen.lock().unwrap().clone();
        assert!(empty.complete);
        assert_eq!(empty.observed_bytes, 0);
        assert_eq!(empty.class, "unknown");
        struct FailedPipe;
        impl std::io::Read for FailedPipe {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("PRIVATE_READ_ERROR"))
            }
        }
        let failed = Arc::new(Mutex::new(EngramControlStderrObservation::default()));
        read_engram_control_stderr(FailedPipe, &failed);
        let observation = failed.lock().unwrap();
        assert!(!observation.complete);
        assert!(observation.read_failed);
        assert_eq!(observation.observed_bytes, 0);
        assert!(!observation.summary().contains("PRIVATE_READ_ERROR"));
    }
}
