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
        self.domain_mailbox_participants_with_basis(state, false)
            .map(|rows| rows.map(|(rows, unavailable, _)| (rows, unavailable)))
    }

    fn domain_mailbox_participants_with_basis(&self, state: &Value, include_basis: bool)
        -> Result<Option<(Vec<Value>, Vec<FederationMailboxUnavailableHost>, Option<FederationMailboxAliasBasis>)>> {
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
            "/api/sessions/{}/mailboxes/{}", self.serving_session_id,
            if include_basis { "participants?aliasBasis=true" } else { "directory" },
        ));
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
                }], None)));
            }
            Err(error) => return Err(error),
        };
        let value = response;
        let directory = if value.is_array() {
            FederationMailboxDirectorySnapshot {
                participants: serde_json::from_value(value)
                    .context("central mailbox directory response shape was invalid")?,
                unavailable_hosts: Vec::new(),
                alias_basis: None, // An old host did not certify completeness.
            }
        } else {
            serde_json::from_value::<FederationMailboxDirectorySnapshot>(value)
                .context("central mailbox alias directory response shape was invalid")?
        };
        let satellite = state["preferences"]["federation"]["role"] == "satellite";
        let caller_host = |snapshot: &Value| {
            let principal = snapshot["sessions"].as_array()?.iter()
                .find(|session| session["id"] == self.serving_session_id)?
                .get("mailboxParticipantId")?.as_str()?;
            directory.participants.iter().find(|row| row.session_id == principal)
                .map(|row| row.binding.identity.host_id.clone())
        };
        let mut local_host = caller_host(state);
        let mut refreshed_state = None;
        if satellite && local_host.is_none() {
            // The first metadata read may precede the directory's automatic
            // admission. Re-read that existing projection once; never infer
            // the host from a native id that can recur on several hosts.
            if let Ok(refreshed) = self.get_json_with_safe_replay("/api/state") {
                local_host = caller_host(&refreshed);
                refreshed_state = Some(refreshed);
            }
        }
        let central_host = state["preferences"]["federation"]["central"]["hostId"].as_str();
        let metadata_state = refreshed_state.as_ref().unwrap_or(state);
        let delegation_child_ids = delegation_child_session_ids(metadata_state);
        // A central's validated settings identify its own host even when the
        // first summary predates automatic mailbox participation.
        let metadata_local_host = local_host.as_deref().or_else(|| {
            if state["preferences"]["federation"]["role"] == "central" {
                central_host
            } else {
                None
            }
        });
        Ok(Some((directory.participants.into_iter().map(|row| {
            let alias = format!("host:{}::{}", row.binding.identity.host_id, row.binding.identity.session_id);
            let native_locator = matches!(FederationMailboxAlias::parse(&alias, None), Ok(FederationMailboxAlias::HostNative { .. }))
                && required_path_identifier(Some(&Value::String(alias.clone())), "sessionId").is_ok();
            // A satellite has a native relay pair with its central only.
            // Local roots use native mail; other hosts keep domain delivery
            // and its separate wake-authority contract.
            let name = if satellite && native_locator
                && local_host.as_deref() == Some(row.binding.identity.host_id.as_str()) {
                row.binding.identity.session_id.clone()
            } else if native_locator && (!satellite
                || central_host == Some(row.binding.identity.host_id.as_str())) {
                alias
            } else {
                row.session_id.clone()
            };
            let summary = mailbox_participant_summary(
                metadata_state, &delegation_child_ids, &row, metadata_local_host,
            );
            let mut listed = mailbox_peer_session_row(summary.unwrap_or(&Value::Null));
            listed["sessionId"] = row.session_id.into();
            listed["name"] = name.into();
            listed["localName"] = row.name.into();
            listed["hostName"] = row.host_name.into();
            listed["hostId"] = row.binding.identity.host_id.into();
            listed["localSessionId"] = row.binding.identity.session_id.into();
            listed["creationId"] = row.binding.identity.creation_id.into();
            listed
        }).collect(), directory.unavailable_hosts, directory.alias_basis)))
    }
}

fn mailbox_participant_summary<'a>(
    state: &'a Value,
    delegation_child_ids: &HashSet<&str>,
    participant: &FederationMailboxParticipant,
    local_host: Option<&str>,
) -> Option<&'a Value> {
    let mut matches = state["sessions"].as_array().into_iter().flatten()
        .filter(|session| is_root_peer_session(session, delegation_child_ids))
        .filter(|session| match session["mailboxParticipantId"].as_str() {
            Some(principal) => principal == participant.session_id,
            None => local_host == Some(participant.binding.identity.host_id.as_str())
                && session.get("remoteId").is_none_or(Value::is_null)
                && session["id"].as_str() == Some(participant.binding.identity.session_id.as_str()),
        });
    let first = matches.next()?;
    // Metadata is display-only. Ambiguous or absent summaries stay unknown;
    // a repeated native id on another host never supplies this row's status.
    matches.next().is_none().then_some(first)
}

fn mailbox_peer_session_row(session: &Value) -> Value {
    json!({
        "sessionId": session.get("id").cloned().unwrap_or(Value::Null),
        "name": session.get("name").cloned().unwrap_or(Value::Null),
        "agent": session.get("agent").cloned().unwrap_or(Value::Null),
        "status": session.get("status").cloned().unwrap_or(Value::Null),
        "workdir": session.get("workdir").cloned().unwrap_or(Value::Null),
        "preview": session.get("preview").cloned().unwrap_or(Value::Null),
        "projectId": session.get("projectId").cloned().unwrap_or(Value::Null),
    })
}

fn mailbox_peer_session_listing(state: &Value, native_only: bool) -> Vec<Value> {
    let delegation_child_ids = delegation_child_session_ids(state);
    state["sessions"].as_array().into_iter().flatten()
        .filter(|session| is_root_peer_session(session, &delegation_child_ids))
        .filter(|session| !native_only || session.get("remoteId").is_none_or(Value::is_null))
        .map(|session| {
            let mut row = mailbox_peer_session_row(session);
            if native_only {
                row["localName"] = row["name"].clone();
                row["name"] = row["sessionId"].clone();
            }
            row
        }).collect()
}

fn automatic_domain_mailbox_discovery(state: &Value, native: &str) -> bool {
    state["preferences"]["federation"]["enabled"] == true
        && matches!(state["preferences"]["federation"]["role"].as_str(), Some("central" | "satellite"))
        && state["sessions"].as_array().and_then(|sessions| sessions.iter().find(|session| session["id"] == native))
            .is_some_and(|session| is_root_peer_session(session, &delegation_child_session_ids(state))
                && session.get("remoteId").is_none_or(Value::is_null))
}

#[cfg(test)]
fn resolve_domain_mailbox_reference(
    rows: &[Value],
    reference: &str,
    local_session: &str,
) -> Result<String> {
    resolve_domain_mailbox_reference_with_basis(rows, reference, local_session, None)
}

fn resolve_domain_mailbox_reference_with_basis(
    rows: &[Value], reference: &str, local_session: &str,
    basis: Option<&FederationMailboxAliasBasis>,
) -> Result<String> {
    let first = select_federation_mailbox_alias(rows, reference, basis, |row| FederationMailboxAliasRow {
        id: row["sessionId"].as_str().unwrap_or(""),
        host_name: row["hostName"].as_str().unwrap_or(""),
        host_id: row["hostId"].as_str().unwrap_or(""),
        name: row["localName"].as_str().unwrap_or(""),
        native_session_id: row["localSessionId"].as_str().unwrap_or(""),
    }).map_err(|error| anyhow!(error.message))?;
    let id = first["sessionId"]
        .as_str()
        .ok_or_else(|| anyhow!("invalid central mailbox participant id"))?;
    // The authoritative backend also checks full-principal self delivery;
    // localSessionId alone cannot distinguish equal ids on different hosts.
    let _ = local_session;
    Ok(id.to_owned())
}
