// Process-local transcript ordering. Metadata mutations never touch this
// counter. A delta claims one value; a commit with unrepresented body changes
// claims one additional value so clients detect a hole and fetch a snapshot.
// Local counters are constructed only at boot or for a new session id. Tail
// eviction and transcript replacement retain them, so the immutable server
// instance id is their epoch. There is no in-process local record reload.

/// An incomplete certificate has legacy semantics, never a guessed epoch.
fn normalize_body_sequence_pair(sequence: &mut Option<u64>, epoch: &mut Option<String>) {
    if sequence.is_some() != epoch.is_some() {
        *sequence = None;
        *epoch = None;
    }
}

impl DeltaEvent {
    fn normalize_body_sequence_pair(&mut self) {
        match self {
            Self::MessageCreated {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::MessageUpdated {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::TextDelta {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::TextReplace {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::CommandUpdate {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::ParallelAgentsUpdate {
                session_seq,
                body_seq_epoch,
                ..
            }
            | Self::TestRunCardUpdated {
                session_seq,
                body_seq_epoch,
                ..
            } => {
                normalize_body_sequence_pair(session_seq, body_seq_epoch);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
struct SessionBodySequence {
    value: u64,
    // Last upstream sequence actually represented by the cached proxy bodies.
    remote_applied: Option<u64>,
    remote_epoch: Option<String>,
    unrepresented_messages: HashSet<String>,
    replacement_pending: bool,
}

impl SessionBodySequence {
    fn advance(&mut self) -> u64 {
        self.value = self
            .value
            .checked_add(1)
            .expect("session body sequence exhausted");
        self.value
    }

    fn delta(&mut self, message_id: &str) -> u64 {
        self.unrepresented_messages.remove(message_id);
        self.advance()
    }

    fn finish_commit(&mut self) {
        if self.replacement_pending || !self.unrepresented_messages.is_empty() {
            self.advance();
            self.unrepresented_messages.clear();
            self.replacement_pending = false;
        }
    }
}

impl SessionRecord {
    fn mark_body_changed(&mut self, message_id: &str) {
        if self.is_local_session() {
            self.body_sequence
                .unrepresented_messages
                .insert(message_id.to_owned());
        }
    }

    /// Called under the same mutex as mutation, before the commit's snapshot.
    /// Callers retain this value in delta parts; publication never reads a
    /// later record value. Batch callers allocate in publication order.
    fn next_body_delta_seq(&mut self, message_id: &str) -> u64 {
        debug_assert!(
            self.is_local_session(),
            "proxies preserve upstream sequences"
        );
        self.body_sequence.delta(message_id)
    }

    fn ensure_remote_body_sequence(
        &self,
        incoming: Option<u64>,
        epoch: Option<&str>,
    ) -> Result<()> {
        if self.body_sequence.remote_epoch.as_deref() != epoch {
            return Err(anyhow!(
                "remote body sequence epoch changed; refresh the transcript"
            ));
        }
        if let Some(incoming) = incoming {
            if self
                .body_sequence
                .remote_applied
                .and_then(|seq| seq.checked_add(1))
                != Some(incoming)
            {
                return Err(anyhow!(
                    "remote body sequence discontinuity; refresh the transcript"
                ));
            }
        }
        Ok(())
    }

    fn apply_remote_body_sequence(&mut self, incoming: Option<u64>, epoch: Option<String>) {
        self.body_sequence.remote_epoch = epoch.clone();
        self.session.body_seq_epoch = epoch;
        self.body_sequence.remote_applied = incoming;
        self.session.body_seq = incoming;
    }

    fn wire_body_seq_epoch(&self, local_instance_id: &str) -> Option<String> {
        self.wire_body_seq()?;
        if self.is_local_session() {
            Some(local_instance_id.to_owned())
        } else {
            self.body_sequence.remote_epoch.clone()
        }
    }

    fn wire_body_seq(&self) -> Option<u64> {
        if self.is_local_session() {
            Some(self.body_sequence.value)
        } else {
            self.body_sequence.remote_applied
        }
    }
}

impl StateInner {
    fn finish_body_sequence_commits(&mut self) {
        for record in &mut self.sessions {
            if record.is_local_session() {
                record.body_sequence.finish_commit();
            }
        }
    }
}
