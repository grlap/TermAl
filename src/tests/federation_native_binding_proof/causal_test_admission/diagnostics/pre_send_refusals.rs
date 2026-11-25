// Local refusals before a causal completion read is sent name the local
// check, not an origin read. Drives the real relay client on the fixture link.
use super::*;

/// Pulls the one original once through a relay client on the fixture's own
/// channel, then returns the original row's pending state.
async fn pull_once(fixture: &Fixture, key: &str) -> Value {
    pull_once_observing_blocked_allocation(fixture, key, Arc::new(|_| {})).await
}

/// As `pull_once`, with `blocked` called whenever the client's counter
/// allocation waits for the sender window.
async fn pull_once_observing_blocked_allocation(
    fixture: &Fixture,
    key: &str,
    blocked: Arc<dyn Fn(u64) + Send + Sync>,
) -> Value {
    pull_once_observing(fixture, key, Arc::new(|| {}), blocked, Arc::new(|| {})).await
}

/// As `pull_once`, with `checking` called each time the client checks its
/// route authority, before the read is sent and after its reply, `blocked`
/// called whenever its counter allocation waits for the window, and
/// `dispatching` called once the signed read is ready to send.
async fn pull_once_observing(
    fixture: &Fixture,
    key: &str,
    checking: Arc<dyn Fn() + Send + Sync>,
    blocked: Arc<dyn Fn(u64) + Send + Sync>,
    dispatching: Arc<dyn Fn() + Send + Sync>,
) -> Value {
    let mut plans = fixture
        .central
        .mailbox_store
        .causal_test_completion_plans()
        .unwrap();
    assert_eq!(plans.len(), 1);
    let plan = plans.pop().unwrap();
    let channel = fixture.channel.clone();
    let checked = channel.clone();
    let mut rpc = FederationRpcClient::new(
        fixture.server.port,
        channel.clone(),
        Arc::new(move || {
            checking();
            checked.current_authority().map(|_| ())
        }),
    )
    .unwrap();
    rpc.allocation_blocked = blocked;
    rpc.before_dispatch = dispatching;
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            channel.snapshot.peer.host_id.clone(),
            Arc::new(FederationPeerHandle { channel, rpc }),
        );
    assert!(
        fixture
            .central
            .read_causal_test_completion_plans(vec![(plan, None)])
            .await
            .unwrap()
            .is_empty()
    );
    pending_state(fixture, key).await
}

#[tokio::test]
async fn an_exhausted_sender_counter_is_a_route_change_not_an_origin_read() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "exhausted-sender-counter";
    original(&fixture, key, input).await;
    // Counter allocation refuses locally, after every earlier check passed.
    fixture
        .channel
        .security_context()
        .unwrap()
        .sender
        .lock()
        .unwrap()
        .next = u64::MAX;
    let state = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, pull_once(&fixture, key))
        .await
        .expect("the refused read ends without waiting");
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(state["reason"], "route_changed");
    assert_eq!(state["releaseCondition"], "original_route_current");
}

/// Holds one sender counter open 64 positions back, so the next counter
/// allocation waits for the sender window. Returns the held counter.
fn hold_sender_window(context: &FederationSecurityContext, now: std::time::Instant) -> u64 {
    let mut sender = context.sender.lock().unwrap();
    let held = sender.next;
    sender.outstanding.insert(
        held,
        FederationSenderDeadline {
            started: now,
            retire_at: now + Duration::from_secs(3600),
            operation: "fixtureHeldCounter",
        },
    );
    sender.next = held + 64;
    held
}

/// Whether a relay read's sender counter was retired as unsent after the
/// deadline, and whether the link's incarnation is still current, not fenced.
fn relay_counter_retired_and_link_current(fixture: &Fixture) -> (bool, bool) {
    let retired = fixture
        .channel
        .security_context()
        .unwrap()
        .sender_events
        .lock()
        .unwrap()
        .iter()
        .any(|event| {
            event.kind == "retiredUnknown"
                && event.operation == "mailboxRelay"
                && event.reason == "unsentAfterDeadline"
        });
    let current = fixture
        .channel
        .current_lease()
        .is_ok_and(|lease| lease.with_incarnation(|| Ok(())).is_ok());
    (retired, current)
}

#[tokio::test]
async fn a_sender_window_full_at_the_read_deadline_is_relay_capacity_not_an_origin_timeout() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "sender-window-full";
    original(&fixture, key, input).await;
    let context = fixture.channel.security_context().unwrap();
    let held = hold_sender_window(&context, fixture.channel.control.clock.now());
    // The fixture link measures a scripted clock. Once the allocation waits
    // for the window, move that clock past the read's deadline.
    let clock = fixture.channel.control.clock.clone();
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = blocked.clone();
    let state = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        pull_once_observing_blocked_allocation(
            &fixture,
            key,
            Arc::new(move |_| {
                if !once.swap(true, Ordering::SeqCst) {
                    clock.advance(Duration::from_secs(6));
                }
            }),
        ),
    )
    .await
    .expect("the read's scripted deadline ends the wait");
    context.sender.lock().unwrap().outstanding.remove(&held);
    context.changed.notify_waiters();
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(
        blocked.load(Ordering::SeqCst),
        "the allocation waited for the window"
    );
    assert_eq!(state["reason"], "relay_capacity_unavailable");
    assert_eq!(state["releaseCondition"], "local_relay_capacity_available");
}

#[tokio::test]
async fn a_sender_counter_freed_only_after_the_read_deadline_is_not_used() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "sender-counter-after-deadline";
    original(&fixture, key, input).await;
    let context = fixture.channel.security_context().unwrap();
    let held = hold_sender_window(&context, fixture.channel.control.clock.now());
    // While the allocation waits, move the scripted clock past the read's
    // deadline and free the window in the same step, so a counter becomes
    // available after the deadline and before the deadline is observed.
    let clock = fixture.channel.control.clock.clone();
    let freeing = context.clone();
    let freed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = freed.clone();
    let state = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        pull_once_observing_blocked_allocation(
            &fixture,
            key,
            Arc::new(move |_| {
                if !once.swap(true, Ordering::SeqCst) {
                    clock.advance(Duration::from_secs(6));
                    freeing.sender.lock().unwrap().outstanding.remove(&held);
                    freeing.changed.notify_waiters();
                }
            }),
        ),
    )
    .await
    .expect("the read ends without waiting on the origin");
    let (retired, link_current) = relay_counter_retired_and_link_current(&fixture);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(
        freed.load(Ordering::SeqCst),
        "the allocation waited for the window"
    );
    assert!(retired, "the allocated counter was retired unsent");
    assert!(link_current, "an unsent counter does not fence the link");
    assert_eq!(state["reason"], "relay_capacity_unavailable");
    assert_eq!(state["releaseCondition"], "local_relay_capacity_available");
}

#[tokio::test]
async fn a_read_deadline_spent_in_local_checks_is_not_relay_capacity() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "read-deadline-in-local-checks";
    original(&fixture, key, input).await;
    // The route check before sending takes the whole budget: it moves the
    // link's scripted clock past the read's deadline. The sender window and
    // the RPC bound are both free, so allocation does not wait.
    let clock = fixture.channel.control.clock.clone();
    let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = checked.clone();
    let blocked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waited = blocked.clone();
    let state = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        pull_once_observing(
            &fixture,
            key,
            Arc::new(move || {
                if !once.swap(true, Ordering::SeqCst) {
                    clock.advance(Duration::from_secs(6));
                }
            }),
            Arc::new(move |_| waited.store(true, Ordering::SeqCst)),
            Arc::new(|| {}),
        ),
    )
    .await
    .expect("the read ends without waiting on the origin");
    let (retired, link_current) = relay_counter_retired_and_link_current(&fixture);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(checked.load(Ordering::SeqCst));
    assert!(retired, "the allocated counter was retired unsent");
    assert!(link_current, "an unsent counter does not fence the link");
    assert!(
        !blocked.load(Ordering::SeqCst),
        "the sender window was free"
    );
    assert_eq!(state["reason"], "read_deadline_before_send");
    assert_eq!(
        state["releaseCondition"],
        "local_checks_finish_within_read_deadline"
    );
}

#[tokio::test]
async fn a_read_deadline_spent_while_signing_is_not_an_origin_timeout() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "read-deadline-while-signing";
    original(&fixture, key, input).await;
    // The counter is allocated in time; the local encoding and signing that
    // follow take the rest of the budget, so the read must not be sent.
    let clock = fixture.channel.control.clock.clone();
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = reached.clone();
    let state = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        pull_once_observing(
            &fixture,
            key,
            Arc::new(|| {}),
            Arc::new(|_| {}),
            Arc::new(move || {
                if !once.swap(true, Ordering::SeqCst) {
                    clock.advance(Duration::from_secs(6));
                }
            }),
        ),
    )
    .await
    .expect("the read ends without waiting on the origin");
    let (retired, link_current) = relay_counter_retired_and_link_current(&fixture);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(reached.load(Ordering::SeqCst), "the signed read was ready");
    assert!(retired, "the allocated counter was retired unsent");
    assert!(link_current, "an unsent counter does not fence the link");
    assert_eq!(state["reason"], "read_deadline_before_send");
    assert_eq!(
        state["releaseCondition"],
        "local_checks_finish_within_read_deadline"
    );
}

#[tokio::test]
async fn a_deadline_passing_while_signing_after_a_timely_window_wait_is_not_capacity() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "deadline-after-timely-window-wait";
    original(&fixture, key, input).await;
    let context = fixture.channel.security_context().unwrap();
    let held = hold_sender_window(&context, fixture.channel.control.clock.now());
    // The allocation waits for the window, which frees 4 s into the 5 s
    // budget; the counter is allocated in time. The deadline then passes in
    // the local signing that follows, so the deciding check is local work,
    // not capacity.
    let waiting_clock = fixture.channel.control.clock.clone();
    let freeing = context.clone();
    let freed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once_freed = freed.clone();
    let signing_clock = fixture.channel.control.clock.clone();
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once_reached = reached.clone();
    let state = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        pull_once_observing(
            &fixture,
            key,
            Arc::new(|| {}),
            Arc::new(move |_| {
                if !once_freed.swap(true, Ordering::SeqCst) {
                    waiting_clock.advance(Duration::from_secs(4));
                    freeing.sender.lock().unwrap().outstanding.remove(&held);
                    freeing.changed.notify_waiters();
                }
            }),
            Arc::new(move || {
                if !once_reached.swap(true, Ordering::SeqCst) {
                    signing_clock.advance(Duration::from_secs(2));
                }
            }),
        ),
    )
    .await
    .expect("the read ends without waiting on the origin");
    let (retired, link_current) = relay_counter_retired_and_link_current(&fixture);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert!(
        freed.load(Ordering::SeqCst),
        "the allocation waited for the window"
    );
    assert!(reached.load(Ordering::SeqCst), "the signed read was ready");
    assert!(retired, "the allocated counter was retired unsent");
    assert!(link_current, "an unsent counter does not fence the link");
    assert_eq!(state["reason"], "read_deadline_before_send");
    assert_eq!(
        state["releaseCondition"],
        "local_checks_finish_within_read_deadline"
    );
}

#[tokio::test]
async fn a_full_peer_rpc_bound_is_relay_capacity_not_an_origin_read() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, _, _) = destination(fixture.central.clone(), &fixture.native);
    let key = "full-peer-rpc-bound";
    original(&fixture, key, input).await;
    // Hold every permit of the channel's default bound of four.
    let held = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        fixture.channel.bound.permits.clone().acquire_many_owned(4),
    )
    .await
    .expect("other requests on the link release their permits")
    .unwrap();
    let state = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, pull_once(&fixture, key))
        .await
        .expect("the refused read ends without waiting");
    drop(held);
    drop(handles);
    server.finish().await;
    fixture.finish().await;

    assert_eq!(state["reason"], "relay_capacity_unavailable");
    assert_eq!(state["releaseCondition"], "local_relay_capacity_available");
}
