// Owns committed native-append availability and pre-commit refusal controls.
// Uses isolated stores and authenticated loopback HTTP, never a live pair.
use super::*;
use mailbox_fixture::MailboxCase;

struct NativeWakeOwner {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl NativeWakeOwner {
    fn start(app: AppState) -> Self {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            let mut worker = FederationMailboxWakeWorker::default();
            app.federation_executor_wake_signal.kick();
            loop {
                worker.tick(&app, false).await;
                tokio::select! {
                    _ = federation_wait_stop(&mut stopped) => break,
                    _ = app.federation_executor_wake_signal.notify.notified() => {},
                }
            }
            worker.shutdown().await;
        });
        Self { stop, task: Some(task) }
    }

    async fn finish(mut self) {
        self.stop.send_replace(true);
        self.task.take().unwrap().await.unwrap();
    }
}

impl Drop for NativeWakeOwner {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

struct ReleaseWake(Option<std::sync::mpsc::Sender<()>>);

impl ReleaseWake {
    fn release(&mut self) {
        if let Some(send) = self.0.take() {
            let _ = send.send(());
        }
    }
}

impl Drop for ReleaseWake {
    fn drop(&mut self) {
        self.release();
    }
}

fn canonical_counts(state: &AppState) -> [u64; 3] {
    let connection = state.mailbox_store.connection().unwrap();
    [
        "mailbox_messages",
        "federation_mailbox_dedup",
        "federation_mailbox_wake_obligations",
    ]
    .map(|table| {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
            .unwrap()
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_commit_and_same_key_reply_do_not_wait_for_native_wake() {
    let case = MailboxCase::new().await;
    // Production owns this worker in the federation supervisor. This isolated
    // HTTP fixture runs that same owner without opening SSH or a live runtime.
    let native_worker = NativeWakeOwner::start(case.pair.central.clone());
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let (finished, settled) = tokio::sync::oneshot::channel();
    let mut release = ReleaseWake(Some(release));
    *case.pair.central.mailbox_store.before_native_wake.lock().unwrap() =
        Some(Box::new(move || {
            let _ = entered.send(());
            let result = released.recv_timeout(phase_sync::DEADLOCK_GUARD);
            let _ = finished.send(());
            result.expect("post-COMMIT wake pause must be released by its owner");
        }));
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let first_client = client.clone();
    let first_lease = lease.clone();
    let first_request = case.request.clone();
    let first = tokio::spawn(async move {
        first_client
            .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
                first_lease,
                first_request,
            ))
            .await
    });
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, reached)
        .await
        .expect("append must reach its actual post-COMMIT native wake")
        .unwrap();
    let counts_while_wake_held = canonical_counts(&case.pair.central);
    let duplicate = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease,
            case.request.clone(),
        ))
        .await;
    let first = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, first)
        .await
        .unwrap()
        .unwrap();
    // Release and join the real blocked handler before any RED assertion.
    // Both replies above were observed while wake delivery was still held.
    release.release();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, settled)
        .await
        .unwrap()
        .unwrap();
    let drained = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        case.pair.lease.channel.bound.permits.clone().acquire_many_owned(4),
    )
    .await
    .unwrap()
    .unwrap();
    drop(drained);
    let counts_after_settlement = canonical_counts(&case.pair.central);
    native_worker.finish().await;
    case.pair.finish().await;
    assert_eq!(counts_while_wake_held, [1, 1, 1]);
    assert_eq!(counts_after_settlement, [1, 1, 1]);
    assert!(
        first.is_ok() && duplicate.is_ok(),
        "a COMMITted native append and its exact-key replay must answer before wake finalization: first={first:?}, duplicate={duplicate:?}"
    );
    let first = first.unwrap().value;
    let duplicate = duplicate.unwrap().value;
    assert!(first["messageId"].is_string());
    assert_eq!(first["sequence"], 1);
    assert_eq!(duplicate["messageId"], first["messageId"]);
    assert_eq!(duplicate["sequence"], first["sequence"]);
    assert_eq!(duplicate["duplicate"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_precommit_refusal_keeps_message_dedup_and_wake_absent() {
    let case = MailboxCase::new().await;
    case.pair.central.mailbox_store.connection().unwrap().execute_batch(
        "CREATE TRIGGER reject_available_append BEFORE INSERT ON federation_mailbox_wake_obligations
         BEGIN SELECT RAISE(ABORT, 'controlled pre-commit refusal'); END;",
    ).unwrap();
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let refused = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease.clone(),
            case.request.clone(),
        ))
        .await;
    let refused_counts = canonical_counts(&case.pair.central);
    case.pair.central.mailbox_store.connection().unwrap()
        .execute_batch("DROP TRIGGER reject_available_append").unwrap();
    let retried = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(lease, case.request))
        .await;
    let committed_counts = canonical_counts(&case.pair.central);
    case.pair.finish().await;
    assert!(refused.is_err());
    assert_eq!(refused_counts, [0, 0, 0]);
    assert_eq!(committed_counts, [1, 1, 1]);
    let retried = retried.unwrap().value;
    assert_eq!(retried["sequence"], 1, "a rollback cannot consume a sequence");
    assert_eq!(retried["duplicate"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_lost_reply_reopens_and_recovers_the_original_committed_identity() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    // Discard the real first reply. Recovery must use the persisted canonical
    // outcome, not an identity copied into a synthetic second response.
    client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease,
            case.request.clone(),
        ))
        .await
        .unwrap();
    let stored: (String, u64) = case.pair.central.mailbox_store.connection().unwrap()
        .query_row("SELECT id, sequence FROM mailbox_messages", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        }).unwrap();
    let path = resolve_coordination_persistence_path(case.pair.central.persistence_path.as_path());
    let reopened = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..case.pair.central.clone()
    };
    let recovered = reopened.execute_federation_mailbox(
        case.request,
        Some(case.pair.lease.clone()),
    );
    let counts = canonical_counts(&reopened);
    case.pair.finish().await;
    let recovered = recovered.unwrap().value;
    assert_eq!(recovered["messageId"], stored.0);
    assert_eq!(recovered["sequence"], stored.1);
    assert_eq!(recovered["duplicate"], true);
    assert_eq!(counts, [1, 1, 1]);
}

fn committed_target(case: &MailboxCase) -> FederationMailboxBinding {
    let encoded: String = case.pair.central.mailbox_store.connection().unwrap()
        .query_row("SELECT target_binding_json FROM federation_mailbox_wake_obligations", [],
            |row| row.get(0)).unwrap();
    serde_json::from_str(&encoded).unwrap()
}

fn queued_mailbox_boundaries(app: &AppState, binding: &FederationMailboxBinding) -> Vec<u64> {
    let inner = app.inner.lock().unwrap();
    let index = inner.find_session_index(&binding.identity.session_id).unwrap();
    inner.sessions[index].queued_prompts.iter()
        .filter_map(|queued| queued.pending_prompt.source.as_ref()?.mailbox.as_ref())
        .map(|mailbox| mailbox.sequence)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_native_obligation_recovers_after_reopen_without_an_http_owner() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let response = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, case.request.clone(),
    )).await.unwrap();
    let binding = committed_target(&case);
    let before = queued_mailbox_boundaries(&case.pair.central, &binding);
    let path = resolve_coordination_persistence_path(case.pair.central.persistence_path.as_path());
    let reopened = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..case.pair.central.clone()
    };
    let first_batch = reopened.clone();
    tokio::task::spawn_blocking(move || first_batch.run_native_mailbox_wake_batch(0)).await.unwrap();
    let after = queued_mailbox_boundaries(&reopened, &binding);
    let pending = reopened.mailbox_store.native_mailbox_wake_pending(
        response.value["messageId"].as_str().unwrap(),
    ).unwrap();
    let second_batch = reopened.clone();
    tokio::task::spawn_blocking(move || second_batch.run_native_mailbox_wake_batch(0)).await.unwrap();
    let repeated = queued_mailbox_boundaries(&reopened, &binding);
    case.pair.finish().await;
    assert!(before.is_empty(), "the HTTP receipt does not dispatch the native wake");
    assert_eq!(after, [1]);
    assert_eq!(repeated, after, "recovery cannot append a duplicate notification");
    assert!(!pending, "durable queue ownership settles the native pending projection");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_native_worker_rechecks_the_exact_target_binding_at_queue_apply() {
    let case = MailboxCase::new().await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, case.request.clone(),
    )).await.unwrap();
    let binding = committed_target(&case);
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut release = ReleaseWake(Some(release));
    *case.pair.central.mailbox_store.before_native_wake.lock().unwrap() =
        Some(Box::new(move || {
            let _ = entered.send(());
            released.recv_timeout(phase_sync::DEADLOCK_GUARD).unwrap();
        }));
    let app = case.pair.central.clone();
    let batch = tokio::task::spawn_blocking(move || app.run_native_mailbox_wake_batch(0));
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, reached).await.unwrap().unwrap();
    {
        let mut inner = case.pair.central.inner.lock().unwrap();
        let index = inner.find_session_index(&binding.identity.session_id).unwrap();
        inner.session_mut_by_index(index).unwrap().federation_mailbox_binding
            .as_mut().unwrap().placement_generation += 1;
    }
    release.release();
    batch.await.unwrap();
    let withheld = queued_mailbox_boundaries(&case.pair.central, &binding);
    {
        let mut inner = case.pair.central.inner.lock().unwrap();
        let index = inner.find_session_index(&binding.identity.session_id).unwrap();
        inner.session_mut_by_index(index).unwrap().federation_mailbox_binding = Some(binding.clone());
    }
    let app = case.pair.central.clone();
    tokio::task::spawn_blocking(move || app.run_native_mailbox_wake_batch(0)).await.unwrap();
    let recovered = queued_mailbox_boundaries(&case.pair.central, &binding);
    case.pair.finish().await;
    assert!(withheld.is_empty(), "a stale target binding cannot enqueue into its successor");
    assert_eq!(recovered, [1], "the obligation remains recoverable under its exact binding");
}
