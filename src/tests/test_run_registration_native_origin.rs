// The real registration boundary captures native provenance. A retry or a
// legacy row cannot acquire a replacement host/session by later observation.
use super::*;

pub(super) fn bind_native_host(fixture: &RunFixture) -> FederationHostIdentity {
    // The ordinary fixture already seeded its private SQLite host identity.
    // Bind it through the same owner as the federation persistence fixtures;
    // assigning an unrelated UUID would correctly fail the writer's fence.
    {
        let inner = fixture.state.inner.lock().unwrap();
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    let connection = rusqlite::Connection::open(fixture.state.persistence_path.as_path()).unwrap();
    let mut inner = fixture.state.inner.lock().unwrap();
    bind_loaded_federation(
        &mut inner,
        &connection,
        fixture.state.persistence_path.as_path(),
    )
    .unwrap();
    inner.federation.local.clone().unwrap()
}

#[test]
fn native_registration_keeps_its_original_host_session_and_project_on_replay() {
    let fixture = RunFixture::new("native-registration-origin");
    let session = owner(&fixture);
    let host = bind_native_host(&fixture);
    let creation = {
        let inner = fixture.state.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&session).unwrap()]
            .federation_native_creation_id
            .clone()
    };
    let run_id = "test-native-registration-origin";
    let directory = registered_run(&fixture, &session, run_id);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    let original = fixture.state.inner.lock().unwrap().test_run_registrations[run_id].clone();
    let origin = original
        .native_origin
        .as_ref()
        .expect("registration captured native provenance");
    assert_eq!(origin.host, host);
    assert_eq!(origin.native_session_id, session);
    assert_eq!(origin.native_creation_id, creation);
    assert_eq!(origin.project_id, fixture.project_id);
    assert_eq!(origin.project_scope, None);
    assert!(original.verification_record.is_none());
    assert_eq!(
        serde_json::from_value::<TestRunRegistration>(serde_json::to_value(&original).unwrap())
            .unwrap(),
        original
    );
    let reloaded = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert_eq!(
        reloaded.test_run_registrations[run_id].native_origin,
        original.native_origin
    );

    // Use the existing continuity owner to advance the durable host generation.
    // A replay must not bless that generation or a replacement native session
    // as the original run's provenance.
    fixture.state.reset_federation_continuity(&host).unwrap();
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        assert!(inner.federation.local.as_ref().unwrap().host_generation > host.host_generation);
        let index = inner.find_session_index(&session).unwrap();
        inner.sessions[index].federation_native_creation_id = Uuid::new_v4().to_string();
    }
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_registrations[run_id],
        original
    );
}

#[test]
fn native_registration_retains_validated_logical_scope_without_exposing_store_path() {
    let fixture = RunFixture::new("native-registration-project-scope");
    let session = owner(&fixture);
    bind_native_host(&fixture);
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        let project = inner
            .projects
            .iter_mut()
            .find(|project| project.id == fixture.project_id)
            .unwrap();
        project.engram = Some(EngramProjectSettings {
            authority_store_key: Some(EngramAuthorityStoreKey {
                database_path: fixture.root.join("private-store.db"),
                project_id: "example.test/owner/repository".to_owned(),
            }),
            ..EngramProjectSettings::default()
        });
    }
    let run_id = "test-native-registration-project-scope";
    let directory = registered_run(&fixture, &session, run_id);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    let inner = fixture.state.inner.lock().unwrap();
    let registration = &inner.test_run_registrations[run_id];
    let origin = registration.native_origin.as_ref().unwrap();
    assert_eq!(
        origin.project_scope.as_deref(),
        Some("example.test/owner/repository")
    );
    let wire = serde_json::to_value(origin).unwrap();
    assert!(!wire.to_string().contains("private-store.db"));
    assert!(wire.get("databasePath").is_none());
    assert!(registration.verification_record.is_none());
}

#[test]
fn legacy_registration_cannot_acquire_native_provenance_after_the_fact() {
    let fixture = RunFixture::new("native-registration-legacy");
    let session = owner(&fixture);
    bind_native_host(&fixture);
    let run_id = "test-native-registration-legacy";
    let directory = registered_run(&fixture, &session, run_id);
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    let original = fixture.state.inner.lock().unwrap().test_run_registrations[run_id].clone();
    assert!(original.native_origin.is_some());
    let mut legacy = serde_json::to_value(&original).unwrap();
    legacy.as_object_mut().unwrap().remove("nativeOrigin");
    let reloaded: TestRunRegistration = serde_json::from_value(legacy).unwrap();
    assert!(reloaded.native_origin.is_none());
    // Model a row written by an older build, without changing the store's
    // actual native identity or bypassing its persistence guards.
    {
        let mut inner = fixture.state.inner.lock().unwrap();
        inner
            .test_run_registrations
            .insert(run_id.to_owned(), reloaded.clone());
        fixture.state.persist_internal_locked(&inner).unwrap();
    }
    let restored = load_state(fixture.state.persistence_path.as_path())
        .unwrap()
        .unwrap();
    assert!(
        restored.test_run_registrations[run_id]
            .native_origin
            .is_none()
    );
    fixture
        .state
        .register_test_run(&session, registration_request(run_id, &directory))
        .unwrap();
    assert_eq!(
        fixture.state.inner.lock().unwrap().test_run_registrations[run_id],
        reloaded
    );
}
