// Owns mailbox namespace transitions, exact references and local wake delivery.
// Does not provision transport or test sender intents. Split from the central
// mailbox integration coverage while exercising domain/standalone boundaries.
use super::federation_mailboxes::{central_store, local_participant, request};
use super::*;

fn standalone(state: &AppState) -> String {
    let id = test_session_id(state, Agent::Codex);
    let mut inner = state.inner.lock().unwrap();
    let index = inner.find_session_index(&id).unwrap();
    inner.session_mut_by_index(index).unwrap().session.status = SessionStatus::Active;
    id
}

fn send_request(target: &str) -> SendMailboxMessageRequest {
    SendMailboxMessageRequest {
        target_session_id: target.to_owned(),
        message: "namespace boundary".to_owned(),
        idempotency_key: "boundary".to_owned(),
        topic: None,
        state_stamp: None,
        class: None,
    }
}

#[test]
fn raw_standalone_append_cannot_cross_a_domain_namespace() {
    let state = central_store();
    let a = standalone(&state);
    let (b, _) = local_participant(&state, "Bound");
    // The public API now qualifies native roots automatically. The raw local
    // store must still refuse crossing that namespace in either direction.
    for (sender, target) in [(&a, &b), (&b, &a)] {
        let error = state.append_mailbox_message_and_notify(sender, send_request(target)).unwrap_err();
        assert_eq!(error.status, StatusCode::CONFLICT);
    }
    let connection = state.mailbox_store.connection().unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[test]
fn explicit_native_join_refuses_existing_standalone_history_without_hiding_it() {
    let state = central_store();
    let a = standalone(&state);
    let b = standalone(&state);
    let sent = state.append_mailbox_message_and_notify(&a, send_request(&b)).unwrap();
    let before = federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap()));
    let error = state.join_federation_mailbox_session(&b).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(&sent.mailbox_id), "{}", error.message);
    assert!(error.message.contains("1") && error.message.contains("fresh session"));
    assert_eq!(before, federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap())));
    assert!(state.federation_mailbox_actor(&b).unwrap().is_none());
    assert_eq!(state.mailbox_store.read_message(&b, &sent.message_id).unwrap().body, "namespace boundary");
    let page = state.mailbox_store.read_range_with_issuance(&b, &sent.mailbox_id, None, 50, true).unwrap();
    state.mailbox_store.acknowledge_page(&b, &sent.mailbox_id, page.receipt.as_deref().unwrap()).unwrap();
    // Processed history is still history, and must stay accessible too.
    assert_eq!(state.join_federation_mailbox_session(&b).unwrap_err().status, StatusCode::CONFLICT);
    let fresh = standalone(&state);
    state.join_federation_mailbox_session(&fresh).unwrap();
}

#[test]
fn concurrent_explicit_native_join_and_standalone_send_have_one_admitted_winner() {
    let state = central_store();
    let a = standalone(&state);
    let b = standalone(&state);
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let sender = state.clone();
    let sender_target = b.clone();
    let send_barrier = barrier.clone();
    let send = std::thread::spawn(move || {
        send_barrier.wait();
        sender.append_mailbox_message_and_notify(&a, send_request(&sender_target))
    });
    let joining = state.clone();
    let join_barrier = barrier.clone();
    let join = std::thread::spawn(move || {
        join_barrier.wait();
        joining.join_federation_mailbox_session(&b)
    });
    barrier.wait();
    let sent = send.join().unwrap();
    let joined = join.join().unwrap();
    assert_ne!(sent.is_ok(), joined.is_ok(), "send={sent:?}, join={joined:?}");
}

#[test]
fn queued_bound_native_wake_records_delivery_and_drops_an_acknowledged_successor() {
    let state = central_store();
    let (a, _) = local_participant(&state, "Sender");
    let (b, target) = local_participant(&state, "Busy target");
    let sent: MailboxAppendReceipt = serde_json::from_value(state.execute_federation_mailbox(
        request(&state, &a, FederationMailboxOperation::Send {
            target: target.session_id.clone(), message: "wake".to_owned(), key: "wake".to_owned(),
            topic: None, state_stamp: None,
        }), None).unwrap().value).unwrap();
    assert_eq!(sent.notification_disposition, "durableButNotWoken");
    {
        let inner = state.inner.lock().unwrap();
        assert!(inner.sessions[inner.find_session_index(&b).unwrap()].queued_prompts.is_empty());
    }
    state.run_native_mailbox_wake_batch(0);
    {
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.sessions[inner.find_session_index(&b).unwrap()].queued_prompts.len(), 1);
    }
    state.mark_mailbox_notification_delivered(&MailboxNotificationDelivery {
        session_id: b.clone(), mailbox_id: sent.mailbox_id.clone(), through_sequence: sent.sequence,
    });
    let message = state.mailbox_store.read_message(&target.session_id, &sent.message_id).unwrap();
    assert_eq!(message.notification_state, "deliveredToIdleSession");
    assert!(state.mailbox_store.mailbox_wake_boundary(&target.session_id, &sent.mailbox_id,
        &sent.message_id).unwrap().unwrap().delivered);
    let page = state.mailbox_store.read_range_with_issuance(&target.session_id, &sent.mailbox_id,
        None, 50, true).unwrap();
    state.execute_federation_mailbox(request(&state, &b, FederationMailboxOperation::Acknowledge {
        mailbox_id: sent.mailbox_id.clone(), receipt: page.receipt.unwrap(),
    }), None).unwrap();
    state.revalidate_front_mailbox_wakeup_for_session(&b).unwrap();
    let inner = state.inner.lock().unwrap();
    assert!(inner.sessions[inner.find_session_index(&b).unwrap()].queued_prompts.is_empty());
}

#[test]
fn canonical_participant_id_wins_over_a_legal_display_alias() {
    let state = central_store();
    let (_, first) = local_participant(&state, "First");
    let (_, alias) = local_participant(&state, &first.session_id);
    let inner = state.inner.lock().unwrap();
    assert_eq!(resolve_federation_mailbox_participant_locked(&inner, &first.session_id).unwrap(), first);
    assert_ne!(first.session_id, alias.session_id);
}

#[test]
fn bridge_canonical_participant_id_wins_over_display_alias() {
    let rows = vec![
        json!({"sessionId":"domain-session-canonical","name":"Central::First","localName":"First"}),
        json!({"sessionId":"domain-session-other","name":"Satellite::Other","localName":"domain-session-canonical"}),
    ];
    assert_eq!(resolve_domain_mailbox_reference(&rows, "domain-session-canonical", "session-1").unwrap(),
        "domain-session-canonical");
}

#[test]
fn bare_native_id_cannot_resolve_another_hosts_enrolled_session() {
    let state = central_store();
    let unenrolled = standalone(&state);
    let (caller, _) = local_participant(&state, "Caller");
    let (_, _, _, remote) = federation_mailbox_authority::remote(&state);
    assert_eq!(remote.binding.identity.session_id, unenrolled);
    let error = state.execute_federation_mailbox(request(&state, &caller,
        FederationMailboxOperation::GetOrCreate { target: unenrolled }), None).unwrap_err();
    assert_eq!(error.status, StatusCode::NOT_FOUND);
    assert!(error.message.contains("qualified id required"), "{}", error.message);
}

#[test]
fn already_bound_central_native_join_is_an_idempotent_no_op() {
    let state = central_store();
    let (native, participant) = local_participant(&state, "Already bound");
    let before = federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap()));
    assert_eq!(state.join_federation_mailbox_session(&native).unwrap(), participant);
    assert_eq!(before, federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap())));
}

#[test]
fn satellite_install_refuses_standalone_history_without_mutating_its_native_session() {
    let state = central_store();
    let central = federation_registry::peer(48001);
    state.configure_federation(FederationSettings {
        enabled: true, role: FederationRole::Satellite, central: Some(central.identity.clone()),
    }).unwrap();
    let peer = federation_registry::active(&state, central);
    assert_eq!(peer.local_role, FederationRole::Satellite);
    let sender = standalone(&state);
    let target = standalone(&state);
    let sent = state.append_mailbox_message_and_notify(&sender, send_request(&target)).unwrap();
    let creation = {
        let inner = state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&target).unwrap()].federation_native_creation_id.clone()
    };
    let binding = FederationMailboxBinding {
        central: peer.peer.clone(),
        identity: FederationSessionIdentity {
            host_id: peer.local.host_id.clone(), session_id: target.clone(),
            creation_id: Uuid::new_v4().to_string(),
        },
        placement_generation: 1,
    };
    let before = federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap()));
    let error = state.install_federation_mailbox_binding(&peer, binding, &creation).unwrap_err();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert!(error.message.contains(&sent.mailbox_id) && error.message.contains("fresh session"));
    assert_eq!(before, federation_persist_image(&PersistedState::metadata_from_inner(&state.inner.lock().unwrap())));
    assert!(state.federation_mailbox_actor(&target).unwrap().is_none());
    let page = state.mailbox_store.read_range_with_issuance(&target, &sent.mailbox_id, None, 50, true).unwrap();
    assert_eq!(page.messages[0].body, "namespace boundary");
    state.mailbox_store.acknowledge_page(&target, &sent.mailbox_id, page.receipt.as_deref().unwrap()).unwrap();
}
