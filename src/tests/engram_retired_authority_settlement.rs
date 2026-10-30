//! Retired work-authority grants after Engram removed them: a leftover entry
//! whose revoke was never confirmed is settled as moot once at boot, visibly,
//! and a retirement handed to the revoke step is settled without a command.
//! State written before this change keeps loading; the ledger's shape is
//! unchanged.

use super::*;

fn retired_grant(
    root: &FsPath,
    grant: &str,
    revoke_confirmed: bool,
) -> EngramRetiredWorkAuthorityGrant {
    EngramRetiredWorkAuthorityGrant {
        home: root.join("home").to_string_lossy().into_owned(),
        project_root: root.to_string_lossy().into_owned(),
        store_key: None,
        project_id: "fixture-project".to_owned(),
        grant_hash: grant.to_owned(),
        retired_at: "2026-09-01T10:00:00.000Z".to_owned(),
        reason: "TermAl project Engram work-authority grant removed".to_owned(),
        revoke_confirmed,
    }
}

/// A host whose ledger holds one unconfirmed and one confirmed retirement
/// for its project, and whose project shows the background-revocation warning.
fn seeded_host(label: &str) -> (AppState, PathBuf, String) {
    let state = test_app_state();
    let root = state
        .test_temp_root
        .as_ref()
        .expect("test root should exist")
        .path()
        .join(label);
    fs::create_dir_all(&root).expect("project root should exist");
    let project_id = create_test_project(&state, &root, "Retired work authority");
    {
        let mut inner = state.inner.lock().expect("state mutex poisoned");
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "grant-unconfirmed", false));
        inner.engram_retired_work_authority_grants.push(retired_grant(&root, "grant-confirmed", true));
        let index = inner
            .projects
            .iter()
            .position(|project| project.id == project_id)
            .expect("project should exist");
        inner.projects[index].engram_cleanup_warning =
            Some(ENGRAM_BACKGROUND_REVOCATION_DEGRADED_NOTICE.to_owned());
        state.commit_locked(&mut inner).expect("seeded ledger should persist");
    }
    (state, root, project_id)
}

fn ledger_grants(inner: &StateInner) -> Vec<(String, bool)> {
    inner
        .engram_retired_work_authority_grants
        .iter()
        .map(|entry| (entry.grant_hash.clone(), entry.revoke_confirmed))
        .collect()
}

fn project_warning(inner: &StateInner, project_id: &str) -> Option<String> {
    inner
        .find_project(project_id)
        .and_then(|project| project.engram_cleanup_warning.clone())
}

#[test]
fn boot_settles_an_unconfirmed_retired_grant_once_and_visibly() {
    let (state, root, project_id) = seeded_host("retired-authority-boot");
    state.shutdown_persist_blocking();
    let restart_path = root.join("restart.sqlite");
    fs::copy(state.persistence_path.as_path(), &restart_path)
        .expect("the persisted store should copy");

    // State written before the change loads unchanged, the unconfirmed entry included.
    let before = load_state(&restart_path).unwrap().unwrap();
    assert_eq!(
        ledger_grants(&before),
        [("grant-unconfirmed".to_owned(), false), ("grant-confirmed".to_owned(), true)]
    );

    // A host booting from it settles the unconfirmed entry, keeps the
    // confirmed one, and replaces the project's warning with the settled notice.
    let booted = AppState::new_with_paths(
        root.to_string_lossy().into_owned(),
        restart_path.clone(),
        root.join("orchestrators.json"),
    )
    .expect("the store should boot");
    booted.shutdown_persist_blocking();
    {
        let inner = booted.inner.lock().expect("state mutex poisoned");
        assert_eq!(ledger_grants(&inner), [("grant-confirmed".to_owned(), true)]);
        assert_eq!(
            project_warning(&inner, &project_id).as_deref(),
            Some(ENGRAM_RETIRED_AUTHORITY_SETTLED_NOTICE)
        );
    }

    // It was persisted in that boot, so the next boot finds nothing to settle.
    let after = load_state(&restart_path).unwrap().unwrap();
    assert_eq!(ledger_grants(&after), [("grant-confirmed".to_owned(), true)]);
    assert_eq!(
        project_warning(&after, &project_id).as_deref(),
        Some(ENGRAM_RETIRED_AUTHORITY_SETTLED_NOTICE)
    );
    booted.settle_retired_engram_work_authority_grants_after_boot();
    let inner = booted.inner.lock().expect("state mutex poisoned");
    assert_eq!(ledger_grants(&inner), [("grant-confirmed".to_owned(), true)]);
}

#[test]
fn a_failed_settlement_commit_restores_the_ledger_and_the_warning() {
    let (mut state, root, project_id) = seeded_host("retired-authority-commit-failure");
    state.shutdown_persist_blocking();
    let failing_path = root.join("settlement-persist-failure.sqlite");
    fs::create_dir_all(&failing_path).expect("a directory at the persistence path forces failure");
    state.persistence_path = Arc::new(failing_path);

    state.settle_retired_engram_work_authority_grants_after_boot();

    let inner = state.inner.lock().expect("state mutex poisoned");
    assert_eq!(
        ledger_grants(&inner),
        [("grant-unconfirmed".to_owned(), false), ("grant-confirmed".to_owned(), true)],
        "a failed commit must leave the entry for the next boot"
    );
    assert_eq!(
        project_warning(&inner, &project_id).as_deref(),
        Some(ENGRAM_BACKGROUND_REVOCATION_DEGRADED_NOTICE)
    );
}

#[test]
fn a_retirement_handed_to_the_revoke_step_settles_without_running_a_command() {
    let temp = TestTempRoot::create("retired-authority-no-command");
    // A binary that does not exist: running it would have failed the revoke.
    let target = EngramAuthorityRevocationTarget {
        binary_path: temp.path().join("missing-engram.exe").to_string_lossy().into_owned(),
        home: temp.path().join("home").to_string_lossy().into_owned(),
        project_root: temp.path().to_string_lossy().into_owned(),
        store_key: None,
        work_authority_grant: "grant-handed-in".to_owned(),
    };
    let settled = revoke_project_engram_authority_off_lock(
        std::slice::from_ref(&target),
        Some("TermAl project Engram work-authority grant removed"),
        None,
        temp.path(),
    );
    assert!(settled.confirmed_targets == vec![target.clone()]);
    assert!(settled.failure.is_none());

    // An unavailable identity still reports its failure; only the revoke became moot.
    let unresolved = revoke_project_engram_authority_off_lock(
        std::slice::from_ref(&target),
        Some("TermAl project Engram work-authority grant removed"),
        Some("identity unavailable".to_owned()),
        temp.path(),
    );
    assert!(unresolved.confirmed_targets == vec![target]);
    assert_eq!(unresolved.failure.as_deref(), Some("identity unavailable"));

    // Without a retirement reason nothing is settled.
    let none = revoke_project_engram_authority_off_lock(&[], None, None, temp.path());
    assert!(none.confirmed_targets.is_empty() && none.failure.is_none());
}
