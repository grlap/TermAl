// The admission-time refresh of a bound session's Engram work binding, and
// the breaker that keeps TermAl from resending a binding Engram refused as
// stale (Engram w-108a13d58018 criterion 3, tm-winf step 3). TermAl reads the
// binding only inside a bind, while an agent claims work mid-session, after
// its session was bound without work; Engram counts a session bound without
// work as always current, so nothing else would ever rebind it. Owns the
// refresh read at a new admission, the decision to rebind on a change, and
// the refused-binding filter that the refresh and the queued bind
// preparation both apply. Does not own the reader
// (`read_engram_work_binding_from_cli` in `engram_held_claims.rs`), the bind,
// the evaluate loop or the begin path in `engram_host_adapter.rs`, or the
// queued bind preparation in `engram_queued_admission.rs`. New fragment
// beside `engram_host_adapter.rs`, created instead of growing it.

/// How long the admission refresh may spend reading the binding. A healthy
/// read takes about 50 ms; a stuck one must not eat the admission budget, so
/// past this the turn goes ahead on the binding it has and the next
/// admission reads again.
const ENGRAM_WORK_BINDING_REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a binding Engram refused as stale stays unsent. Engram validates
/// a binding against more than the read checks (the item's ancestors, the
/// run's root execution, a pending handoff), so the read can keep offering a
/// binding every bind and evaluate refuses; resending it would refuse every
/// turn. The read leaves it out meanwhile (`EngramBindingPreference`), so the
/// session binds another claim it holds, or no work, which Engram always
/// admits, and tries the binding again after this long, since a pending
/// handoff, say, may since have been declined. A claim that moved on (a new
/// revision or fence) is another binding, which is not held back.
const ENGRAM_REFUSED_WORK_BINDING_RETRY_AFTER: Duration = Duration::from_secs(300);

/// At most this many refused bindings are remembered per session, each for
/// its own retry window; past it the oldest is forgotten. Every one is left
/// out of the reads, so claims Engram refuses cannot take turns being sent.
const ENGRAM_REFUSED_WORK_BINDING_LIMIT: usize = 16;

/// Whether a refusal at `refused_at` is still within its retry window.
fn engram_refusal_in_window(refused_at: std::time::Instant, now: std::time::Instant) -> bool {
    now.saturating_duration_since(refused_at) < ENGRAM_REFUSED_WORK_BINDING_RETRY_AFTER
}

/// The bindings Engram refused as stale whose retry windows still last.
fn engram_refused_work_bindings_in_window(
    refused: &[(EngramControlWorkBinding, std::time::Instant)],
    now: std::time::Instant,
) -> Vec<EngramControlWorkBinding> {
    refused
        .iter()
        .filter(|(_, refused_at)| engram_refusal_in_window(*refused_at, now))
        .map(|(binding, _)| binding.clone())
        .collect()
}

/// The binding to use once the refused-binding breaker has seen a read, and
/// whether the breaker withheld it. Refusals whose windows ended are
/// forgotten. The read leaves the refused bindings out already; a read that
/// returns one anyway within its window (one taken before the refusal was
/// recorded) is withheld and the session binds without work. Other reads
/// leave the refusals standing, since the read leaving them out is why they
/// differ.
fn engram_binding_after_refusal(
    read: Option<EngramControlWorkBinding>,
    refused: &mut Vec<(EngramControlWorkBinding, std::time::Instant)>,
    now: std::time::Instant,
) -> (Option<EngramControlWorkBinding>, bool) {
    refused.retain(|(_, refused_at)| engram_refusal_in_window(*refused_at, now));
    if read
        .as_ref()
        .is_some_and(|read| refused.iter().any(|(binding, _)| binding == read))
    {
        return (None, true);
    }
    (read, false)
}

/// A binding as the log shows it: its item, revision and claim fence.
fn describe_engram_work_binding(binding: Option<&EngramControlWorkBinding>) -> String {
    binding.map_or_else(
        || "none".to_owned(),
        |binding| {
            format!(
                "work {} revision {} fence {}",
                binding.work_id, binding.work_revision, binding.claim_fence
            )
        },
    )
}

impl AppState {
    /// At a new admission of a session that is already bound, reads its work
    /// binding again and, when that differs from the binding the session is
    /// bound with, arms a rebind that binds exactly what was read, so a claim
    /// taken, released or moved on since the last bind reaches Engram before
    /// the turn is evaluated. The claiming turn itself cannot carry evidence:
    /// Engram refuses a rebind while a turn is begun and takes evidence only
    /// under the binding its grant was issued with, so the next turn is the
    /// first that can.
    ///
    /// It reads only when a turn has begun since the binding was last read:
    /// only the session's own agent takes or releases its claim, during a
    /// turn, and a claim that moved on for another reason makes Engram refuse
    /// the binding as stale, which the evaluate loop's heal rebinds. A
    /// retained evaluate or bind never reaches this: it is replayed exactly,
    /// under the binding it was prepared with. A failed or timed-out read
    /// leaves the current binding in place and is logged; it is not "no
    /// claim", and the next admission reads again. Only a lost queue owner or
    /// a vanished session is an error.
    fn refresh_engram_work_binding_off_lock(
        &self,
        target: &mut EngramBindingTarget,
        started_at: std::time::Instant,
        owner: Option<&EngramQueuedAdmissionOwner>,
    ) -> std::result::Result<(), EngramTransportError> {
        let session_id = target.connection.session_id.clone();
        // A backed-off bind could not act on a change before the turn, and
        // would withhold the turn trying to.
        if target
            .next_bind_retry_at
            .is_some_and(|retry_at| retry_at > std::time::Instant::now())
        {
            return Ok(());
        }
        // No turn since the last read, so no claim the agent could have taken
        // or released: the binding read then is still the one to use.
        let (current, refused) = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let Some(engram) = inner
                .find_session_index(&session_id)
                .map(|index| &inner.sessions[index].engram)
                .filter(|engram| engram.turn_begun_since_binding_read)
            else {
                return Ok(());
            };
            (
                engram.work_binding.clone(),
                engram_refused_work_bindings_in_window(
                    &engram.refused_work_bindings,
                    std::time::Instant::now(),
                ),
            )
        };
        let Some(timeout) = target
            .remaining_dispatch_timeout(started_at)
            .map(|remaining| remaining.min(ENGRAM_WORK_BINDING_REFRESH_TIMEOUT))
        else {
            // The evaluate that follows reports the exhausted budget.
            return Ok(());
        };
        if let Some(owner) = owner {
            self.require_queued_engram_owner(&session_id, owner, "Engram work-binding refresh")?;
        }
        let read = target.adapter.read_work_binding(
            &target.connection,
            EngramBindingPreference {
                current: current.as_ref(),
                refused: &refused,
            },
            timeout,
        );
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&session_id) else {
            return Err(EngramTransportError::local_state(
                "Session disappeared during the Engram work-binding refresh",
            ));
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        if owner.is_some_and(|owner| !owner.matches(record)) {
            return Err(EngramTransportError::local_state(
                "Engram work-binding refresh no longer owns the queued prompt",
            ));
        }
        // Whatever rebound, or asked to, while the read ran decides instead.
        if record.engram.routing_token != target.routing_token
            || record.engram.rebind_required
            || record.engram.bind_in_progress
        {
            return Ok(());
        }
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                eprintln!(
                    "engram> session={session_id} work-binding refresh failed; the turn keeps \
                     its current binding ({}): {error}",
                    describe_engram_work_binding(record.engram.work_binding.as_ref())
                );
                return Ok(());
            }
        };
        let read_description = describe_engram_work_binding(read.as_ref());
        let (binding, withheld) = engram_binding_after_refusal(
            read,
            &mut record.engram.refused_work_bindings,
            std::time::Instant::now(),
        );
        if withheld {
            eprintln!(
                "engram> session={session_id} work-binding refresh read {read_description}, \
                 which Engram refused as stale; it stays unsent"
            );
        }
        if binding == record.engram.work_binding {
            record.engram.turn_begun_since_binding_read = false;
            return Ok(());
        }
        // The flag stays set until the rebind is accepted, so after a rebind
        // that fails a new admission reads again; resuming the withheld turn
        // replays the retained rebind exactly instead.
        record.engram.work_binding_refresh_rebinds =
            record.engram.work_binding_refresh_rebinds.saturating_add(1);
        eprintln!(
            "engram> session={session_id} work binding changed from {} to {}; rebinding \
             (refresh rebind {} in this process)",
            describe_engram_work_binding(record.engram.work_binding.as_ref()),
            describe_engram_work_binding(binding.as_ref()),
            record.engram.work_binding_refresh_rebinds
        );
        target.rebind_required = true;
        target.refreshed_work_binding = Some(binding);
        Ok(())
    }

    /// What a read for a bind of `session_id` prefers and leaves out
    /// (`EngramBindingPreference`): the binding it is bound with now and the
    /// ones Engram refused as stale within their retry windows.
    fn engram_work_binding_preference(
        &self,
        session_id: &str,
    ) -> (
        Option<EngramControlWorkBinding>,
        Vec<EngramControlWorkBinding>,
    ) {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .map(|index| {
                let engram = &inner.sessions[index].engram;
                (
                    engram.work_binding.clone(),
                    engram_refused_work_bindings_in_window(
                        &engram.refused_work_bindings,
                        std::time::Instant::now(),
                    ),
                )
            })
            .unwrap_or_default()
    }

    /// The binding the session is bound with now.
    fn engram_bound_work_binding(&self, session_id: &str) -> Option<EngramControlWorkBinding> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner
            .find_session_index(session_id)
            .and_then(|index| inner.sessions[index].engram.work_binding.clone())
    }

    /// Applies the refused-binding breaker to a binding read for a bind,
    /// logging when it withholds one.
    fn engram_work_binding_for_bind(
        &self,
        session_id: &str,
        read: Option<EngramControlWorkBinding>,
    ) -> Option<EngramControlWorkBinding> {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return read;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        let read_description = describe_engram_work_binding(read.as_ref());
        let (binding, withheld) = engram_binding_after_refusal(
            read,
            &mut record.engram.refused_work_bindings,
            std::time::Instant::now(),
        );
        if withheld {
            eprintln!(
                "engram> session={session_id} the bind read {read_description}, which Engram \
                 refused as stale; binding without work instead"
            );
        }
        binding
    }

    /// Records that Engram refused `binding` as stale, so reads leave it out
    /// for its retry window beside any other refused binding still within
    /// its own (a repeated refusal restarts its window).
    fn note_engram_refused_work_binding(
        &self,
        session_id: &str,
        binding: Option<&EngramControlWorkBinding>,
    ) {
        let Some(binding) = binding else {
            return;
        };
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(session_id) else {
            return;
        };
        let record = inner
            .session_mut_by_index(index)
            .expect("session index should be valid");
        eprintln!(
            "engram> session={session_id} Engram refused {} as stale",
            describe_engram_work_binding(Some(binding))
        );
        let refused = &mut record.engram.refused_work_bindings;
        refused.retain(|(refused, _)| refused != binding);
        if refused.len() >= ENGRAM_REFUSED_WORK_BINDING_LIMIT {
            refused.remove(0);
        }
        refused.push((binding.clone(), std::time::Instant::now()));
    }

    /// Records that Engram refused as stale the binding `sent_under`, the one
    /// the refused evaluate went out under or the refused grant was issued
    /// under, unless the session has been rebound since: then the request
    /// raced the rebind, and neither binding is to blame.
    fn note_engram_request_binding_refused(
        &self,
        session_id: &str,
        sent_under: Option<&EngramControlWorkBinding>,
    ) {
        if self.engram_bound_work_binding(session_id).as_ref() == sent_under {
            self.note_engram_refused_work_binding(session_id, sent_under);
        } else {
            eprintln!(
                "engram> session={session_id} Engram refused as stale a request sent under a \
                 binding the session has since been rebound from; blaming neither"
            );
        }
    }
}
