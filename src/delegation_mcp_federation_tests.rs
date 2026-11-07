// Observable fixture projections and command registration only. Existing
// route tests own networking, sampling, authentication and runtime lifecycle.
use super::*;

fn identity(name: &str) -> Value {
    json!({"hostId":name,"hostGeneration":1})
}
fn metric(value: u64) -> Value {
    json!({"state":"available","value":value,"reason":null})
}
fn registry(role: &str) -> Value {
    json!({"enabled":true,"role":role,"local":identity("central"),
        "central":null,"peers":[],"satellite":null})
}
fn roster() -> Value {
    json!({"schemaVersion":1,"rosterState":"available","rosterAgeMs":14,
        "rosterTotal":1,"complete":true,"hosts":[{
        "host":identity("central"),"os":"macos","architecture":"aarch64",
        "state":"available","reason":null,"receivedAt":"2026-11-07T22:00:00Z",
        "snapshot":{"schemaVersion":1,"host":identity("central"),
            "os":"macos","architecture":"aarch64","measuredAt":"2026-11-07T21:59:59Z",
            "sourceAgeMs":1000,"staleAfterMs":30000,
            "cpu":{"logicalCount":metric(8),"busyPercent":metric(23)},
            "memory":{"totalBytes":metric(1000),"inUseBytes":metric(600),
                "availableBytes":{"state":"unsupported","value":null,"reason":"notExposedByPlatform"}},
            "volumes":{"complete":true,"data":{"state":"available","reason":null,
                "value":[{"identity":"/private/secret-volume","totalBytes":metric(5000),
                "freeBytes":metric(3000),"availableToCallerBytes":metric(2900)}]}}}}]})
}

#[test]
fn agent_federation_status_preserves_peer_states_without_making_link_claims() {
    let mut fed = registry("central");
    fed["peers"] = json!([
        {"host":identity("active"),"state":"active","verificationCurrent":true,
            "enrollmentGeneration":2,"attestation":{"state":"authenticated","worker":{"stage":"monitor","attempt":3}}},
        {"host":identity("parked"),"state":"enrolled","verificationCurrent":true,
            "attestation":{"worker":{"stage":"materialChanged","attempt":4}}},
        {"host":identity("quarantined"),"state":"enrolled","verificationCurrent":false,
            "attestation":{"worker":{"stage":"runtimeQuarantine","quarantinedForRuntime":true,"attempt":5}}},
        {"host":identity("unverified"),"state":"enrolled","verificationCurrent":false,
            "attestation":{"state":"unverified","worker":null}},
        {"host":identity("disconnected"),"state":"active","verificationCurrent":false,
            "lastObserved":{"linkIncarnation":"historical"},
            "attestation":{"worker":{"stage":"retryBackoff","currentBasis":false}}}
    ]);
    let out = agent_federation_status(&fed, &roster(), Some(200), None).unwrap();
    let peers = &out["federation"]["peers"];
    assert_eq!(peers.as_array().unwrap().len(), 5);
    assert_eq!(peers[0]["state"], "active");
    assert_eq!(peers[0]["enrollmentGeneration"], 2);
    assert_eq!(
        peers[1]["attestation"]["worker"]["stage"],
        "materialChanged"
    );
    assert_eq!(
        peers[2]["attestation"]["worker"]["quarantinedForRuntime"],
        true
    );
    assert_eq!(peers[3]["verificationCurrent"], false);
    assert_eq!(peers[4]["attestation"]["worker"]["currentBasis"], false);
    assert!(peers[4].get("linkConnected").is_none());
    assert_eq!(peers[4]["lastObserved"]["linkIncarnation"], "historical");
}

#[test]
fn agent_federation_status_preserves_metrics_freshness_and_partial_roster() {
    let mut util = roster();
    util["complete"] = json!(false);
    util["rosterState"] = json!("stale");
    util["hosts"][0]["state"] = json!("unavailable");
    util["hosts"][0]["reason"] = json!("peerDisconnected");
    util["hosts"][0]["snapshot"]["cpu"]["busyPercent"]["state"] = json!("stale");
    let out = agent_federation_status(&registry("central"), &util, Some(200), None).unwrap();
    let util = &out["hostUtilization"];
    assert_eq!(util["rosterState"], "stale");
    assert_eq!(util["complete"], false);
    let host = &util["hosts"][0];
    assert_eq!(host["reason"], "peerDisconnected");
    assert_eq!(
        host["snapshot"]["cpu"]["busyPercent"],
        json!({"state":"stale","value":23,"reason":null})
    );
    assert_eq!(host["snapshot"]["cpu"]["logicalCount"]["value"], 8);
    assert_eq!(
        host["snapshot"]["memory"]["availableBytes"],
        json!({"state":"unsupported","value":null,"reason":"notExposedByPlatform"})
    );
    assert_eq!(host["snapshot"]["memory"]["inUseBytes"]["value"], 600);
    assert_eq!(host["snapshot"]["sourceAgeMs"], 1000);
    assert_eq!(host["snapshot"]["measuredAt"], "2026-11-07T21:59:59Z");
    let volume = &host["snapshot"]["volumes"]["data"]["value"][0];
    assert_eq!(volume["totalBytes"]["value"], 5000);
    assert_eq!(volume["freeBytes"]["value"], 3000);
    assert_eq!(volume["availableToCallerBytes"]["value"], 2900);
    assert!(volume.get("identity").is_none());
}

#[test]
fn agent_federation_status_noncentral_and_unavailable_have_no_fake_roster() {
    for role in ["satellite", "standalone"] {
        let mut fed = registry(role);
        if role == "satellite" {
            fed["satellite"] = json!({"central":identity("upstream"),"boundSessions":2,
                "centralPeer":{"enrolled":true,"membership":"active","verificationCurrent":true,"linkConnected":false}});
        }
        for (status, reason) in [
            (Some(409), "routeRefused"),
            (Some(503), "routeRefused"),
            (None, "routeUnavailable"),
        ] {
            let out = agent_federation_status(&fed, &Value::Null, status, Some(reason)).unwrap();
            assert_eq!(out["federation"]["role"], role);
            assert_eq!(out["federation"]["satellite"], fed["satellite"]);
            assert!(out["hostUtilization"].is_null());
            assert_eq!(out["hostUtilizationStatus"], json!(status));
            assert_eq!(out["hostUtilizationReason"], reason);
        }
    }
}

#[test]
fn agent_federation_status_redacts_nonstatus_inputs_and_rejects_bounds() {
    let mut fed = registry("central");
    fed["peers"] = json!([{"host":identity("peer"),"state":"active",
        "pairSecret":"SECRET","mac":"SECRET","pairCapabilityRef":"SECRET",
        "profile":{"binarySha256":"SECRET","toolchainIdentity":"C:/secret"},
        "endpoint":{"knownHostsPath":"C:/secret"},
        "attestation":{"worker":{"stage":"sshSpawn","ssh":{"class":"authentication","stderr":"SECRET","credential":"SECRET"}}}}]);
    let out = agent_federation_status(&fed, &roster(), Some(200), None).unwrap();
    let encoded = serde_json::to_string(&out).unwrap();
    for forbidden in [
        "SECRET",
        "secret-volume",
        "C:/secret",
        "pairCapabilityRef",
        "binarySha256",
        "stderr\"",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    assert!(encoded.len() <= AGENT_FEDERATION_OUTPUT_BYTES);
    fed["peers"] = json!(vec![json!({}); 257]);
    assert!(agent_federation_status(&fed, &roster(), Some(200), None).is_err());
    let mut oversized = roster();
    let valid_host = oversized["hosts"][0].clone();
    oversized["hosts"] = json!(vec![valid_host; 258]);
    assert!(
        agent_status_utilization(&oversized)
            .unwrap_err()
            .to_string()
            .contains("host count")
    );
    let out = agent_federation_status(&registry("central"), &oversized, Some(200), None).unwrap();
    assert!(out["hostUtilization"].is_null());
    assert_eq!(out["hostUtilizationReason"], "invalidProjection");
}

#[test]
fn agent_federation_status_mcp_and_cli_registration() {
    let list = mcp_tools_list_result();
    assert!(
        list["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "termal_federation_status")
    );
    assert!(tool_requires_root_session("termal_federation_status"));
    let parsed = parse_coordination_cli_args_with_default_session_id(
        ["federation", "status", "--json"].map(str::to_owned),
        Some("session-root".into()),
    )
    .unwrap();
    assert_eq!(
        parsed.command,
        CoordinationCliCommand::FederationStatus {
            as_session: "session-root".into()
        }
    );
    let out = agent_federation_status(&registry("central"), &roster(), Some(200), None).unwrap();
    validate_coordination_cli_output(&parsed.command, &out).unwrap();
    let mut rendered = Vec::new();
    render_coordination_cli_output(&parsed.command, &out, &mut rendered).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&rendered).unwrap(), out);
}

#[test]
fn agent_federation_status_keeps_fixed_reasons_and_bounds_valid_volume_rows() {
    let mut fed = registry("central");
    fed["admissionQuarantine"] = json!("declaredRestore");
    let mut util = roster();
    util["hosts"][0]["snapshot"]["volumes"]["failureReasons"] =
        json!(["probeFailed", "collectionLimit"]);
    let out = agent_federation_status(&fed, &util, Some(200), None).unwrap();
    assert_eq!(out["federation"]["admissionQuarantine"], "declaredRestore");
    assert_eq!(
        out["hostUtilization"]["hosts"][0]["snapshot"]["volumes"]["failureReasons"],
        json!(["probeFailed", "collectionLimit"])
    );
    let snapshot = &mut util["hosts"][0]["snapshot"];
    let volume = snapshot["volumes"]["data"]["value"][0].clone();
    snapshot["volumes"]["data"]["value"] = json!(vec![volume.clone(); 32]);
    assert!(agent_status_snapshot(snapshot).is_ok());
    snapshot["volumes"]["data"]["value"] = json!(vec![volume; 33]);
    assert!(
        agent_status_snapshot(snapshot)
            .unwrap_err()
            .to_string()
            .contains("volume count")
    );
}

#[test]
fn agent_federation_status_large_valid_roster_is_bounded_without_truncation() {
    let mut util = roster();
    let mut host = util["hosts"][0].clone();
    let mut volume = host["snapshot"]["volumes"]["data"]["value"][0].clone();
    for key in ["totalBytes", "freeBytes", "availableToCallerBytes"] {
        volume[key] = metric(u64::MAX);
    }
    host["snapshot"]["volumes"]["data"]["value"] = json!(vec![volume; 32]);
    util["hosts"] = json!(vec![host; 257]);
    util["rosterTotal"] = json!(257);
    let out = agent_federation_status(&registry("central"), &util, Some(200), None).unwrap();
    assert!(serde_json::to_vec(&out).unwrap().len() <= AGENT_FEDERATION_OUTPUT_BYTES);
    assert!(out["hostUtilization"].is_null());
    assert_eq!(out["hostUtilizationReason"], "outputLimit");
    assert_eq!(out["federation"]["role"], "central");
}
