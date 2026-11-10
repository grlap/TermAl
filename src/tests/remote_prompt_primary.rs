// Actual queue/receipt and same-connection durability-fence controls. These
// do not claim current permission admission, provider entry or retirement.
use super::*;

fn queued_fixture() -> (StateInner, String, remote_prompt_authority::Intent) {
    let mut inner = StateInner::new();
    let record = inner.create_session(
        Agent::Claude,
        Some("Direct receipt".into()),
        ".".into(),
        None,
        None,
    );
    let session = record.session.id.clone();
    // create_session returns a clone; queue on the actual primary row.
    let record = inner.session_mut_by_index(0).unwrap();
    let base = remote_prompt_journal::intent("same-key", "exact body\r\n ");
    let (basis, reference, key, text) = base.components();
    let mut basis = basis.clone();
    basis.target = remote_prompt_authority::NativeRoot::new(
        Uuid::from_u128(2),
        1,
        session.clone(),
        Uuid::parse_str(&record.federation_native_creation_id).unwrap(),
    )
    .unwrap();
    let intent =
        remote_prompt_authority::Intent::new(basis, reference, key.into(), text.into()).unwrap();
    let id = RemotePromptPrimaryAssociation::new(&intent)
        .unwrap()
        .prompt_id();
    queue_receipted_orchestrator_prompt_on_record(
        record,
        PendingPrompt {
            id,
            timestamp: stamp_now(),
            text: text.into(),
            expanded_text: None,
            source: None,
            attachments: vec![],
            engram_interrupted: false,
            is_engram_retained: false,
        },
        vec![],
    );
    (inner, session, intent)
}

#[test]
fn remote_prompt_primary_association_dedup_keeps_settled_missing_queue_history() {
    let (mut inner, session, intent) = queued_fixture();
    let record = inner.session_mut_by_index(0).unwrap();
    assert_eq!(record.session.id, session);
    assert!(associate_remote_prompt_receipt(record, &intent).unwrap());
    let id = RemotePromptPrimaryAssociation::new(&intent)
        .unwrap()
        .prompt_id();
    record.prompt_deliveries.get_mut(&id).unwrap().disposition =
        PromptDeliveryDisposition::Accepted;
    // Ordinary settled display receipts must not evict the immutable remote
    // identity, even when it is the oldest receipt and its queue is gone.
    let mut ordinary = record.prompt_deliveries[&id].clone();
    ordinary.remote_prompt = None;
    for sequence in 1..=PROMPT_DELIVERY_SETTLED_LIMIT + 1 {
        ordinary.sequence = u64::try_from(sequence).unwrap() + 100;
        record
            .prompt_deliveries
            .insert(format!("ordinary-{sequence}"), ordinary.clone());
    }
    record.queued_prompts.clear();
    prune_prompt_deliveries(record);
    assert!(record.prompt_deliveries.contains_key(&id));
    assert_eq!(
        record.prompt_deliveries.len(),
        PROMPT_DELIVERY_SETTLED_LIMIT + 1
    );
    let stored = serde_json::to_string(&PersistedSessionRecord::from_record(record)).unwrap();
    let loaded: PersistedSessionRecord = serde_json::from_str(&stored).unwrap();
    let mut restored = loaded.into_record().unwrap();
    assert!(
        restored.prompt_deliveries[&id]
            .remote_prompt
            .as_ref()
            .unwrap()
            .identity
            .matches(&intent)
    );
    assert!(restored.prompt_deliveries[&id].entry.is_none());
    assert!(!associate_remote_prompt_receipt(&mut restored, &intent).unwrap());
    assert!(
        restored.queued_prompts.is_empty(),
        "a duplicate cannot recreate an entry"
    );
    let (basis, reference, key, _) = intent.components();
    let conflict = remote_prompt_authority::Intent::new(
        basis.clone(),
        reference,
        key.into(),
        "changed".into(),
    )
    .unwrap();
    assert!(associate_remote_prompt_receipt(&mut restored, &conflict).is_err());
}

#[test]
fn remote_prompt_primary_association_refuses_wrong_body_and_target_before_mutation() {
    for target_changed in [false, true] {
        let (mut inner, _, intent) = queued_fixture();
        let record = inner.session_mut_by_index(0).unwrap();
        if target_changed {
            record.federation_native_creation_id = Uuid::new_v4().to_string();
        } else {
            record
                .queued_prompts
                .front_mut()
                .unwrap()
                .pending_prompt
                .text
                .push('x');
        }
        let before = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
        assert!(associate_remote_prompt_receipt(record, &intent).is_err());
        assert_eq!(
            serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap(),
            before
        );
    }
}

#[test]
fn remote_prompt_primary_queue_fence_requires_exact_body_association_and_nonce() {
    let (mut inner, session, intent) = queued_fixture();
    let record = inner.session_mut_by_index(0).unwrap();
    associate_remote_prompt_receipt(record, &intent).unwrap();
    let association = RemotePromptPrimaryAssociation::new(&intent).unwrap();
    let id = association.prompt_id();
    let hold = &record.queued_prompt_durability_holds[&id];
    let target = PersistFenceTarget::RemotePromptQueue {
        session_id: session.clone(),
        prompt_id: id.clone(),
        content: queued_prompt_durability_fence_content(&hold.nonce, &hold.content),
        association: Box::new(association),
        write_ahead: record.prompt_deliveries[&id].write_ahead.clone(),
    };
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    ensure_sqlite_state_schema(&connection).unwrap();
    let original = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    let write = |row: &Value| {
        connection.execute("INSERT INTO sessions VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET value_json=excluded.value_json",
            rusqlite::params![session, row.to_string()]).unwrap();
    };
    write(&original);
    assert!(target.is_already_durable(&connection).unwrap());
    for field in ["body", "association", "nonce"] {
        let mut changed: PersistedSessionRecord = serde_json::from_value(original.clone()).unwrap();
        match field {
            "body" => changed.queued_prompts[0].pending_prompt.text = "changed".into(),
            "association" => {
                changed
                    .prompt_deliveries
                    .get_mut(&id)
                    .unwrap()
                    .remote_prompt = None
            }
            "nonce" => {
                changed.prompt_deliveries.get_mut(&id).unwrap().write_ahead =
                    Uuid::new_v4().to_string()
            }
            _ => unreachable!(),
        }
        write(&serde_json::to_value(changed).unwrap());
        assert!(!target.is_already_durable(&connection).unwrap(), "{field}");
    }
    write(&original);
    assert!(target.is_already_durable(&connection).unwrap());
}

#[test]
fn remote_prompt_primary_association_alone_cannot_start_handoff() {
    let state = test_app_state();
    let (mut inner, _, intent) = queued_fixture();
    let record = inner.session_mut_by_index(0).unwrap();
    associate_remote_prompt_receipt(record, &intent).unwrap();
    let id = RemotePromptPrimaryAssociation::new(&intent)
        .unwrap()
        .prompt_id();
    assert!(!state.prompt_delivery_gate_locked(&mut inner, 0, &id, false));
    assert_eq!(inner.sessions[0].prompt_deliveries[&id].attempt, 0);
    assert!(!state.prompt_delivery_gate_locked(&mut inner, 0, &id, true));
    state.begin_prompt_delivery_write_ahead_locked(&mut inner, 0, &id, true, None);
    assert_eq!(inner.sessions[0].prompt_deliveries[&id].attempt, 0);
}

#[test]
fn remote_prompt_primary_resume_refuses_without_beginning_an_attempt() {
    let state = test_app_state();
    let (mut inner, session, intent) = queued_fixture();
    let record = inner.session_mut_by_index(0).unwrap();
    associate_remote_prompt_receipt(record, &intent).unwrap();
    record.orchestrator_auto_dispatch_blocked = true;
    let accepted = receipted_resume_identity(record, None).unwrap();
    let before = serde_json::to_value(PersistedSessionRecord::from_record(record)).unwrap();
    *state.inner.lock().unwrap() = inner;
    assert!(matches!(
        state.resume_receipted_head(&session, None, &accepted),
        ReceiptedResume::Refused(_)
    ));
    let inner = state.inner.lock().unwrap();
    assert_eq!(
        serde_json::to_value(PersistedSessionRecord::from_record(&inner.sessions[0])).unwrap(),
        before
    );
}
