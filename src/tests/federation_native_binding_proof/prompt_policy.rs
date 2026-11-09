// Owns prompt permission/native proof over the existing real MAC fixture.
// Native roots, peers and links are disposable fixture setup, not live grants.
use super::*;
#[path = "prompt_policy_contention.rs"]
mod contention;
#[path = "prompt_policy_cuts.rs"]
mod cuts;
#[path = "prompt_policy_freshness.rs"]
mod freshness;
#[path = "prompt_policy_freshness_cuts.rs"]
mod freshness_cuts;
#[path = "prompt_operator.rs"]
mod operator;
#[path = "prompt_policy_replay.rs"]
mod replay;
#[path = "prompt_policy_timeout.rs"]
mod timeout;

struct WriterOwners(Vec<AppState>);

impl Drop for WriterOwners {
    fn drop(&mut self) {
        for app in &self.0 {
            app.shutdown_persist_blocking();
        }
    }
}

struct PolicyFixture {
    fixture: Fixture,
    _writers: WriterOwners,
}

impl Drop for PolicyFixture {
    fn drop(&mut self) {
        for app in [&self.fixture.central, &self.fixture.satellite] {
            // Registered channels retain an AppState clone, including this map.
            // Break that fixture cycle before dropping its states and writers.
            let handles = std::mem::take(
                &mut *app
                    .federation_peer_handles
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            );
            // Release the registry lock before destroying channels and clients.
            drop(handles);
        }
    }
}

impl std::ops::Deref for PolicyFixture {
    type Target = Fixture;
    fn deref(&self) -> &Fixture {
        &self.fixture
    }
}

impl std::ops::DerefMut for PolicyFixture {
    fn deref_mut(&mut self) -> &mut Fixture {
        &mut self.fixture
    }
}

fn policy_router(channel: FederationChannel) -> Router {
    let checked = channel.clone();
    policy_router_with_check(
        channel,
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
}

fn policy_router_with_check(
    channel: FederationChannel,
    ensure_current: FederationAuthorityCheck,
) -> Router {
    let listener = FederationListenerState {
        channel: channel.clone(),
        ensure_current: ensure_current.clone(),
    };
    let policy = federation_prompt_policy_routes()
        .layer(axum::middleware::from_fn_with_state(
            listener.clone(),
            federation_authenticate,
        ))
        .with_state(listener);
    federation_router(channel, ensure_current).merge(policy)
}

async fn install_policy_transport(fixture: &mut Fixture) {
    fixture.server.finish().await;
    fixture.server = DomainServer::start(policy_router(fixture.satellite_channel.clone())).await;
    let checked = fixture.channel.clone();
    fixture.rpc = FederationRpcClient::new(
        fixture.server.port,
        fixture.channel.clone(),
        Arc::new(move || checked.current_authority().map(|_| ())),
    )
    .unwrap();
}

async fn unpersisted_policy_fixture(placed: bool) -> Fixture {
    let mut fixture = Fixture::new(placed).await;
    install_policy_transport(&mut fixture).await;
    fixture
}

async fn policy_fixture(placed: bool) -> PolicyFixture {
    let mut fixture = Fixture::new(placed).await;
    let mut writers = WriterOwners(Vec::new());
    for app in [&mut fixture.central, &mut fixture.satellite] {
        super::super::federation_entrypoints_writer::start_writer(app);
        writers.0.push(app.clone());
    }
    // Channels capture AppState by value. Install the replacement writer
    // senders before cloning those apps into the authenticated policy handler.
    fixture.channel.app = Some(fixture.central.clone());
    fixture.satellite_channel.app = Some(fixture.satellite.clone());
    install_policy_transport(&mut fixture).await;
    PolicyFixture {
        fixture,
        _writers: writers,
    }
}

fn scope(fixture: &Fixture, placed: bool) -> FederationPromptScope {
    let inner = fixture.central.inner.lock().unwrap();
    let source = FederationMutationContext::locked(&inner)
        .prompt_native_root("session-1")
        .unwrap();
    FederationPromptScope {
        source_host: inner.federation.local.clone().unwrap(),
        source,
        target_host: fixture.satellite_channel.snapshot.local.clone(),
        target: FederationSessionIdentity {
            host_id: fixture.satellite_channel.snapshot.local.host_id.clone(),
            session_id: fixture.native.clone(),
            creation_id: fixture.native_creation.clone(),
        },
        enrollment_generation: fixture.channel.snapshot.enrollment_generation,
        placement_generation: placed.then_some(fixture.binding.placement_generation),
    }
}

fn version(app: &AppState) -> u64 {
    app.inner.lock().unwrap().federation.version
}

fn consent(fixture: &Fixture, scope: &FederationPromptScope) -> FederationPromptPermission {
    fixture
        .satellite
        .issue_federation_prompt_consent(
            version(&fixture.satellite),
            "consent".into(),
            scope.clone(),
        )
        .unwrap()
}

#[tokio::test]
async fn federation_prompt_policy_rpc_standalone_proof_is_read_only_and_consent_scoped() {
    let mut fixture = policy_fixture(false).await;
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let before = fixture.native_snapshot();
    let lease = fixture.channel.current_lease().unwrap();
    let proof = fixture
        .central
        .read_federation_prompt_policy(
            &fixture.rpc,
            &lease,
            FederationPromptPolicyOperation::DestinationProof {
                scope: scope.clone(),
                consent: consent.identity.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(proof.permission, consent);
    assert_eq!(proof.capabilities, FederationPromptCapabilities::local());
    assert_eq!(
        fixture.native_snapshot(),
        before,
        "proof creates no placement, binding, work or runtime"
    );
    assert!(before["binding"].is_null());
    let mut wrong = scope.clone();
    wrong.target.creation_id = Uuid::new_v4().to_string();
    assert!(
        fixture
            .central
            .read_federation_prompt_policy(
                &fixture.rpc,
                &lease,
                FederationPromptPolicyOperation::DestinationProof {
                    scope: wrong,
                    consent: consent.identity.clone()
                }
            )
            .await
            .is_err()
    );
    fixture
        .satellite
        .revoke_federation_prompt_permission(version(&fixture.satellite), &consent.identity.id)
        .unwrap();
    assert!(
        fixture
            .central
            .read_federation_prompt_policy(
                &fixture.rpc,
                &lease,
                FederationPromptPolicyOperation::DestinationProof {
                    scope,
                    consent: consent.identity
                }
            )
            .await
            .is_err()
    );
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_rpc_grant_requires_actual_native_proof_and_keeps_independent_consent()
 {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let before = fixture.native_snapshot();
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    assert_eq!(grant.consent, Some(consent.identity.clone()));
    assert_eq!(grant.issuer, scope.source_host);
    assert_ne!(grant.identity.id, consent.identity.id);
    let inner = fixture.central.inner.lock().unwrap();
    federation_prompt_dispatch_policy_locked(
        &inner,
        &scope,
        &grant.identity,
        &consent.identity,
        FederationRole::Central,
    )
    .unwrap();
    assert!(inner.federation_durable);
    drop(inner);
    assert_eq!(fixture.native_snapshot(), before);
    let mut wrong = consent.identity.clone();
    wrong.generation += 1;
    assert!(
        fixture
            .central
            .issue_federation_prompt_grant_on_link(
                version(&fixture.central),
                "wrong".into(),
                scope,
                wrong,
                &lease,
                &fixture.rpc
            )
            .await
            .is_err()
    );
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_automatic_mailbox_participation_is_not_execution_placement() {
    let mut fixture = policy_fixture(false).await;
    let prior = fixture
        .central
        .inner
        .lock()
        .unwrap()
        .federation
        .placements
        .get("native-proof")
        .unwrap()
        .clone();
    fixture
        .central
        .reconcile_federation_placement(&prior, true)
        .unwrap();
    let handle = Arc::new(FederationPeerHandle {
        channel: fixture.channel.clone(),
        rpc: fixture.rpc.clone(),
    });
    let lease = fixture.channel.current_lease().unwrap();
    let participant = fixture
        .central
        .admit_automatic_mailbox_root(
            &handle,
            &lease,
            FederationParticipationRoot {
                session_id: fixture.native.clone(),
                native_creation_id: fixture.native_creation.clone(),
                binding: None,
                name: "Automatic prompt target".into(),
                automatic: true,
                catalog_after: None,
            },
        )
        .await
        .unwrap();
    {
        let inner = fixture.central.inner.lock().unwrap();
        let placement = inner
            .federation
            .placements
            .values()
            .find(|row| row.identity == participant.binding.identity)
            .unwrap();
        assert!(placement.mailbox_only);
        assert_ne!(placement.state, FederationPlacementState::Completed);
    }
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&fixture.native).unwrap()];
        assert!(record.federation_mailbox_binding.is_none());
        assert_eq!(record.mailbox_binding(), Some(&participant.binding));
    }
    let scope = scope(&fixture, false);
    let consent = consent(&fixture, &scope);
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "mailbox-member-grant".into(),
            scope.clone(),
            consent.identity,
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(scope.target_host.host_id.clone(), handle);
    let (intent, _, _, _, _) = fixture
        .central
        .capture_remote_prompt_for_caller(
            &scope.source.session_id,
            &remote_prompt_protocol::OwnPrompt {
                reference: Uuid::parse_str(&grant.identity.id).unwrap(),
                idempotency_key: "mailbox-member-prompt".into(),
                text: "exact body".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(intent.components().0.placement_generation, None);
    assert_eq!(
        intent.components().0.target.components().3.to_string(),
        fixture.native_creation
    );
    assert!(
        fixture
            .satellite
            .inner
            .lock()
            .unwrap()
            .sessions
            .iter()
            .all(|record| record.queued_prompts.is_empty())
    );
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_rpc_local_revoke_is_not_receiver_observed_until_durable_ack() {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    let revoked = fixture
        .central
        .revoke_federation_prompt_permission(version(&fixture.central), &grant.identity.id)
        .unwrap();
    assert_eq!(revoked.receiver_observed_generation, None);
    let reply = fixture
        .central
        .read_federation_prompt_policy(
            &fixture.rpc,
            &lease,
            FederationPromptPolicyOperation::ObserveRevocation {
                permission: revoked.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reply.permission.receiver_observed_generation, Some(2));
    let connection =
        rusqlite::Connection::open(fixture.satellite.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    verify_federation_image_marker(&connection, &image).unwrap();
    let policy: FederationState = serde_json::from_value(image["domain"].clone()).unwrap();
    assert_eq!(
        policy
            .prompt_permissions
            .unwrap()
            .find(&grant.identity.id)
            .unwrap(),
        &reply.permission
    );
    // Reading the authenticated acknowledgement alone does not mutate the
    // issuer's retained evidence; its explicit observation commit is separate.
    assert_eq!(
        fixture
            .central
            .inner
            .lock()
            .unwrap()
            .federation
            .prompt_permissions
            .as_ref()
            .unwrap()
            .find(&grant.identity.id)
            .unwrap()
            .receiver_observed_generation,
        None
    );
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_rpc_own_caller_capture_and_receiver_observation_share_existing_owner()
 {
    let mut fixture = policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let grant = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "grant".into(),
            scope.clone(),
            consent.identity.clone(),
            &lease,
            &fixture.rpc,
        )
        .await
        .unwrap();
    fixture
        .central
        .federation_peer_handles
        .lock()
        .unwrap()
        .insert(
            scope.target_host.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: fixture.channel.clone(),
                rpc: fixture.rpc.clone(),
            }),
        );
    let mut reverse = DomainServer::start(policy_router(fixture.channel.clone())).await;
    let check = fixture.satellite_channel.clone();
    let reverse_rpc = FederationRpcClient::new(
        reverse.port,
        fixture.satellite_channel.clone(),
        Arc::new(move || check.current_authority().map(|_| ())),
    )
    .unwrap();
    let request = remote_prompt_protocol::OwnPrompt {
        reference: Uuid::parse_str(&grant.identity.id).unwrap(),
        idempotency_key: "same exact key".into(),
        text: "  /not-a-command\r\n exact UTF-8 ą\n".into(),
    };
    assert!(
        fixture
            .central
            .capture_remote_prompt_for_caller("someone-else", &request)
            .await
            .is_err()
    );
    let (intent, proof, handle, _, freshness) = fixture
        .central
        .capture_remote_prompt_for_caller(&scope.source.session_id, &request)
        .await
        .unwrap();
    freshness
        .ensure_fresh_at(handle.channel.control.clock.now())
        .unwrap();
    assert_eq!(intent.components().3, request.text);
    assert!(
        fixture
            .satellite
            .inner
            .lock()
            .unwrap()
            .federation
            .prompt_permissions
            .as_ref()
            .unwrap()
            .find(&grant.identity.id)
            .is_none()
    );
    let receiver_lease = fixture.satellite_channel.current_lease().unwrap();
    fixture
        .satellite
        .observe_remote_prompt_grant(&receiver_lease, &reverse_rpc, &intent, &proof)
        .await
        .unwrap();
    {
        let inner = fixture.satellite.inner.lock().unwrap();
        federation_prompt_intent_policy_locked(&inner, &intent, FederationRole::Satellite).unwrap();
        assert!(!federation_prompt_intent_revoked_locked(&inner, &intent));
    }
    let observed = fixture
        .central
        .revoke_federation_prompt_permission(version(&fixture.central), &grant.identity.id)
        .unwrap();
    assert!(observed.receiver_observed_generation.is_none());
    assert!(!federation_prompt_intent_revoked_locked(
        &fixture.satellite.inner.lock().unwrap(),
        &intent
    ));
    let confirmed = fixture
        .central
        .confirm_remote_prompt_revocation(&grant.identity.id)
        .await
        .unwrap();
    assert_eq!(confirmed.receiver_observed_generation, Some(2));
    assert!(federation_prompt_intent_revoked_locked(
        &fixture.satellite.inner.lock().unwrap(),
        &intent
    ));
    assert!(
        fixture
            .central
            .capture_remote_prompt_for_caller(&scope.source.session_id, &request)
            .await
            .is_err()
    );
    // Both existing native queues remain untouched by permission/proof work.
    for app in [&fixture.central, &fixture.satellite] {
        assert!(
            app.inner
                .lock()
                .unwrap()
                .sessions
                .iter()
                .all(|record| record.queued_prompts.is_empty())
        );
    }
    reverse.finish().await;
    fixture.server.finish().await;
}

#[tokio::test]
async fn federation_prompt_policy_rpc_missing_writer_never_confirms_grant() {
    let mut fixture = unpersisted_policy_fixture(true).await;
    let scope = scope(&fixture, true);
    let consent = consent(&fixture, &scope);
    let lease = fixture.channel.current_lease().unwrap();
    let result = fixture
        .central
        .issue_federation_prompt_grant_on_link(
            version(&fixture.central),
            "no-writer".into(),
            scope.clone(),
            consent.identity,
            &lease,
            &fixture.rpc,
        )
        .await;
    let error = result.unwrap_err();
    assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(error.message.contains("persistence unavailable"));
    let inner = fixture.central.inner.lock().unwrap();
    assert!(
        !inner.federation_durable,
        "uncommitted policy is never dispatch authority"
    );
    assert!(
        federation_prompt_scope_current_locked(&inner, &scope, FederationRole::Central).is_err()
    );
    drop(inner);
    let connection =
        rusqlite::Connection::open(fixture.central.persistence_path.as_path()).unwrap();
    let image = read_federation_persist_image(&connection).unwrap().unwrap();
    assert!(
        image["domain"]["promptPermissions"].is_null(),
        "no grant was committed"
    );
    fixture.server.finish().await;
}
