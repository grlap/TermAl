// Runs the disposable native satellite. Commands arrange fixture state only;
// activation, placement and binding under test use production HTTP routes.
use super::federation_entrypoints_fixture::*;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn satellite_child() {
    if std::env::var(CHILD_ENV).as_deref() != Ok("1") {
        return;
    }
    let base = federation_registry::domain_state();
    let path = resolve_coordination_persistence_path(base.persistence_path.as_path());
    let mut app = AppState {
        mailbox_store: Arc::new(MailboxStore::open(&path).unwrap()),
        ..base
    };
    let writer_control = start_writer(&mut app);
    let session = test_session_id(&app, Agent::Codex);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let native = {
        let inner = app.inner.lock().unwrap();
        Native {
            host: inner.federation.local.clone().unwrap(),
            session: session.clone(),
            creation: inner.sessions[inner.find_session_index(&session).unwrap()]
                .federation_native_creation_id
                .clone(),
            port: listener.local_addr().unwrap().port(),
        }
    };
    println!("ENTRY_NATIVE {}", serde_json::to_string(&native).unwrap());
    std::io::stdout().flush().unwrap();
    let setup: Setup = tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    })
    .await
    .unwrap();
    writer_control.set_central_store(setup.central_state_path.clone());
    app.configure_federation(FederationSettings {
        enabled: true,
        role: FederationRole::Satellite,
        central: Some(setup.central.clone()),
    })
    .unwrap();
    let mut record = federation_registry::peer(setup.central_port);
    record.identity = setup.central.clone();
    record.profile = federation_onboarding_fixture::profile();
    record.platform = std::env::consts::OS.to_owned();
    record.arch = std::env::consts::ARCH.to_owned();
    record.endpoint.satellite_federation_port = native.port;
    record.endpoint.satellite_reverse_forward_port = setup.central_port;
    app.enroll_federation_peer(record).unwrap();
    let mut channel = FederationChannel::for_app(
        app.federation_peer_snapshot(&setup.central.host_id)
            .unwrap(),
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        app.clone(),
    );
    channel.control.clock = FederationControlClock::scripted();
    let hold_after_install = Arc::new(AtomicBool::new(false));
    let hold_active = Arc::new(AtomicBool::new(false));
    let held_commit = Arc::new(AtomicBool::new(false));
    let failed_postcheck = Arc::new(AtomicBool::new(false));
    let arm = hold_after_install.clone();
    let active = hold_active.clone();
    let committed = held_commit.clone();
    let failed = failed_postcheck.clone();
    let observed_app = app.clone();
    let observed_session = session.clone();
    channel.control.observed = Arc::new(move |event, status| {
        if event == "bindingInstalled" && arm.swap(false, Ordering::SeqCst) {
            let stored = load_state(observed_app.persistence_path.as_path())
                .unwrap()
                .unwrap();
            let index = stored.find_session_index(&observed_session).unwrap();
            committed.store(
                stored.sessions[index].federation_mailbox_binding.is_some(),
                Ordering::SeqCst,
            );
            observed_app.inner.lock().unwrap().federation_durable = false;
            active.store(true, Ordering::SeqCst);
        } else if event == "bindingPostcheck" && active.swap(false, Ordering::SeqCst) {
            failed.store(status != 0, Ordering::SeqCst);
            // The handler observed the hold, but signing sees current authority.
            observed_app.inner.lock().unwrap().federation_durable = true;
        }
    });
    let checked = channel.clone();
    let check: FederationAuthorityCheck = Arc::new(move || checked.current_authority().map(|_| ()));
    let rpc = FederationRpcClient::new(setup.central_port, channel.clone(), check.clone()).unwrap();
    app.federation_peer_handles.lock().unwrap().insert(
        setup.central.host_id.clone(),
        Arc::new(FederationPeerHandle {
            channel: channel.clone(),
            rpc: rpc.clone(),
        }),
    );
    let lose_reply = Arc::new(AtomicBool::new(false));
    let dropping = lose_reply.clone();
    let intent_durable = Arc::new(AtomicBool::new(false));
    let observing_intent = intent_durable.clone();
    let central_state_path = setup.central_state_path.clone();
    let install_requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let recording_requests = install_requests.clone();
    let observed = app.clone();
    let observed_session = session.clone();
    let router = federation_router(channel.clone(), check)
        .layer(axum::Extension(app.clone()))
        .layer(axum::Extension(FederationOnboardingHost {
            app: app.clone(),
            rpc,
            io: federation_onboarding_fixture::io(),
        }))
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let dropping = dropping.clone();
                let observed = observed.clone();
                let session = observed_session.clone();
                let observing_intent = observing_intent.clone();
                let central_state_path = central_state_path.clone();
                let recording_requests = recording_requests.clone();
                async move {
                    let binding_route = request.uri().path() == "/federation/v1/mailbox/binding";
                    let request = if binding_route {
                        let (parts, body) = request.into_parts();
                        let bytes = to_bytes(body, 16384).await.unwrap();
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        if body["operation"] == "install" {
                            recording_requests.lock().unwrap().push(body.clone());
                            let connection =
                                rusqlite::Connection::open(&central_state_path).unwrap();
                            let image = read_federation_persist_image(&connection)
                                .unwrap()
                                .unwrap()
                                .to_string();
                            let durable = [
                                body["expectedNativeCreationId"].as_str(),
                                body["binding"]["identity"]["creationId"].as_str(),
                                body["operationKey"].as_str(),
                            ]
                            .into_iter()
                            .all(|value| {
                                value
                                    .is_some_and(|value| !value.is_empty() && image.contains(value))
                            });
                            observing_intent.store(durable, Ordering::SeqCst);
                        }
                        Request::from_parts(parts, Body::from(bytes))
                    } else {
                        request
                    };
                    let response = next.run(request).await;
                    let installed = {
                        let inner = observed.inner.lock().unwrap();
                        inner.sessions[inner.find_session_index(&session).unwrap()]
                            .federation_mailbox_binding
                            .is_some()
                    };
                    if binding_route
                        && response.status().is_success()
                        && installed
                        && dropping.swap(false, Ordering::SeqCst)
                    {
                        // The real handler has completed its durable install; erase
                        // its successful reply at the fake transport boundary.
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        response
                    }
                }
            },
        ))
        .layer(axum::middleware::from_fn(close_connections));
    let (stop, rx) = tokio::sync::watch::channel(false);
    let server = tokio::spawn(serve_federation_control_listener(
        listener,
        router,
        rx,
        channel.clone(),
    ));
    println!("ENTRY_CONFIGURED true");
    std::io::stdout().flush().unwrap();
    loop {
        let line = tokio::task::spawn_blocking(|| {
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).unwrap();
            line
        })
        .await
        .unwrap();
        if line.is_empty() {
            break;
        }
        let command: Value = serde_json::from_str(&line).unwrap();
        if command["command"] == "stop" {
            break;
        }
        let result = match command["command"].as_str().unwrap() {
            "inspect" => {
                let mut value = {
                    let inner = app.inner.lock().unwrap();
                    let row = &inner.sessions[inner.find_session_index(&session).unwrap()];
                    json!({"binding":row.federation_mailbox_binding,"creation":row.federation_native_creation_id,
                        "version":inner.federation.version,"durable":inner.federation_durable,
                        "intentDurableBeforeInstall":intent_durable.load(Ordering::SeqCst),
                        "bindingCommittedAtHold":held_commit.load(Ordering::SeqCst),
                        "bindingPostcheckFailed":failed_postcheck.load(Ordering::SeqCst)})
                };
                let connection =
                    rusqlite::Connection::open(app.persistence_path.as_path()).unwrap();
                value["image"] =
                    serde_json::to_value(read_federation_persist_image(&connection).unwrap())
                        .unwrap();
                let stored = load_state(app.persistence_path.as_path()).unwrap().unwrap();
                if let Some(index) = stored.find_session_index(&session) {
                    value["persistedBinding"] =
                        serde_json::to_value(&stored.sessions[index].federation_mailbox_binding)
                            .unwrap();
                    value["persistedCreation"] =
                        json!(stored.sessions[index].federation_native_creation_id);
                }
                value
            }
            "nonRoot" => {
                let mut inner = app.inner.lock().unwrap();
                let index = inner.find_session_index(&session).unwrap();
                inner.sessions[index].session.parent_delegation_id = Some("fixture-child".into());
                json!({"changed":true})
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
                let inner = app.inner.lock().unwrap();
                json!({"creation":inner.sessions[inner.find_session_index(&session).unwrap()].federation_native_creation_id})
            }
            "bindFixture" => {
                let binding: FederationMailboxBinding =
                    serde_json::from_value(command["binding"].clone()).unwrap();
                app.install_federation_mailbox_binding(
                    &app.federation_peer_snapshot(&setup.central.host_id)
                        .unwrap(),
                    binding,
                    &native.creation,
                )
                .unwrap();
                json!({"bound":true})
            }
            "loseInstallReply" => {
                lose_reply.store(true, Ordering::SeqCst);
                json!({"armed":true})
            }
            "revokeCentral" => {
                let peer = app
                    .federation_peer_snapshot(&setup.central.host_id)
                    .unwrap();
                app.transition_federation_peer(&peer, FederationMembershipState::Revoked)
                    .unwrap();
                json!({"revoked":true})
            }
            "installRequests" => serde_json::to_value(&*install_requests.lock().unwrap()).unwrap(),
            "omitBindingRow" => {
                writer_control.omit_next_binding();
                json!({"armed":true})
            }
            "holdBindingPostcheck" => {
                hold_after_install.store(true, Ordering::SeqCst);
                json!({"armed":true})
            }
            "waitOmittedFence" => writer_control.wait_for_omitted_fence(),
            "releaseBindingRow" => {
                writer_control.release(&app);
                json!({"released":true})
            }
            _ => panic!("unknown entrypoint fixture command"),
        };
        println!("ENTRY_RESULT {result}");
        std::io::stdout().flush().unwrap();
    }
    writer_control.release(&app);
    channel.invalidate();
    stop.send_replace(true);
    server.await.unwrap().unwrap();
    app.federation_peer_handles.lock().unwrap().clear();
    app.shutdown_persist_blocking();
}
