// Host registration owns pin validation and durable replay. Scripted process
// output exercises that boundary without executing a fixture or a live store.
use super::*;
use crate::tests::engram_source_owner::{source_owner_fixture_binary, source_owner_reply};

fn configured_owner(fixture: &RunFixture) {
    let binary = source_owner_fixture_binary(&fixture.root);
    fixture.state.inner.lock().unwrap().projects.iter_mut()
        .find(|project| project.id == fixture.project_id).unwrap().engram = Some(EngramProjectSettings {
            binary_path: Some(binary.to_str().unwrap().into()), ..EngramProjectSettings::default()
        });
}

fn observed_owner(state: &AppState, session: &str, deadline: std::time::Instant)
    -> Result<(EngramSourceOwnerPin, EngramSourceOwnerAuthority), ApiError>
{
    state.source_owner_metadata_snapshot_with(session, deadline,
        |_, _, args, _, _| Ok(source_owner_reply(args)))
}

fn pinned_run(fixture: &RunFixture, session: &str, id: &str, pin: &EngramSourceOwnerPin) -> PathBuf {
    let directory = registered_run(fixture, session, id);
    let path = directory.join("request.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["sourceOwner"] = serde_json::to_value(pin).unwrap();
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    directory
}

#[test]
fn source_owner_registration_persists_verified_pin_and_replays_the_original_identity() {
    let fixture = RunFixture::new("source-pin-reg");
    let session = owner(&fixture);
    configured_owner(&fixture);
    let (pin, _) = observed_owner(&fixture.state, &session,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).unwrap();
    let id = "test-source-owner-pin";
    let directory = pinned_run(&fixture, &session, id, &pin);
    for _ in 0..2 {
        let response = fixture.state.register_test_run_inner_with_source_owner(&session,
            registration_request(id, &directory),
            &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id),
            |state, session, deadline| {
                assert!(deadline <= std::time::Instant::now() + TEST_RUN_REGISTRATION_DURABILITY_BUDGET);
                observed_owner(state, session, deadline)
            }).unwrap();
        assert!(response.registered);
        assert_eq!(fixture.state.inner.lock().unwrap().test_run_registrations[id].source_owner, Some(pin.clone()));
    }
    let loaded = load_state(fixture.state.persistence_path.as_path()).unwrap().unwrap();
    assert_eq!(loaded.test_run_registrations[id].source_owner.as_ref(), Some(&pin));
    let original = fixture.state.inner.lock().unwrap().test_run_registrations[id].clone();
    let original_bytes = fs::read(&pin.binary_path).unwrap();
    let mut upgraded = original_bytes.clone();
    upgraded.extend_from_slice(b"upgraded producer");
    fs::write(&pin.binary_path, upgraded).unwrap();
    let error = fixture.state.register_test_run_inner_with_source_owner(&session,
        registration_request(id, &directory),
        &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id), observed_owner)
        .err().unwrap();
    assert!(error.message.contains("pin does not match"), "{error:?}");
    assert_eq!(fixture.state.inner.lock().unwrap().test_run_registrations[id], original);
    fs::write(&pin.binary_path, original_bytes).unwrap();
    let path = directory.join("request.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("sourceOwner");
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    let error = fixture.state.register_test_run_inner_with_source_owner(&session,
        registration_request(id, &directory),
        &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id),
        |_, _, _| panic!("a legacy request must not acquire a source owner")).err().unwrap();
    assert_eq!(error.status, StatusCode::CONFLICT);
    assert_eq!(fixture.state.inner.lock().unwrap().test_run_registrations[id], original);
}

#[test]
fn source_owner_registration_refuses_pin_mismatch_and_configuration_race_before_publication() {
    let fixture = RunFixture::new("source-pin-race");
    let session = owner(&fixture);
    configured_owner(&fixture);
    let (pin, _) = observed_owner(&fixture.state, &session,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).unwrap();
    let mut wrong = pin.clone();
    wrong.sha256 = "0".repeat(64);
    let id = "test-source-owner-mismatch";
    let directory = pinned_run(&fixture, &session, id, &wrong);
    let error = fixture.state.register_test_run_inner_with_source_owner(&session,
        registration_request(id, &directory),
        &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id), observed_owner).err().unwrap();
    assert!(error.message.contains("pin does not match"), "{error:?}");
    assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(id));
    let id = "test-source-owner-race";
    let directory = pinned_run(&fixture, &session, id, &pin);
    let error = fixture.state.register_test_run_inner_with_source_owner(&session,
        registration_request(id, &directory),
        &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id),
        |state, session, deadline| {
            let observed = observed_owner(state, session, deadline)?;
            state.inner.lock().unwrap().projects.iter_mut().find(|p| p.id == fixture.project_id)
                .unwrap().engram.as_mut().unwrap().binary_path = Some("changed".into());
            Ok(observed)
        }).err().unwrap();
    assert!(error.message.contains("configuration changed before publication"), "{error:?}");
    assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(id));
}

#[test]
fn source_owner_request_is_closed_and_only_present_pins_change_legacy_identity() {
    let root = TestTempRoot::create("source-pin-identity");
    let pin = EngramSourceOwnerPin { binary_path: root.join("engram.exe").to_str().unwrap().into(),
        sha256: "a".repeat(64), binary_version: "engram fixture".into(), source_version: 1 };
    let legacy = json!({"runId":"test-owner-identity", "root":root.to_str().unwrap()});
    let original = test_run_registration_identity(&legacy).unwrap();
    assert!(test_run_request_source_owner(&legacy).unwrap().is_none());
    let mut request = legacy.clone();
    request["sourceOwner"] = serde_json::to_value(&pin).unwrap();
    assert_ne!(test_run_registration_identity(&request).unwrap(), original);
    assert_eq!(test_run_request_source_owner(&request).unwrap(), Some(pin));
    for (field, value) in [("binaryPath", json!("engram")), ("sha256", json!("A".repeat(64))),
        ("sourceVersion", json!(2)), ("binaryVersion", json!("unexpected")), ("extra", json!(true))] {
        let mut invalid = request.clone();
        invalid["sourceOwner"][field] = value;
        assert!(test_run_request_source_owner(&invalid).is_err(), "{field}");
    }
    request["sourceOwner"] = Value::Null;
    assert!(test_run_request_source_owner(&request).is_err());
}

#[test]
fn source_owner_registration_rechecks_binary_bytes_after_the_metadata_boundary() {
    let fixture = RunFixture::new("source-pin-bytes");
    let session = owner(&fixture);
    configured_owner(&fixture);
    let (pin, _) = observed_owner(&fixture.state, &session,
        std::time::Instant::now() + ENGRAM_SOURCE_COMMAND_BUDGET).unwrap();
    let id = "test-source-owner-replaced";
    let directory = pinned_run(&fixture, &session, id, &pin);
    let error = fixture.state.register_test_run_inner_with_source_owner(&session,
        registration_request(id, &directory),
        &mut TestRunRegistrationDiagnostics::new(&fixture.state.server_instance_id),
        |state, session, deadline| {
            let observed = observed_owner(state, session, deadline)?;
            fs::write(&pin.binary_path, b"replacement after metadata").unwrap();
            Ok(observed)
        }).err().unwrap();
    assert!(error.message.contains("no longer matches the pin"), "{error:?}");
    assert!(!fixture.state.inner.lock().unwrap().test_run_registrations.contains_key(id));
}
