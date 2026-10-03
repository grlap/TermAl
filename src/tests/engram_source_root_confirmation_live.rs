//! Current selection-loss presentation against a pinned disposable producer.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn restored_live_selection_omits_the_obsolete_rename_instruction() {
    let fixture = completion_fixture();
    let session = &fixture.live.session_id;
    let added = fixture.work(
        session,
        &["add", "Inspect restored selection presentation", "--json"],
    );
    let work = added["work"]["short_ref"]
        .as_str()
        .or_else(|| added["short_ref"].as_str())
        .unwrap();
    fixture.work(session, &["claim", work, "--json"]);
    fixture
        .live
        .state
        .ensure_engram_session_bound_off_lock(session)
        .unwrap()
        .unwrap();
    let request = || EngramSourceRootRequest {
        work: work.to_owned(),
        path: Some(Some(fixture.root.to_string_lossy().into_owned())),
    };
    let first = fixture
        .live
        .state
        .name_engram_source_root(session, request())
        .unwrap();
    let target = AppState::engram_binding_target_for_session_shape_locked(
        &fixture.live.state.inner.lock().unwrap(),
        session,
        true,
    )
    .unwrap()
    .unwrap();
    let binding = target.work_binding.clone().unwrap();
    let token = target.routing_token.clone().unwrap();
    // A real canonical successor makes the original local selection obsolete.
    // This is a protocol operation in the disposable producer, not a store edit
    // or an injected root-read response.
    let successor = first.generation + 1;
    let receipt: EngramNamedRootReceipt = parse_engram_result(
        fixture
            .live
            .transport
            .request(
                &target.connection,
                &EngramControlRequest::NamedRootBind {
                    routing_token: token.clone(),
                    claim_id: binding.claim_id.clone(),
                    claim_fence: binding.claim_fence,
                    workspace_id: first.root.clone().unwrap(),
                    generation: successor as i64,
                    named_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    kind: EngramNamedRootKind::Bound,
                    end_reason: None,
                    idempotency_key: format!(
                        "selection-presentation-successor:{}:{successor}",
                        binding.claim_id
                    ),
                },
                target.settings.call_timeout(),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt.generation, successor as i64);
    let changed = fixture.canonical_read(session, &binding);
    assert!(
        matches!(changed.named_root, EngramNamedRootState::Bound { generation, .. }
        if generation == successor as i64)
    );
    fixture
        .live
        .state
        .reconcile_engram_named_root(
            session,
            &token,
            Some(&binding),
            Some(changed.named_root),
            successor,
        )
        .unwrap();
    fixture.record(session, |record| {
        assert!(record.engram.source_root_notices.iter().any(|notice|
            matches!(&notice.kind, EngramSourceRootNoticeKind::SelectionLoss { selection }
                if selection.claim_id == binding.claim_id && selection.generation == first.generation)));
    });
    let restored = fixture
        .live
        .state
        .name_engram_source_root(session, request())
        .unwrap();
    assert!(restored.generation > successor);
    let kept = fixture
        .live
        .state
        .name_engram_source_root(session, request())
        .unwrap();
    assert_eq!(kept.generation, restored.generation);
    let confirmed = fixture.canonical_read(session, &binding);
    assert!(
        matches!(confirmed.named_root, EngramNamedRootState::Bound { generation, .. }
        if generation == restored.generation as i64)
    );
    let dispatch = dispatch_live_root(
        &fixture.live.state,
        session,
        "Inspect the confirmed selection.",
        None,
    );
    deliver_turn_dispatch(&fixture.live.state, dispatch).unwrap();
    let CodexRuntimeCommand::Prompt { command, .. } = fixture.live.receiver.try_recv().unwrap()
    else {
        panic!("fresh provider prompt");
    };
    assert!(
        !command
            .prompt
            .contains("Engram no longer confirms this claim's local source-root selection"),
        "{}",
        command.prompt
    );
    fixture.record(session, |record| {
        assert!(matches!(record.engram.active_turn_root_capture,
            Some(EngramRootCapture::Recorded { generation, state: EngramSourceRootState::Named, .. })
                if generation == restored.generation as i64));
        assert_eq!(record.engram.active_turn_start_basis.as_ref().unwrap().source_root_generation,
            Some(restored.generation as i64));
        assert!(record.engram.source_root_notices.is_empty());
    });
    finish_live_root(&fixture.live.state, session);
}
