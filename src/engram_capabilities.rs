// Capabilities read from the ordinary work-session child. A read belongs to
// one exact connection and live child generation; replacing either invalidates
// it. Capability discovery never sends a write or grants turn authority.

/// The capability name a connected Engram lists when it accepts the
/// `verification_record` operation.
const ENGRAM_VERIFICATION_RECORD_CAPABILITY: &str = "verification_record_v1";
const ENGRAM_REMOTE_VERIFICATION_RECORD_CAPABILITY: &str = "remote_verification_record_v1";

fn engram_reply_supports_host_work(result: &Value) -> bool {
    result.get("capabilities").and_then(Value::as_array).is_some_and(|entries| {
        entries.iter().all(Value::is_string)
            && entries.iter().any(|entry| entry.as_str() == Some("host_work_v1"))
    })
}

/// Whether one connection accepts `verification_record`, and when not, why.
/// Only `Supported` lets a record be sent.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordSupport {
    /// The connection's work-session answer lists the capability.
    Supported,
    /// No current work-session answer is available.
    NotRead,
    /// The last answer has no `capabilities` field (an older Engram).
    FieldMissing,
    /// The last answer lists capabilities, but not this one.
    EntryMissing,
    /// The last answer's `capabilities` is not a list of names.
    Unparseable,
}

impl EngramVerificationRecordSupport {
    fn is_supported(self) -> bool {
        self == Self::Supported
    }
}

/// What one work-session `result` says about `verification_record`. An
/// explicit null reads as a missing field; any entry that is not a string
/// makes the whole list unreadable, so a malformed list never supports it.
fn engram_reply_verification_record_support(result: &Value) -> EngramVerificationRecordSupport {
    engram_reply_record_support(result, ENGRAM_VERIFICATION_RECORD_CAPABILITY)
}

fn engram_reply_record_support(result: &Value, capability: &str) -> EngramVerificationRecordSupport {
    let capabilities = match result.get("capabilities") {
        None | Some(Value::Null) => return EngramVerificationRecordSupport::FieldMissing,
        Some(capabilities) => capabilities,
    };
    let Some(entries) = capabilities.as_array() else {
        return EngramVerificationRecordSupport::Unparseable;
    };
    let mut listed = false;
    for entry in entries {
        match entry.as_str() {
            Some(name) => listed |= name == capability,
            None => return EngramVerificationRecordSupport::Unparseable,
        }
    }
    if listed {
        EngramVerificationRecordSupport::Supported
    } else {
        EngramVerificationRecordSupport::EntryMissing
    }
}

/// One exact connection's answer under the child that produced it.
#[derive(Clone, Debug)]
struct EngramConnectionCapabilityRead {
    connection: EngramConnectionConfig,
    generation: Option<u64>,
    work_session: bool,
    evidence_git_tree_applicability: bool,
    support: EngramVerificationRecordSupport,
    remote_support: EngramVerificationRecordSupport,
}

/// The per-connection answers, by the connection's session. An entry counts
/// only for the exact connection it was read on, under the same live child,
/// and for the same connection: another binary, home or project behind the
/// same session or a respawned child reads as not read.
#[derive(Default)]
struct EngramConnectionCapabilities {
    by_session: Mutex<HashMap<String, EngramConnectionCapabilityRead>>,
}

impl EngramConnectionCapabilities {
    /// Only a work-session read can replace this capability record.
    fn note_reply(
        &self,
        connection: &EngramConnectionConfig,
        generation: Option<u64>,
        request: &EngramControlRequest,
        reply: &std::result::Result<Value, EngramTransportError>,
    ) {
        if !matches!(request, EngramControlRequest::WorkSessionRead { .. }) {
            return;
        }
        let work_session = matches!(request, EngramControlRequest::WorkSessionRead { .. })
            && reply.as_ref().is_ok_and(|result|
                result.get("work_session").and_then(Value::as_bool) == Some(true)
                    && engram_reply_supports_host_work(result)
                    && result.get("session_id").and_then(Value::as_str)
                        == Some(connection.session_id.as_str()));
        let support = match reply {
            Ok(result) if work_session => engram_reply_verification_record_support(result),
            _ => EngramVerificationRecordSupport::NotRead,
        };
        let remote_support = match reply {
            Ok(result) if work_session =>
                engram_reply_record_support(result, ENGRAM_REMOTE_VERIFICATION_RECORD_CAPABILITY),
            _ => EngramVerificationRecordSupport::NotRead,
        };
        let evidence_git_tree_applicability = reply.as_ref().is_ok_and(|result| {
            work_session
                && result.get("verification_evidence_git_tree_applicability_v1")
                    .and_then(Value::as_bool) == Some(true)
        });
        self.by_session
            .lock()
            .expect("Engram capabilities mutex poisoned")
            .insert(
                connection.session_id.clone(),
                EngramConnectionCapabilityRead {
                    connection: connection.clone(),
                    generation,
                    work_session,
                    evidence_git_tree_applicability,
                    support,
                    remote_support,
                },
            );
    }

    /// Drops what the session's connection said; its next connection is read
    /// afresh.
    fn forget(&self, session_id: &str) {
        self.by_session
            .lock()
            .expect("Engram capabilities mutex poisoned")
            .remove(session_id);
    }

    /// The read that still stands for `connection` under the live child
    /// `generation`: none when no child serves it, or the child changed.
    fn current(&self, connection: &EngramConnectionConfig, generation: Option<u64>) -> Option<EngramConnectionCapabilityRead> {
        generation?;
        self.by_session
            .lock()
            .expect("Engram capabilities mutex poisoned")
            .get(&connection.session_id)
            .filter(|read| read.connection == *connection && read.generation == generation)
            .cloned()
    }

    fn work_session(&self, connection: &EngramConnectionConfig, generation: Option<u64>) -> bool {
        self.current(connection, generation).is_some_and(|read| read.work_session)
    }

    fn work_session_verification_record(
        &self, connection: &EngramConnectionConfig, generation: Option<u64>,
    ) -> EngramVerificationRecordSupport {
        self.current(connection, generation).filter(|read| read.work_session)
            .map_or(EngramVerificationRecordSupport::NotRead, |read| read.support)
    }

    #[allow(dead_code)] // Used when the remote delivery sender is activated.
    fn work_session_remote_verification_record(
        &self, connection: &EngramConnectionConfig, generation: Option<u64>,
    ) -> EngramVerificationRecordSupport {
        self.current(connection, generation).filter(|read| read.work_session)
            .map_or(EngramVerificationRecordSupport::NotRead, |read| read.remote_support)
    }

    // Restricted-record producers must check this separately from ordinary
    // verification_record support. Missing or malformed flags grant nothing.
    #[cfg_attr(not(test), allow(dead_code))]
    fn work_session_git_tree_applicability(
        &self, connection: &EngramConnectionConfig, generation: Option<u64>,
    ) -> bool {
        self.current(connection, generation).is_some_and(|read|
            read.work_session && read.evidence_git_tree_applicability)
    }
}
