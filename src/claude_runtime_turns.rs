// Claude turns no prompt of TermAl's owns: what the host does with a turn
// Claude Code starts by itself or one it cannot tie to a waiting prompt, and
// how a Claude `result` reaches the turn it ended.
//
// Owns: opening such a turn on its session. A turn Claude Code started by
// itself on an idle session is adopted: Active under a new turn generation,
// with prompts queuing behind it. Every such turn gets an immediate
// transcript notice that TermAl did not mediate it. What the recorder sees in
// it is kept locally as unassigned, away from every Engram grant and never
// credited. This file also routes each `result` to the turn its frames
// opened, so a result never finishes, errors, checkpoints, replay-clears or
// queue-drains another turn.
//
// Does not own: deciding which turn a frame belongs to
// (`claude_turn_ownership.rs`), the stdout reader loop (`claude_spawn.rs`),
// the ordinary finish/error paths it calls (`turn_lifecycle.rs`), or Engram
// mediation of such a turn, which waits on Engram's producer contract for
// observed turns: no retroactive turn evaluation or begin is made here.
//
// New file; nothing was split out of another.

/// At most this many unassigned observations are kept per session, newest
/// last.
const UNASSIGNED_CLAUDE_OBSERVATION_LIMIT: usize = 64;

/// A Claude turn no prompt of TermAl's owns, open on its session.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UnmediatedClaudeTurn {
    /// The session turn generation it was adopted under; `None` when it was
    /// not adopted (the session was busy with another turn, or the turn is
    /// unassigned).
    adopted_generation: Option<u64>,
    /// The transcript notice that announced it, which its unassigned
    /// observations name.
    notice_message_id: String,
}

/// Something the recorder saw in a turn no prompt owns, kept on the session
/// as unassigned: never reported to Engram and never credited.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UnassignedClaudeObservation {
    notice_message_id: String,
    observed_at: String,
    what: String,
}

/// Whether what the recorder sees in this session must stay away from every
/// Engram grant: a turn no prompt owns is open on it, adopted as the current
/// turn or running beside another turn that it may not report into.
fn unmediated_claude_turn_hides_observations(record: &SessionRecord) -> bool {
    unmediated_claude_turn_is_current(record)
        || record.unmediated_claude_turn.as_ref().is_some_and(|turn| {
            match turn.adopted_generation {
                Some(generation) => generation == record.active_turn_generation,
                None => true,
            }
        })
}

/// Whether the session's current turn generation is one the host adopted for
/// a turn Claude Code started by itself: it was granted nothing, so no
/// terminal path of it reports execution on a grant. Read from the one
/// generation-scoped provenance value, which outlives the turn's open
/// segment: it holds through deferred Stop callbacks, failed or successful
/// Stops, runtime exits and replays of the same generation, and goes inert
/// only when a successor generation begins.
fn unmediated_claude_turn_is_current(record: &SessionRecord) -> bool {
    record.adopted_claude_turn_generation == Some(record.active_turn_generation)
}

fn unmediated_claude_turn_notice(cause: ClaudeUnownedCause, adopted: bool) -> String {
    let unrecorded = "TermAl did not mediate it: Engram has no record of it, and its edits \
                      and test runs are kept unassigned, not reported or credited.";
    match cause {
        ClaudeUnownedCause::Unassigned => format!(
            "TermAl could not tie this Claude turn to the prompt it is waiting on. {unrecorded} \
             Its end finalizes no turn; if the session stays busy after it, stop the session."
        ),
        ClaudeUnownedCause::TaskNotice | ClaudeUnownedCause::NoPrompt => {
            let start = if cause == ClaudeUnownedCause::TaskNotice {
                "Claude Code started this turn by itself, after a background-task notice."
            } else {
                "Claude Code started this turn by itself, with no prompt from TermAl."
            };
            let busy = if adopted {
                " Prompts sent meanwhile wait until it ends."
            } else {
                " It runs beside the turn already in progress and is not part of it."
            };
            format!("{start} {unrecorded}{busy}")
        }
    }
}

/// Keeps `what` on `record` as unassigned, under the notice of the unowned
/// turn open there (or the one that just ended), bounded and newest last.
fn push_unassigned_claude_observation(record: &mut SessionRecord, what: String) {
    let notice_message_id = record
        .unmediated_claude_turn
        .as_ref()
        .map(|turn| turn.notice_message_id.clone())
        .unwrap_or_default();
    let kept = &mut record.unassigned_claude_observations;
    if kept.len() >= UNASSIGNED_CLAUDE_OBSERVATION_LIMIT {
        kept.pop_front();
    }
    kept.push_back(UnassignedClaudeObservation {
        notice_message_id,
        observed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        what,
    });
}

/// A short account of a recorder observation, for the unassigned record.
fn unassigned_claude_observation_text(observation: &EngramRecorderObservation<'_>) -> String {
    match observation {
        EngramRecorderObservation::CommandStarted { key, ran, cwd } => format!(
            "command {key} started: {} in {}",
            ran.unwrap_or("(not reported)"),
            cwd.unwrap_or("(no directory reported)")
        ),
        EngramRecorderObservation::CommandDescribed { key, ran, cwd } => format!(
            "command {key} described: {} in {}",
            ran.unwrap_or("(not reported)"),
            cwd.unwrap_or("(no directory reported)")
        ),
        EngramRecorderObservation::CommandFinished {
            key, command, exit, ..
        } => format!("command {key} finished: {command} ({exit:?})"),
        EngramRecorderObservation::CommandAbandoned { key } => {
            format!("command {key} abandoned")
        }
        EngramRecorderObservation::WorkspaceEdit { .. } => "a file edit".to_owned(),
    }
}

impl AppState {
    /// Records what one Claude stdout frame does to the runtime's turns and
    /// applies it to the session, as the reader does with a frame router's
    /// plan.
    #[cfg(test)]
    fn observe_claude_frame_ownership(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        ownership: &ClaudeTurnOwnership,
        message: &Value,
    ) -> ClaudeFrameOwnership {
        let observed = lock_claude_turn_ownership(ownership).observe(message);
        self.apply_claude_frame_ownership(session_id, token, ownership, &observed);
        observed
    }

    /// Applies to the session what one frame did to the runtime's turns
    /// (`ClaudeFramePlan::ownership`): opens there a turn no prompt owns, and
    /// marks a grant whose turn such a turn overlapped.
    fn apply_claude_frame_ownership(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        ownership: &ClaudeTurnOwnership,
        observed: &ClaudeFrameOwnership,
    ) {
        match observed {
            ClaudeFrameOwnership::OpenedRuntime { cause } => {
                let adopted = self.open_unmediated_claude_turn_logged(session_id, token, *cause);
                lock_claude_turn_ownership(ownership).note_runtime_adoption(adopted);
                // Beside the session's own turn: its grant's measurement
                // interval overlaps this turn from now on, whether the grant
                // closes at the unowned result or earlier (a stop, a runtime
                // exit, a reset).
                if adopted.is_none() {
                    self.mark_engram_turn_attribution_mixed(session_id, token);
                }
            }
            // The unowned turn took up a waiting prompt: what ran before is
            // no one's, and what follows is not separable from it, so the
            // prompt's grant is marked mixed and the turn stays unassigned
            // for what the recorder sees until its result.
            ClaudeFrameOwnership::JoinedHost(_) => {
                self.mark_engram_turn_attribution_mixed(session_id, token);
            }
            // Contradictory identities: from now on nothing the recorder sees
            // is anyone's, and the transcript says so at once.
            ClaudeFrameOwnership::BecameUnresolved => {
                if self
                    .open_unmediated_claude_turn_of(session_id, token)
                    .is_none()
                {
                    self.open_unmediated_claude_turn_logged(
                        session_id,
                        token,
                        ClaudeUnownedCause::Unassigned,
                    );
                }
                self.mark_engram_turn_attribution_mixed(session_id, token);
            }
            ClaudeFrameOwnership::Unchanged | ClaudeFrameOwnership::OpenedHost(_) => {}
        }
    }

    fn open_unmediated_claude_turn_logged(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        cause: ClaudeUnownedCause,
    ) -> Option<u64> {
        match self.open_unmediated_claude_turn(session_id, token, cause) {
            Ok(adopted) => adopted,
            Err(err) => {
                eprintln!(
                    "runtime state warning> failed to open the unowned Claude turn of \
                     session `{session_id}`: {err:#}"
                );
                None
            }
        }
    }

    /// Opens a turn no prompt of TermAl's owns. A turn Claude Code started by
    /// itself on an idle session (or one idle after an error) is adopted:
    /// Active under a new turn generation, so prompts queue behind it and its
    /// result finishes it. On a busy session, or when the turn is unassigned,
    /// it is marked but not adopted. Either way the transcript says at once
    /// that TermAl did not mediate it. Returns the adopted generation.
    fn open_unmediated_claude_turn(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        cause: ClaudeUnownedCause,
    ) -> Result<Option<u64>> {
        // Where the session writes, for the overlap marks the turn's start
        // makes under the lock, as a dispatched turn's start does.
        self.engram_host().turn_starting(session_id);
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let index = inner
            .find_session_index(session_id)
            .ok_or_else(|| anyhow!("session `{session_id}` not found"))?;
        if !inner.sessions[index].runtime.matches_runtime_token(token) {
            return Ok(None);
        }
        let message_id = inner.next_message_id();
        let (
            adopted_generation,
            message,
            message_index,
            message_count,
            preview,
            status,
            stamp,
            session_seq,
        ) = {
            let record = inner
                .session_mut_by_index(index)
                .expect("session index should be valid");
            let adopt = cause != ClaudeUnownedCause::Unassigned
                && record.runtime_projection_allowed()
                && !record.runtime_stop_in_progress
                && matches!(
                    record.session.status,
                    SessionStatus::Idle | SessionStatus::Error
                );
            let adopted_generation = adopt.then(|| {
                record.active_turn_generation =
                    record.active_turn_generation.wrapping_add(1).max(1);
                record.active_turn_start_message_count = Some(record.session.messages.len());
                record.active_turn_mailbox_notification = None;
                record.active_turn_file_changes.clear();
                record.active_turn_file_change_grace_deadline = None;
                record.session.status = SessionStatus::Active;
                record.session.live_activity = Some(SessionLiveActivity {
                    prompt: "Started by Claude Code".to_owned(),
                    command: None,
                    command_status: None,
                });
                record.session.preview = "Turn started by Claude Code.".to_owned();
                record.active_turn_generation
            });
            if adopted_generation.is_some() {
                record.adopted_claude_turn_generation = adopted_generation;
            }
            record.unmediated_claude_turn = Some(UnmediatedClaudeTurn {
                adopted_generation,
                notice_message_id: message_id.clone(),
            });
            let message = Message::Text {
                attachments: Vec::new(),
                id: message_id,
                timestamp: stamp_now(),
                author: Author::System,
                text: unmediated_claude_turn_notice(cause, adopted_generation.is_some()),
                expanded_text: None,
                source: None,
            };
            let message_index = push_message_on_record(record, message.clone());
            let session_seq = record.next_body_delta_seq(message.id());
            (
                adopted_generation,
                message,
                global_message_index(record, message_index),
                session_message_count(record),
                record.session.preview.clone(),
                record.session.status,
                record.mutation_stamp,
                session_seq,
            )
        };
        // The session may now write under a check another session of its
        // worktree has open. An adopted turn starts as a dispatched one does,
        // and its earlier turn's commands are over; one running beside the
        // session's own turn only adds its presence.
        if adopted_generation.is_some() {
            EngramHost::turn_started(&mut inner, index);
        } else {
            EngramHost::turn_running_beside(&mut inner, index);
        }
        let revision = self.commit_persisted_delta_locked(&mut inner)?;
        self.publish_delta_locked(
            &inner,
            DeltaEvent::MessageCreated {
                revision,
                session_id: session_id.to_owned(),
                message_id: message.id().to_owned(),
                message_index,
                message_count,
                message,
                preview,
                status,
                session_queue: None,
                session_mutation_stamp: Some(stamp),
                session_seq: Some(session_seq),
                body_seq_epoch: Some(self.server_instance_id.clone()),
            },
        );
        Ok(adopted_generation)
    }

    /// Clears the open unowned turn `turn`, if it is still the one open on
    /// the runtime `token` names, and says how many unassigned observations
    /// it left. A replaced runtime's late frames clear nothing.
    fn clear_unmediated_claude_turn(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        turn: &UnmediatedClaudeTurn,
    ) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        if !inner.sessions[index].runtime.matches_runtime_token(token)
            || inner.sessions[index].unmediated_claude_turn.as_ref() != Some(turn)
        {
            return;
        }
        let Some(record) = inner.session_mut_by_index(index) else {
            return;
        };
        record.unmediated_claude_turn = None;
        let kept = record
            .unassigned_claude_observations
            .iter()
            .filter(|observation| observation.notice_message_id == turn.notice_message_id)
            .count();
        if kept > 0 {
            eprintln!(
                "runtime state> Claude session `{session_id}`: the unowned turn announced by \
                 message `{}` ended with {kept} unassigned observation(s), kept and not credited",
                turn.notice_message_id
            );
        }
    }

    /// A turn no prompt owned ran beside the session's open grant: its
    /// interval overlapped the grant's measurement interval, between the
    /// grant's begin basis and its close. Nothing proves where one ended and
    /// the grant's own turn began (the runtime may start the waiting prompt
    /// before this reader processes the unowned result), so the grant's
    /// measured begin, and the watcher's hints, are kept as they are. The
    /// grant is marked as having mixed attribution instead, and its own
    /// source report is withheld as uncertain (the turn report in
    /// `engram_turn_checks.rs`); checks keep their own proof. The overlap is
    /// kept as unassigned.
    /// Only for the runtime `token` names: a replaced runtime's late frames
    /// mark no grant of its successor.
    fn mark_engram_turn_attribution_mixed(&self, session_id: &str, token: &RuntimeToken) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = &mut inner.sessions[index];
        if !record.runtime.matches_runtime_token(token) {
            return;
        }
        let Some(grant_id) = record.engram.active_grant_id.clone() else {
            return;
        };
        if record.engram.active_turn_mixed_attribution.as_deref() == Some(grant_id.as_str()) {
            return;
        }
        record.engram.active_turn_mixed_attribution = Some(grant_id.clone());
        // Outstanding Claude work is part of what the session says about the
        // restriction (`claude_outstanding_work.rs`).
        if record.claude_outstanding.any() {
            record.claude_outstanding.notice_due = true;
        }
        push_unassigned_claude_observation(
            record,
            format!(
                "a turn no prompt owned overlapped grant {grant_id}'s measurement interval; \
                 what changed in it may be that turn's or the grant's, so the grant's own \
                 source report is withheld as uncertain"
            ),
        );
    }

    /// Finalizes the turn a Claude `result` ended, and only that turn.
    /// `error_detail` is set for an error result.
    fn finish_claude_result(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        owner: ClaudeResultOwner,
        error_detail: Option<&str>,
    ) -> Result<()> {
        match owner {
            // A late duplicate ends nothing (the router never passes one).
            ClaudeResultOwner::Stale => Ok(()),
            ClaudeResultOwner::Host(owner) => match error_detail {
                Some(detail) => self.mark_turn_error_if_runtime_and_generation_match(
                    session_id,
                    token,
                    owner.turn_generation,
                    detail,
                ),
                None => self.finish_turn_ok_if_runtime_and_generation_match(
                    session_id,
                    token,
                    owner.turn_generation,
                ),
            },
            ClaudeResultOwner::Runtime(owner) => {
                let turn = self.open_unmediated_claude_turn_of(session_id, token);
                // The adopted turn is finished first, while it is still
                // marked, so the finish path's Engram checkpoint sees it and
                // skips; the mark is cleared after, and only if still ours.
                let result = match (owner.adopted_generation, error_detail) {
                    (Some(generation), Some(detail)) => self
                        .mark_turn_error_if_runtime_and_generation_match(
                            session_id, token, generation, detail,
                        ),
                    (Some(generation), None) => self
                        .finish_turn_ok_if_runtime_and_generation_match(
                            session_id, token, generation,
                        ),
                    // Not adopted: only this segment ends; the session's own
                    // turn, and any prompt still waiting, stay as they are,
                    // and an open grant it overlapped is marked uncertain.
                    (None, _) => {
                        self.mark_engram_turn_attribution_mixed(session_id, token);
                        Ok(())
                    }
                };
                if let Some(turn) = turn {
                    self.clear_unmediated_claude_turn(session_id, token, &turn);
                }
                result
            }
            ClaudeResultOwner::HostAfterUnowned(owner) => {
                // The result names this prompt alone, so it settles it; the
                // grant was marked mixed when the unowned turn took the
                // prompt up, and the mark of that turn is cleared after.
                let turn = self.open_unmediated_claude_turn_of(session_id, token);
                self.mark_engram_turn_attribution_mixed(session_id, token);
                let result = match error_detail {
                    Some(detail) => self.mark_turn_error_if_runtime_and_generation_match(
                        session_id,
                        token,
                        owner.turn_generation,
                        detail,
                    ),
                    None => self.finish_turn_ok_if_runtime_and_generation_match(
                        session_id,
                        token,
                        owner.turn_generation,
                    ),
                };
                if let Some(turn) = turn {
                    self.clear_unmediated_claude_turn(session_id, token, &turn);
                }
                result
            }
            ClaudeResultOwner::Uncorrelated { prompt_waiting } => {
                eprintln!(
                    "runtime state warning> Claude session `{session_id}` reported a result \
                     with no turn open; it finalizes nothing"
                );
                if prompt_waiting {
                    self.push_claude_system_notice(
                        session_id,
                        token,
                        "Claude reported the end of a turn TermAl could not tie to the prompt \
                         it is waiting on, so nothing was finalized. If the session stays busy, \
                         stop the session.",
                    )?;
                }
                Ok(())
            }
            ClaudeResultOwner::Unresolved {
                ended_unowned,
                prompt_waiting,
                adopted_interval,
            } => {
                // The result ends an interval the host adopted for a turn
                // Claude Code started by itself, with no prompt part of it
                // or waiting: that observed interval is terminated through
                // the guarded terminal paths, on this runtime and its own
                // adopted generation only. Its identities are still nobody's:
                // no prompt is settled, no grant is checkpointed or credited
                // (the generation was granted nothing), and the transcript
                // says so.
                if let Some(generation) = adopted_interval
                    .as_ref()
                    .and_then(|owner| owner.adopted_generation)
                    .filter(|generation| {
                        self.claude_adopted_generation_is_current(session_id, token, *generation)
                    })
                {
                    let turn = self.open_unmediated_claude_turn_of(session_id, token);
                    let result = match error_detail {
                        Some(detail) => self.mark_turn_error_if_runtime_and_generation_match(
                            session_id, token, generation, detail,
                        ),
                        None => self.finish_turn_ok_if_runtime_and_generation_match(
                            session_id, token, generation,
                        ),
                    };
                    if let Some(turn) = turn {
                        self.clear_unmediated_claude_turn(session_id, token, &turn);
                    }
                    self.push_claude_system_notice(
                        session_id,
                        token,
                        "Claude ended a turn it had started by itself, with message identities \
                         TermAl could not resolve. That turn was closed; no prompt and no grant \
                         was credited with it.",
                    )?;
                    return result;
                }
                // Only the observed segment ends; every attempt it may have
                // involved stays as it is, with no synthetic success.
                eprintln!(
                    "runtime state warning> Claude session `{session_id}` reported a result \
                     whose message identities TermAl could not resolve; it finalizes nothing \
                     (began unowned: {ended_unowned}, prompt waiting: {prompt_waiting})"
                );
                if let Some(turn) = self.open_unmediated_claude_turn_of(session_id, token) {
                    self.mark_engram_turn_attribution_mixed(session_id, token);
                    self.clear_unmediated_claude_turn(session_id, token, &turn);
                }
                self.push_claude_system_notice(
                    session_id,
                    token,
                    "Claude reported the end of a turn whose message identities TermAl could \
                     not resolve (missing, unknown, contradictory, or naming more than one \
                     prompt), so nothing was finalized. If the session stays busy, stop the \
                     session.",
                )
            }
        }
    }

    /// A transcript notice, only while the runtime `token` names is the
    /// session's: a replaced runtime's late result says nothing.
    fn push_claude_system_notice(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        text: &str,
    ) -> Result<()> {
        if !self.claude_runtime_is_current(session_id, token) {
            return Ok(());
        }
        self.push_message(
            session_id,
            Message::Text {
                attachments: Vec::new(),
                id: self.allocate_message_id(),
                timestamp: stamp_now(),
                author: Author::System,
                text: text.to_owned(),
                expanded_text: None,
                source: None,
            },
        )
    }

    /// The unowned turn open on `session_id` while the runtime `token` names
    /// is its runtime, if any.
    fn open_unmediated_claude_turn_of(
        &self,
        session_id: &str,
        token: &RuntimeToken,
    ) -> Option<UnmediatedClaudeTurn> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .filter(|&index| inner.sessions[index].runtime.matches_runtime_token(token))
            .and_then(|index| inner.sessions[index].unmediated_claude_turn.clone())
    }

    /// Whether the runtime `token` names is still the session's, its current
    /// turn is still `generation`, and that generation is still the one the
    /// host adopted for a turn Claude Code started by itself.
    fn claude_adopted_generation_is_current(
        &self,
        session_id: &str,
        token: &RuntimeToken,
        generation: u64,
    ) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.find_session_index(session_id).is_some_and(|index| {
            let record = &inner.sessions[index];
            record.runtime.matches_runtime_token(token)
                && record.active_turn_generation == generation
                && record.adopted_claude_turn_generation == Some(generation)
        })
    }

    fn claude_runtime_is_current(&self, session_id: &str, token: &RuntimeToken) -> bool {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .is_some_and(|index| inner.sessions[index].runtime.matches_runtime_token(token))
    }
}
