// Owns the regression for a named-root clear that completes while a real Begin
// request is in flight: the Begin's older Bound receipt, released after the
// clear, must not republish the root, restore its naming or reselect it, and
// an unchanged root must stay Bound and current under the same authority
// owner the guard before Begin prepared. Drives the ClaimedRoot fixture with a
// transport that holds the first Begin after its reply is made. Does not own
// the guard's budget share (engram_root_begin_reserve.rs) or unsent Begins
// (engram_root_begin_unsent.rs). New file.
use super::*;

#[derive(Default)]
struct HeldBeginState {
    claimed: bool,
    held: bool,
    released: bool,
}

type HeldBeginShared = Arc<(Mutex<HeldBeginState>, std::sync::Condvar)>;

fn held_begin_state(shared: &HeldBeginShared) -> std::sync::MutexGuard<'_, HeldBeginState> {
    shared
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Holds the first Begin in flight, off the state lock, after the scripted
/// transport made its reply, so the reply carries the named root as it was
/// when Begin was sent. Every other request goes straight on.
struct HeldBeginTransport {
    control: Arc<ScriptedEngramControlTransport>,
    shared: HeldBeginShared,
}

impl EngramControlTransport for HeldBeginTransport {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> Result<Value, EngramTransportError> {
        // The first Begin claims the hold in one critical section, so a
        // second Begin can never also be held.
        let first_begin = matches!(request, EngramControlRequest::TurnBegin { .. })
            && !std::mem::replace(&mut held_begin_state(&self.shared).claimed, true);
        let reply = self.control.request(connection, request, timeout);
        if !first_begin {
            return reply;
        }
        held_begin_state(&self.shared).held = true;
        self.shared.1.notify_all();
        let deadline = std::time::Instant::now() + DEADLOCK_GUARD;
        let mut state = held_begin_state(&self.shared);
        while !state.released {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return Err(EngramTransportError::transport(
                    "the held Begin was never released",
                ));
            };
            state = self
                .shared
                .1
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        reply
    }

    fn read_observation_policy(
        &self,
        connection: &EngramConnectionConfig,
        store: &EngramAuthorityStoreKey,
        timeout: Duration,
        host_workdir: &FsPath,
    ) -> Result<EngramObservationPolicyBasis, EngramTransportError> {
        self.control
            .read_observation_policy(connection, store, timeout, host_workdir)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.control.shutdown_session(session_id);
    }

    fn read_work_binding(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.control
            .read_work_binding(connection, preference, timeout)
    }

    fn read_work_binding_for_boot(
        &self,
        connection: &EngramConnectionConfig,
        preference: EngramBindingPreference<'_>,
        timeout: Duration,
    ) -> Result<Option<EngramControlWorkBinding>, EngramTransportError> {
        self.control
            .read_work_binding_for_boot(connection, preference, timeout)
    }

    fn read_held_claims(
        &self,
        connection: &EngramConnectionConfig,
        timeout: Duration,
    ) -> Result<EngramHeldClaims, EngramTransportError> {
        self.control.read_held_claims(connection, timeout)
    }
}

/// The test's handle on the held Begin. Dropped, also on unwind, it lets the
/// held request through.
struct HeldBegin(HeldBeginShared);

impl HeldBegin {
    fn install(claimed: &ClaimedRoot) -> Self {
        let shared = HeldBeginShared::default();
        install_control_only_transport(
            &claimed.state,
            Arc::new(HeldBeginTransport {
                control: claimed.transport.clone(),
                shared: shared.clone(),
            }),
        );
        Self(shared)
    }

    fn wait_held(&self) {
        let deadline = std::time::Instant::now() + DEADLOCK_GUARD;
        let mut state = held_begin_state(&self.0);
        while !state.held {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("no Begin request was held in time");
            state = self
                .0
                .1
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    fn release(&self) {
        held_begin_state(&self.0).released = true;
        self.0.1.notify_all();
    }
}

impl Drop for HeldBegin {
    fn drop(&mut self) {
        self.release();
    }
}

/// A claimed root whose worktree is named, Bound and current, with its first
/// Begin held in flight on a worker thread. Returns the fixture, the named
/// generation, the hold and the worker delivering the turn.
fn named_root_with_held_begin(
    label: &str,
) -> (
    ClaimedRoot,
    u64,
    HeldBegin,
    std::thread::JoinHandle<TurnDispatchDeliveryOutcome>,
) {
    let grant_id = format!("turn-observation-{label}-grant");
    let claimed = ClaimedRoot::new_scripted(
        label,
        vec![
            bind_reply(&format!("turn-observation-{label}-token")),
            grant_reply(&grant_id),
            begin_reply(&grant_id),
            checkpoint_reply(&grant_id),
        ],
    );
    prepare_confirmed_claimed_opening(&claimed, label);
    let worktree = add_claimed_root_worktree(&claimed.root);
    let named = name_claimed_root_source_root(&claimed, label, &worktree);
    let held = HeldBegin::install(&claimed);
    let worker = claimed.state.clone();
    let session = claimed.session_id.clone();
    let task = std::thread::spawn(move || {
        let dispatch = match worker
            .dispatch_turn(
                &session,
                SendMessageRequest {
                    text: "Change the root workspace.".to_owned(),
                    expanded_text: None,
                    attachments: Vec::new(),
                    source_session_id: None,
                    source_mailbox: None,
                },
            )
            .unwrap()
        {
            DispatchTurnResult::Dispatched(dispatch)
            | DispatchTurnResult::DispatchedAfterQueue(dispatch) => dispatch,
            DispatchTurnResult::Queued => panic!("idle fixture must dispatch"),
        };
        deliver_turn_dispatch(&worker, dispatch)
    });
    held.wait_held();
    (claimed, named.generation, held, task)
}

/// The work's current authority owner, as the naming history records it.
fn naming_owner(claimed: &ClaimedRoot, label: &str) -> Option<(String, u64)> {
    let store = claimed_root_store(claimed);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    let inner = claimed.state.inner.lock().unwrap();
    inner
        .engram_work_naming_history
        .iter()
        .find(|history| history.store == store && history.work_id == binding.work_id)
        .and_then(|history| history.transition.as_ref())
        .map(|owner| (owner.id.clone(), owner.version))
}

/// The work's selected source root for the fixture's claim, if any.
fn selected_root(claimed: &ClaimedRoot, label: &str) -> Option<EngramWorkSourceRoot> {
    let store = claimed_root_store(claimed);
    let binding = test_control_work_binding(&format!("turn-observation-{label}"), 1);
    let inner = claimed.state.inner.lock().unwrap();
    engram_work_source_root_for_claim(
        &inner.engram_work_source_roots,
        &store,
        &binding.work_id,
        &binding.claim_id,
    )
    .cloned()
}

#[test]
fn a_clear_during_a_held_begin_is_not_undone_by_its_older_bound_receipt() {
    let label = "begin-inflight-clear";
    let (claimed, generation, held, task) = named_root_with_held_begin(label);
    // A real, authorized clear completes while Begin is in flight.
    let cleared = name_root(&claimed, label, None, vec![claimed_root_held(label)])
        .expect("the clear completes while Begin is held");
    assert!(cleared.root.is_none(), "the clear names no root");
    assert!(selected_root(&claimed, label).is_none());
    // Begin's reply, made before the clear, still projects the old Bound root.
    let sent_before_release = claimed.transport.requests().len();
    held.release();
    let outcome = task.join().expect("the worker delivers the turn");
    // The released reply is reconciled: the turn is delivered and its stale
    // projection is checked against a canonical read made after release.
    assert!(
        matches!(outcome, TurnDispatchDeliveryOutcome::Delivered),
        "the turn proceeds after the clear: {outcome:?}"
    );
    assert!(
        claimed.transport.requests()[sent_before_release..]
            .iter()
            .any(|request| request.request["operation"] == "named_root_read"),
        "the released Begin's reply is reconciled by a canonical read"
    );

    assert!(
        selected_root(&claimed, label).is_none(),
        "the older Bound receipt must not reselect the cleared root"
    );
    let named_root = claimed.record(|record| record.engram.named_root.clone());
    assert!(
        matches!(named_root, Some(EngramNamedRootState::None)),
        "the older Bound receipt must not republish the root: {named_root:?}"
    );
    // The capture stays this host's own confirmed clear of the named
    // generation; it is not restored to the named root.
    let capture = claimed.state.engram_root_capture(&claimed.session_id);
    assert!(
        matches!(
            &capture,
            EngramRootCapture::Recorded {
                generation: ended,
                state: EngramSourceRootState::Ended,
                ..
            } if u64::try_from(*ended).ok() == Some(generation)
        ),
        "the cleared work must keep its confirmed clear: {capture:?}"
    );
}

#[test]
fn a_held_begin_with_no_clear_keeps_its_root_bound_under_the_same_owner() {
    let label = "begin-inflight-unchanged";
    let (claimed, generation, held, task) = named_root_with_held_begin(label);
    // The guard before Begin has prepared the owner this Begin runs under.
    let owner_before = naming_owner(&claimed, label).expect("an owner is prepared before Begin");
    held.release();
    let outcome = task.join().expect("the worker delivers the turn");
    assert!(
        matches!(outcome, TurnDispatchDeliveryOutcome::Delivered),
        "an unchanged root delivers the turn: {outcome:?}"
    );

    let selected = selected_root(&claimed, label).expect("the named root stays selected");
    assert_eq!(selected.generation, generation);
    let named_root = claimed.record(|record| record.engram.named_root.clone());
    assert!(
        matches!(
            &named_root,
            Some(EngramNamedRootState::Bound { generation: bound, .. })
                if u64::try_from(*bound).ok() == Some(generation)
        ),
        "the root stays Bound at its generation: {named_root:?}"
    );
    assert_eq!(
        naming_owner(&claimed, label),
        Some(owner_before),
        "Begin's reconciliation reuses the owner its guard prepared"
    );
}
