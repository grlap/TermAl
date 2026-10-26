// Owns domain alias grammar and the central directory's discovery basis.
// This projection grants no authority, persists nothing and does not replace
// current participant/placement checks at append time. Standalone names differ.

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxAliasBasis {
    complete: bool,
    hosts: Vec<FederationMailboxAliasHost>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FederationMailboxAliasHost {
    id: String,
    name: String,
}

enum FederationMailboxAlias<'a> {
    Name(&'a str),
    Host(&'a str, &'a str),
    HostNative { host: Uuid, session_id: &'a str },
    Legacy(&'a str),
}

fn federation_mailbox_alias_error(reference: &str) -> String {
    let reference: String = reference
        .chars()
        .take(480)
        .map(|ch| {
            if ch.is_control() || matches!(ch, '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect();
    if let Some(qualified) = reference.strip_prefix("host:") {
        let name = qualified.split_once("::").map(|(_, name)| name).unwrap_or("<exact local name>");
        return format!("mailbox host alias is invalid or ambiguous; use host:<host UUID>::{name} or the exact directory sessionId");
    }
    if let Some(name) = reference.strip_prefix("name:") {
        return format!("mailbox literal alias is missing or ambiguous; use host:<host UUID>::{name} or the exact directory sessionId");
    }
    format!(
        "mailbox alias is ambiguous or its directory is incomplete; use name:{reference} for a literal name, host:{reference} for Host::Name, or the exact directory sessionId"
    )
}

fn federation_mailbox_host_prefix(reference: &str, host: &str) -> bool {
    !host.is_empty()
        && reference.get(..host.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(host))
        && reference.get(host.len()..).is_some_and(|tail| tail.starts_with("::"))
}

impl<'a> FederationMailboxAlias<'a> {
    fn parse(reference: &'a str, basis: Option<&FederationMailboxAliasBasis>) -> Result<Self> {
        let alias = if let Some(name) = reference.strip_prefix("name:") {
            Self::Name(name)
        } else if let Some(qualified) = reference.strip_prefix("host:") {
            let (host, name) = qualified
                .split_once("::")
                .filter(|(host, name)| !host.is_empty() && !name.is_empty())
                .ok_or_else(|| anyhow!(federation_mailbox_alias_error(reference)))?;
            if let Some(suffix) = name.strip_prefix("session-") {
                // Native ids are never names. Only an explicit host UUID can
                // form this typed locator; discovery still needs a current row.
                let host = Uuid::parse_str(host).ok().filter(|id| {
                    !id.is_nil() && id.to_string().eq_ignore_ascii_case(host)
                }).ok_or_else(|| anyhow!(
                    "reserved mailbox identity; native ids require host:<host UUID>::<native session id>"
                ))?;
                if suffix.is_empty() || !federation_native_session_id_valid(name) {
                    bail!("invalid exact native mailbox target");
                }
                required_path_identifier(Some(&Value::String(name.to_owned())), "native session id")?;
                if !matches!(mailbox_reference_handle(name), Ok(None)) {
                    bail!("reserved mailbox reference spelling; resolve its canonical handle");
                }
                return Ok(Self::HostNative { host, session_id: name });
            }
            Self::Host(host, name)
        } else if reference.contains("::") {
            Self::Legacy(reference)
        } else {
            Self::Name(reference)
        };
        let value = match &alias {
            Self::Name(name) => *name,
            Self::Host(_, name) | Self::Legacy(name) => *name,
            Self::HostNative { session_id, .. } => *session_id,
        };
        if value.is_empty() {
            bail!("mailbox alias name is empty; use an exact directory sessionId or a nonempty name:/host: alias");
        }
        if value.starts_with("session-")
            || value.starts_with("domain-session-")
            || value.starts_with("relay-principal-")
            || !matches!(mailbox_reference_handle(value), Ok(None))
        {
            bail!(
                "reserved mailbox identity; use the exact principal id or resolve its canonical reference handle"
            );
        }
        if let Self::Legacy(reference) = &alias {
            if basis.is_none_or(|basis| {
                !basis.complete
                    || basis.hosts.iter().any(|known| {
                        federation_mailbox_host_prefix(reference, &known.id)
                            || federation_mailbox_host_prefix(reference, &known.name)
                    })
            }) {
                bail!(federation_mailbox_alias_error(reference));
            }
        }
        Ok(alias)
    }

    fn matches(&self, host_name: &str, host_id: &str, name: &str, native_session_id: &str) -> bool {
        match self {
            Self::Name(literal) | Self::Legacy(literal) => name.eq_ignore_ascii_case(literal),
            Self::Host(host, literal) => {
                name.eq_ignore_ascii_case(literal)
                    && (host_name.eq_ignore_ascii_case(host) || host_id.eq_ignore_ascii_case(host))
            }
            Self::HostNative { host, session_id } => {
                Uuid::parse_str(host_id).is_ok_and(|row_host| row_host == *host)
                    && native_session_id == *session_id
            }
        }
    }
}

// One discovery selector for all three adapters. Currency, transport and append
// authority remain with their existing owners before/after this pure selection.
struct FederationMailboxAliasRow<'a> {
    id: &'a str,
    host_name: &'a str,
    host_id: &'a str,
    name: &'a str,
    native_session_id: &'a str,
}

fn federation_mailbox_alias_row(row: &FederationMailboxParticipant) -> FederationMailboxAliasRow<'_> {
    FederationMailboxAliasRow {
        id: &row.session_id,
        host_name: &row.host_name,
        host_id: &row.binding.identity.host_id,
        name: &row.name,
        native_session_id: &row.binding.identity.session_id,
    }
}

fn select_federation_mailbox_alias<'a, T>(
    rows: &'a [T],
    reference: &str,
    basis: Option<&FederationMailboxAliasBasis>,
    project: impl for<'r> Fn(&'r T) -> FederationMailboxAliasRow<'r>,
) -> Result<&'a T, ApiError> {
    if !matches!(mailbox_reference_handle(reference), Ok(None)) {
        return Err(ApiError::not_found("mailbox reference requires explicit resolution"));
    }
    if !reference.is_empty() {
        if let Some(exact) = rows.iter().find(|row| project(row).id == reference) {
            return Ok(exact);
        }
    }
    if reference.starts_with("domain-session-") || reference.starts_with("session-") {
        return Err(ApiError::not_found(
            "qualified id required; use the directory sessionId or host:Host::Name",
        ));
    }
    let alias = FederationMailboxAlias::parse(reference, basis)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let legacy = matches!(&alias, FederationMailboxAlias::Legacy(..));
    // Directory row labels may outlive a registry label change. They are also
    // selectors accepted by explicit host: matching, so they must fence legacy
    // literals even when the registry projection came from an older host.
    if legacy && rows.iter().any(|row| {
        let row = project(row);
        federation_mailbox_host_prefix(reference, row.host_name)
            || federation_mailbox_host_prefix(reference, row.host_id)
    }) {
        return Err(ApiError::bad_request(federation_mailbox_alias_error(reference)));
    }
    let mut matches = rows.iter().filter(|row| {
        let row = project(row);
        alias.matches(row.host_name, row.host_id, row.name, row.native_session_id)
    });
    let first = matches.next().ok_or_else(|| {
        if legacy { ApiError::bad_request(federation_mailbox_alias_error(reference)) }
        else { ApiError::not_found("qualified mailbox participant not found") }
    })?;
    if matches.next().is_some() {
        return Err(ApiError::bad_request(federation_mailbox_alias_error(reference)));
    }
    Ok(first)
}

fn federation_mailbox_alias_basis_locked(inner: &StateInner) -> FederationMailboxAliasBasis {
    let mut hosts = Vec::new();
    if let Some(local) = &inner.federation.local {
        hosts.push(FederationMailboxAliasHost {
            id: local.host_id.clone(),
            name: "Central".into(),
        });
    }
    for peer in inner.federation.peers.values() {
        // Retain even unavailable/retired host selectors so a disappearance
        // cannot turn a former qualified spelling into an unrelated literal.
        hosts.push(FederationMailboxAliasHost {
            id: peer.identity.host_id.clone(),
            name: peer.display_name.clone(),
        });
    }
    // Retained row labels fence ambiguity even after their participant retires;
    // they never make that participant eligible for selection.
    for row in inner.federation.mailbox_directory.values() {
        hosts.push(FederationMailboxAliasHost {
            id: row.binding.identity.host_id.clone(),
            name: row.host_name.clone(),
        });
    }
    hosts.sort_by(|left, right| (&left.id, &left.name).cmp(&(&right.id, &right.name)));
    hosts.dedup_by(|left, right| left.id == right.id && left.name == right.name);
    let complete = inner
        .federation
        .peers
        .values()
        .filter(|peer| peer.membership != FederationMembershipState::Revoked)
        .all(|peer| inner.federation.links.contains_key(&peer.identity.host_id));
    FederationMailboxAliasBasis { complete, hosts }
}

impl AppState {
    async fn federation_mailbox_alias_directory(
        &self,
        session: &str,
    ) -> Result<FederationMailboxDirectorySnapshot, ApiError> {
        self.mailbox_directory_snapshot(session).await
    }
}
