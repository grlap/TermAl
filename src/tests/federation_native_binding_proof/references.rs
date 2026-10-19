// Owns fresh mailbox-reference provisioning and actual facade regressions.
// Does not inject a successful association, install native bindings over RPC,
// represent references as executable sessions, or claim live onboarding.
use super::*;

mod durability;
mod authority;
mod outage;
mod publication;

struct ReferenceFixture {
    api: DomainServer,
    proof: Fixture,
    caller: String,
    canonical: String,
    _handles: ReferenceHandleGuard,
}

// The owned channel retains AppState. Break that test-only strong cycle even
// if construction or a contract assertion unwinds before awaited finish().
struct ReferenceHandleGuard(AppState);

impl Drop for ReferenceHandleGuard {
    fn drop(&mut self) {
        self.0.federation_peer_handles.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()).clear();
    }
}

impl ReferenceFixture {
    async fn new(installed: bool) -> Self {
        Self::with_router(installed, |router| router).await
    }

    async fn with_router(installed: bool, wrap: impl FnOnce(Router) -> Router) -> Self {
        let mut proof = Fixture::with_router(installed, wrap).await;
        // Real SQL and production fences remain in use. Only elapsed logical
        // time is controlled while the durability fixtures deliberately hold.
        proof.channel.durability_clock = EngramBudgetClock::scripted();
        let handles = ReferenceHandleGuard(proof.central.clone());
        let caller = proof.native.clone(); // deliberately equal C/S raw ids
        let canonical = proof.binding.participant_key();
        if installed {
            // Existing participant registration, not reference evidence. The
            // future provisioner still has to obtain S's real proof over HTTP.
            proof.central.execute_federation_mailbox(FederationMailboxRequest {
                protocol_version: 1,
                schema_version: "1".to_owned(),
                central: proof.binding.central.clone(),
                actor: Some(FederationMailboxActor {
                    binding: proof.binding.clone(), name: "Satellite native".to_owned(),
                }),
                operation: FederationMailboxOperation::Register,
            }, Some(proof.channel.current_lease().unwrap())).unwrap();
        }
        // This is the actual owned channel that completed mutual proof/health,
        // not a replacement transport or a fabricated successful proof client.
        assert!(proof.central.federation_peer_handles.lock().unwrap().insert(
            proof.binding.identity.host_id.clone(),
            Arc::new(FederationPeerHandle {
                channel: proof.channel.clone(), rpc: proof.rpc.clone(),
            }),
        ).is_none());
        let api = DomainServer::start(app_router(proof.central.clone())).await;
        Self { api, proof, caller, canonical, _handles: handles }
    }

    fn url(&self, suffix: &str) -> String {
        format!("http://127.0.0.1:{}/api/sessions/{}/{suffix}", self.api.port, self.caller)
    }

    async fn create_response(&self, key: &str) -> reqwest::Response {
        self.proof.client.post(self.url("mailbox-references"))
            .json(&json!({"participantId":self.canonical,"idempotencyKey":key}))
            .send().await.unwrap()
    }

    async fn provision(&self, key: &str) -> Value {
        let response = self.create_response(key).await;
        // A missing provisioner is the intended RED, before JSON decoding.
        assert_eq!(response.status(), StatusCode::CREATED,
            "production durable mailbox-reference provisioner is missing or refused");
        let value: Value = response.json().await.unwrap();
        let reference = value["reference"].clone();
        let id = reference["id"].as_str().expect("new reference handle");
        assert!(id.starts_with("mailbox-reference-"));
        assert_ne!(id, self.caller);
        assert_ne!(id, self.canonical);
        assert_eq!(reference["kind"], "federationMailboxReference");
        assert!(reference["generation"].as_u64().is_some_and(|generation| generation > 0));
        assert_eq!(reference["participantId"], self.canonical);
        assert_eq!(reference["nativeCreationId"], self.proof.native_creation);
        assert_eq!(reference["binding"], serde_json::to_value(&self.proof.binding).unwrap());
        // No generic SessionRecord means no accidental actor/runtime/executor
        // routing via the established session-id dispatch paths.
        assert!(self.proof.central.inner.lock().unwrap().find_session_index(id).is_none());
        reference
    }

    async fn resolve(&self, id: &str) -> reqwest::Response {
        self.proof.client.get(self.url(&format!("mailbox-references/{id}/resolve")))
            .send().await.unwrap()
    }

    async fn tool(&self, name: &str, arguments: Value) -> Result<Value> {
        self.tool_as(&self.caller, name, arguments).await
    }

    async fn tool_as(&self, caller: &str, name: &str, arguments: Value) -> Result<Value> {
        let caller = caller.to_owned();
        let base = format!("http://127.0.0.1:{}", self.api.port);
        let name = name.to_owned();
        // The production bridge is blocking; keep its client/runtime creation,
        // use and drop off the asynchronous fixture's runtime threads.
        tokio::task::spawn_blocking(move || -> Result<Value> {
            let mut bridge = TermalDelegationMcpBridge::new(caller, base)?;
            bridge.client = reqwest::blocking::Client::builder().no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(phase_sync::DEADLOCK_GUARD).build()?;
            let response = bridge.handle_tool_call(json!({"name":name,"arguments":arguments}))?;
            if response["isError"] == true {
                bail!("facade refused: {response}");
            }
            serde_json::from_str(response["content"][0]["text"].as_str()
                .ok_or_else(|| anyhow!("missing facade text result"))?).map_err(Into::into)
        }).await.unwrap()
    }

    async fn send(&self, target: &str, key: &str) -> Result<Value> {
        self.tool("termal_send_to_session", json!({
            "sessionId":target,"message":"reference fixture body","idempotencyKey":key,
        })).await
    }

    fn mail_counts(&self) -> (u64, u64, u64) {
        let connection = self.proof.central.mailbox_store.connection().unwrap();
        (
            connection.query_row("SELECT COUNT(*) FROM mailboxes", [], |r| r.get(0)).unwrap(),
            connection.query_row("SELECT COUNT(*) FROM mailbox_participants", [], |r| r.get(0)).unwrap(),
            connection.query_row("SELECT COUNT(*) FROM mailbox_messages", [], |r| r.get(0)).unwrap(),
        )
    }

    fn intent_count(&self) -> u64 {
        self.proof.central.mailbox_store.connection().unwrap()
            .query_row("SELECT COUNT(*) FROM federation_mailbox_intents", [], |r| r.get(0))
            .unwrap()
    }

    async fn finish(mut self) {
        self.api.finish().await;
        self.proof.central.federation_peer_handles.lock().unwrap().clear();
        self.proof.finish().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_fixture_unwind_breaks_owned_handle_cycle() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let handles = fixture.proof.central.federation_peer_handles.clone();
    assert_eq!(handles.lock().unwrap().len(), 1);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owned = fixture;
        panic!("exercise reference fixture unwind cleanup");
    }));
    assert!(result.is_err());
    assert!(handles.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_canonical_mcp_control_uses_one_central_mailbox() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let sent = fixture.send(&fixture.canonical, "canonical-control").await.unwrap();
    let duplicate = fixture.send(&fixture.canonical, "canonical-control").await.unwrap();
    assert_eq!(sent["sessionId"], fixture.canonical);
    assert_eq!(sent["messageId"], duplicate["messageId"]);
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_production_provision_then_actual_mcp_is_canonical_equivalent() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let before = fixture.proof.native_snapshot();
    let reference = fixture.provision("fresh-reference").await;
    let id = reference["id"].as_str().unwrap();
    assert_eq!(fixture.proof.native_snapshot(), before, "provisioning reference has no remote effect");
    assert_eq!(fixture.mail_counts(), (0, 0, 0), "reference creates no mailbox or participant");
    let replay = fixture.provision("fresh-reference").await;
    assert_eq!(replay, reference, "exact provision intent cannot allocate another handle");
    let resolved = fixture.resolve(id).await;
    assert_eq!(resolved.status(), StatusCode::OK);
    let resolved: Value = resolved.json().await.unwrap();
    assert_eq!(resolved["participantId"], fixture.canonical);
    assert_eq!(resolved["nativeCreationId"], fixture.proof.native_creation);
    let canonical = fixture.send(&fixture.canonical, "one-logical-send").await.unwrap();
    let via_reference = fixture.send(id, "one-logical-send").await.unwrap();
    assert_eq!(via_reference["sessionId"], fixture.canonical);
    assert_eq!(via_reference["resolvedFrom"], id);
    assert_eq!(via_reference["messageId"], canonical["messageId"]);
    assert_eq!(via_reference["mailboxId"], canonical["mailboxId"]);
    assert_eq!(via_reference["duplicate"], true);
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    let refused = fixture.proof.client.post(fixture.url("mailboxes/send"))
        .json(&json!({"targetSessionId":id,"message":"raw-reference","idempotencyKey":"raw"}))
        .send().await.unwrap();
    assert!(refused.status().is_client_error(), "ordinary HTTP must still require canonical target");
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    fixture.finish().await;
}

fn reference_alias_spellings(id: &str) -> Vec<String> {
    let suffix = id.strip_prefix("mailbox-reference-").unwrap();
    let mut variants = Vec::new();
    // Both prefix separators independently present/absent, across three cases.
    for spelling in [format!("mailbox-reference-{suffix}"), format!("mailboxreference-{suffix}"),
        format!("mailbox-reference{suffix}"), format!("mailboxreference{suffix}")] {
        variants.push(spelling.to_ascii_uppercase());
        variants.push(spelling.replacen("mailbox", "MailBox", 1).replacen("reference", "ReFeReNcE", 1));
        variants.push(spelling);
    }
    variants.push(format!("Central::{id}"));
    variants.push(format!("{id}::Other"));
    for colons in [":", "::", ":::", "::::"] {
        variants.push(format!("Lab{colons}{id}"));
        variants.push(format!("{id}{colons}Other"));
    }
    variants
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_intent_prepare_reserved_target_precedes_actor_and_storage() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("intent-guard-order").await;
    let id = reference["id"].as_str().unwrap();
    let unbound = test_session_id(&fixture.proof.central, Agent::Codex);
    let before = fixture.intent_count();
    for target in reference_alias_spellings(id) {
        let response = fixture.proof.client.post(format!(
            "http://127.0.0.1:{}/api/sessions/{unbound}/mailbox-intents", fixture.api.port,
        )).json(&json!({"targetSessionId":target,"message":"no intent alias",
            "idempotencyKey":"reserved-before-actor"})).send().await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(fixture.intent_count(), before, "reserved input writes no sender intent");
        assert_eq!(status, StatusCode::NOT_FOUND,
            "reserved input must refuse before actor admission, not fail for an unrelated binding: {body}");
        assert!(body.contains("mailbox reference requires explicit resolution"), "{body}");
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_raw_handle_cannot_alias_another_local_participant() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let mut fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("alias-collision").await;
    let id = reference["id"].as_str().unwrap();
    let (collision_id, collision) = federation_mailboxes::local_participant(&fixture.proof.central, id);
    let (foreign, foreign_participant) = federation_mailboxes::local_participant(&fixture.proof.central, "Other caller");
    let unbound = test_session_id(&fixture.proof.central, Agent::Codex);
    assert_ne!(collision.session_id, fixture.canonical);
    let variants = reference_alias_spellings(id);
    for phase in 0..3 {
        if phase == 1 { fixture.proof.server.finish().await; }
        if phase == 2 {
            fixture.proof.channel.invalidate();
            assert!(fixture.proof.channel.current_lease().is_err());
        }
        for target in &variants {
            {
                let mut inner = fixture.proof.central.inner.lock().unwrap();
                let index = inner.find_session_index(&collision_id).unwrap();
                inner.session_mut_by_index(index).unwrap().session.name = target.clone();
                assert!(resolve_federation_mailbox_participant_locked(&inner, target).is_err());
                assert!(test_run_resolve_session(&inner, target).is_none());
            }
            let rows = [json!({"sessionId":collision.session_id,"localName":target,
                "name":format!("Central::{target}"),"hostId":collision.binding.identity.host_id})];
            assert!(resolve_domain_mailbox_reference(&rows, target, &fixture.caller).is_err());
            let mut delivery_collision = collision.clone();
            delivery_collision.name = target.clone();
            assert!(resolve_federation_delivery_target(&[delivery_collision], target).is_err(),
                "satellite delivery resolver must not select the colliding principal: {target}");
            assert!(fixture.proof.central.mailbox_peer_names(&fixture.caller, target).is_err());
            let before = fixture.mail_counts();
            let intents_before = fixture.intent_count();
            let native_before = fixture.proof.native_snapshot();
            let resolved = fixture.resolve(target).await;
            assert_eq!(resolved.status().is_success(), phase == 0 && target.as_str() == id,
                "typed HTTP resolve: phase {phase}, {target}");
            assert_eq!(fixture.mail_counts(), before);
            for caller in [&fixture.caller, &foreign, &unbound] {
                for route in ["mailboxes/send", "mailboxes/create", "mailbox-intents"] {
                    // Standalone callers have no domain-create route; its
                    // existing no-binding refusal is also effect-free.
                    let response = fixture.proof.client.post(format!(
                        "http://127.0.0.1:{}/api/sessions/{caller}/{route}", fixture.api.port,
                    )).json(&if route != "mailboxes/create" {
                        json!({"targetSessionId":target,"message":"no alias","idempotencyKey":"matrix-raw"})
                    } else { json!({"targetSessionId":target}) }).send().await.unwrap();
                    let status = response.status();
                    let body = response.text().await.unwrap();
                    if route == "mailbox-intents" {
                        assert_eq!(status, StatusCode::NOT_FOUND, "phase {phase}, {target}: {body}");
                        assert!(body.contains("mailbox reference requires explicit resolution"), "{body}");
                    } else {
                        assert!(status.is_client_error(), "phase {phase}, {route}, {target}");
                    }
                    assert_eq!(fixture.mail_counts(), before);
                    assert_eq!(fixture.intent_count(), intents_before,
                        "raw reserved spelling creates no sender intent: phase {phase}, {target}");
                }
                // Only the canonical handle's actual owner may resolve it
                // while proof is live. Everything else must fail, not alias.
                if phase == 0 && target.as_str() == id && caller == &fixture.caller { continue; }
                assert!(fixture.tool_as(caller, "termal_send_to_session", json!({
                    "sessionId":target,"message":"no MCP alias","idempotencyKey":"matrix-tool",
                })).await.is_err(), "phase {phase}, caller {caller}, target {target}");
                assert_eq!(fixture.mail_counts(), before);
            }
            assert_eq!(fixture.proof.native_snapshot(), native_before);
        }
        if phase == 0 {
            // Typed resolution is not disabled: a real proof and canonical
            // append reach the satellite, never the colliding local root.
            let sent = fixture.send(id, "resolved-collision-control").await.unwrap();
            assert_eq!(sent["sessionId"], fixture.canonical);
            assert_ne!(sent["sessionId"], collision.session_id);
            assert_eq!(fixture.mail_counts(), (1, 2, 1));
            // Canonical participant ids and ordinary nonreserved names still
            // work; the reservation is not a ban on creating named roots.
            assert_eq!(fixture.proof.central.mailbox_peer_names(&fixture.caller, &collision_id)
                .unwrap().1, variants.last().unwrap().as_str());
            let inner = fixture.proof.central.inner.lock().unwrap();
            assert_eq!(resolve_federation_mailbox_participant_locked(&inner, "Other caller")
                .unwrap().session_id, foreign_participant.session_id);
            assert_eq!(resolve_federation_delivery_target(&[collision.clone()], &collision.session_id)
                .unwrap().session_id, collision.session_id);
            assert_eq!(resolve_federation_delivery_target(&[foreign_participant.clone()], "Other caller")
                .unwrap().session_id, foreign_participant.session_id);
            assert_eq!(test_run_resolve_session(&inner, "Other caller"), Some(foreign.clone()));
        }
    }
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_missing_native_binding_cannot_be_opted_in_by_provision() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(false).await;
    let before = fixture.proof.native_snapshot();
    let response = fixture.create_response("no-binding").await;
    assert_eq!(fixture.proof.native_snapshot(), before);
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    assert_eq!(response.status(), StatusCode::CONFLICT,
        "reference provision must visibly refuse rather than install native binding");
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_new_handle_does_not_upgrade_legacy_or_inherit_executor_routes() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let native = {
        let inner = fixture.proof.satellite.inner.lock().unwrap();
        inner.sessions[inner.find_session_index(&fixture.proof.native).unwrap()].session.clone()
    };
    let legacy = {
        let mut inner = fixture.proof.central.inner.lock().unwrap();
        // No configured transport exists: this old proxy is never contacted.
        // Equal raw id/name/host presentation is intentionally not attestation.
        upsert_remote_proxy_session_record(&mut inner, "unassociated-legacy", &native, None)
    };
    let old_snapshot = {
        let inner = fixture.proof.central.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&legacy).unwrap()];
        json!({"session":record.session,"creation":record.federation_native_creation_id,
            "binding":record.federation_mailbox_binding,"remote":record.remote_id,
            "remoteSession":record.remote_session_id})
    };
    let reference = fixture.provision("new-not-upgrade").await;
    let id = reference["id"].as_str().unwrap();
    assert_ne!(id, legacy);
    assert!(fixture.send(&legacy, "old-handle").await.is_err());
    {
        let inner = fixture.proof.central.inner.lock().unwrap();
        let record = &inner.sessions[inner.find_session_index(&legacy).unwrap()];
        assert_eq!(json!({"session":record.session,"creation":record.federation_native_creation_id,
            "binding":record.federation_mailbox_binding,"remote":record.remote_id,
            "remoteSession":record.remote_session_id}), old_snapshot);
        assert!(inner.find_session_index(id).is_none());
    }
    let before = fixture.proof.native_snapshot();
    let unsupported = fixture.proof.client.post(format!(
        "http://127.0.0.1:{}/api/sessions/{id}/messages", fixture.api.port,
    )).json(&json!({"text":"must not execute","attachments":[]})).send().await.unwrap();
    let status = unsupported.status();
    assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND
        || status == StatusCode::NOT_IMPLEMENTED);
    assert_eq!(fixture.proof.native_snapshot(), before);
    assert_eq!(fixture.mail_counts(), (0, 0, 0));
    fixture.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_reference_recreated_native_never_retargets_old_handle_or_send_intent() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = ReferenceFixture::new(true).await;
    let reference = fixture.provision("before-recreation").await;
    let id = reference["id"].as_str().unwrap();
    let sent = fixture.send(id, "old-intent").await.unwrap();
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    {
        let mut inner = fixture.proof.satellite.inner.lock().unwrap();
        let index = inner.find_session_index(&fixture.proof.native).unwrap();
        inner.remove_session_at(index).unwrap();
        inner.next_session_number = fixture.proof.native.trim_start_matches("session-").parse().unwrap();
    }
    assert_eq!(test_session_id(&fixture.proof.satellite, Agent::Codex), fixture.proof.native);
    let before = fixture.proof.native_snapshot();
    assert_ne!(before["creation"], fixture.proof.native_creation);
    let resolved = fixture.resolve(id).await;
    assert_eq!(resolved.status(), StatusCode::CONFLICT);
    assert!(fixture.send(id, "old-intent").await.is_err(), "never rewrite old intent to successor");
    assert!(fixture.send(id, "new-key-old-handle").await.is_err());
    assert_eq!(fixture.proof.native_snapshot(), before);
    assert_eq!(fixture.mail_counts(), (1, 2, 1));
    assert!(sent["messageId"].as_str().is_some());
    fixture.finish().await;
}
