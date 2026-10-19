// Production store witnesses; the sibling frozen journal tests cover physical
// prerequisites. Injected-return cuts and abrupt process exits are separate
// witnesses. Neither simulates power loss or remote/runtime integration.
use super::*;
use std::time::Instant;

fn operation(key: &str) -> FederationExecutorOperation {
    FederationExecutorOperation {
        central_host_id: "central-fixture".into(), central_epoch: 7,
        operation_key: key.into(), target_host_id: "satellite-fixture".into(),
        enrollment_generation: 3, session_id: "session-fixture".into(),
        creation_id: "creation-fixture".into(), placement_generation: 5,
        command_kind: "wake".into(), payload: "retained wake payload".into(),
    }
}

fn receive(store: &MailboxStore, request: &FederationExecutorOperation) -> FederationExecutorRecord {
    store.admit_federation_executor_operation(request, || Ok(())).unwrap()
}

fn assert_kind<T: std::fmt::Debug>(result: Result<T>, kind: MailboxStoreErrorKind) {
    let error = result.unwrap_err();
    assert_eq!(error.downcast_ref::<MailboxStoreError>().unwrap().kind, kind);
}

fn wait_for_executor_child(mut child: Child, phase: &str) -> std::process::Output {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("executor child wait failed at {phase}: {error}");
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("executor child exceeded its guard at {phase}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn federation_m4_store_duplicate_conflicts_and_lost_acceptance_response() {
    let root = TestTempRoot::create("executor-store-dedup");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    let request = operation("one");
    FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some("afterReceiveCommit"));
    assert!(store.admit_federation_executor_operation(&request, || Ok(())).is_err());
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    let original = receive(&reopened, &request);
    assert_eq!(original.stage, "received");
    assert_eq!(receive(&reopened, &request), original);
    for field in 0..9 {
        let mut changed = request.clone();
        match field {
            0 => changed.payload.push('!'), 1 => changed.central_epoch += 1,
            2 => changed.target_host_id.push('!'), 3 => changed.enrollment_generation += 1,
            4 => changed.session_id.push('!'), 5 => changed.creation_id.push('!'),
            6 => changed.placement_generation += 1, 7 => changed.command_kind = "stop".into(),
            _ => changed.payload = "different".into(),
        }
        assert_kind(reopened.admit_federation_executor_operation(&changed, || Ok(())),
            MailboxStoreErrorKind::Conflict);
    }
    assert_eq!(reopened.pending_federation_executor_operations("central-fixture", "", 100)
        .unwrap(), vec![original]);
}

#[test]
fn federation_m4_store_before_commit_and_authority_failure_leave_no_acceptance() {
    let root = TestTempRoot::create("executor-store-before-commit");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    let request = operation("one");
    FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some("beforeReceiveCommit"));
    assert!(store.admit_federation_executor_operation(&request, || Ok(())).is_err());
    assert!(store.admit_federation_executor_operation(&request,
        || -> Result<()> { bail!("authority changed before commit") }).is_err());
    drop(store);
    let reopened = MailboxStore::open(&path).unwrap();
    assert!(reopened.pending_federation_executor_operations("central-fixture", "", 100)
        .unwrap().is_empty());
    let original = receive(&reopened, &request);
    assert!(reopened.admit_federation_executor_operation(&request,
        || -> Result<()> { bail!("duplicate also requires current authority") }).is_err());
    assert_eq!(receive(&reopened, &request), original);
}

#[test]
fn federation_m4_store_concurrent_connections_return_one_identical_operation() {
    let root = TestTempRoot::create("executor-store-concurrency");
    let path = root.path().join("coordination.sqlite");
    let left = MailboxStore::open(&path).unwrap();
    let right = MailboxStore::open(&path).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for store in [left, right] {
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            receive(&store, &operation("one"))
        }));
    }
    let results: Vec<_> = threads.into_iter().map(|thread| thread.join().unwrap()).collect();
    assert_eq!(results[0], results[1]);
    let reopened = MailboxStore::open(&path).unwrap();
    assert_eq!(reopened.pending_federation_executor_operations("central-fixture", "", 100)
        .unwrap().len(), 1);
}

#[test]
fn federation_m4_store_evidence_cuts_and_processed_ack_keep_unknown_effect() {
    let root = TestTempRoot::create("executor-store-evidence");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    let request = operation("one");
    let received = receive(&store, &request);
    FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some("beforeEvidenceCommit"));
    assert!(store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::Queued { queue_entry_id: "queue".into(),
            delivery_attempt_id: "attempt".into() }, || Ok(())).is_err());
    assert_eq!(receive(&store, &request), received);
    store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::Queued { queue_entry_id: "queue".into(),
            delivery_attempt_id: "attempt".into() }, || Ok(())).unwrap();
    FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some("afterEvidenceCommit"));
    assert!(store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: "attempt".into() },
        || Ok(())).is_err());
    drop(store);
    let store = MailboxStore::open(&path).unwrap();
    let processed = store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
            evidence: "validated-central-receipt".into() }, || Ok(())).unwrap();
    assert_eq!(processed.stage, "processed");
    assert_eq!(processed.effect_state, "unknown");
    assert!(processed.runtime_ack.is_none());
    assert_eq!(store.pending_federation_executor_operations("central-fixture", "", 100)
        .unwrap(), vec![processed]);
    assert_kind(store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id: "other-attempt".into(),
            acknowledgement: "ack".into() }, || Ok(())), MailboxStoreErrorKind::Conflict);
    let accepted = store.record_federation_executor_evidence(&request,
        FederationExecutorEvidence::RuntimeAccepted { delivery_attempt_id: "attempt".into(),
            acknowledgement: "ack".into() }, || Ok(())).unwrap();
    assert_eq!(accepted.stage, "processed", "later evidence cannot erase processing");
    assert_eq!(accepted.effect_state, "confirmed");
    assert!(store.pending_federation_executor_operations("central-fixture", "", 100)
        .unwrap().is_empty());
}

#[test]
fn federation_m4_store_ack_without_queue_and_receipt_pruning_do_not_erase_dedup() {
    let root = TestTempRoot::create("executor-store-retention");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    for index in 0..70 {
        let request = operation(&format!("operation-{index}"));
        receive(&store, &request);
        let processed = store.record_federation_executor_evidence(&request,
            FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
                evidence: format!("validated-central-receipt-{index}") }, || Ok(())).unwrap();
        assert!(processed.queue_entry_id.is_none());
        assert!(processed.runtime_ack.is_none());
    }
    drop(store);
    let store = MailboxStore::open(&path).unwrap();
    assert_eq!(receive(&store, &operation("operation-0")).stage, "processed");
    let mut changed = operation("operation-0");
    changed.payload.push('!');
    assert_kind(store.admit_federation_executor_operation(&changed, || Ok(())),
        MailboxStoreErrorKind::Conflict);
    assert_eq!(store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get::<_, u64>(0))
        .unwrap(), 70);
}

#[test]
fn federation_m4_store_capacity_refuses_before_acceptance_and_keeps_exact_duplicates() {
    let root = TestTempRoot::create("executor-store-capacity");
    let store = MailboxStore::open(&root.path().join("coordination.sqlite")).unwrap();
    let request = operation("one");
    let original = receive(&store, &request);
    // Seed pressure only; the boundary under test is the production API below.
    store.connection().unwrap().execute(
        "WITH RECURSIVE slots(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM slots WHERE n<?1)
         INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
           target_host_id,enrollment_generation,session_id,creation_id,placement_generation,
           command_kind,payload_digest,payload,payload_bytes,stage,effect_state)
         SELECT central_host_id,'pressure-'||n,central_epoch,target_host_id,enrollment_generation,
           session_id,creation_id,placement_generation,command_kind,payload_digest,payload,
           payload_bytes,'received','unknown' FROM federation_executor_operations,slots
         WHERE operation_key='one'", [FEDERATION_EXECUTOR_MAX_COUNT-1],
    ).unwrap();
    assert_kind(store.admit_federation_executor_operation(&operation("overflow"), || Ok(())),
        MailboxStoreErrorKind::Capacity);
    assert_eq!(receive(&store, &request), original);
    let pending = store.pending_federation_executor_operations("central-fixture", "", usize::MAX).unwrap();
    assert_eq!(pending.len(), 100);
    assert_eq!(store.connection().unwrap().query_row(
        "SELECT COUNT(*) FROM federation_executor_operations", [], |row| row.get::<_, u64>(0))
        .unwrap(), FEDERATION_EXECUTOR_MAX_COUNT);
    let mut oversized = operation("oversized");
    oversized.payload = "x".repeat(FEDERATION_EXECUTOR_MAX_PAYLOAD+1);
    assert_kind(store.admit_federation_executor_operation(&oversized, || Ok(())),
        MailboxStoreErrorKind::Validation);
}

#[test]
fn federation_m4_store_version_three_migration_retains_delivery_rows() {
    let root = TestTempRoot::create("executor-store-v3-upgrade");
    let path = root.path().join("coordination.sqlite");
    let old = rusqlite::Connection::open(&path).unwrap();
    old.execute_batch(COORDINATION_SCHEMA_V1_SQL).unwrap();
    old.execute_batch(MAILBOX_READ_PAGES_SCHEMA_SQL).unwrap();
    old.execute_batch(FEDERATION_MAILBOX_DELIVERY_SCHEMA_SQL).unwrap();
    old.execute_batch("INSERT INTO meta VALUES('coordination_schema_version','3');
        INSERT INTO federation_mailbox_retired_namespaces VALUES('retired-sender','retired');
        INSERT INTO federation_mailbox_dedup VALUES('sender-host','sender','key','target',
          'digest','homeCommitted','message',1);
        INSERT INTO federation_mailbox_intents VALUES('sender','key','sender-host','owner','creation',
          'target','digest','{}','appendOutcomeUnknown','payload',7,NULL,NULL,NULL);")
        .unwrap();
    drop(old);
    let connection = bootstrap_coordination_database(&path).unwrap();
    validate_current_coordination_schema(&connection).unwrap();
    for table in ["federation_mailbox_intents", "federation_mailbox_dedup",
        "federation_mailbox_retired_namespaces"]
    {
        assert_eq!(connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [],
            |row| row.get::<_, u64>(0)).unwrap(), 1);
    }
    assert_eq!(connection.query_row("SELECT payload FROM federation_mailbox_intents", [],
        |row| row.get::<_, String>(0)).unwrap(), "payload");
}

#[test]
fn federation_m4_store_version_three_guard_refuses_upgrade_without_rewriting_rows() {
    let root = TestTempRoot::create("executor-store-old-guard");
    let path = root.path().join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    let original = receive(&store, &operation("one"));
    let connection = store.connection().unwrap();
    let schema = coordination_schema_objects(&connection).unwrap();
    // This is the unchanged pre-increment schema guard, not execution of an
    // old TermAl binary. The latter remains a separate pinned rollback check.
    let error = validate_coordination_schema_version(&connection, "3").unwrap_err();
    assert!(error.to_string().contains("unsupported coordination database schema"));
    assert_eq!(coordination_schema_objects(&connection).unwrap(), schema);
    assert_eq!(federation_executor_read(&connection, "central-fixture", "one").unwrap(), Some(original));
    assert_eq!(connection.query_row("SELECT value FROM meta WHERE key='coordination_schema_version'",
        [], |row| row.get::<_, String>(0)).unwrap(), "4");
}

#[test]
fn federation_m4_store_process_exit_before_and_after_commit_recovers_original_operation() {
    const DATABASE: &str = "TERMAL_EXECUTOR_CRASH_DATABASE";
    const PHASE: &str = "TERMAL_EXECUTOR_CRASH_PHASE";
    if let (Some(database), Ok(phase)) = (std::env::var_os(DATABASE), std::env::var(PHASE)) {
        let phase = match phase.as_str() {
            "beforeReceiveCommit" => "beforeReceiveCommit",
            "afterReceiveCommit" => "afterReceiveCommit",
            _ => panic!("unknown child crash phase"),
        };
        let store = MailboxStore::open(FsPath::new(&database)).unwrap();
        FEDERATION_EXECUTOR_TEST_CUT.with(|cut| *cut.borrow_mut() = Some(phase));
        FEDERATION_EXECUTOR_TEST_CRASH.with(|crash| crash.set(true));
        receive(&store, &operation("one"));
        panic!("expected abrupt process exit at the production commit boundary");
    }
    for (phase, expected) in [("beforeReceiveCommit", 0), ("afterReceiveCommit", 1)] {
        let root = TestTempRoot::create("executor-store-process-exit");
        let path = root.path().join("coordination.sqlite");
        let test_module = module_path!().split_once("::").unwrap().1;
        let filter = format!("{test_module}::federation_m4_store_process_exit_before_and_after_commit_recovers_original_operation");
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact").arg(&filter).arg("--nocapture")
            .env(DATABASE, &path).env(PHASE, phase)
            .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        let output = wait_for_executor_child(child, phase);
        assert_eq!(output.status.code(), Some(73), "{phase}: {}",
            String::from_utf8_lossy(&output.stderr));
        let store = MailboxStore::open(&path).unwrap();
        assert_eq!(store.pending_federation_executor_operations("central-fixture", "", 100)
            .unwrap().len(), expected, "{phase}");
        let recovered = receive(&store, &operation("one"));
        assert_eq!(receive(&store, &operation("one")), recovered);
        assert_eq!(store.pending_federation_executor_operations("central-fixture", "", 100)
            .unwrap().len(), 1);
    }
}

#[test]
fn federation_m4_store_byte_capacity_is_independent_of_row_capacity() {
    let root = TestTempRoot::create("executor-store-byte-capacity");
    let store = MailboxStore::open(&root.path().join("coordination.sqlite")).unwrap();
    let mut request = operation("one");
    request.payload = "x".repeat(FEDERATION_EXECUTOR_MAX_PAYLOAD);
    let original = receive(&store, &request);
    let rows = FEDERATION_EXECUTOR_MAX_BYTES / FEDERATION_EXECUTOR_MAX_PAYLOAD as u64;
    assert!(rows < FEDERATION_EXECUTOR_MAX_COUNT);
    // Valid bounded pressure fixture; admission/refusal remains production code.
    store.connection().unwrap().execute(
        "WITH RECURSIVE slots(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM slots WHERE n<?1)
         INSERT INTO federation_executor_operations(central_host_id,operation_key,central_epoch,
           target_host_id,enrollment_generation,session_id,creation_id,placement_generation,
           command_kind,payload_digest,payload,payload_bytes,stage,effect_state)
         SELECT central_host_id,'bytes-'||n,central_epoch,target_host_id,enrollment_generation,
           session_id,creation_id,placement_generation,command_kind,payload_digest,payload,
           payload_bytes,'received','unknown' FROM federation_executor_operations,slots
         WHERE operation_key='one'", [rows-1],
    ).unwrap();
    assert_kind(store.admit_federation_executor_operation(&operation("overflow"), || Ok(())),
        MailboxStoreErrorKind::Capacity);
    assert_eq!(receive(&store, &request), original);
}

#[test]
fn federation_m4_store_modified_schema_is_refused_without_repair_or_data_loss() {
    let root = TestTempRoot::create("executor-store-schema-refusal");
    let path = root.path().join("coordination.sqlite");
    {
        let store = MailboxStore::open(&path).unwrap();
        receive(&store, &operation("one"));
        store.connection().unwrap().execute_batch(
            "DROP TRIGGER federation_executor_identity_immutable;").unwrap();
    }
    assert!(bootstrap_coordination_database(&path).is_err());
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM federation_executor_operations", [],
        |row| row.get::<_, u64>(0)).unwrap(), 1);
    assert_eq!(connection.query_row("SELECT COUNT(*) FROM sqlite_schema
        WHERE name='federation_executor_identity_immutable'", [],
        |row| row.get::<_, u64>(0)).unwrap(), 0);
}

#[test]
fn federation_m4_store_command_outcome_is_not_a_mailbox_ack_or_runtime_ack() {
    let root = TestTempRoot::create("executor-store-command-evidence");
    let store = MailboxStore::open(&root.path().join("coordination.sqlite")).unwrap();
    for command in ["start", "stop"] {
        let mut request = operation(command);
        request.command_kind = command.into();
        receive(&store, &request);
        store.record_federation_executor_evidence(&request,
            FederationExecutorEvidence::Queued { queue_entry_id: command.into(),
                delivery_attempt_id: command.into() }, || Ok(())).unwrap();
        store.record_federation_executor_evidence(&request,
            FederationExecutorEvidence::EffectAttempt { delivery_attempt_id: command.into() },
            || Ok(())).unwrap();
        assert_kind(store.record_federation_executor_evidence(&request,
            FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::MailboxAck,
                evidence: "receipt-is-not-command-outcome".into() }, || Ok(())),
            MailboxStoreErrorKind::Conflict);
        let processed = store.record_federation_executor_evidence(&request,
            FederationExecutorEvidence::Processed { kind: FederationExecutorProcessedKind::CommandOutcome,
                evidence: format!("validated-{command}-outcome") }, || Ok(())).unwrap();
        assert_eq!(processed.stage, "processed");
        assert_eq!(processed.processed_evidence_kind.as_deref(), Some("command_outcome"));
        assert_eq!(processed.effect_state, "unknown");
        assert!(processed.runtime_ack.is_none());
        assert!(store.pending_federation_executor_operations("central-fixture", "", 100)
            .unwrap().iter().any(|row| row.operation == request));
    }
}

#[test]
fn federation_m4_store_recovery_does_not_adopt_locally_woken_central_obligations() {
    let state = federation_mailboxes::central_store();
    let (sender, _) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Central native");
    let receipt: MailboxAppendReceipt = serde_json::from_value(state
        .execute_federation_mailbox(federation_mailboxes::request(&state, &sender,
            FederationMailboxOperation::Send {
                target: target.session_id.clone(), message: "native wake".into(),
                key: "native-only".into(), topic: None, state_stamp: None,
            }), None).unwrap().value).unwrap();
    let connection = state.mailbox_store.connection().unwrap();
    let before: (String, String, String) = connection.query_row(
        "SELECT target_session_id,target_binding_json,state
         FROM federation_mailbox_wake_obligations WHERE message_id=?1",
        [&receipt.message_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
    let binding: FederationMailboxBinding = serde_json::from_str(&before.1).unwrap();
    assert_eq!(binding.identity.host_id, binding.central.host_id);
    assert_eq!(before.2, "pending");
    // Retain the local-dispatch witness while M3's obligation is still pending.
    // Executor recovery may neither drain it nor create remote ownership for it.
    connection.execute("UPDATE mailbox_messages SET dispatch_outcome='queuedBehindActiveTurn'
        WHERE id=?1", [&receipt.message_id]).unwrap();
    drop(connection);
    let path = resolve_coordination_persistence_path(state.persistence_path.as_path());
    let store = MailboxStore::open(&path).unwrap();
    assert!(store.pending_federation_executor_operations(
        &binding.central.host_id, "", 100).unwrap().is_empty());
    let request = operation("unrelated-remote-operation");
    receive(&store, &request);
    assert_eq!(store.pending_federation_executor_operations(
        "central-fixture", "", 100).unwrap().len(), 1);
    assert!(store.pending_federation_executor_operations(
        &binding.central.host_id, "", 100).unwrap().is_empty());
    let connection = store.connection().unwrap();
    let after = connection.query_row(
        "SELECT target_session_id,target_binding_json,state
         FROM federation_mailbox_wake_obligations WHERE message_id=?1",
        [&receipt.message_id], |row| Ok((row.get::<_, String>(0)?,
            row.get::<_, String>(1)?, row.get::<_, String>(2)?))).unwrap();
    assert_eq!(after, before);
    assert_eq!(connection.query_row("SELECT dispatch_outcome FROM mailbox_messages WHERE id=?1",
        [&receipt.message_id], |row| row.get::<_, String>(0)).unwrap(), "queuedBehindActiveTurn");
}

// Deliberately separate from the ordinary gate: this needs the installed
// pre-increment production binary and its immutable source/hash pin. Run
// explicitly with --ignored after provisioning; an ignored test is no evidence.
#[cfg(windows)]
#[test]
#[ignore = "requires the pinned pre-increment binary; a separate rollback check is mandatory"]
fn federation_m4_store_pinned_previous_binary_refuses_without_mailbox_data_loss() {
    use std::io::Read;
    let pin_path = FsPath::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp/federation-executor-rollback-pin.json");
    let pin: Value = serde_json::from_slice(&fs::read(&pin_path)
        .expect("provision the pinned installed previous binary first")).unwrap();
    let binary = FsPath::new(pin["binary"].as_str().unwrap());
    let repository = FsPath::new(env!("CARGO_MANIFEST_DIR"));
    let main_checkout = repository.parent().unwrap().parent().unwrap();
    let expected_copy = main_checkout.join(".evidence/federation-m4-store-12326/pre-m4-installed-binary/termal-8ebf2f8e-deff537d.exe");
    assert_eq!(binary, expected_copy);
    let binary = fs::canonicalize(binary).unwrap();
    assert!(binary.starts_with(fs::canonicalize(main_checkout).unwrap().join(".evidence")),
        "the rollback child must use the evidence copy, never the installed executable");
    let expected_hash = "deff537d093c7e264bd3edb4ea1bdecaa0522a46f85fe4f6f511e2509e7bf7a8";
    assert_eq!(pin["sha256"], expected_hash);
    assert_eq!(pin["sourceCommit"], "8ebf2f8e6798f82021bc91daa01ed72f176a30fd");
    let mut file = fs::File::open(&binary).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 41382912);
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 1024*1024];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 { break; }
        digest.update(&buffer[..count]);
    }
    assert_eq!(format!("{:x}", digest.finalize()), expected_hash);
    let _directory = TestTempRootDirectoryScope::repository_local();
    let root = TestTempRoot::create("executor-store-pinned-rollback");
    assert!(fs::canonicalize(root.path()).unwrap()
        .starts_with(fs::canonicalize(repository).unwrap().join(".tmp")));
    let isolated_home = root.path().join("isolated-home");
    let data_dir = isolated_home.join(".termal");
    fs::create_dir_all(&data_dir).unwrap();
    let empty_path = isolated_home.join("empty-bin");
    let scratch = isolated_home.join("scratch");
    fs::create_dir(&empty_path).unwrap();
    fs::create_dir(&scratch).unwrap();
    let path = data_dir.join("coordination.sqlite");
    let store = MailboxStore::open(&path).unwrap();
    let append = store.append(&MailboxAppendInput {
        sender_session_id: "sender".into(), sender_name: "Sender".into(),
        target_session_id: "target".into(), target_name: "Target".into(),
        body: "original mailbox content".into(), idempotency_key: "key".into(),
        topic: None, state_stamp: None,
    }).unwrap();
    store.record_initial_dispatch_outcome(&append.receipt.message_id, "durableButNotWoken").unwrap();
    drop(append.finalization);
    let page = store.read_range_authorized("target", &append.receipt.mailbox_id,
        None, 10, true, || Ok(())).unwrap();
    let receipt = page.receipt.unwrap();
    // Boot fails at the existing coordination-schema barrier, before any
    // HTTP listener, runtime recovery or persist worker starts. Every path
    // belongs to this disposable fixture; no installed host is contacted.
    let mut rollback_child = Command::new(binary);
    // The guarded Windows command exposes explicit per-name removal, not
    // mutable access to the underlying command. Keep guarded process creation.
    for (name, _) in std::env::vars_os() {
        rollback_child.env_remove(name);
    }
    let child = rollback_child
        .current_dir(root.path())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW; this test is Windows-only.
        .env("SystemRoot", std::env::var_os("SystemRoot").expect("Windows system directory"))
        .env("HOME", &isolated_home).env("USERPROFILE", &isolated_home)
        .env("APPDATA", isolated_home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", isolated_home.join("AppData/Local"))
        .env("TEMP", &scratch).env("TMP", &scratch).env("TMPDIR", &scratch)
        .env("PATH", &empty_path)
        .env("CODEX_HOME", isolated_home.join(".codex"))
        .env("TERMAL_PORT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let output = wait_for_executor_child(child, "pinned previous schema opener");
    assert!(!output.status.success(), "old binary must refuse the v4 database");
    let diagnostic = format!("{}{}", String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    assert!(diagnostic.contains("unsupported coordination database schema"), "{diagnostic}");
    assert!(diagnostic.contains("coordination.sqlite"), "{diagnostic}");
    assert!(!diagnostic.contains("listening:"), "refusal must precede serving: {diagnostic}");
    let connection = store.connection().unwrap();
    validate_current_coordination_schema(&connection).unwrap();
    assert_eq!(connection.query_row("SELECT body FROM mailbox_messages WHERE id=?1",
        [&append.receipt.message_id], |row| row.get::<_, String>(0)).unwrap(),
        "original mailbox content");
    assert_eq!(connection.query_row("SELECT receipt FROM mailbox_read_pages WHERE receipt=?1",
        [&receipt], |row| row.get::<_, String>(0)).unwrap(), receipt);
    assert_eq!(mailbox_processed_through(&connection, &append.receipt.mailbox_id, "target").unwrap(), 0);
}
