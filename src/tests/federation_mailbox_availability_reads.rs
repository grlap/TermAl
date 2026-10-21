// Owns fail-first native page availability across coordination writer waits.
// Exercises the authenticated central read, not a UI preview or live mailbox.
use super::*;
use mailbox_fixture::MailboxCase;

pub(super) fn incoming(case: &MailboxCase, key: &str, body: &str) -> Value {
    let target = match &case.request.operation {
        FederationMailboxOperation::Send { target, .. } => target,
        _ => unreachable!("mailbox fixture starts with a send"),
    };
    let native_sender = {
        let inner = case.pair.central.inner.lock().unwrap();
        inner.federation.mailbox_directory.get(target).unwrap()
            .binding.identity.session_id.clone()
    };
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    // Seed an actual incoming message. Sending as the reader would advance
    // its own cursor, so an omitted afterSequence would correctly be empty.
    let seed = super::super::federation_mailboxes::request(
        &case.pair.central, &native_sender, FederationMailboxOperation::Send {
            target: reader.clone(),
            message: body.to_owned(),
            key: key.to_owned(),
            topic: None,
            state_stamp: None,
        },
    );
    case.pair.central.execute_federation_mailbox(seed, None).unwrap().value
}

pub(super) async fn read_request(case: &MailboxCase) -> FederationMailboxRequest {
    let sent = incoming(case, "incoming-page-fixture", "one durable central append");
    let mailbox_id = sent["mailboxId"].as_str().unwrap().to_owned();
    let reader = case.request.actor.as_ref().unwrap().binding.participant_key();
    assert_eq!(sent["senderProcessedThrough"], 1);
    let reader_cursor: u64 = case.pair.central.mailbox_store.connection().unwrap()
        .query_row("SELECT processed_through FROM mailbox_participants
                    WHERE mailbox_id = ?1 AND session_id = ?2",
            rusqlite::params![&mailbox_id, &reader], |row| row.get(0)).unwrap();
    assert_eq!(reader_cursor, 0, "incoming message must remain unread for the remote actor");
    FederationMailboxRequest {
        operation: FederationMailboxOperation::Read {
            mailbox_id,
            after_sequence: None,
            limit: 200,
            issue_receipt: true,
        },
        ..case.request.clone()
    }
}

fn assert_pending_page(response: Result<FederationMailboxResponse, ApiError>) {
    assert!(response.is_ok(),
        "native read must return its committed page while receipt issuance is blocked: {response:?}");
    let page = response.unwrap().value;
    assert_eq!(page["messages"][0]["body"], "one durable central append");
    assert_eq!(page["messages"][0]["sequence"], 1);
    assert_eq!(page["receiptState"], "receiptPending");
    assert!(page["receipt"].is_null(), "pending grants no ACK authority");
    assert!(page["readKey"].as_str().is_some_and(|key| !key.is_empty()));
    assert_eq!(page["afterSequence"], 0);
    assert_eq!(page["processedThrough"], 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unchanged_actor_read_issues_a_receipt_while_the_primary_writer_is_held() {
    let case = MailboxCase::new().await;
    let request = read_request(&case).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let primary = sqlite_state_write_lock(case.pair.central.persistence_path.as_path());
    let held = lock_sqlite_state_writer(&primary);
    let response = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, request,
    )).await;
    drop(held);
    case.pair.finish().await;
    let page = response.expect("unchanged actor read has no primary persistence fence").value;
    assert_eq!(page["messages"][0]["body"], "one durable central append");
    assert!(page["receipt"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_page_survives_held_coordination_writer_without_receipt_authority() {
    let case = MailboxCase::with_writer_timeout(Duration::from_millis(100)).await;
    let request = read_request(&case).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let writer = case.pair.central.mailbox_store.write_lock.clone();
    let held = lock_sqlite_state_writer(&writer);
    let response = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, request,
    )).await;
    drop(held);
    let issued: u64 = case.pair.central.mailbox_store.connection().unwrap()
        .query_row("SELECT COUNT(*) FROM mailbox_read_pages", [], |row| row.get(0)).unwrap();
    case.pair.finish().await;
    assert_eq!(issued, 0, "a pending page is never durable issuance");
    assert_pending_page(response);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_page_does_not_wait_for_the_long_lived_writer_connection_mutex() {
    let case = MailboxCase::new().await;
    let request = read_request(&case).await;
    let client = case.client(case.central_port());
    let lease = client.channel.current_lease().unwrap();
    let held = case.pair.central.mailbox_store.connection().unwrap();
    let response = client.request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
        lease, request,
    )).await;
    // The old blocking worker can outlive the HTTP waiter. Release and join
    // its capacity before asserting the intended availability failure.
    drop(held);
    let drained = tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        case.pair.lease.channel.bound.permits.clone().acquire_many_owned(4))
        .await.unwrap().unwrap();
    drop(drained);
    case.pair.finish().await;
    assert_pending_page(response);
}
