// Host-private settlement on the ORIGINAL mailbox send row. No local run,
// separate completion registry, public message field or human ACK is created.
// Each target retains the typed request (one frame) AND its exact wire string
// (up to two frames after JSON string escaping). Reserve another frame each
// for the frozen connection and the actual receipt. The separate completion
// can retain two full SourceId endpoints even when no request fits a frame.
// The encoded aggregate is still checked before every save and decode.
const CAUSAL_SETTLEMENT_MAX_BYTES: usize = MAILBOX_TEST_REQUEST_MAX_TARGETS
    * (5 * ENGRAM_CONTROL_MAX_FRAME_BYTES)
    + 2 * ENGRAM_SOURCE_JSON_LIMIT
    + MAILBOX_TEST_REQUEST_MAX_BYTES;

// These codes live in the existing bounded pending string. Older readers
// still accept that string, and this projection never exports arbitrary text.
#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CausalAdmissionCheck {
    #[serde(alias = "destination connection has not been read")]
    DestinationNotChecked,
    SenderScopeUnavailable,
    RequesterRootUnavailable,
    RequesterCreationChanged,
    RequesterHostChanged,
    DestinationTargetUnavailable,
    DestinationTargetInvalid,
    DestinationProjectChanged,
    DestinationStoreChanged,
    DestinationSessionChanged,
    #[serde(alias = "original destination connection changed; no route replacement")]
    DestinationConnectionChanged,
    DestinationSnapshotChanged,
    CapabilityReadFailed,
    CapabilityUnavailable,
    #[serde(alias = "original destination connection unavailable")]
    DestinationUnconfirmed,
    LocalConnectionUnavailable,
    AdmissionDeadline,
    AdmissionTransportFailed,
    AdmissionProtocolFailed,
    #[serde(alias = "remote verification deferred; retry original operation")]
    AdmissionDeferred,
    #[serde(alias = "unconfirmed malformed remote receipt")]
    AdmissionReceiptInvalid,
    #[serde(other)]
    AdmissionUnconfirmed,
}

impl CausalAdmissionCheck {
    fn stored(self) -> String {
        let Value::String(code) = json!(self) else {
            unreachable!("causal admission checks serialize as strings");
        };
        code
    }

    fn release_condition(self) -> &'static str {
        match self {
            Self::DestinationNotChecked => "original_target_is_checked",
            Self::SenderScopeUnavailable => "original_sender_scope_available",
            Self::RequesterRootUnavailable => "original_requester_root_available",
            Self::RequesterCreationChanged => "requester_creation_matches_original",
            Self::RequesterHostChanged => "requester_host_matches_original",
            Self::DestinationTargetUnavailable => "destination_target_available",
            Self::DestinationTargetInvalid => "destination_target_resolves",
            Self::DestinationProjectChanged => "destination_project_matches_original",
            Self::DestinationStoreChanged => "destination_store_matches_original",
            Self::DestinationSessionChanged => "destination_session_matches_original",
            Self::DestinationConnectionChanged => "destination_connection_matches_original",
            Self::DestinationSnapshotChanged => {
                "destination_snapshot_stable_during_capability_read"
            }
            Self::CapabilityReadFailed => "destination_capability_read_succeeds",
            Self::CapabilityUnavailable => "destination_advertises_remote_verification",
            Self::DestinationUnconfirmed => "original_destination_resolves",
            Self::LocalConnectionUnavailable => "original_admission_connection_available",
            Self::AdmissionDeadline
            | Self::AdmissionTransportFailed
            | Self::AdmissionProtocolFailed
            | Self::AdmissionDeferred
            | Self::AdmissionReceiptInvalid
            | Self::AdmissionUnconfirmed => "original_operation_returns_valid_receipt",
        }
    }
}

fn project_causal_admission_checks(state: &mut Value, encoded: &str) {
    // Skip the completion, wire, request and receipt bodies during decoding.
    // This is explanatory metadata only, never settlement/receipt validation.
    #[derive(Deserialize)]
    struct PendingAdmission {
        pending: Option<CausalAdmissionCheck>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct PendingSettlement {
        schema_version: u16,
        admissions: Vec<PendingAdmission>,
    }
    let Ok(saved) = serde_json::from_str::<PendingSettlement>(encoded) else {
        return;
    };
    if saved.schema_version != 1 || saved.admissions.len() > MAILBOX_TEST_REQUEST_MAX_TARGETS {
        return;
    }
    let checks = saved
        .admissions
        .iter()
        .enumerate()
        .filter_map(|(index, admission)| admission.pending.map(|check| (index + 1, check)));
    if let Some((_, check)) = checks
        .clone()
        .find(|(_, check)| *check != CausalAdmissionCheck::DestinationNotChecked)
        .or_else(|| checks.clone().next())
    {
        state["check"] = json!(check);
        state["releaseCondition"] = json!(check.release_condition());
        state["admissionChecks"] = json!(checks.map(|(target, check)| {
            json!({"target":target, "check":check, "releaseCondition":check.release_condition()})
        }).collect::<Vec<_>>());
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CausalTestAdmission {
    request: Option<engram_remote_verification::Request>,
    wire: Option<String>,
    connection: Option<EngramConnectionConfig>,
    receipt: Option<Value>,
    refusal: Option<String>,
    pending: Option<String>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CausalTestSettlement {
    schema_version: u16,
    completion: native_test_completion::Completion,
    admissions: Vec<CausalTestAdmission>,
}

impl std::fmt::Debug for CausalTestSettlement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CausalTestSettlement(<host-private>)")
    }
}

fn causal_test_admission_request(
    selector: &CausalTestCompletionSelector,
    scope: Option<&MailboxTestSenderScope>,
    completion: &native_test_completion::Completion,
    index: usize,
) -> Result<engram_remote_verification::Request, ApiError> {
    use engram_remote_verification::{Association, IntendedUse, Purpose, Request};
    let target = &selector.payload.targets[index];
    let captured = target.association.as_ref().ok_or_else(|| {
        ApiError::conflict("association_stale: original request has no captured association")
    })?;
    let scope = scope.ok_or_else(|| {
        ApiError::conflict("producer_unsupported: original destination mapping is unavailable")
    })?;
    let intended_use = match &target.target.r#use {
        MailboxTestInputUse::CommittedInput {
            expected_commit,
            purpose,
        } => IntendedUse::CommittedInput {
            expected_commit: expected_commit.clone(),
            purpose: match purpose {
                MailboxTestPurpose::Verification => Purpose::Verification,
                MailboxTestPurpose::LandingGate => Purpose::LandingGate,
            },
        },
        MailboxTestInputUse::AuthorizedTestRef {
            ref_name,
            expected_tree,
            ..
        } => IntendedUse::AuthorizedTestRef {
            ref_name: ref_name.clone(),
            expected_tree: expected_tree.clone(),
            purpose: Purpose::LandingGate,
        },
    };
    // Facts do not enter the key: a changed terminal cannot manufacture a new
    // operation. The original key and body survive retries and reordering.
    let key = sha256_hex(
        &serde_json::to_vec(&(
            "termal-remote-v1",
            selector,
            scope,
            &completion.origin,
            &completion.run_id,
            index,
        ))
        .map_err(|_| ApiError::internal("remote operation identity encoding failed"))?,
    );
    Request::new(
        format!("termal-remote-{key}"),
        completion,
        Association {
            binding: captured.binding.clone(),
            root: captured.root.clone(),
            intended_use,
        },
    )
}

impl CausalTestSettlement {
    fn new(delivery: &CausalTestCompletionDelivery) -> Self {
        let admissions = (0..delivery.selector.payload.targets.len())
            .map(|index| {
                let built = causal_test_admission_request(
                    &delivery.selector,
                    delivery.scope.as_ref(),
                    &delivery.completion,
                    index,
                )
                .and_then(|request| {
                    encode_engram_control_request(&request.clone().into_control_request())
                        .map_err(|_| {
                            ApiError::bad_request(
                                "invalid_request: remote request exceeds the control frame",
                            )
                        })?;
                    Ok(request)
                });
                match built {
                    Ok(request) => CausalTestAdmission {
                        wire: Some(
                            String::from_utf8(
                                encode_engram_control_request(
                                    &request.clone().into_control_request(),
                                )
                                .expect("validated frame"),
                            )
                            .expect("JSON UTF-8"),
                        ),
                        request: Some(request),
                        connection: None,
                        receipt: None,
                        refusal: None,
                        pending: Some(CausalAdmissionCheck::DestinationNotChecked.stored()),
                    },
                    Err(error) => CausalTestAdmission {
                        request: None,
                        wire: None,
                        connection: None,
                        receipt: None,
                        refusal: Some(error.message),
                        pending: None,
                    },
                }
            })
            .collect();
        Self {
            schema_version: 1,
            completion: delivery.completion.clone(),
            admissions,
        }
    }

    fn complete(&self) -> bool {
        self.admissions
            .iter()
            .all(|a| a.receipt.is_some() || a.refusal.is_some())
    }

    fn validate(
        &self,
        selector: &CausalTestCompletionSelector,
        scope: Option<&MailboxTestSenderScope>,
    ) -> Result<()> {
        selector
            .validate()
            .map_err(|_| anyhow!("invalid original causal request"))?;
        self.completion
            .validate()
            .map_err(|_| anyhow!("invalid retained causal completion"))?;
        let runner = selector
            .runner()
            .map_err(|_| anyhow!("original request has no runner"))?;
        if self.schema_version != 1
            || !self.completion.authoritative_terminal()
            || !matches!(
                self.completion.input,
                native_test_completion::Input::SourceId(_)
            )
            || self.completion.origin.host_id != runner.host.host_id
            || self.completion.origin.runner_session_id != runner.native_session_id
            || !matches!(
                (selector.payload.mode, self.completion.mode),
                (MailboxTestMode::Full, native_test_completion::Mode::Full)
                    | (
                        MailboxTestMode::Focused,
                        native_test_completion::Mode::Focused
                    )
            )
            || self.admissions.len() != selector.payload.targets.len()
            || serde_json::to_vec(self)?.len() > CAUSAL_SETTLEMENT_MAX_BYTES
        {
            bail!("causal settlement differs from its original request");
        }
        for (index, admission) in self.admissions.iter().enumerate() {
            let expected =
                causal_test_admission_request(selector, scope, &self.completion, index).ok();
            if admission.request != expected
                || admission.wire.as_deref()
                    != admission
                        .request
                        .as_ref()
                        .and_then(|r| {
                            encode_engram_control_request(&r.clone().into_control_request()).ok()
                        })
                        .as_deref()
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                || admission.receipt.is_some()
                    && (admission.refusal.is_some() || admission.pending.is_some())
                || admission.refusal.is_some() && admission.pending.is_some()
                || admission
                    .refusal
                    .as_ref()
                    .is_some_and(|why| why.is_empty() || why.len() > 512)
                || admission
                    .pending
                    .as_ref()
                    .is_some_and(|why| why.is_empty() || why.len() > 512)
                || admission.connection.as_ref().is_some_and(|connection| {
                    connection.session_id != selector.payload.requester.native_session_id
                        || connection.project_root.as_os_str().is_empty()
                        || connection.project_file.as_os_str().is_empty()
                        || connection.home.as_os_str().is_empty()
                        || connection.binary_path.as_os_str().is_empty()
                })
            {
                bail!("invalid original causal settlement admission");
            }
            if let Some(receipt) = &admission.receipt {
                let request = admission
                    .request
                    .as_ref()
                    .ok_or_else(|| anyhow!("receipt without original request"))?;
                request
                    .receipt(receipt.clone())
                    .map_err(|_| anyhow!("invalid original remote receipt"))?;
                if admission.connection.is_none() {
                    bail!("receipt without original connection");
                }
            } else if admission.request.is_none() && admission.refusal.is_none() {
                bail!("unprepared causal settlement without refusal");
            }
        }
        Ok(())
    }

    fn decode(
        encoded: &str,
        selector: &CausalTestCompletionSelector,
        scope: Option<&MailboxTestSenderScope>,
    ) -> Result<Self> {
        if encoded.len() > CAUSAL_SETTLEMENT_MAX_BYTES {
            bail!("causal settlement exceeds its bound");
        }
        let value: Value = serde_json::from_str(encoded)?;
        let saved: Self = serde_json::from_value(value.clone())?;
        if serde_json::to_value(&saved)? != value {
            bail!("unsupported causal settlement shape");
        }
        saved.validate(selector, scope)?;
        Ok(saved)
    }
}

impl MailboxStore {
    // Reads and writes use the original owner/key, refusing an ambiguous UNION.
    // A valid saved receipt needs no current claim or connection to be read.
    fn causal_test_settlement(
        &self,
        selector: &CausalTestCompletionSelector,
        scope: Option<&MailboxTestSenderScope>,
    ) -> Result<Option<CausalTestSettlement>> {
        let connection = self.connection()?;
        let (_, _, _, encoded) = causal_settlement_row(&connection, selector, scope)?;
        encoded
            .map(|s| CausalTestSettlement::decode(&s, selector, scope))
            .transpose()
    }

    fn save_causal_test_settlement(
        &self,
        selector: &CausalTestCompletionSelector,
        scope: Option<&MailboxTestSenderScope>,
        before: Option<&CausalTestSettlement>,
        after: &CausalTestSettlement,
    ) -> Result<()> {
        after.validate(selector, scope)?;
        if let Some(before) = before {
            if before.completion != after.completion
                || before.admissions.len() != after.admissions.len()
                || before
                    .admissions
                    .iter()
                    .zip(&after.admissions)
                    .any(|(a, b)| {
                        a.request != b.request
                            || a.wire != b.wire
                            || a.connection.is_some() && a.connection != b.connection
                            || a.receipt.is_some() && a != b
                            || a.refusal.is_some() && a != b
                    })
            {
                bail!("original remote operation or receipt cannot be replaced");
            }
        }
        let mut connection = self.connection()?;
        let transaction =
            connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (table, owner, key, encoded) = causal_settlement_row(&transaction, selector, scope)?;
        let current = encoded
            .as_deref()
            .map(|s| CausalTestSettlement::decode(s, selector, scope))
            .transpose()?;
        if current.as_ref() != before {
            bail!("causal settlement changed before publication");
        }
        let complete = after.complete();
        // Publish the admission phase with the first incomplete settlement.
        // The worker can stop before its separate attempt outcome is recorded.
        let admission_phase = if before.is_none() && !complete {
            let metadata = transaction.query_row(
                &format!(
                    "SELECT test_request_pending_json FROM {table}
                     WHERE {owner}=?1 AND idempotency_key=?2"
                ),
                rusqlite::params![selector.payload.requester.native_session_id, key],
                |row| CausalPendingMetadata::from_column(row, 0),
            )?;
            match metadata {
                CausalPendingMetadata::Readable(mut state) => {
                    state.reason = CausalPendingReason::AdmissionPending;
                    state.legacy_custody_digest = None;
                    Some(serde_json::to_string(&state)?)
                }
                CausalPendingMetadata::Retired | CausalPendingMetadata::Malformed => None,
            }
        } else {
            None
        };
        let text = serde_json::to_string(after)?;
        let count = transaction.execute(&format!(
            "UPDATE {table} SET test_request_settlement_json=?1,
             test_request_pending_json=CASE WHEN ?4 THEN NULL ELSE COALESCE(?5,test_request_pending_json) END,
             test_request_pending_order=CASE WHEN ?4 THEN NULL ELSE test_request_pending_order END
             WHERE {owner}=?2 AND idempotency_key=?3"
        ), rusqlite::params![text, selector.payload.requester.native_session_id, key, complete, admission_phase])?;
        if count != 1 {
            bail!("original send disappeared before settlement");
        }
        let (_, _, _, readback) = causal_settlement_row(&transaction, selector, scope)?;
        if readback.as_deref() != Some(text.as_str()) {
            bail!("causal settlement readback changed");
        }
        transaction.commit()?;
        Ok(())
    }
}

// The strings substituted into SQL are fixed table/column names, never input.
fn causal_settlement_row(
    connection: &rusqlite::Connection,
    selector: &CausalTestCompletionSelector,
    scope: Option<&MailboxTestSenderScope>,
) -> Result<(&'static str, &'static str, String, Option<String>)> {
    let mut statement = connection.prepare(
        "SELECT test_request_json,test_request_scope_json,test_request_settlement_json,0,id FROM mailbox_messages
         WHERE sender_session_id=?1 AND idempotency_key=?2
         UNION ALL SELECT test_request_json,test_request_scope_json,test_request_settlement_json,1,receipt_json FROM mailbox_relay_intents
         WHERE owner_session_id=?1 AND idempotency_key=?2 LIMIT 2")?;
    let rows = statement
        .query_map(
            rusqlite::params![
                selector.payload.requester.native_session_id,
                selector.payload.request_send_key
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() != 1 {
        bail!("missing or ambiguous original send settlement owner");
    }
    let (payload, mapping, encoded, relay, canonical) = &rows[0];
    let canonical = if *relay {
        serde_json::from_str::<MailboxAppendReceipt>(canonical)?.message_id
    } else {
        canonical.clone()
    };
    if canonical != selector.canonical_message_id {
        bail!("original message receipt changed before settlement");
    }
    if serde_json::from_str::<MailboxTestPayload>(payload)? != selector.payload
        || mapping
            .as_deref()
            .map(serde_json::from_str::<MailboxTestSenderScope>)
            .transpose()?
            .as_ref()
            != scope
    {
        bail!("original test-request custody changed before settlement");
    }
    Ok((
        if *relay {
            "mailbox_relay_intents"
        } else {
            "mailbox_messages"
        },
        if *relay {
            "owner_session_id"
        } else {
            "sender_session_id"
        },
        selector.payload.request_send_key.clone(),
        encoded.clone(),
    ))
}
