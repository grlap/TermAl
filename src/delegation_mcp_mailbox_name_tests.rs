// Exercises the send boundary with a disposable directory and append endpoint.
use super::*;

const LOCAL_HOST: &str = "11111111-1111-4111-8111-111111111111";
const REMOTE_HOST: &str = "22222222-2222-4222-8222-222222222222";

fn participant(host: &str, native: &str, name: &str) -> FederationMailboxParticipant {
    let binding = FederationMailboxBinding {
        central: FederationHostIdentity {
            host_id: LOCAL_HOST.into(),
            host_generation: 1,
        },
        identity: FederationSessionIdentity {
            host_id: host.into(),
            session_id: native.into(),
            creation_id: Uuid::new_v4().to_string(),
        },
        placement_generation: 1,
    };
    FederationMailboxParticipant {
        session_id: binding.participant_key(),
        binding,
        host_name: if host == LOCAL_HOST {
            "Central"
        } else {
            "Remote"
        }
        .into(),
        name: name.into(),
    }
}

fn state() -> Value {
    json!({"preferences":{"federation":{"enabled":true,"role":"central",
        "central":{"hostId":LOCAL_HOST,"hostGeneration":1}}}, "sessions":[
        {"id":"session-parent","name":"Caller"},
        {"id":"session-peer","name":"Termal::Peer"}
    ], "delegations":[]})
}

fn directory(rows: Vec<FederationMailboxParticipant>) -> Value {
    json!({"participants":rows,"unavailableHosts":[],"aliasBasis":{
        "complete":true,"hosts":[{"id":LOCAL_HOST,"name":"Central"},
            {"id":REMOTE_HOST,"name":"Remote"}]}})
}

fn receipt() -> Value {
    json!({"mailboxId":"mailbox-peer","messageId":"message-peer","sequence":1,
        "unreadDepth":1,"notificationDisposition":"queuedBehindActiveTurn",
        "duplicate":false,"senderProcessedThrough":1,"senderCursorAdvanced":true})
}

fn send(bridge: &TermalDelegationMcpBridge, target: &str) -> Result<Value> {
    bridge.tool_send_to_session(json!({"sessionId":target,"message":"one intent",
        "idempotencyKey":"name-routing"}))
}

#[test]
fn display_names_refuse_before_append_with_local_and_remote_native_remedies() {
    for host in [LOCAL_HOST, REMOTE_HOST] {
        let row = participant(host, "session-peer", "Termal::Peer");
        let snapshot = directory(vec![row]);
        let native = format!("host:{host}::session-peer");
        let expected = native.clone();
        let (url, requests, server) = spawn_test_mcp_http_server(3, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (200, state()),
                ("GET", "/api/sessions/session-parent/mailboxes/participants?aliasBasis=true") => {
                    (200, snapshot.clone())
                }
                ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                    let body: Value = serde_json::from_str(&request.body).unwrap();
                    assert_eq!(body["targetSessionId"], expected);
                    (202, receipt())
                }
                _ => panic!("unexpected request: {request:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
        let error = send(&bridge, "Termal::Peer").unwrap_err().to_string();
        assert!(error.contains("before saving"), "{error}");
        assert!(error.contains(&native), "{error}");
        assert!(requests.lock().unwrap().iter().all(|r| r.method == "GET"));
        // The remedy keeps the same intent/key: the refusal has appended nothing.
        let sent = send(&bridge, &native).unwrap();
        assert_eq!(sent["sessionId"], native);
        server.join().unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 1);
    }
}

#[test]
fn ambiguous_or_missing_display_names_never_reach_append() {
    for rows in [
        vec![],
        vec![
            participant(LOCAL_HOST, "session-peer", "Termal::Peer"),
            participant(REMOTE_HOST, "session-peer", "Termal::Peer"),
        ],
    ] {
        let snapshot = directory(rows);
        let (url, requests, server) = spawn_test_mcp_http_server(2, move |request| {
            assert_eq!(request.method, "GET");
            match request.path.as_str() {
                "/api/state" => (200, state()),
                "/api/sessions/session-parent/mailboxes/participants?aliasBasis=true" => {
                    (200, snapshot.clone())
                }
                _ => panic!("unexpected request: {request:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
        let error = send(&bridge, "Termal::Peer").unwrap_err().to_string();
        assert!(error.contains("name:Termal::Peer"), "{error}");
        assert!(error.contains("host:Termal::Peer"), "{error}");
        assert!(!error.contains("display-name send refused"), "{error}");
        server.join().unwrap();
        assert!(requests.lock().unwrap().iter().all(|r| r.method == "GET"));
    }
}

#[test]
fn satellite_remedies_use_the_listed_local_central_or_domain_route() {
    const SATELLITE_HOST: &str = "33333333-3333-4333-8333-333333333333";
    for target_host in [SATELLITE_HOST, LOCAL_HOST, REMOTE_HOST] {
        let caller = participant(SATELLITE_HOST, "session-parent", "Caller");
        let target = participant(target_host, "session-peer", "Termal::Peer");
        let domain = target.session_id.clone();
        let sibling = target_host == REMOTE_HOST;
        let remedy = if target_host == SATELLITE_HOST {
            "session-peer".to_owned()
        } else if target_host == LOCAL_HOST {
            format!("host:{LOCAL_HOST}::session-peer")
        } else {
            domain.clone()
        };
        let mut metadata = state();
        metadata["preferences"]["federation"]["role"] = json!("satellite");
        metadata["sessions"][0]["mailboxParticipantId"] = json!(caller.session_id);
        let mut snapshot = directory(vec![caller, target]);
        snapshot["aliasBasis"]["hosts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":SATELLITE_HOST,"name":"Local satellite"}));
        let expected = remedy.clone();
        let (url, requests, server) = spawn_test_mcp_http_server(3, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (200, metadata.clone()),
                ("GET", "/api/sessions/session-parent/mailboxes/participants?aliasBasis=true") => {
                    (200, snapshot.clone())
                }
                ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                    let body: Value = serde_json::from_str(&request.body).unwrap();
                    assert_eq!(body["targetSessionId"], expected);
                    let mut reply = receipt();
                    if sibling {
                        reply["notificationDisposition"] = json!("durableButNotWoken");
                    }
                    (202, reply)
                }
                _ => panic!("unexpected request: {request:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
        let error = send(&bridge, "Termal::Peer").unwrap_err().to_string();
        assert!(error.contains("before saving"), "{error}");
        assert!(error.contains(&format!("`{remedy}`")), "{error}");
        if sibling {
            assert!(error.contains("no native route is listed"), "{error}");
            assert!(
                error.contains("does not guarantee a native wake"),
                "{error}"
            );
            assert!(
                !error.contains(&format!("host:{REMOTE_HOST}::session-peer")),
                "{error}"
            );
        } else {
            assert!(error.contains("exact native target"), "{error}");
        }
        assert!(requests.lock().unwrap().iter().all(|r| r.method == "GET"));
        assert_eq!(send(&bridge, &remedy).unwrap()["sessionId"], remedy);
        server.join().unwrap();
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == "POST")
                .count(),
            1
        );
    }
}

#[test]
fn exact_native_and_domain_ids_keep_their_append_targets_without_discovery() {
    let domain = participant(REMOTE_HOST, "session-peer", "Termal::Peer").session_id;
    for target in [
        "session-peer".to_owned(),
        format!("host:{LOCAL_HOST}::session-peer"),
        format!("host:{REMOTE_HOST}::session-peer"),
        domain,
    ] {
        let expected = target.clone();
        let (url, requests, server) = spawn_test_mcp_http_server(1, move |request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/api/sessions/session-parent/mailboxes/send");
            let body: Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(body["targetSessionId"], expected);
            assert_eq!(body["idempotencyKey"], "name-routing");
            (202, receipt())
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
        assert_eq!(send(&bridge, &target).unwrap()["sessionId"], target);
        server.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), 1);
    }
}

#[test]
fn explicit_domain_aliases_keep_their_selected_principal() {
    for alias in ["name:Termal::Peer", "host:Remote::Termal::Peer"] {
        let row = participant(REMOTE_HOST, "session-peer", "Termal::Peer");
        let expected = row.session_id.clone();
        let snapshot = directory(vec![row]);
        let (url, requests, server) = spawn_test_mcp_http_server(3, move |request| {
            match (request.method.as_str(), request.path.as_str()) {
                ("GET", "/api/state") => (200, state()),
                ("GET", "/api/sessions/session-parent/mailboxes/participants?aliasBasis=true") => {
                    (200, snapshot.clone())
                }
                ("POST", "/api/sessions/session-parent/mailboxes/send") => {
                    let body: Value = serde_json::from_str(&request.body).unwrap();
                    assert_eq!(body["targetSessionId"], expected);
                    (202, receipt())
                }
                _ => panic!("unexpected request: {request:?}"),
            }
        });
        let bridge = TermalDelegationMcpBridge::new("session-parent".into(), url).unwrap();
        assert!(
            send(&bridge, alias).unwrap()["sessionId"]
                .as_str()
                .unwrap()
                .starts_with("domain-session-")
        );
        server.join().unwrap();
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == "POST")
                .count(),
            1
        );
    }
}
