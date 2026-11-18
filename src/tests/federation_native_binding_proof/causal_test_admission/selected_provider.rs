// Connect the real provider facade from test_run_selected_provider.rs to the
// existing authenticated pull and settlement owners. Consumer replies are
// scripted: these assertions prove protocol custody, never genuine credit.
use super::*;
use crate::tests::test_runs::test_run_registration::selected_provider::{
    SelectedProviderWorktree, selected_provider_completion,
};

async fn real_provider_settles_original(mode: MailboxTestMode) {
    real_provider_settles_original_at(mode, SelectedProviderWorktree::Session).await;
}

async fn real_provider_settles_original_at(
    mode: MailboxTestMode,
    placement: SelectedProviderWorktree,
) {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) = super::super::causal_test_completion::connect(&fixture).await;
    let (mut input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    input.mode = mode;
    let registration =
        selected_provider_completion(fixture.satellite.clone(), mode, placement, |runner| {
            let fixture = &fixture;
            async move {
                let key = "real-provider-admission";
                let capture = fixture
                    .central
                    .prepare_mailbox_test_request(&fixture.native, key, Some(&input))
                    .unwrap()
                    .expect("the ordinary sender captures the destination association");
                assert!(capture.sender_scope.is_some());
                assert!(
                    capture
                        .payload
                        .targets
                        .iter()
                        .all(|target| target.association.is_some())
                );
                let request = SendMailboxMessageRequest {
                    target_session_id: format!(
                        "host:{}::{runner}",
                        fixture.satellite_channel.snapshot.local.host_id
                    ),
                    message: "run the original request through the real provider".into(),
                    idempotency_key: key.into(),
                    topic: None,
                    state_stamp: None,
                    class: None,
                };
                let receipt = fixture
                    .central
                    .send_native_mailbox_relay_with_test_custody(
                        &fixture.native,
                        &request,
                        Some(&capture),
                    )
                    .await
                    .unwrap()
                    .expect("the authenticated relay retains the original typed request");
                let page = fixture
                    .satellite
                    .read_native_mailbox_relay(
                        &runner,
                        MailboxRelayLocator::decode(&receipt.mailbox_id, false)
                            .unwrap()
                            .unwrap(),
                        ReadMailboxRequest {
                            read_key: None,
                            issue_receipt: false,
                            after_sequence: None,
                            limit: 50,
                        },
                    )
                    .await
                    .unwrap();
                page.messages
                    .into_iter()
                    .find(|message| message.id == receipt.message_id)
                    .expect("the real provider receives the original request's actual selector")
                    .id
            }
        })
        .await;
    // The helper used EngramHost.observe for start, RUN output and terminal,
    // real registration, artifact refresh and exact durable native readback.
    // It supplies no replacement observation or manufactured completion here.
    let original = registration
        .native_completion
        .as_ref()
        .unwrap()
        .completion
        .clone();
    let causal = test_run_causal_request(&registration).unwrap();
    assert_eq!(causal.payload.mode, mode);
    assert_eq!(
        causal.runner.native_session_id,
        registration.owning_session_id
    );
    let selector = CausalTestCompletionSelector {
        canonical_message_id: causal.canonical_message_id.clone(),
        payload: causal.payload.clone(),
    };
    selector.validate().unwrap();
    assert!(
        writes(&transport).is_empty(),
        "the producer never writes destination credit"
    );

    // This invokes the authenticated origin READ through Central's ordinary
    // page/currentness/lease checks, before any settlement has been created.
    let pulled = completion(&fixture.central).await;
    assert_eq!(
        pulled.selector, selector,
        "Central pulls the original causal key"
    );
    assert_eq!(
        pulled.completion, original,
        "Central retains the exact provider completion"
    );
    assert!(
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_none()
    );

    let accepted = receipt(match mode {
        MailboxTestMode::Full => None,
        MailboxTestMode::Focused => Some("not_full_mode"),
    });
    script(
        &transport,
        &scope,
        &fixture.native,
        Ok(accepted.clone()),
        true,
    );
    let mut recovery = CausalTestCompletionRecovery::default();
    for _ in 0..8 {
        recovery.tick(&fixture.central, true).await;
        recovery.task.take().unwrap().await.unwrap();
        if fixture
            .central
            .mailbox_store
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .is_some_and(|saved| saved.complete())
        {
            break;
        }
    }
    recovery.shutdown().await;
    let saved = fixture
        .central
        .mailbox_store
        .causal_test_settlement(&selector, Some(&scope))
        .unwrap()
        .expect("the supervisor retains the destination receipt on the original send");
    assert!(
        saved.complete(),
        "the original request must reach a final scripted receipt"
    );
    assert_eq!(saved.completion, original);
    assert_eq!(saved.admissions.len(), 1);
    assert_eq!(saved.admissions[0].receipt, Some(accepted));
    let sent = writes(&transport);
    assert_eq!(
        sent.len(),
        1,
        "exactly one original remote admission is written"
    );
    assert_eq!(sent[0].connection.session_id, fixture.native);
    assert_eq!(
        sent[0].request["completion"],
        serde_json::to_value(&original).unwrap()
    );
    assert_eq!(
        sent[0].request["association"]["binding"],
        serde_json::to_value(
            &selector.payload.targets[0]
                .association
                .as_ref()
                .unwrap()
                .binding
        )
        .unwrap()
    );

    let reopened = MailboxStore::open(&resolve_coordination_persistence_path(
        &fixture.central.persistence_path,
    ))
    .unwrap();
    assert_eq!(
        reopened
            .causal_test_settlement(&selector, Some(&scope))
            .unwrap()
            .unwrap(),
        saved
    );
    let request_count = transport.requests().len();
    fixture
        .central
        .settle_causal_test_completion(&pulled)
        .unwrap();
    assert_eq!(
        transport.requests().len(),
        request_count,
        "final receipt replay performs no destination RPC"
    );
    assert!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .test_run_registrations
            .is_empty(),
        "Central does not fabricate a requester-local run"
    );
    assert!(
        fixture
            .satellite
            .inner
            .lock()
            .unwrap()
            .test_run_registrations[&registration.run_id]
            .verification_record
            .is_none(),
        "a scripted destination receipt never mints producer-local credit"
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_real_provider_completion_crosses_central_pull_and_settles_original_receipt() {
    real_provider_settles_original(MailboxTestMode::Full).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn focused_real_provider_completion_crosses_central_pull_and_keeps_uncredited_receipt() {
    real_provider_settles_original(MailboxTestMode::Focused).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_streamed_completion_crosses_central_pull_and_keeps_original_receipt() {
    real_provider_settles_original_at(
        MailboxTestMode::Focused,
        SelectedProviderWorktree::SessionStream,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_streamed_completion_crosses_central_pull_and_keeps_original_receipt() {
    real_provider_settles_original_at(
        MailboxTestMode::Focused,
        SelectedProviderWorktree::LinkedStream,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_full_completion_crosses_central_pull_and_settles_original_receipt() {
    real_provider_settles_original_at(
        MailboxTestMode::Full,
        SelectedProviderWorktree::LinkedArtifact,
    )
    .await;
}
