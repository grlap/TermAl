// A late terminal remains attached to its original work/claim/fence.
// Changing focus never copies that fact into another ordinary trace.

pub(super) fn held_snapshot(
    bindings: &[&EngramControlWorkBinding],
    focused: &EngramControlWorkBinding,
) -> EngramHeldClaims {
    EngramHeldClaims {
        items: bindings.iter().map(|binding| EngramHeldClaim {
            work_id: binding.work_id.clone(),
            short_ref: binding.work_id.clone(),
            claim_id: binding.claim_id.clone(),
            claim_fence: binding.claim_fence,
            focused: binding.work_id == focused.work_id,
            control_binding: Some((*binding).clone()),
        }).collect(),
        omitted: 0,
    }
}

fn select_next_claim(turn: &EOCheckedTurn, claims: EngramHeldClaims) {
    let current = turn.record(|record| record.engram.work_binding.clone());
    let selected = select_engram_held_binding(claims.items.clone(), claims.omitted,
        EngramBindingPreference { current: current.as_ref() })
        .expect("the fixture has a focused held claim");
    turn.transport.set_current_work_binding(Some(selected.clone()));
    turn.transport.replace_held_claims([Ok(claims)]);
    turn.transport.enable_named_roots(&turn.session_id, &selected);
    turn.state.name_engram_source_root(&turn.session_id, EngramSourceRootRequest {
        work: selected.work_id,
        path: Some(Some(turn.root.join(".worktrees").join("wt")
            .to_string_lossy().into_owned())),
    }).expect("ordinary work services name the selected claim's root");
}

fn original_owner(turn: &EOCheckedTurn) -> (String, String, i64, String) {
    turn.record(|record| {
        let carried = record.engram.carried_checks.iter()
            .find(|carried| carried.check.key == ITEM).expect("original carried check");
        (carried.work_id.clone(), carried.claim_id.clone(), carried.claim_fence,
            carried.check.key.clone())
    })
}

#[test]
fn an_ended_codex_carry_keeps_its_original_owner_when_focus_moves_to_b_and_back() {
    let (turn, worktree) = two_turns("reported-held-focus");
    let launch = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let other = test_control_work_binding("reported-held-other", 1);
    turn.transport.replace_held_claims([Ok(held_snapshot(&[&launch, &other], &launch))]);
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);
    let owner = original_owner(&turn);
    assert_eq!(owner, (launch.work_id.clone(), launch.claim_id.clone(),
        launch.claim_fence, ITEM.to_owned()));

    select_next_claim(&turn, held_snapshot(&[&launch, &other], &other));
    let on_b = next_turn_observation(&turn);
    assert_eq!(on_b["binding"]["work_id"], other.work_id);
    assert_eq!(on_b["binding"]["claim_id"], other.claim_id);
    assert!(observed_checks(&on_b).is_empty(), "{on_b:#}");
    assert_eq!(original_owner(&turn), owner,
        "focus does not relabel the original local result as B's");

    select_next_claim(&turn, held_snapshot(&[&launch, &other], &launch));
    let on_a = next_turn_observation(&turn);
    assert_eq!(on_a["binding"]["claim_id"], launch.claim_id);
    assert!(observed_checks(&on_a).is_empty(),
        "returning to A does not create a new occurrence of its old check: {on_a:#}");
    assert_no_automatic_credit(&turn);
}

#[test]
fn an_ended_codex_carry_cannot_be_relabelled_after_release_replacement_or_recovery() {
    for cause in ["released", "replaced", "recovered"] {
        let (turn, worktree) = two_turns(&format!("reported-claim-{cause}"));
        let launch = turn.record(|record| record.engram.work_binding.clone().unwrap());
        start_test(&turn, &worktree, ITEM);
        assert_carried_at_close(&turn);
        report_late_end(&turn, THREAD, ITEM, 0);
        wait_for_carried_snapshots(&turn);
        let owner = original_owner(&turn);
        let mut current = match cause {
            "released" => test_control_work_binding("reported-unrelated-held", 1),
            _ => launch.clone(),
        };
        match cause {
            "replaced" => {
                current.claim_id = "replacement-reported-claim".to_owned();
                current.root_execution_id = "replacement-reported-root".to_owned();
                current.run_id = "replacement-reported-run".to_owned();
            }
            "recovered" => current.claim_fence += 1,
            _ => {}
        }
        current.work_revision += 1;
        select_next_claim(&turn, held_snapshot(&[&current], &current));
        let successor = next_turn_observation(&turn);
        assert_eq!(successor["binding"]["claim_id"], current.claim_id);
        assert_eq!(successor["binding"]["claim_fence"], current.claim_fence);
        assert!(observed_checks(&successor).is_empty(), "{cause}: {successor:#}");
        turn.record(|record| {
            for carried in record.engram.carried_checks.iter()
                .filter(|carried| carried.check.key == ITEM) {
                assert_eq!((carried.work_id.clone(), carried.claim_id.clone(),
                    carried.claim_fence, carried.check.key.clone()), owner,
                    "{cause}: custody is either retained under its original identity or retired locally");
                assert_ne!((carried.claim_id.as_str(), carried.claim_fence),
                    (current.claim_id.as_str(), current.claim_fence));
            }
        });
        assert_no_automatic_credit(&turn);
    }
}
