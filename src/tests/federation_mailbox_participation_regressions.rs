// Review counterexamples for transition ownership and authority at consumers.
// Uses authenticated disposable hosts; never live providers or user stores.
use super::*;

#[path = "federation_mailbox_participation_lifecycle.rs"]
mod lifecycle;

pub(super) async fn public_send(
    state: &AppState,
    from: &str,
    target: &str,
    key: &str,
    body: &str,
) -> Result<MailboxAppendReceipt, ApiError> {
    send_mailbox_message(
        AxumPath(from.to_owned()),
        State(state.clone()),
        Json(
            serde_json::from_value(json!({"targetSessionId":target,
            "idempotencyKey":key,"message":body}))
            .unwrap(),
        ),
    )
    .await
    .map(|(_, Json(receipt))| receipt)
}

#[tokio::test]
async fn automatic_binding_cannot_admit_a_signed_executor_wake() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let s = native(&pair.satellite, "S");
    let sent = public_send(
        &pair.central,
        &c,
        "S",
        "no-executor-grant",
        "ordinary mailbox message",
    )
    .await
    .unwrap();
    let target = pair
        .satellite
        .federation_mailbox_actor(&s)
        .unwrap()
        .unwrap()
        .binding;
    {
        let inner = pair.satellite.inner.lock().unwrap();
        let root = &inner.sessions[inner.find_session_index(&s).unwrap()];
        assert!(root.federation_mailbox_binding.is_none());
        assert_eq!(
            root.federation_automatic_mailbox_binding.as_ref(),
            Some(&target)
        );
        let saved = serde_json::to_value(PersistedSessionRecord::from_record(root)).unwrap();
        assert!(
            saved.get("federationMailboxBinding").is_none(),
            "older receiver must see no grant-shaped binding"
        );
        assert_eq!(
            saved["federationAutomaticMailboxBinding"],
            serde_json::to_value(&target).unwrap()
        );
    }
    let sender = pair
        .central
        .federation_mailbox_actor(&c)
        .unwrap()
        .unwrap()
        .binding
        .participant_key();
    let result = pair
        .rpc
        .request::<FederationExecutorReceipt>(FederationRpcRoute::ExecutorWake(
            pair.lease.clone(),
            FederationExecutorWakeRequest {
                protocol_version: FEDERATION_PROTOCOL_VERSION,
                operation_key: format!("mailbox-wake:{}", sent.message_id),
                target,
                wake: FederationExecutorWake {
                    mailbox_id: sent.mailbox_id,
                    message_id: sent.message_id,
                    sequence: sent.sequence,
                    unread_count: 1,
                    sender_session_id: sender,
                    sender_name: "C".into(),
                    topic: None,
                },
            },
        ))
        .await;
    assert!(
        result.is_err(),
        "mailbox qualification must not authorize the executor receiver"
    );
    let connection = pair.satellite.mailbox_store.connection().unwrap();
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM federation_executor_operations",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
        0
    );
    drop(connection);
    pair.finish().await;
}

#[tokio::test]
async fn committed_standalone_retry_keeps_its_receipt_after_automatic_join() {
    let state = federation_mailboxes::central_store();
    let identity = state
        .inner
        .lock()
        .unwrap()
        .federation
        .local
        .clone()
        .unwrap();
    state
        .configure_federation(FederationSettings {
            enabled: false,
            role: FederationRole::Standalone,
            central: None,
        })
        .unwrap();
    let a = native(&state, "A");
    let b = native(&state, "B");
    let original = public_send(&state, &a, &b, "lost-reply", "committed original bytes")
        .await
        .unwrap();
    state
        .configure_federation(FederationSettings {
            enabled: true,
            role: FederationRole::Central,
            central: Some(identity),
        })
        .unwrap();
    directory(&state, &a).await;
    let retry = public_send(&state, &a, &b, "lost-reply", "committed original bytes")
        .await
        .unwrap();
    assert_eq!(retry.mailbox_id, original.mailbox_id);
    assert_eq!(retry.message_id, original.message_id);
    assert_eq!(retry.sequence, original.sequence);
    let conflict = public_send(&state, &a, &b, "lost-reply", "changed bytes")
        .await
        .unwrap_err();
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(
        state
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| row
                .get::<_, u64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn automatic_local_history_listing_reactivates_before_its_snapshot() {
    let state = federation_mailboxes::central_store();
    let a = native(&state, "A");
    let b = native(&state, "B");
    let original = state
        .append_mailbox_message_and_notify(
            &a,
            SendMailboxMessageRequest {
                target_session_id: b.clone(),
                message: "local history".into(),
                idempotency_key: "left-row".into(),
                topic: None,
                state_stamp: None,
                class: None,
            },
        )
        .unwrap();
    state.mailbox_store.mark_session_left(&b).unwrap();
    directory(&state, &b).await;
    let listed = list_mailboxes_with_availability(&b, &state).await.unwrap();
    assert!(
        listed
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.id == original.mailbox_id)
    );
}

#[tokio::test]
async fn known_bare_local_target_does_not_need_an_unavailable_peer() {
    let pair = qualified_pair().await;
    let a = native(&pair.central, "A");
    let b = native(&pair.central, "B");
    directory(&pair.central, &a).await;
    // Verification/link stay current; only the unrelated reverse RPC handle
    // disappears. No call to that peer is needed to address these local roots.
    pair.central.federation_peer_handles.lock().unwrap().clear();
    let sent = public_send(&pair.central, &a, &b, "local-independent", "local bytes").await;
    assert!(
        sent.is_ok(),
        "known local target depended on unrelated discovery: {sent:?}"
    );
    let created: Result<MailboxSummary, ApiError> = pair
        .central
        .federation_mailbox_operation(
            &a,
            FederationMailboxOperation::GetOrCreate { target: b.clone() },
        )
        .await;
    assert_eq!(
        created.unwrap_err().status,
        StatusCode::NOT_FOUND,
        "domain-only create cannot reinterpret a bare native target"
    );
    let target = pair
        .central
        .federation_mailbox_actor(&b)
        .unwrap()
        .unwrap()
        .binding
        .participant_key();
    let created: Result<MailboxSummary, ApiError> = pair
        .central
        .federation_mailbox_operation(&a, FederationMailboxOperation::GetOrCreate { target })
        .await;
    assert!(
        created.is_ok(),
        "known local GetOrCreate depended on unrelated discovery"
    );
    pair.finish().await;
}

#[tokio::test]
async fn one_fenced_root_does_not_hide_other_qualified_roots() {
    let pair = qualified_pair().await;
    let c = native(&pair.central, "C");
    let good = native(&pair.satellite, "Good");
    let bad = native(&pair.satellite, "Fenced");
    directory(&pair.central, &c).await;
    {
        let mut inner = pair.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&bad).unwrap();
        inner
            .session_mut_by_index(index)
            .unwrap()
            .federation_automatic_mailbox_binding
            .as_mut()
            .unwrap()
            .central
            .host_generation += 1;
    }
    let rows = directory(&pair.central, &c).await;
    assert!(
        rows.iter()
            .any(|row| row.binding.identity.session_id == good)
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.binding.identity.session_id == bad)
    );
    pair.finish().await;
}

#[test]
fn catalog_steps_bound_inventory_work_and_preserve_every_eligible_root() {
    let state = federation_mailboxes::central_store();
    let ids: HashSet<_> = (0..130)
        .map(|index| native(&state, &format!("Native {index}")))
        .collect();
    let inner = state.inner.lock().unwrap();
    let central = inner.federation.local.clone().unwrap();
    let mut after: Option<String> = None;
    let mut seen = HashSet::new();
    let mut prior = 0;
    loop {
        let page = federation_native_catalog_locked(
            &inner,
            after.as_deref(),
            central.clone(),
            central.clone(),
            String::new(),
        )
        .unwrap();
        assert!(page.roots.len() <= FEDERATION_CATALOG_PAGE);
        for root in page.roots {
            assert!(seen.insert(root.session_id));
        }
        let Some(next) = page.next else {
            break;
        };
        let position: FederationCatalogPosition = serde_json::from_str(&next).unwrap();
        assert!(position.session > prior && position.session - prior <= FEDERATION_CATALOG_SCAN);
        prior = position.session;
        after = Some(next);
    }
    assert_eq!(seen, ids);
}

/// Reads a central-domain page through the public route, as the MCP and CLI
/// bridges do, and returns it with its issued receipt.
async fn routed_domain_page(
    state: &AppState,
    native: &str,
    mailbox: &str,
    after_sequence: Option<u64>,
) -> MailboxReadResponse {
    routed_read(state, native, mailbox, after_sequence)
        .await
        .expect("the reader's domain page is readable")
}

async fn routed_read(
    state: &AppState,
    native: &str,
    mailbox: &str,
    after_sequence: Option<u64>,
) -> Result<MailboxReadResponse, ApiError> {
    read_mailbox(
        AxumPath((native.to_owned(), mailbox.to_owned())),
        State(state.clone()),
        Json(ReadMailboxRequest {
            read_key: None,
            issue_receipt: true,
            after_sequence,
            limit: 20,
        }),
    )
    .await
    .map(|page| page.0)
}

/// A central store with one root bound to the central domain and one native
/// root created after discovery, so it stays unbound.
async fn domain_and_native_roots() -> (AppState, String, String) {
    let state = federation_mailboxes::central_store();
    let domain = test_session_id(&state, Agent::Codex);
    activate(&state, &domain);
    directory(&state, &domain).await;
    assert!(state.federation_mailbox_actor(&domain).unwrap().is_some());
    let native = test_session_id(&state, Agent::Codex);
    activate(&state, &native);
    assert!(state.federation_mailbox_actor(&native).unwrap().is_none());
    (state, domain, native)
}

fn activate(state: &AppState, native: &str) {
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(native).unwrap();
    inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
}

/// A mailbox id that is neither the minted shape nor a valid relay locator is
/// refused with 400, naming the received value escaped and bounded, on the
/// read and the acknowledge route, for a native root and for a central-domain
/// root. The shown text is cut on a whole escape sequence at 96 characters.
#[tokio::test]
async fn malformed_mailbox_ids_are_refused_with_the_received_value() {
    let (state, domain, native) = domain_and_native_roots().await;
    let cases = [
        (
            "mailbox-93903743-d329-4a85-bb31-4aa20538567a ".to_owned(),
            "\"mailbox-93903743-d329-4a85-bb31-4aa20538567a \"".to_owned(),
        ),
        ("mailbox-\u{7}".to_owned(), "\"mailbox-\\u{7}\"".to_owned()),
        (
            format!("mailbox-{}", "a".repeat(5000)),
            format!("\"mailbox-{}\"… (5008 characters)", "a".repeat(88)),
        ),
        (
            format!("mailbox-{}", "\u{7}".repeat(96)),
            format!("\"mailbox-{}\"… (104 characters)", "\\u{7}".repeat(17)),
        ),
        (
            "relay-mailbox-garbage".to_owned(),
            "invalid mailbox relay locator: mailboxId \"relay-mailbox-garbage\"".to_owned(),
        ),
    ];
    for caller in [&native, &domain] {
        for (id, shown) in &cases {
            let read = routed_read(&state, caller, id, None).await.unwrap_err();
            let acknowledged = routed_domain_ack(&state, caller, id, "receipt", false)
                .await
                .unwrap_err();
            for (route, error) in [("read", read), ("acknowledge", acknowledged)] {
                assert_eq!(
                    error.status,
                    StatusCode::BAD_REQUEST,
                    "{route} by {caller}: {}",
                    error.message
                );
                assert!(
                    error.message.contains(shown.as_str()),
                    "{route} names the received id: {}",
                    error.message
                );
                assert!(
                    !error.message.contains('\u{7}'),
                    "control characters are escaped"
                );
                assert!(
                    error.message.chars().count() < 512,
                    "the echo is bounded: {} characters",
                    error.message.chars().count()
                );
            }
        }
    }
}

/// A well-formed id that names no mailbox of the caller still reaches the
/// central-domain lookup, and its 404 names the id received.
#[tokio::test]
async fn an_unknown_canonical_mailbox_id_is_named_in_the_domain_not_found() {
    let (state, domain, _) = domain_and_native_roots().await;
    let unknown = "mailbox-00000000-0000-4000-8000-000000000002";
    let read = routed_read(&state, &domain, unknown, None)
        .await
        .unwrap_err();
    let acknowledged = routed_domain_ack(&state, &domain, unknown, "receipt", false)
        .await
        .unwrap_err();
    for (route, error) in [("read", read), ("acknowledge", acknowledged)] {
        assert_eq!(
            error.status,
            StatusCode::NOT_FOUND,
            "{route}: {}",
            error.message
        );
        assert!(
            error.message.contains(&format!("and mailbox {unknown} in")),
            "{route} names the received id: {}",
            error.message
        );
    }
}

async fn routed_domain_ack(
    state: &AppState,
    native: &str,
    mailbox: &str,
    receipt: &str,
    no_continuation: bool,
) -> Result<MailboxSummary, ApiError> {
    acknowledge_mailbox(
        AxumPath((native.to_owned(), mailbox.to_owned())),
        State(state.clone()),
        Json(AcknowledgeMailboxRequest {
            receipt: Some(receipt.to_owned()),
            expected_processed_through: None,
            processed_through: None,
            no_continuation,
        }),
    )
    .await
    .map(|summary| summary.0)
}

/// The reported sequence: a page of the other side's mail is acknowledged,
/// then the reader's own reply is the only row of a page read from the
/// previous cursor. That page is acknowledged too, after the refused
/// noContinuation attempt the reporter made first. Runs for an automatic
/// and an explicit binding.
#[tokio::test]
async fn a_domain_page_holding_only_the_readers_own_send_is_acknowledged() {
    for explicit in [false, true] {
        let state = federation_mailboxes::central_store();
        let (reader, writer, reader_key, writer_key) = if explicit {
            let (reader, reader_row) = federation_mailboxes::local_participant(&state, "Reader");
            let (writer, writer_row) = federation_mailboxes::local_participant(&state, "Writer");
            (reader, writer, reader_row.session_id, writer_row.session_id)
        } else {
            let reader = test_session_id(&state, Agent::Codex);
            let writer = test_session_id(&state, Agent::Codex);
            {
                let mut inner = state.inner.lock().unwrap();
                for native in [&reader, &writer] {
                    let index = inner.find_session_index(native).unwrap();
                    inner.session_mut_by_index(index).unwrap().session.status =
                        SessionStatus::Active;
                }
            }
            let rows = directory(&state, &reader).await;
            let key = |native: &str| {
                rows.iter()
                    .find(|row| row.binding.identity.session_id == native)
                    .expect("both roots participate automatically")
                    .session_id
                    .clone()
            };
            let (reader_key, writer_key) = (key(&reader), key(&writer));
            (reader, writer, reader_key, writer_key)
        };
        let send = |from: String, target: String, key: &'static str| {
            let state = state.clone();
            async move {
                state
                    .federation_mailbox_operation::<MailboxAppendReceipt>(
                        &from,
                        FederationMailboxOperation::Send {
                            target,
                            message: format!("body of {key}"),
                            key: key.to_owned(),
                            topic: None,
                            state_stamp: None,
                        },
                    )
                    .await
                    .expect("the domain send is admitted")
            }
        };

        let inbound = send(writer.clone(), reader_key.clone(), "inbound").await;
        let mailbox = inbound.mailbox_id.clone();
        let first = routed_domain_page(&state, &reader, &mailbox, None).await;
        assert_eq!(first.messages.len(), 1);
        let first_receipt = first.receipt.expect("the inbound page issues a receipt");
        routed_domain_ack(&state, &reader, &mailbox, &first_receipt, false)
            .await
            .expect("the inbound page is acknowledged");

        let own = send(reader.clone(), writer_key.clone(), "own-reply").await;
        assert_eq!(own.mailbox_id, mailbox);
        // With nothing unread, the send moves the reader's own cursor past
        // its reply, so a default read is empty. The reported page holding
        // that reply was read from the previous cursor explicitly.
        assert!(own.sender_cursor_advanced, "explicit: {explicit}");
        let second = routed_domain_page(&state, &reader, &mailbox, Some(inbound.sequence)).await;
        assert_eq!(
            second
                .messages
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>(),
            vec![own.message_id.clone()],
            "the next page holds only the reader's own send (explicit: {explicit})"
        );
        let receipt = second.receipt.expect("the own-send page issues a receipt");
        let refused = routed_domain_ack(&state, &reader, &mailbox, &receipt, true)
            .await
            .unwrap_err();
        assert_eq!(refused.status, StatusCode::BAD_REQUEST);
        let summary = routed_domain_ack(&state, &reader, &mailbox, &receipt, false)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "the own-send page must be acknowledged (explicit: {explicit}): {} {}",
                    error.status, error.message
                )
            });
        assert!(
            summary
                .participants
                .iter()
                .any(|row| row.session_id == reader_key && row.processed_through == own.sequence),
            "the reader's cursor reaches its own send (explicit: {explicit})"
        );
    }
}
