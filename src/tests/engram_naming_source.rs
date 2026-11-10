// Naming's private owner capture, public summary and historical seal decoding.
use super::*;
use crate::tests::engram_source_command::source_envelope;
use crate::tests::engram_source_owner::{source_owner_output, source_owner_reply};

fn owner_identity(root: &FsPath) -> EngramSourceId {
    let mut envelope = source_envelope(root, "not_clean");
    envelope["source_id"]["submodules"] = json!([{
        "path":"child", "commit":"d".repeat(40), "changes":[{
            "path":"nested.txt", "status":" M", "origin":null,
            "blob":"e".repeat(40), "mode":"100644"
        }]
    }]);
    serde_json::from_value(envelope["source_id"].clone()).unwrap()
}

fn install_identity(turn: &EOCheckedTurn, identity: EngramSourceId) {
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
        if args[0] == "--version" {
            return Ok(source_owner_reply(args));
        }
        let mut envelope = source_envelope(FsPath::new(&args[2]), "not_clean");
        envelope["source_id"] = serde_json::to_value(&identity).unwrap();
        Ok(source_owner_output(
            0,
            serde_json::to_vec(&envelope).unwrap(),
        ))
    }));
}

#[test]
fn naming_and_sealing_keep_the_entire_owner_object_private() {
    for rename in [false, true] {
        let label = if rename {
            "owner-rename"
        } else {
            "owner-clear"
        };
        let turn = EOCheckedTurn::idle(label, true);
        let worktree = naming_worktree(&turn.root);
        let identity = owner_identity(&worktree);
        install_identity(&turn, identity.clone());
        let named =
            request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
        let basis = named.source_basis.as_ref().unwrap();
        assert_eq!(basis.source_id.as_ref(), Some(&identity));
        assert!(basis.source_revision.is_empty());
        let public = serde_json::to_value(&named).unwrap();
        assert!(public.get("sourceRevision").is_none());
        assert!(public.get("sourceId").is_none());
        assert!(public.get("sourceBasis").is_none());
        assert!(public.get("unmeasured").is_none());
        assert_eq!(public["root"], named.root.as_ref().unwrap().as_str());

        turn.begin("Keep the original root and generation.");
        let response = request_naming(
            &turn,
            label,
            rename.then_some(turn.root.as_path()),
            vec![naming_claim(label)],
        )
        .unwrap();
        let public = serde_json::to_value(&response).unwrap();
        assert_eq!(public["sealed"], json!({"root":named.root}));
        turn.record(|record| {
            let root = record.engram.observed_source_root().unwrap();
            assert_eq!(root.generation, named.generation);
            assert_eq!(root.sealed_source_id.as_ref(), Some(&identity));
            assert!(root.sealed_revision.is_none());
            let private = serde_json::to_value(root).unwrap();
            assert_eq!(
                private["sealed_source_id"],
                serde_json::to_value(&identity).unwrap()
            );
            assert!(private.get("sealed_revision").is_none());
            let decoded: EngramTurnSourceRoot = serde_json::from_value(private).unwrap();
            assert_eq!(decoded, *root);
        });
        // Binding/read requests keep their identity-only wire contract.
        let requests = turn.transport.requests();
        let root_requests = requests
            .iter()
            .filter(|request| {
                matches!(
                    request.request["operation"].as_str(),
                    Some("named_root_bind" | "named_root_read")
                )
            })
            .collect::<Vec<_>>();
        for operation in ["named_root_bind", "named_root_read"] {
            assert!(
                root_requests
                    .iter()
                    .any(|request| request.request["operation"] == operation),
                "the fixture must exercise {operation}"
            );
        }
        for request in root_requests {
            assert!(request.request.get("source_id").is_none());
            assert!(request.request.get("source_revision").is_none());
        }
        turn.end();
    }
}

#[test]
fn clearing_a_new_generation_does_not_overwrite_the_opening_turns_first_seal() {
    let label = "owner-seal-generation";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    let first_identity = owner_identity(&worktree);
    install_identity(&turn, first_identity.clone());
    let first_name =
        request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    turn.begin("Keep this original generation through both clear operations.");
    let first_clear = request_naming(&turn, label, None, vec![naming_claim(label)]).unwrap();
    assert_eq!(
        first_clear.sealed.unwrap().source_id,
        Some(first_identity.clone())
    );

    let mut later_identity = first_identity.clone();
    later_identity.submodules[0].commit = "f".repeat(40);
    install_identity(&turn, later_identity.clone());
    let later_name =
        request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    assert!(later_name.generation > first_name.generation);
    assert_eq!(
        later_name.source_basis.unwrap().source_id,
        Some(later_identity)
    );
    let later_clear = request_naming(&turn, label, None, vec![naming_claim(label)]).unwrap();
    assert!(
        later_clear.sealed.is_none(),
        "the current name belongs to a later generation"
    );
    turn.record(|record| {
        let original = record.engram.observed_source_root().unwrap();
        assert_eq!(original.generation, first_name.generation);
        assert_eq!(original.sealed_source_id.as_ref(), Some(&first_identity));
        assert!(original.sealed_revision.is_none());
    });
    turn.end();
}

#[test]
fn naming_owner_unavailable_never_falls_back_to_local_git() {
    let label = "owner-unavailable";
    let turn = EOCheckedTurn::idle(label, true);
    // The historical reader excludes machine Git configuration. Keep this
    // fixture's checkout normalization consistent with its positive control.
    run_git_test_command(&turn.root, &["config", "core.autocrlf", "false"]);
    let worktree = naming_worktree(&turn.root);
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner =
        Some(Arc::new(|_, _, args, _, _| Ok(source_owner_reply(args))));
    // Historical clean-commit diagnostics can measure this root; naming must not use them.
    assert!(
        engram_place_source_basis_with(
            &EngramBasisPlace::Workdir(worktree.to_string_lossy().into_owned()),
            true
        )
        .is_some()
    );
    let named = request_naming(&turn, label, Some(&worktree), vec![naming_claim(label)]).unwrap();
    assert!(named.source_basis.is_none());
    assert!(named.unmeasured.is_some());
    assert!(named.generation > 0, "absence is not a new naming gate");
    turn.begin("No owner identity is available to seal.");
    let cleared = request_naming(&turn, label, None, vec![naming_claim(label)]).unwrap();
    assert!(cleared.sealed.unwrap().source_id.is_none());
    turn.record(|record| {
        let root = record.engram.observed_source_root().unwrap();
        assert!(root.sealed_source_id.is_none());
        assert!(root.sealed_revision.is_none());
    });
    turn.end();
}

#[test]
fn historical_string_seals_decode_without_conversion() {
    let saved = json!({"root":"historical/root", "common_dir_key":"historical/git",
        "short_ref":"historical-work", "claim_id":"historical-claim", "generation":4,
        "sealed_revision":"content-v1:original-stored-text"});
    let decoded: EngramTurnSourceRoot = serde_json::from_value(saved.clone()).unwrap();
    assert_eq!(
        decoded.sealed_revision.as_deref(),
        Some("content-v1:original-stored-text")
    );
    assert!(decoded.sealed_source_id.is_none());
    assert_eq!(serde_json::to_value(decoded).unwrap(), saved);
}

#[test]
fn naming_capture_rechecks_connection_and_original_worktree() {
    for change in ["connection", "worktree"] {
        let turn = EOCheckedTurn::idle(&format!("owner-stale-{change}"), true);
        let worktree = naming_worktree(&turn.root);
        let root = fs::canonicalize(&worktree)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let common =
            engram_exact_path_key(&engram_source_root_common_dir(FsPath::new(&root)).unwrap());
        let target = AppState::engram_source_root_target_locked(
            &turn.state.inner.lock().unwrap(),
            &turn.session_id,
        )
        .unwrap()
        .unwrap();
        let state = turn.state.clone();
        let project = target.project_id.clone();
        let changed_root = worktree.clone();
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = Some(Arc::new(move |_, _, args, _, _| {
            if args[0] == "--version" {
                return Ok(source_owner_reply(args));
            }
            let reply = source_owner_output(
                0,
                serde_json::to_vec(&source_envelope(FsPath::new(&args[2]), "clean")).unwrap(),
            );
            if change == "connection" {
                state
                    .inner
                    .lock()
                    .unwrap()
                    .projects
                    .iter_mut()
                    .find(|p| p.id == project)
                    .unwrap()
                    .engram
                    .as_mut()
                    .unwrap()
                    .binary_path = Some("changed-owner".into());
            } else {
                fs::rename(
                    changed_root.join(".git"),
                    changed_root.join("removed-dot-git"),
                )
                .unwrap();
            }
            Ok(reply)
        }));
        assert!(
            turn.state
                .engram_naming_source_basis_within(
                    &target,
                    &root,
                    &common,
                    ENGRAM_SOURCE_COMMAND_BUDGET
                )
                .is_none(),
            "{change}"
        );
        turn.state
            .inner
            .lock()
            .unwrap()
            .test_observation_source_owner = None;
    }
}

#[test]
fn naming_owner_capture_keeps_the_existing_capacity_and_deadline_bounds() {
    use std::sync::atomic::Ordering;
    let label = "owner-bounds";
    let turn = EOCheckedTurn::idle(label, true);
    let worktree = naming_worktree(&turn.root);
    let root = fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let common = engram_exact_path_key(&engram_source_root_common_dir(FsPath::new(&root)).unwrap());
    let target = AppState::engram_source_root_target_locked(
        &turn.state.inner.lock().unwrap(),
        &turn.session_id,
    )
    .unwrap()
    .unwrap();
    let live = turn
        .state
        .inner
        .lock()
        .unwrap()
        .engram_turn_basis_captures_live
        .clone();
    live.store(ENGRAM_TURN_BASIS_CAPTURE_LIMIT, Ordering::SeqCst);
    assert!(
        turn.state
            .engram_naming_source_basis_within(
                &target,
                &root,
                &common,
                ENGRAM_SOURCE_COMMAND_BUDGET
            )
            .is_none()
    );
    live.store(0, Ordering::SeqCst);
    assert!(
        turn.state
            .engram_naming_source_basis_within(&target, &root, &common, Duration::ZERO)
            .is_none()
    );

    let bounds = Arc::new(Mutex::new(Vec::new()));
    let observed = bounds.clone();
    turn.state
        .inner
        .lock()
        .unwrap()
        .test_observation_source_owner = Some(Arc::new(move |_, _, args, deadline, limit| {
        observed.lock().unwrap().push((deadline, limit));
        if args[0] == "--version" {
            return Ok(source_owner_reply(args));
        }
        Ok(source_owner_output(
            0,
            serde_json::to_vec(&source_envelope(FsPath::new(&args[2]), "clean")).unwrap(),
        ))
    }));
    let before = std::time::Instant::now();
    assert!(
        turn.state
            .engram_naming_source_basis_within(
                &target,
                &root,
                &common,
                ENGRAM_SOURCE_COMMAND_BUDGET * 2
            )
            .is_some()
    );
    let bounds = bounds.lock().unwrap();
    assert_eq!(bounds.len(), 2);
    assert_eq!(
        bounds[0].0, bounds[1].0,
        "both probes share the aggregate deadline"
    );
    assert!(bounds[0].0 >= before);
    assert!(bounds[0].0 <= std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET);
    assert_eq!(bounds[0].1, 4096);
    assert_eq!(bounds[1].1, ENGRAM_SOURCE_JSON_LIMIT + 1);
}
