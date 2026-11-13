// Launch-member provenance is retained without binding or credit.
// Actual passive settlement is covered by engram_batch_gate_flow.
use super::*;

const ROOT: &str = r"\\?\C:\repo\.tmp\wt-batch";
const OTHER_ROOT: &str = r"\\?\C:\repo\.tmp\wt-other";
const COMMON: &str = "c:/repo/.git";
const SESSION: &str = "session-merger";

fn store(project: &str) -> EngramAuthorityStoreKey {
    EngramAuthorityStoreKey {
        database_path: PathBuf::from(r"C:\store\engram.db"),
        project_id: project.to_owned(),
    }
}

fn entry(work: &str, root: &str, generation: u64) -> EngramWorkSourceRoot {
    EngramWorkSourceRoot {
        store: store("termal"),
        work_id: format!("work-{work}"),
        short_ref: format!("w-{work}"),
        claim_id: format!("claim-{work}"),
        claim_fence: 3,
        root: root.to_owned(),
        common_dir_key: COMMON.to_owned(),
        named_by_session: SESSION.to_owned(),
        named_at: "2026-10-18T23:00:00.000Z".to_owned(),
        generation,
    }
}

fn binding(work: &str, fence: i64) -> EngramControlWorkBinding {
    EngramControlWorkBinding {
        root_execution_id: format!("root-{work}"),
        work_id: format!("work-{work}"),
        run_id: format!("run-{work}"),
        work_revision: 5,
        claim_id: format!("claim-{work}"),
        claim_fence: fence,
    }
}

fn held(claims: &[(&str, i64)]) -> EngramHeldClaims {
    EngramHeldClaims {
        items: claims
            .iter()
            .map(|(work, fence)| EngramHeldClaim {
                work_id: format!("work-{work}"),
                short_ref: format!("w-{work}"),
                claim_id: format!("claim-{work}"),
                claim_fence: *fence,
                focused: false,
                control_binding: Some(binding(work, *fence)),
                ..Default::default()
            })
            .collect(),
        omitted: 0,
    }
}

#[test]
fn a_launch_records_the_other_items_this_session_named_on_the_same_worktree() {
    let launch = entry("launch", ROOT, 10);
    let mut foreign = entry("foreign-session", ROOT, 13);
    foreign.named_by_session = "session-other".to_owned();
    let mut other_store = entry("other-store", ROOT, 14);
    other_store.store = store("engram");
    let mut other_repo = entry("other-repo", ROOT, 15);
    other_repo.common_dir_key = "c:/elsewhere/.git".to_owned();
    let roots = vec![
        launch.clone(),
        entry("member-a", ROOT, 11),
        entry("member-b", ROOT, 12),
        entry("member-b", ROOT, 12),
        entry("elsewhere", OTHER_ROOT, 16),
        foreign,
        other_store,
        other_repo,
    ];
    // member-a was named at fence 3 and recovered to fence 4 before the
    // launch; its root entry keeps fence 3. member-b is not held at launch.
    let held_at_launch = held(&[("launch", 3), ("member-a", 4)]);
    let targets = engram_bind_targets_at_launch(&roots, &launch, SESSION, &held_at_launch);
    assert_eq!(
        targets
            .iter()
            .map(|target| (
                target.short_ref.as_str(),
                target.claim_fence,
                target.root_generation
            ))
            .collect::<Vec<_>>(),
        vec![("w-member-a", 4, 11), ("w-member-b", 3, 12)],
        "only this session's other items on the same root, repository and store, \
         once each, at the fence each claim has at launch"
    );
    // A single-item gate has nothing to bind.
    assert!(
        engram_bind_targets_at_launch(&[launch.clone()], &launch, SESSION, &held_at_launch)
            .is_empty()
    );
    // Launch provenance retains every member, without a bind request limit.
    let many: Vec<EngramWorkSourceRoot> = (0..20)
        .map(|index| entry(&format!("many-{index}"), ROOT, 20 + index))
        .collect();
    assert_eq!(
        engram_bind_targets_at_launch(&many, &launch, SESSION, &held(&[])).len(),
        20
    );
}
