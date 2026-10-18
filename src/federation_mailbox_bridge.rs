// Owns domain participant discovery for the agent MCP/CLI facade.
// Does not cache directory content, authorize a caller, or serve offline reads.
// Split from delegation_mcp.rs for the new central directory protocol.

impl TermalDelegationMcpBridge {
    fn domain_mailbox_participants(&self, state: &Value) -> Result<Option<Vec<Value>>> {
        let caller = state["sessions"].as_array().and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["id"].as_str() == Some(self.serving_session_id.as_str()))
        });
        if caller
            .and_then(|session| session["mailboxParticipantId"].as_str())
            .is_none()
        {
            return Ok(None);
        }
        let rows: Vec<FederationMailboxParticipant> =
            serde_json::from_value(self.get_json_with_safe_replay(&format!(
                "/api/sessions/{}/mailboxes/participants",
                self.serving_session_id
            ))?)
            .context("central mailbox directory response shape was invalid")?;
        Ok(Some(rows.into_iter().map(|row|json!({
            "sessionId":row.session_id,"name":format!("{}::{}",row.host_name,row.name),
            "localName":row.name,"hostName":row.host_name,"hostId":row.binding.identity.host_id,
            "localSessionId":row.binding.identity.session_id,"creationId":row.binding.identity.creation_id
        })).collect()))
    }
}

fn resolve_domain_mailbox_reference(
    rows: &[Value],
    reference: &str,
    local_session: &str,
) -> Result<String> {
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
