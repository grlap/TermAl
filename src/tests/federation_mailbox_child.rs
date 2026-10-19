// Owns the satellite side of the native two-process central mailbox proof.
// Does not fake successful RPCs or replicate C's mailbox store. New companion
// to federation_mailbox_process.rs; all state uses the normal test home.
use super::federation_mailbox_process::*;
use super::*;

#[test]
fn satellite_child() {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return;
    }
    let base = federation_registry::domain_state();
    let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
    let app = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..base
    };
    let session = test_session_id(&app, Agent::Codex);
    let control_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let native = {
        let inner = app.inner.lock().unwrap();
        NativeSession {
            control_port: control_listener.local_addr().unwrap().port(),
            host: inner.federation.local.clone().unwrap(),
            session: session.clone(),
            creation: inner.sessions[inner.find_session_index(&session).unwrap()]
                .federation_native_creation_id
                .clone(),
        }
    };
    println!("M2_NATIVE {}", serde_json::to_string(&native).unwrap());
    std::io::stdout().flush().unwrap();
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let config: Configuration = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    app.configure_federation(FederationSettings {
        enabled: true,
        role: FederationRole::Satellite,
        central: Some(config.central.clone()),
    })
    .unwrap();
    let mut record = config.record;
    record.identity = config.central.clone();
    record.display_name = "Central".to_owned();
    record.record_generation = 1;
    record.transport_generation = 1;
    record.enrollment_generation = 1;
    record.membership = FederationMembershipState::Enrolled;
    record.verification = None;
    record.last_observed = None;
    app.enroll_federation_peer(record).unwrap();
    let snapshot = app
        .federation_peer_snapshot(&config.central.host_id)
        .unwrap();
    let channel = FederationChannel::for_app(
        snapshot,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let check = channel.clone();
    let rpc = {
        let _entered = runtime.enter();
        FederationRpcClient::new(
            config.port,
            channel.clone(),
            Arc::new(move || check.current_authority().map(|_| ())),
        )
        .unwrap()
    };
    app.federation_peer_handles.lock().unwrap().insert(
        config.central.host_id.clone(),
        Arc::new(FederationPeerHandle {
            channel: channel.clone(),
            rpc,
        }),
    );
    let (control_stop, control_task) = runtime.block_on(async {
        control_listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(control_listener).unwrap();
        let router = federation_router(channel.clone(), federation_link::allowed());
        let (stop, rx) = tokio::sync::watch::channel(false);
        let channel = channel.clone();
        let task = tokio::spawn(serve_federation_control_listener(
            listener, router, rx, channel,
        ));
        (stop, task)
    });
    let server_app = app.clone();
    let (stop, task, port) = runtime.block_on(async move {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            axum::serve(listener, app_router(server_app))
                .with_graceful_shutdown(async move { federation_wait_stop(&mut rx).await })
                .await
                .unwrap();
        });
        (stop, task, port)
    });
    println!("M2_STARTED {port}");
    std::io::stdout().flush().unwrap();
    let mut binding: Option<FederationMailboxBinding> = None;
    for line in lines {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let response = match command["command"].as_str().unwrap() {
            "inspectCentral" => {
                let peer = app
                    .federation_peer_snapshot(&config.central.host_id)
                    .unwrap();
                json!({"membership":peer.membership})
            }
            "revokeCentral" => {
                let peer = app
                    .federation_peer_snapshot(&config.central.host_id)
                    .unwrap();
                app.transition_federation_peer(&peer, FederationMembershipState::Revoked)
                    .unwrap();
                json!({"revoked":true})
            }
            "inspectIntents" => {
                let connection = app.mailbox_store.connection().unwrap();
                let (count, bytes): (u64, u64) = connection.query_row(
                    "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM federation_mailbox_intents
                     WHERE state IN ('localIntentDurable','appendOutcomeUnknown')",
                    [], |row| Ok((row.get(0)?, row.get(1)?)),
                ).unwrap();
                json!({"pending":count,"bytes":bytes,
                    "total":connection.query_row("SELECT COUNT(*) FROM federation_mailbox_intents",
                        [],|row|row.get::<_,u64>(0)).unwrap()})
            }
            "bind" => {
                let current = app
                    .federation_peer_snapshot(&config.central.host_id)
                    .unwrap();
                // Production keeps the satellite's accepted central Enrolled.
                // Verify/activate applies to the central's satellite record.
                assert_eq!(current.membership, FederationMembershipState::Enrolled);
                assert!(
                    channel.current_lease().is_ok(),
                    "real proof precedes placement binding"
                );
                let observed: FederationMailboxBinding =
                    serde_json::from_value(command["binding"].clone()).unwrap();
                app.install_federation_mailbox_binding(
                    &current,
                    observed.clone(),
                    &native.creation,
                )
                .unwrap();
                binding = Some(observed);
                json!({"bound":true})
            }
            "inspect" => {
                let directory = app.inner.lock().unwrap().federation.mailbox_directory.len();
                let connection = app.mailbox_store.connection().unwrap();
                json!({"directory":directory,
                    "mailboxes":connection.query_row("SELECT COUNT(*) FROM mailboxes",[],|r|r.get::<_,u64>(0)).unwrap(),
                    "messages":connection.query_row("SELECT COUNT(*) FROM mailbox_messages",[],|r|r.get::<_,u64>(0)).unwrap()})
            }
            "recreate" => {
                {
                    let mut inner = app.inner.lock().unwrap();
                    let index = inner.find_session_index(&session).unwrap();
                    inner.remove_session_at(index).unwrap();
                    inner.next_session_number =
                        session.trim_start_matches("session-").parse().unwrap();
                }
                assert_eq!(test_session_id(&app, Agent::Codex), session);
                let error = app
                    .install_federation_mailbox_binding(
                        &app.federation_peer_snapshot(&config.central.host_id)
                            .unwrap(),
                        binding.as_ref().unwrap().clone(),
                        &native.creation,
                    )
                    .unwrap_err();
                json!({"status":error.status.as_u16(),"bound":app.federation_mailbox_actor(&session).unwrap().is_some()})
            }
            "stop" => {
                println!("M2_RESULT {{\"stopped\":true}}");
                std::io::stdout().flush().unwrap();
                break;
            }
            _ => panic!("unexpected test command"),
        };
        println!("M2_RESULT {response}");
        std::io::stdout().flush().unwrap();
    }
    stop.send_replace(true);
    runtime.block_on(task).unwrap();
    control_stop.send_replace(true);
    runtime.block_on(control_task).unwrap().unwrap();
    app.federation_peer_handles.lock().unwrap().clear();
}
