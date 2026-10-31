// What the connected Engram says it supports, as its own session_bind and
// session_status answers state it (`result.capabilities`, a list of names).
// Owns the reading of that optional list from one answer, the typed answer to
// "does this connection support the `verification_record` operation", and the
// per-connection record of it, with the routing token and the transport's
// child it was read under, replaced by every bind or status answer the
// connection gives, void once another child serves the connection, and
// dropped when the connection's session is shut down.
// Does not own the bind or status calls themselves, their schemas or their
// recovery (src/engram_host_adapter.rs), or what is sent once the operation
// is supported (src/engram_verification_record.rs). Nothing is inferred from
// a build hash or version, and nothing is probed by writing: an answer that
// does not name the capability, names it in an unreadable list, or has not
// been read yet, is "not supported". New module.

/// The capability name a connected Engram lists when it accepts the
/// `verification_record` operation.
const ENGRAM_VERIFICATION_RECORD_CAPABILITY: &str = "verification_record_v1";

/// Whether one connection accepts `verification_record`, and when not, why.
/// Only `Supported` lets a record be sent.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EngramVerificationRecordSupport {
    /// The connection's last bind or status answer lists the capability.
    Supported,
    /// No bind or status answer of this connection has been read, or the
    /// last such call failed.
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

/// What one bind or status `result` says about `verification_record`. An
/// explicit null reads as a missing field; any entry that is not a string
/// makes the whole list unreadable, so a malformed list never supports it.
fn engram_reply_verification_record_support(result: &Value) -> EngramVerificationRecordSupport {
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
            Some(name) => listed |= name == ENGRAM_VERIFICATION_RECORD_CAPABILITY,
            None => return EngramVerificationRecordSupport::Unparseable,
        }
    }
    if listed {
        EngramVerificationRecordSupport::Supported
    } else {
        EngramVerificationRecordSupport::EntryMissing
    }
}

/// One connection's last bind or status answer: the exact connection, the
/// child that answered (`EngramControlTransport::connection_generation`), the
/// routing token the answer gave (a bind) or was read under (a status), and
/// what it said about `verification_record`.
#[derive(Clone, Debug)]
struct EngramConnectionCapabilityRead {
    connection: EngramConnectionConfig,
    generation: Option<u64>,
    routing_token: Option<String>,
    support: EngramVerificationRecordSupport,
}

/// The per-connection answers, by the connection's session. An entry counts
/// only for the exact connection it was read on, under the same live child,
/// and for the same binding: another binary, home or project behind the same
/// session, a respawned child, or another routing token reads as not read.
/// Only bind and status answers are recorded, so no other reply (a reader's
/// tracker read included) can enable anything.
#[derive(Default)]
struct EngramConnectionCapabilities {
    by_session: Mutex<HashMap<String, EngramConnectionCapabilityRead>>,
}

impl EngramConnectionCapabilities {
    /// Records what `reply` to `request` says, when `request` is a bind or a
    /// status read, under `generation`, the child serving the connection
    /// once it answered; every other call leaves the record as it is. A
    /// failed bind or status call forgets the earlier answer: the connection
    /// may have been replaced, and only a fresh answer says what it supports.
    fn note_reply(
        &self,
        connection: &EngramConnectionConfig,
        generation: Option<u64>,
        request: &EngramControlRequest,
        reply: &std::result::Result<Value, EngramTransportError>,
    ) {
        let routing_token = match (request, reply) {
            (EngramControlRequest::SessionBind { .. }, Ok(result)) => result
                .get("routing_token")
                .and_then(Value::as_str)
                .map(str::to_owned),
            (EngramControlRequest::SessionStatus { routing_token }, Ok(_)) => Some(routing_token.clone()),
            (EngramControlRequest::SessionBind { .. } | EngramControlRequest::SessionStatus { .. }, Err(_)) => None,
            _ => return,
        };
        let support = match (reply, &routing_token) {
            (Ok(result), Some(_)) => engram_reply_verification_record_support(result),
            _ => EngramVerificationRecordSupport::NotRead,
        };
        self.by_session
            .lock()
            .expect("Engram capabilities mutex poisoned")
            .insert(
                connection.session_id.clone(),
                EngramConnectionCapabilityRead {
                    connection: connection.clone(),
                    generation,
                    routing_token,
                    support,
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

    /// Whether `connection`, bound under `routing_token`, supports
    /// `verification_record`, by its own last bind or status answer under
    /// the live child `generation`.
    fn verification_record(
        &self,
        connection: &EngramConnectionConfig,
        generation: Option<u64>,
        routing_token: &str,
    ) -> EngramVerificationRecordSupport {
        self.current(connection, generation)
            .filter(|read| read.routing_token.as_deref() == Some(routing_token))
            .map_or(EngramVerificationRecordSupport::NotRead, |read| read.support)
    }

    /// The routing token of `connection`'s last successful bind or status
    /// answer under the live child `generation`.
    fn routing_token(&self, connection: &EngramConnectionConfig, generation: Option<u64>) -> Option<String> {
        self.current(connection, generation)?.routing_token
    }
}
