// Owns real-client mailbox uncertainty and same-key recovery regressions.
// Does not synthesize MACs or central receipts. Split from security request tests.

use super::*;
use mailbox_fixture::{MailboxCase, ReceiptRelay};

async fn committed_outcome(replay: bool) {
    let case = MailboxCase::new().await;
    let mut relay = ReceiptRelay::new(case.central_port(), replay).await;
    let client = case.client(relay.port);
    let lease = client.channel.current_lease().unwrap();
    let context = client.channel.security_context().unwrap();
    let request = case.request.clone();
    let first_client = client.clone();
    let first_lease = lease.clone();
    let first = tokio::spawn(async move {
        first_client
            .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(first_lease, request))
            .await
    });
    let (committed_status, receipt) =
        tokio::time::timeout(phase_sync::DEADLOCK_GUARD, relay.committed.recv())
            .await
            .unwrap()
            .unwrap();
    let count_at_commit = case.message_count();
    if !replay {
        case.pair.satellite.inner.lock().unwrap().federation_durable = false;
        relay.release.send(()).unwrap();
    }
    let outcome = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, first)
        .await
        .unwrap()
        .unwrap();
    let error = outcome.err();
    let observed = error.as_ref().map(|error| {
        let raw: anyhow::Error = TermalDelegationApiError {
            status: error.status,
            message: error.message.clone(),
        }
        .into();
        let unknown = termal_delegation_outcome_is_unknown(&raw);
        let bridge = mailbox_send_bridge_error(raw);
        (
            error.status,
            unknown,
            bridge
                .downcast_ref::<TermalDelegationUnknownOutcome>()
                .is_some(),
            error.message.contains("same idempotencyKey"),
            error.message.clone(),
        )
    });
    let retained = client.channel.security_current(&context).is_ok()
        && lease.with_incarnation(|| Ok(())).is_ok();
    let transport_settled = context.sender.lock().unwrap().outstanding.is_empty();
    // This test controls only the post-dispatch authority hold. Exact durable
    // reconciliation has separate mutation/writer tests; release this hold so
    // the original application key can recover the real committed receipt.
    case.pair.satellite.inner.lock().unwrap().federation_durable = true;
    let recovered = client
        .request::<FederationMailboxResponse>(FederationRpcRoute::Mailbox(
            lease,
            case.request.clone(),
        ))
        .await;
    let final_count = case.message_count();
    relay.finish().await;
    case.pair.finish().await;
    // Drain all real IO before checking the RED/ GREEN discriminating result.
    assert_eq!(
        committed_status,
        StatusCode::OK,
        "central must actually commit: {receipt}"
    );
    assert_eq!(count_at_commit, 1);
    assert!(
        retained,
        "authenticated uncertain refusal must preserve exact transport"
    );
    assert!(
        transport_settled,
        "the verified transport response settles its counter only"
    );
    let observed = observed.expect("a withheld/unusable committed receipt remains an error");
    assert!(
        observed.1 && observed.2 && observed.3,
        "committed append must remain bridge-unknown with original-key guidance; replay={replay}, observed={observed:?}"
    );
    let recovered = recovered.unwrap();
    assert_eq!(recovered.value["messageId"], receipt["value"]["messageId"]);
    assert_eq!(recovered.value["duplicate"], true);
    assert_eq!(final_count, 1, "same-key recovery must not append again");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_local_hold_after_signed_mailbox_receipt_keeps_unknown_outcome() {
    committed_outcome(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_security_signed_replay_refusal_cannot_hide_committed_mailbox_outcome() {
    committed_outcome(true).await;
}
