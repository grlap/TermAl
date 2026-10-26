// Owns alias completeness through actual authenticated Directory transport
// and the blocking MCP resolver. Disposable API fixtures are not live routing.
use super::*;

async fn bridge_call<T: Send + 'static>(
    port: u16,
    caller: &str,
    call: impl FnOnce(TermalDelegationMcpBridge) -> Result<T> + Send + 'static,
) -> Result<T> {
    let caller = caller.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut bridge =
            TermalDelegationMcpBridge::new(caller, format!("http://127.0.0.1:{port}"))?;
        bridge.client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(phase_sync::DEADLOCK_GUARD)
            .build()?;
        call(bridge)
    })
    .await
    .unwrap()
}

async fn bridge_resolve(port: u16, caller: &str, reference: &str) -> Result<String> {
    let reference = reference.to_owned();
    bridge_call(port, caller, move |bridge| bridge.resolve_session_reference(&reference)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_mailbox_alias_grammar_real_directory_rpc_and_mcp_retain_inventory_names() {
    // Reuse the automatic-participation fixture: real authenticated catalogs
    // must be available before the composed directory can certify completeness.
    let pair = federation_mailbox_participation::qualified_pair().await;
    let central_native = federation_mailbox_participation::native(&pair.central, "Caller");
    let satellite_native = federation_mailbox_participation::native(&pair.satellite, "Satellite caller");
    let natives: Vec<_> = ["Termal::O1-A", "Termal::WindowsB-Gate-Courier",
        "session-display-only", "Name/with/slash"].into_iter()
        .map(|name| federation_mailbox_participation::native(&pair.central, name)).collect();
    let directory = pair.satellite.federation_mailbox_alias_directory(&satellite_native).await.unwrap();
    assert!(directory.alias_basis.as_ref().unwrap().complete);
    let central_id = pair.central.inner.lock().unwrap().federation.local.clone().unwrap().host_id;
    let targets: Vec<_> = directory.participants.iter()
        .filter(|row| row.binding.identity.host_id == central_id
            && natives.contains(&row.binding.identity.session_id)).cloned().collect();
    assert_eq!(targets.len(), natives.len());
    let handle = pair.satellite.federation_peer_handle(&central_id).unwrap();
    let legacy: FederationMailboxResponse = handle.rpc.request(FederationRpcRoute::Mailbox(
        handle.channel.current_lease().unwrap(),
        FederationMailboxRequest {
            protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
            schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.into(),
            central: pair.satellite.federation_mailbox_actor(&satellite_native).unwrap().unwrap().binding.central,
            actor: pair.satellite.federation_mailbox_actor(&satellite_native).unwrap(),
            operation: FederationMailboxOperation::Directory { alias_basis: false },
        },
    )).await.unwrap();
    assert!(legacy.alias_basis.is_none(), "real no-query request keeps the old envelope");
    assert!(serde_json::to_value(legacy).unwrap().get("aliasBasis").is_none());
    let mut api = DomainServer::start(app_router(pair.central.clone())).await;
    let central_caller = central_native.as_str();
    for target in &targets {
        let target_id = target.session_id.clone();
        let (listed_name, receipt) = bridge_call(api.port, central_caller, move |bridge| {
            let listing = bridge.tool_list_sessions(json!({}))?;
            let row = listing["sessions"].as_array().unwrap().iter()
                .find(|row| row["sessionId"].as_str() == Some(target_id.as_str())).unwrap();
            let name = row["name"].as_str().unwrap().to_owned();
            assert!(name == target_id || name.starts_with("host:"));
            if name != target_id {
                assert!(name.contains(row["hostId"].as_str().unwrap()));
            }
            let receipt = bridge.tool_send_to_session(json!({
                "sessionId":name, "message":"listed domain name round trip",
                "idempotencyKey":format!("listed-{}", Uuid::new_v4()),
            }))?;
            Ok((name, receipt))
        }).await.unwrap();
        assert_eq!(receipt["sessionId"], listed_name,
            "the thin MCP adapter preserves an exact native locator for backend routing");
        assert_eq!(receipt["resolvedFrom"], listed_name);
        let stored_target: String = pair.central.mailbox_store.connection().unwrap()
            .query_row("SELECT target_session_id FROM mailbox_messages WHERE id=?1",
                [receipt["messageId"].as_str().unwrap()], |row| row.get(0)).unwrap();
        assert_eq!(stored_target, target.binding.identity.session_id,
            "the listed self-host native locator appends to that exact local root");
        assert_eq!(listed_name, format!("host:{}::{}",
            target.binding.identity.host_id, target.binding.identity.session_id));
        if target.name.starts_with("session-") || target.name.contains('/') {
            continue;
        }
        assert_eq!(
            resolve_federation_delivery_target_with_basis(
                &directory.participants,
                &target.name,
                directory.alias_basis.as_ref()
            )
            .unwrap()
            .session_id,
            target.session_id
        );
        assert_eq!(
            bridge_resolve(api.port, central_caller, &target.name)
                .await
                .unwrap(),
            target.session_id
        );
        assert_eq!(
            bridge_resolve(api.port, central_caller, &format!("name:{}", target.name))
                .await
                .unwrap(),
            target.session_id
        );
        assert_eq!(
            bridge_resolve(
                api.port,
                central_caller,
                &format!("host:Central::{}", target.name)
            )
            .await
            .unwrap(),
            target.session_id
        );
    }
    // No participant on this enrolled but unavailable peer: visible rows alone
    // still look complete. Only the central registry can expose the gap.
    pair.central
        .enroll_federation_peer(federation_registry::peer(48005))
        .unwrap();
    let directory = pair.satellite
        .federation_mailbox_alias_directory(&satellite_native)
        .await
        .unwrap();
    assert!(!directory.alias_basis.as_ref().unwrap().complete);
    for target in &targets {
        if target.name.starts_with("session-") || target.name.contains('/') {
            continue;
        }
        let error = bridge_resolve(api.port, central_caller, &target.name)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("mailbox alias inventory is unavailable"), "{error}");
        assert!(error.contains("known exact participant id"), "{error}");
        assert!(error.contains("explicit host:"), "{error}");
        let error = bridge_resolve(api.port, central_caller, &format!("name:{}", target.name))
            .await.unwrap_err().to_string();
        assert!(error.contains("mailbox alias inventory is unavailable"),
            "an enrolled unavailable host also fences global literal aliases: {error}");
        assert_eq!(
            resolve_federation_delivery_target_with_basis(
                &directory.participants,
                &format!("name:{}", target.name),
                directory.alias_basis.as_ref()
            )
            .unwrap()
            .session_id,
            target.session_id
        );
        assert_eq!(
            bridge_resolve(api.port, central_caller, &target.session_id)
                .await
                .unwrap(),
            target.session_id
        );
        let exact_native = format!("host:{}::{}",
            target.binding.identity.host_id, target.binding.identity.session_id);
        assert_eq!(bridge_resolve(api.port, central_caller, &exact_native).await.unwrap(),
            exact_native, "an unrelated outage cannot reinterpret the explicit native locator");
    }
    api.finish().await;
    pair.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_mailbox_alias_grammar_old_local_api_missing_basis_refuses_legacy_but_exact_id_skips_lookup()
 {
    let central = federation_mailboxes::central_store();
    let (_, target) = federation_mailboxes::local_participant(&central, "Termal::O1-A");
    let state =
        json!({"sessions":[{"id":"session-1","mailboxParticipantId":"domain-session-caller"}]});
    let participants = serde_json::to_value(vec![target.clone()]).unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state_calls = calls.clone();
    let directory_calls = calls.clone();
    let mut old_api = DomainServer::start(
        Router::new()
            .route(
                "/api/state",
                get(move || {
                    state_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let state = state.clone();
                    async move { Json(state) }
                }),
            )
            .route(
                "/api/sessions/session-1/mailboxes/participants",
                get(move || {
                    directory_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let participants = participants.clone();
                    async move { Json(participants) }
                }),
            ),
    )
    .await;
    let error = bridge_resolve(old_api.port, "session-1", &target.name)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("mailbox alias inventory is unavailable"), "{error}");
    assert!(error.contains("known exact participant id"), "{error}");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        bridge_resolve(old_api.port, "session-1", &target.session_id)
            .await
            .unwrap(),
        target.session_id
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "exact id performs no directory or state request"
    );
    assert_eq!(
        bridge_resolve(old_api.port, "session-1", &format!("name:{}", target.name))
            .await.unwrap(),
        target.session_id,
        "an explicit literal selects the old directory's current row"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4,
        "the two alias resolutions each read state and the same directory");
    let exact_native = format!("host:{}::{}",
        target.binding.identity.host_id, target.binding.identity.session_id);
    assert_eq!(bridge_resolve(old_api.port, "session-1", &exact_native).await.unwrap(),
        exact_native, "the adapter preserves the locator; backend native admission still proves the peer");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 4,
        "the explicit locator performs no lookup after the refused bare and successful explicit-literal lookups");
    old_api.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_mailbox_alias_grammar_non_domain_project_names_remain_literal() {
    let state = json!({"sessions":[
        {"id":"session-1","name":"Caller"},
        {"id":"session-2","name":"Termal::Merger2"}
    ]});
    let mut api = DomainServer::start(Router::new().route(
        "/api/state",
        get(move || {
            let state = state.clone();
            async move { Json(state) }
        }),
    ))
    .await;
    assert_eq!(
        bridge_resolve(api.port, "session-1", "Termal::Merger2")
            .await
            .unwrap(),
        "session-2"
    );
    api.finish().await;
}
