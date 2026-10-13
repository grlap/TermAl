// Claim routing of a completed Codex test carried past its launching turn.

fn held_snapshot(
    bindings: &[&EngramControlWorkBinding],
    focused: &EngramControlWorkBinding,
) -> EngramHeldClaims {
    EngramHeldClaims {
        items: bindings
            .iter()
            .map(|binding| EngramHeldClaim {
                work_id: binding.work_id.clone(),
                short_ref: binding.work_id.clone(),
                claim_id: binding.claim_id.clone(),
                claim_fence: binding.claim_fence,
                focused: binding.work_id == focused.work_id,
                control_binding: Some((*binding).clone()),
            })
            .collect(),
        omitted: 0,
    }
}

fn select_next_claim(turn: &CheckedTurn, claims: EngramHeldClaims, token: &str) {
    let current = turn.record(|record| record.engram.work_binding.clone());
    let selected = select_engram_held_binding(
        claims.items.clone(),
        claims.omitted,
        EngramBindingPreference {
            current: current.as_ref(),
            refused: &[],
        },
    );
    *turn.transport.work_bindings.lock().unwrap() = [Ok(selected)].into();
    turn.transport
        .responses
        .lock()
        .unwrap()
        .push_front(bind_reply(token));
    turn.transport.replace_held_claims([Ok(claims)]);
}

#[test]
fn an_ended_codex_carry_defers_on_held_b_then_credits_only_as_a() {
    const B: &str = "reported-focus-b-grant";
    const A: &str = "reported-return-a-grant";
    let (turn, worktree) = later_turns("reported-held-focus", &[B, A]);
    let launch = turn.record(|record| record.engram.work_binding.clone().unwrap());
    let launch_named = turn.record(|record| record.engram.named_root.clone());
    let other = test_control_work_binding("reported-held-other", 1);
    turn.record_mut(|record| record.engram.work_binding = Some(other.clone()));
    register_named_root(&turn, &worktree, 2);
    turn.record_mut(|record| {
        record.engram.work_binding = Some(launch.clone());
        record.engram.named_root = launch_named;
    });
    turn.transport.enable_named_roots(&turn.session_id, &launch);
    turn.transport
        .replace_held_claims([Ok(held_snapshot(&[&launch, &other], &launch))]);
    start_test(&turn, &worktree, ITEM);
    assert_carried_at_close(&turn);
    report_late_end(&turn, THREAD, ITEM, 0);
    wait_for_carried_snapshots(&turn);
    turn.record(|record| {
        let carried = &record.engram.carried_checks[0];
        assert!(carried.ends_by_report && carried.ended());
        assert!(carried.terminal_digest.is_none());
    });

    select_next_claim(
        &turn,
        held_snapshot(&[&launch, &other], &other),
        "reported-b-token",
    );
    let on_b = next_turn_checkpoint(&turn);
    assert_eq!(on_b["grant_id"], B, "{on_b:#}");
    assert!(on_b.get("verification_evidence").is_none(), "{on_b:#}");
    assert_eq!(turn.record(|record| record.engram.carried_checks.len()), 1);
    let line = holder_line(&turn);
    assert!(
        line.contains("finished") && line.contains("next checkpoint focused on"),
        "{line}"
    );
    assert!(line.contains(&launch.work_id), "{line}");
    assert!(!line.contains("earned no credit"), "{line}");

    select_next_claim(
        &turn,
        held_snapshot(&[&launch, &other], &launch),
        "reported-a-token",
    );
    let on_a = next_turn_checkpoint(&turn);
    assert_eq!(on_a["grant_id"], A, "{on_a:#}");
    assert_eq!(
        on_a["verification_evidence"].as_array().map(Vec::len),
        Some(1),
        "{on_a:#}"
    );
    assert!(
        observations(&on_a)
            .iter()
            .any(|observation| observation["effect"] == "observe"
                && observation["outcome"] == "succeeded"),
        "{on_a:#}"
    );
    assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
    assert_eq!(
        turn.record(|record| record.engram.work_binding.clone()),
        Some(launch)
    );
}

#[test]
fn an_ended_codex_carry_refuses_released_replaced_or_recovered_authority() {
    for cause in ["released", "replaced", "recovered"] {
        let (turn, worktree) = two_turns(&format!("reported-claim-{cause}"));
        let launch = turn.record(|record| record.engram.work_binding.clone().unwrap());
        let mut current = match cause {
            "released" => test_control_work_binding("reported-unrelated-held", 1),
            _ => launch.clone(),
        };
        if cause == "released" {
            let launch_named = turn.record(|record| record.engram.named_root.clone());
            turn.record_mut(|record| record.engram.work_binding = Some(current.clone()));
            register_named_root(&turn, &worktree, 2);
            turn.record_mut(|record| {
                record.engram.work_binding = Some(launch.clone());
                record.engram.named_root = launch_named;
            });
            turn.transport.enable_named_roots(&turn.session_id, &launch);
        }
        start_test(&turn, &worktree, ITEM);
        assert_carried_at_close(&turn);
        report_late_end(&turn, THREAD, ITEM, 0);
        wait_for_carried_snapshots(&turn);
        turn.record(|record| {
            let carried = &record.engram.carried_checks[0];
            assert!(carried.ends_by_report && carried.ended());
            assert!(carried.terminal_digest.is_none());
        });
        if cause == "replaced" {
            current.claim_id = "replacement-reported-claim".to_owned();
        }
        if cause == "recovered" {
            current.claim_fence += 1;
        }
        current.work_revision += 1;
        select_next_claim(
            &turn,
            held_snapshot(&[&current], &current),
            "reported-loss-token",
        );
        let checkpoint = next_turn_checkpoint(&turn);
        assert!(
            checkpoint.get("verification_evidence").is_none(),
            "{checkpoint:#}"
        );
        assert!(turn.record(|record| record.engram.carried_checks.is_empty()));
        let line = holder_line(&turn);
        assert!(
            line.contains("Codex test")
                && line.contains("earned no credit")
                && line.contains("its claim is no longer the one this session holds"),
            "{cause}: {line}"
        );
        assert!(
            !line.contains("next checkpoint focused on"),
            "{cause}: {line}"
        );
    }
}
