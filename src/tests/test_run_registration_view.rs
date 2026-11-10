// Real router, SQLite registration fence and shipped Node adapter witness for
// large source identities. Included beside the existing owned HistoryService;
// no live store, source capture, stage execution or verification credit.

#[test]
fn large_source_registration_uses_the_bounded_view_and_keeps_full_detail() {
    let _placement = TestTempRootDirectoryScope::repository_local();
    let (service, root, owner, _) = clock_service("bounded-view");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut confirmation_sizes = Vec::new();
        for (case, count) in [("small", 1), ("large", 1024)] {
            let (id, directory) = clock_registration_request(&root, &owner, case);
            let request_path = directory.join("request.json");
            let mut request: Value = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
            request["detached"] = json!(false);
            let mut source = launcher_source_id_fixture();
            source["changes"] = Value::Array((0..count).map(|index| json!({
                "path": format!("changed-{index:04}.rs"), "status": " M", "origin": null,
                "blob": "c".repeat(40), "mode": "100644"
            })).collect());
            if count > 1 {
                assert!(serde_json::to_vec(&source).unwrap().len() > 64 * 1024);
            }
            request["sourceId"] = source.clone();
            fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
            fs::write(directory.join("results.json"), serde_json::to_vec(&json!({
                "state": "running", "stages": [], "sourceIdBefore": source,
                "sourceIdAfter": source
            })).unwrap()).unwrap();
            let state = service.state.as_ref().unwrap();
            // Index before registration: the real adapter's POST must still
            // establish durable ownership, then confirm through the new view.
            state.refresh_test_runs_with(&|_, _| true, &|_, _| {});
            let mut command = BoundedReadCommand::new("node");
            command.current_dir(env!("CARGO_MANIFEST_DIR"))
                .arg(FsPath::new(env!("CARGO_MANIFEST_DIR")).join("scripts/test-registration-view-fixture.mjs"))
                .arg(&service.url).arg(&owner).arg(&directory);
            let output = run_bounded_read_command(&mut command,
                std::time::Instant::now() + phase_sync::DEADLOCK_GUARD, 16 * 1024, true)
                .expect("adapter fixture must complete");
            assert!(output.status.success(), "adapter {case}: {}", String::from_utf8_lossy(&output.stderr));
            assert!(String::from_utf8_lossy(&output.stdout).contains("REGISTRATION_VIEW_CONFIRMED"));

            let client = reqwest::blocking::Client::builder()
                .timeout(phase_sync::DEADLOCK_GUARD).build().unwrap();
            let full_bytes = client.get(format!("{}/api/test-runs/{id}", service.url))
                .send().unwrap().error_for_status().unwrap().bytes().unwrap();
            let full: Value = serde_json::from_slice(&full_bytes).unwrap();
            assert_eq!(full["detail"]["sourceId"], source);
            assert_eq!(full["detail"]["sourceIdBefore"], source);
            assert_eq!(full["detail"]["sourceIdAfter"], source);
            if count > 1 { assert!(full_bytes.len() > 64 * 1024); }
            let bytes = client.get(format!("{}/api/test-runs/{id}?view=registration", service.url))
                .send().unwrap().error_for_status().unwrap().bytes().unwrap();
            let confirmation: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(confirmation["source"], full["source"]);
            assert_eq!(confirmation.as_object().unwrap().len(), 2);
            assert!(confirmation.get("detail").is_none());
            for key in ["runId", "ownerSessionId", "initiatingSessionId", "registrationState",
                "trackingDurability", "trackingProvenances", "creatorPid", "startedAt",
                "detached", "runDir", "worktree"] {
                assert_eq!(confirmation["run"][key], full["run"][key], "{case}:{key}");
            }
            assert!(bytes.len() < 4096, "confirmation is metadata, not source data");
            confirmation_sizes.push(bytes.len());
            assert_eq!(fs::read(&request_path).unwrap(), serde_json::to_vec(&request).unwrap());
            assert!(state.inner.lock().unwrap().test_run_registrations.contains_key(&id));
            if count > 1 {
                let results_path = directory.join("results.json");
                let saved = fs::read(&results_path).unwrap();
                fs::write(&results_path, b"{").unwrap();
                assert!(!client.get(format!("{}/api/test-runs/{id}", service.url))
                    .send().unwrap().status().is_success());
                assert!(client.get(format!("{}/api/test-runs/{id}?view=registration", service.url))
                    .send().unwrap().status().is_success(),
                    "confirmation must not read broken stage/source detail");
                fs::write(results_path, saved).unwrap();
            }
        }
        // Run ids contain different small/large tags of equal length. No
        // changed-path count contributes bytes to this confirmation projection.
        assert_eq!(confirmation_sizes[0], confirmation_sizes[1]);
    }));
    finish_clock_service(service, outcome);
}
