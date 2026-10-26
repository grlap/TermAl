// Owns reserved-reference classification and domain participant discovery.
// Does not cache directory content, authorize a caller, or serve offline reads.
// Split from delegation_mcp.rs for the new central directory protocol.

// ONE namespace boundary for every name -> principal resolver. Case and omitted
// hyphens are folded only to recognize reserved spellings, never to authorize
// them. Only the exact canonical UUID spelling may reach typed resolution.
fn mailbox_reference_handle(reference: &str) -> Result<Option<String>> {
    let reserved = reference.split(':').any(|part| {
        let folded: String = part.trim().chars().filter(|ch| *ch != '-')
            .map(|ch| ch.to_ascii_lowercase()).collect();
        folded.starts_with("mailboxreference")
    });
    if !reserved {
        return Ok(None);
    }
    let canonical = reference.strip_prefix("mailbox-reference-")
        .and_then(|id| Uuid::parse_str(id).ok())
        .map(|id| format!("mailbox-reference-{id}"));
    match canonical {
        Some(canonical) if canonical == reference => Ok(Some(canonical)),
        _ => bail!("reserved mailbox reference spelling; use its exact canonical handle or a principal id"),
    }
}

impl TermalDelegationMcpBridge {
    fn domain_mailbox_participants(&self, state: &Value) -> Result<Option<(Vec<Value>, Vec<FederationMailboxUnavailableHost>)>> {
        let caller = state["sessions"].as_array().and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["id"].as_str() == Some(self.serving_session_id.as_str()))
        });
        let explicit = caller.and_then(|session| session["mailboxParticipantId"].as_str()).is_some();
        let automatic = automatic_domain_mailbox_discovery(state, &self.serving_session_id);
        if !explicit && !automatic {
            return Ok(None);
        }
        let response = self.get_json_with_safe_replay(&format!(
            "/api/sessions/{}/mailboxes/directory", self.serving_session_id));
        let response = match response {
            Ok(value) => value,
            Err(error) if automatic && state["preferences"]["federation"]["role"] == "satellite" => {
                let central: FederationHostIdentity = serde_json::from_value(
                    state["preferences"]["federation"]["central"].clone())
                    .context("satellite central identity was invalid")?;
                let status = error.downcast_ref::<TermalDelegationApiError>()
                    .map(|error| error.status.as_u16()).unwrap_or(503);
                // These are local native locators, never replacement domain
                // principals. Unavailable inventory still fences global aliases.
                return Ok(Some((mailbox_peer_session_listing(state, true), vec![FederationMailboxUnavailableHost {
                    host_id: central.host_id, host_name: "Central".into(), status,
                    reason: format!("central mailbox directory unavailable: {error}"),
                }])));
            }
            Err(error) => return Err(error),
        };
        let snapshot: FederationMailboxDirectorySnapshot = serde_json::from_value(response)
            .context("central mailbox directory response shape was invalid")?;
        let rows = snapshot.participants.into_iter().map(|row|json!({
            "sessionId":row.session_id,"name":format!("{}::{}",row.host_name,row.name),
            "localName":row.name,"hostName":row.host_name,"hostId":row.binding.identity.host_id,
            "localSessionId":row.binding.identity.session_id,"creationId":row.binding.identity.creation_id
        })).collect();
        Ok(Some((rows, snapshot.unavailable_hosts)))
    }
}

fn mailbox_peer_session_listing(state: &Value, native_only: bool) -> Vec<Value> {
    let delegation_child_ids = delegation_child_session_ids(state);
    state["sessions"].as_array().into_iter().flatten()
        .filter(|session| is_root_peer_session(session, &delegation_child_ids))
        .filter(|session| !native_only || session.get("remoteId").is_none_or(Value::is_null))
        .map(|session| json!({
            "sessionId": session.get("id").cloned().unwrap_or(Value::Null),
            "name": session.get("name").cloned().unwrap_or(Value::Null),
            "agent": session.get("agent").cloned().unwrap_or(Value::Null),
            "status": session.get("status").cloned().unwrap_or(Value::Null),
            "workdir": session.get("workdir").cloned().unwrap_or(Value::Null),
            "preview": session.get("preview").cloned().unwrap_or(Value::Null),
        })).collect()
}

fn automatic_domain_mailbox_discovery(state: &Value, native: &str) -> bool {
    state["preferences"]["federation"]["enabled"] == true
        && matches!(state["preferences"]["federation"]["role"].as_str(), Some("central" | "satellite"))
        && state["sessions"].as_array().and_then(|sessions| sessions.iter().find(|session| session["id"] == native))
            .is_some_and(|session| is_root_peer_session(session, &delegation_child_session_ids(state))
                && session.get("remoteId").is_none_or(Value::is_null))
}

fn resolve_domain_mailbox_reference(
    rows: &[Value],
    reference: &str,
    local_session: &str,
) -> Result<String> {
    if !matches!(mailbox_reference_handle(reference), Ok(None)) {
        bail!("mailbox reference requires explicit typed resolution");
    }
    if let Some(exact) = rows
        .iter()
        .find(|row| row["sessionId"].as_str() == Some(reference))
    {
        return Ok(exact["sessionId"]
            .as_str()
            .expect("exact id checked")
            .to_owned());
    }
    if reference.starts_with("domain-session-") || reference.starts_with("session-") {
        bail!("qualified id required; use a current directory sessionId or exact Host::Name");
    }
    let mut matches = rows.iter().filter(|row| {
        ["name", "localName"].iter().any(|key| {
            row[*key]
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(reference))
        }) || format!(
            "{}::{}",
            row["hostId"].as_str().unwrap_or(""),
            row["localName"].as_str().unwrap_or("")
        )
        .eq_ignore_ascii_case(reference)
    });
    let first = matches.next().ok_or_else(|| {
        anyhow!("no qualified mailbox participant matches `{reference}`; call termal_list_sessions")
    })?;
    if matches.next().is_some() {
        bail!("mailbox participant name `{reference}` is ambiguous; use its exact sessionId");
    }
    let id = first["sessionId"]
        .as_str()
        .ok_or_else(|| anyhow!("invalid central mailbox participant id"))?;
    // The authoritative backend also checks full-principal self delivery;
    // localSessionId alone cannot distinguish equal ids on different hosts.
    let _ = local_session;
    Ok(id.to_owned())
}
