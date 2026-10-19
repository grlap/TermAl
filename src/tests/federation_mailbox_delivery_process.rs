// Owns a killed native fixture at real completed durability boundaries.
// Does not substitute a rollback error for a process crash or provision SSH.
// New companion to delivery_crash tests; all stores use normal test homes.
use super::*;

const CHILD_ENV: &str = "TERMAL_TEST_FEDERATION_MAILBOX_CUT";
const CHILD_ROOT_ENV: &str = "TERMAL_TEST_FEDERATION_MAILBOX_CUT_ROOT";

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct CutReceipt {
    pub phase: String,
    pub path: PathBuf,
    pub sender: String,
    pub target: String,
}

pub(super) struct CrashChild {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    root: Option<TestTempRoot>,
}

impl CrashChild {
    pub fn spawn(phase: &str) -> (Self, CutReceipt) {
        // The process is deliberately killed, so only its surviving parent
        // can own cleanup. Every child fixture is contained inside this root.
        let root = TestTempRoot::create("termal-delivery-cut");
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::federation_mailbox_delivery_process::crash_cut_child",
                "--nocapture",
            ])
            .env(CHILD_ENV, phase)
            .env(CHILD_ROOT_ENV, root.path())
            .env_remove("TERMAL_BASE_URL")
            .env_remove("TERMAL_SESSION_ID")
            .env_remove("TERMAL_CLI")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let output = child.take_stdout().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let (tx, exit) = mpsc::channel();
        let waiting = child.clone();
        let waiter = std::thread::spawn(move || {
            let _ = tx.send(waiting.wait());
        });
        let process = Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            exit,
            waiter: Some(waiter),
            root: Some(root),
        };
        let began = Instant::now();
        let receipt = loop {
            let remaining = phase_sync::DEADLOCK_GUARD
                .checked_sub(began.elapsed())
                .expect("child crash-cut phase deadline");
            let line = process
                .lines
                .recv_timeout(remaining)
                .expect("child exited or stalled before its real durability phase");
            if let Some((_, json)) = line.split_once("DELIVERY_CUT ") {
                break serde_json::from_str::<CutReceipt>(json).unwrap();
            }
        };
        assert_eq!(receipt.phase, phase);
        assert!(receipt.path.starts_with(process.root.as_ref().unwrap().path()),
            "the child must use the parent's explicitly assigned fixture root");
        (process, receipt)
    }

    pub fn kill_at_cut(&mut self) {
        self.child.kill().unwrap();
        let status = self
            .exit
            .recv_timeout(phase_sync::DEADLOCK_GUARD)
            .unwrap()
            .unwrap();
        assert!(
            !status.success(),
            "fixture must be killed, not allowed to shut down cleanly"
        );
        self.input.take();
        self.join();
        // Keep the parent guard alive for the caller's crash/reopen assertions.
    }

    fn join(&mut self) {
        if let Some(waiter) = self.waiter.take() {
            waiter.join().unwrap();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

impl Drop for CrashChild {
    fn drop(&mut self) {
        self.input.take();
        if self.waiter.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.join();
        }
        if let Some(root) = self.root.take() {
            let path = root.path().to_path_buf();
            let marker = TestTempRoot::owner_marker(&path);
            drop(root);
            if !std::thread::panicking() {
                assert!(!path.exists(), "reaped crash child left its parent-owned fixture root");
                assert!(!marker.exists(), "reaped crash child left its owner marker");
            }
        }
    }
}

#[test]
fn crash_cut_child() {
    let Ok(phase) = std::env::var(CHILD_ENV) else {
        return;
    };
    let root = PathBuf::from(std::env::var_os(CHILD_ROOT_ENV).expect("parent-assigned child root"));
    assert!(root.is_absolute() && root.is_dir());
    // Confine the normal test-state constructor and its sibling owner markers
    // to the parent's guard. This thread-local scope changes no process env.
    let _placement = TestTempRootDirectoryScope {
        previous: TEST_TEMP_ROOT_DIRECTORY.with(|slot| slot.replace(Some(root))),
        _thread_bound: std::marker::PhantomData,
    };
    let state = federation_mailboxes::central_store();
    let (sender, actor) = federation_mailboxes::local_participant(&state, "Sender");
    let (_, target) = federation_mailboxes::local_participant(&state, "Target");
    let receipt = CutReceipt {
        phase: phase.clone(),
        path: resolve_coordination_persistence_path(state.persistence_path.as_path()),
        sender: actor.session_id.clone(),
        target: target.session_id.clone(),
    };
    let cut = match phase.as_str() {
        "beforeLocalDurability" => Some("beforeLocalDurability"),
        "beforeCentralCommit" => Some("beforeCentralCommit"),
        _ => None,
    };
    if let Some(cut) = cut {
        let receipt = receipt.clone();
        FEDERATION_DELIVERY_TEST_CUT.with(|slot| {
            *slot.borrow_mut() = Some((cut, Box::new(move || report_cut_and_park(&receipt))));
        });
    }
    match phase.as_str() {
        "beforeLocalDurability" => {
            let delivery = FederationDelivery {
                version: 1,
                sender: actor.binding.clone(),
                target: target.binding.clone(),
                key: "crash-key".into(),
                message: "crash body".into(),
                topic: None,
                state_stamp: None,
            };
            let creation = state.federation_intent_owner(&sender).unwrap();
            state
                .mailbox_store
                .prepare_federation_intent(&delivery, &sender, &creation, || {
                    state.lock_federation_intent_owner(&sender, &creation, &delivery.sender)
                })
                .unwrap();
            panic!("local durability hook did not stop the real transaction");
        }
        "afterLocalDurability" => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let (status, value) =
                runtime.block_on(federation_mailbox_intent_lifecycle::intent_api(
                    &state,
                    &sender,
                    "",
                    Some(federation_mailbox_intent_lifecycle::queued_body(
                        &target.session_id,
                        "crash-key",
                    )),
                ));
            assert_eq!(status, StatusCode::ACCEPTED);
            assert_eq!(value["state"], "localIntentDurable");
        }
        "afterLocalCapabilityRefusal" => {
            let delivery = FederationDelivery { version: 1, sender: actor.binding.clone(),
                target: target.binding.clone(), key: "crash-key".into(), message: "crash body".into(),
                topic: None, state_stamp: None };
            let creation = state.federation_intent_owner(&sender).unwrap();
            state.mailbox_store.prepare_federation_intent(&delivery, &sender, &creation, || {
                state.lock_federation_intent_owner(&sender, &creation, &delivery.sender)
            }).unwrap();
            let reason = state.mailbox_store.refuse_unattempted_federation_intent(
                &delivery, &sender, &creation, || {
                    state.lock_federation_intent_owner(&sender, &creation, &delivery.sender)
                },
            ).unwrap();
            assert!(matches!(reason, Some(FederationIntentRefusal::CentralCapabilityUnsupported)));
        }
        "beforeCentralCommit" | "afterCentralCommit" => {
            let request = federation_mailboxes::request(
                &state,
                &sender,
                FederationMailboxOperation::Send {
                    target: target.session_id.clone(),
                    message: "crash body".to_owned(),
                    key: "crash-key".to_owned(),
                    topic: None,
                    state_stamp: None,
                },
            );
            // This is the central receive path, not a sender acknowledgement.
            state.execute_federation_mailbox(request, None).unwrap();
            assert_ne!(
                phase, "beforeCentralCommit",
                "central hook must stop before commit"
            );
        }
        _ => panic!("unknown controlled crash phase"),
    }
    report_cut_and_park(&receipt);
}

fn report_cut_and_park(receipt: &CutReceipt) {
    println!("DELIVERY_CUT {}", serde_json::to_string(receipt).unwrap());
    std::io::stdout().flush().unwrap();
    // No destructor or graceful shutdown is allowed to establish durability.
    // The parent kills this process only after the real operation above returns.
    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
    panic!("crash fixture must be killed at the reported cut");
}
