/* Process-local proofs that an accepted mailbox continuation is fulfilled.
Receipt durability is owned by MailboxStore. Coverage never authorizes a
provider handoff or changes retained authorization, Stop, or boot recovery. */

#[derive(Clone, Debug, PartialEq, Eq)]
struct MailboxCoverageTurn {
    generation: u64,
    runtime: RuntimeToken,
}

impl MailboxCoverageTurn {
    fn capture(record: &SessionRecord) -> Option<Self> {
        if !(record.session.status == SessionStatus::Active
            && record.active_turn_generation != 0
            && !record.runtime_stop_in_progress
            && !record.orchestrator_auto_dispatch_blocked
            && !record.dedicated_cleanup_holds_admission())
        {
            return None;
        }
        Some(Self {
            generation: record.active_turn_generation,
            runtime: record.runtime.runtime_token()?,
        })
    }

    fn matches(&self, record: &SessionRecord) -> bool {
        Self::capture(record).as_ref() == Some(self)
    }
}

#[derive(Clone)]
struct MailboxCoverageRead {
    receipt: String,
    after: u64,
    through: u64,
    turn: MailboxCoverageTurn,
}

#[derive(Clone)]
struct MailboxCoverageProof {
    read: MailboxCoverageRead,
    explicit: bool,
}

#[derive(Clone)]
struct MailboxCoverageAcceptance {
    message: String,
    sequence: u64,
    generation: u64,
}

#[derive(Clone, Default)]
struct MailboxContinuationCoverage {
    accepted: BTreeMap<String, MailboxCoverageAcceptance>,
    reads: BTreeMap<String, MailboxCoverageRead>,
    proofs: BTreeMap<String, MailboxCoverageProof>,
    removed: u64,
}

fn mailbox_coverage_diagnostic(
    session_id: &str,
    mailbox_id: &str,
    boundary: u64,
    explicit: bool,
) -> String {
    format!(
        "mailbox coverage> session={} mailbox={} boundary={} proof={}",
        mailbox_preview(session_id),
        mailbox_preview(mailbox_id),
        boundary,
        if explicit { "explicit" } else { "later-turn" }
    )
}

fn mailbox_continuation_can_be_covered(
    record: &SessionRecord,
    queued: &QueuedPromptRecord,
) -> bool {
    queued.source == QueuedPromptSource::Mailbox
        && !queued.is_engram_retained()
        && !queued_prompt_kept_standalone(record, queued)
        && queued.promoted_message_index.is_none()
        && queued.promotion_disposition_known
        && queued.attachments.is_empty()
        && queued.pending_prompt.attachments.is_empty()
        && queued.pending_prompt.expanded_text.is_none()
        && queued
            .pending_prompt
            .source
            .as_ref()
            .is_some_and(MessageSource::is_mailbox)
}

/// Called only at live enqueue/coalescing, never while loading persisted wakes.
fn note_mailbox_continuation_boundary(record: &mut SessionRecord, prompt_id: &str) {
    let coverage = &mut record.mailbox_continuations;
    coverage.accepted.retain(|id, _| {
        record
            .queued_prompts
            .iter()
            .any(|q| q.pending_prompt.id == *id)
    });
    coverage.reads.retain(|id, _| {
        record.queued_prompts.iter().any(|q| {
            q.pending_prompt
                .source
                .as_ref()
                .and_then(|s| s.mailbox.as_ref())
                .is_some_and(|m| m.mailbox_id == *id)
        })
    });
    coverage.proofs.retain(|id, _| {
        record.queued_prompts.iter().any(|q| {
            q.pending_prompt
                .source
                .as_ref()
                .and_then(|s| s.mailbox.as_ref())
                .is_some_and(|m| m.mailbox_id == *id)
        })
    });
    let Some(mailbox) = record
        .queued_prompts
        .iter()
        .find(|q| q.pending_prompt.id == prompt_id && q.source == QueuedPromptSource::Mailbox)
        .and_then(|q| q.pending_prompt.source.as_ref()?.mailbox.as_ref())
    else {
        return;
    };
    if coverage
        .accepted
        .get(prompt_id)
        .is_some_and(|old| old.message == mailbox.message_id && old.sequence == mailbox.sequence)
    {
        return;
    }
    coverage.reads.remove(&mailbox.mailbox_id);
    coverage.proofs.remove(&mailbox.mailbox_id);
    coverage.accepted.insert(
        prompt_id.to_owned(),
        MailboxCoverageAcceptance {
            message: mailbox.message_id.clone(),
            sequence: mailbox.sequence,
            generation: record.active_turn_generation,
        },
    );
}

impl AppState {
    fn mailbox_coverage_turn(&self, session_id: &str) -> Option<MailboxCoverageTurn> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        MailboxCoverageTurn::capture(&inner.sessions[inner.find_session_index(session_id)?])
    }

    fn observe_mailbox_coverage_read(
        &self,
        session_id: &str,
        mailbox_id: &str,
        turn: Option<MailboxCoverageTurn>,
        page: &MailboxReadResponse,
    ) {
        let Some(turn) = turn else {
            return;
        };
        let (Some(receipt), Some(through)) = (&page.receipt, page.next_after_sequence) else {
            return;
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        if !turn.matches(record)
            || !record.queued_prompts.iter().any(|q| {
                q.source == QueuedPromptSource::Mailbox
                    && q.pending_prompt
                        .source
                        .as_ref()
                        .and_then(|s| s.mailbox.as_ref())
                        .is_some_and(|m| m.mailbox_id == mailbox_id)
            })
        {
            return;
        }
        // Bound evidence by the queued mailbox set, including after queue cancellation.
        record.mailbox_continuations.reads.retain(|id, _| {
            record.queued_prompts.iter().any(|q| {
                q.pending_prompt
                    .source
                    .as_ref()
                    .and_then(|s| s.mailbox.as_ref())
                    .is_some_and(|m| m.mailbox_id == *id)
            })
        });
        record.mailbox_continuations.reads.insert(
            mailbox_id.to_owned(),
            MailboxCoverageRead {
                receipt: receipt.clone(),
                after: page.after_sequence,
                through,
                turn,
            },
        );
    }

    fn observe_mailbox_coverage_ack(
        &self,
        session_id: &str,
        mailbox_id: &str,
        turn: Option<MailboxCoverageTurn>,
        receipt: &str,
        through: u64,
        explicit: bool,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        let Some(turn) = turn.filter(|t| t.matches(record)) else {
            return;
        };
        let Some(read) = record
            .mailbox_continuations
            .reads
            .get(mailbox_id)
            .filter(|read| read.receipt == receipt && read.through == through && read.turn == turn)
            .cloned()
        else {
            return;
        };
        let explicit = explicit
            || record
                .mailbox_continuations
                .proofs
                .get(mailbox_id)
                .is_some_and(|old| {
                    old.explicit && old.read.receipt == read.receipt && old.read.turn == read.turn
                });
        record.mailbox_continuations.proofs.insert(
            mailbox_id.to_owned(),
            MailboxCoverageProof { read, explicit },
        );
    }

    /// Remove covered entries before checkpoint selection, and at drain.
    /// Storage I/O is off-lock; exact head, turn, proof and holds are rechecked.
    fn settle_covered_mailbox_continuations(
        &self,
        session_id: &str,
        expected: Option<(&RuntimeToken, Option<u64>)>,
    ) -> Result<()> {
        self.settle_covered_mailbox_continuations_before_apply(session_id, expected, || {})
    }

    // The callback is an off-lock boundary for deterministic race tests. The
    // production caller supplies no work; all state checks remain below it.
    fn settle_covered_mailbox_continuations_before_apply(
        &self,
        session_id: &str,
        expected: Option<(&RuntimeToken, Option<u64>)>,
        mut before_apply: impl FnMut(),
    ) -> Result<()> {
        loop {
            let snapshot = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                let Some(index) = inner.find_session_index(session_id) else {
                    return Ok(());
                };
                let record = &inner.sessions[index];
                let Some(turn) = MailboxCoverageTurn::capture(record) else {
                    return Ok(());
                };
                if expected.is_some_and(|(token, generation)| {
                    token != &turn.runtime || generation.is_some_and(|g| g != turn.generation)
                }) {
                    return Ok(());
                }
                let Some((head, mailbox, proof)) = record.queued_prompts.iter().find_map(|head| {
                    if !mailbox_continuation_can_be_covered(record, head) {
                        return None;
                    }
                    let mailbox = head.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
                    let proof = record
                        .mailbox_continuations
                        .proofs
                        .get(&mailbox.mailbox_id)
                        .filter(|p| {
                            p.read.turn == turn
                                && p.read.after < mailbox.sequence
                                && mailbox.sequence <= p.read.through
                        })?;
                    let later = record
                        .mailbox_continuations
                        .accepted
                        .get(&head.pending_prompt.id)
                        .is_some_and(|a| {
                            a.message == mailbox.message_id
                                && a.sequence == mailbox.sequence
                                && a.generation < turn.generation
                        });
                    (proof.explicit || later)
                        .then(|| (head.clone(), mailbox.clone(), proof.clone()))
                }) else {
                    return Ok(());
                };
                (head, mailbox, turn, proof)
            };
            let (head, mailbox, turn, proof) = snapshot;
            let epoch = self.mailbox_store.continuation_epoch.load(Ordering::SeqCst);
            if self
                .mailbox_store
                .unread_wakeup_for_mailbox(session_id, &mailbox.mailbox_id)?
                .is_some()
            {
                return Ok(());
            }
            let Some(boundary) = self.mailbox_store.mailbox_wake_boundary(
                session_id,
                &mailbox.mailbox_id,
                &mailbox.message_id,
            )?
            else {
                return Ok(());
            };
            if boundary.delivered {
                return Ok(());
            }
            before_apply();
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session_id) else {
                return Ok(());
            };
            let record = &mut inner.sessions[index];
            if self.mailbox_store.continuation_epoch.load(Ordering::SeqCst) != epoch {
                return Ok(());
            }
            if !turn.matches(record) {
                return Ok(());
            }
            let Some(position) = record
                .queued_prompts
                .iter()
                .position(|q| q.pending_prompt.id == head.pending_prompt.id)
            else {
                return Ok(());
            };
            if record.queued_prompts.get(position).is_none_or(|q| {
                q.pending_prompt.id != head.pending_prompt.id
                    || q.pending_prompt.text != head.pending_prompt.text
                    || q.pending_prompt.source != head.pending_prompt.source
                    || !mailbox_continuation_can_be_covered(record, q)
            }) {
                return Ok(());
            }
            if !record
                .mailbox_continuations
                .proofs
                .get(&mailbox.mailbox_id)
                .is_some_and(|current| {
                    current.read.receipt == proof.read.receipt
                        && current.read.turn == turn
                        && current.read.through == proof.read.through
                        && current.explicit == proof.explicit
                })
            {
                return Ok(());
            }
            let record = inner
                .session_mut_by_index(index)
                .expect("validated session");
            let old = record.mailbox_continuations.clone();
            let old_peer_messages = record.queued_peer_messages.clone();
            let old_deliveries = record.prompt_deliveries.clone();
            let removed = record
                .queued_prompts
                .remove(position)
                .expect("validated entry");
            record
                .mailbox_continuations
                .accepted
                .remove(&removed.pending_prompt.id);
            record
                .mailbox_continuations
                .proofs
                .remove(&mailbox.mailbox_id);
            sync_pending_prompts(record);
            if let Err(error) = self.commit_locked(&mut inner) {
                let record = &mut inner.sessions[index];
                record.queued_prompts.insert(position, removed);
                record.mailbox_continuations = old;
                record.queued_peer_messages = old_peer_messages;
                record.prompt_deliveries = old_deliveries;
                sync_pending_prompts(record);
                return Err(error);
            }
            inner.sessions[index].mailbox_continuations.removed = old.removed.saturating_add(1);
            eprintln!(
                "{}",
                mailbox_coverage_diagnostic(
                    session_id,
                    &mailbox.mailbox_id,
                    mailbox.sequence,
                    proof.explicit
                )
            );
        }
    }
}
