// Bounded publication records owned by the existing review-result mailbox.
// A candidate MAC proves authorship; only the publication proof grants custody.
const REVIEW_CUSTODY_SLOT_KIND: &str = "delegation-review-terminal-slot/v1";

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewPublicationProof {
    result_id: String,
    predecessor_result_id: Option<String>,
    ordinal: u32,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReviewCustodySlot {
    schema_version: u32,
    kind: String,
    parent_session_id: String,
    delegation_id: String,
    child_session_id: String,
    slot: u32,
    submission_attempt: u32,
    candidate: Option<SignedReviewAttempt>,
    publication: Option<ReviewPublicationProof>,
}

fn encode_review_value<T: Serialize>(value: &T, key: &str) -> Result<String, ApiError> {
    let payload = serde_json::to_string(value).map_err(|_| review_attempt_corrupt())?;
    let tag = review_attempt_mac(key, &payload)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    serde_json::to_string(&SignedReviewAttempt {
        payload,
        authentication_tag: tag,
    })
    .map_err(|_| review_attempt_corrupt())
}

fn decode_review_value<T: serde::de::DeserializeOwned>(
    body: &str,
    key: &str,
) -> Result<T, ApiError> {
    let signed: SignedReviewAttempt =
        serde_json::from_str(body).map_err(|_| review_attempt_corrupt())?;
    if signed.authentication_tag.len() != 64 {
        return Err(review_attempt_corrupt());
    }
    let tag = signed
        .authentication_tag
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok())
                .ok_or_else(review_attempt_corrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    federation_verify_mac(
        key.as_bytes(),
        &[
            REVIEW_ATTEMPT_TOPIC.as_bytes(),
            b"\0",
            signed.payload.as_bytes(),
        ],
        &tag,
    )
    .map_err(|_| review_attempt_corrupt())?;
    serde_json::from_str(&signed.payload).map_err(|_| review_attempt_corrupt())
}

fn review_candidate_id(candidate: &SignedReviewAttempt) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(candidate).map_err(|_| review_attempt_corrupt())?;
    Ok(format!("review-result-{:x}", Sha256::digest(bytes)))
}

fn review_slot_prefix(delegation: &str) -> String {
    format!("{REVIEW_ATTEMPT_KEY_PREFIX}-{delegation}-")
}

const REVIEW_CUSTODY_KEYS_BY_CHILD_SQL: &str =
    "SELECT sender_session_id, idempotency_key FROM mailbox_messages
     WHERE sender_session_id=?1 AND idempotency_key>=?2 AND idempotency_key<?3
       AND topic=?4 ORDER BY idempotency_key LIMIT 65";

const REVIEW_CUSTODY_KEYS_WITHOUT_CHILD_SQL: &str =
    "SELECT sender_session_id, idempotency_key FROM mailbox_messages
     WHERE ?1 IS NULL AND topic=?4 AND idempotency_key>=?2 AND idempotency_key<?3
     ORDER BY idempotency_key, sender_session_id LIMIT 65";

fn review_slot_rows_on(
    connection: &rusqlite::Connection,
    child: Option<&str>,
    delegation: &str,
) -> Result<Vec<MailboxIdempotencyRecord>> {
    let prefix = review_slot_prefix(delegation);
    let upper = format!("{REVIEW_ATTEMPT_KEY_PREFIX}-{delegation}.");
    let keys = {
        let mut query = connection.prepare(if child.is_some() {
            REVIEW_CUSTODY_KEYS_BY_CHILD_SQL
        } else {
            REVIEW_CUSTODY_KEYS_WITHOUT_CHILD_SQL
        })?;
        let rows = query.query_map(
            rusqlite::params![child, prefix, upper, REVIEW_ATTEMPT_TOPIC],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if keys.len() > MAX_RETAINED_REVIEW_ATTEMPTS as usize {
        return Err(mailbox_store_error(
            MailboxStoreErrorKind::Conflict,
            "review retention capacity is exhausted; no new custody slot was committed",
        ));
    }
    keys.into_iter()
        .map(|(sender, key)| {
            let record = mailbox_message_for_idempotency_key(connection, &sender, &key)?
                .ok_or_else(|| anyhow!("review custody row disappeared"))?;
            Ok(record)
        })
        .collect()
}

fn decode_review_slots(
    records: &[MailboxIdempotencyRecord],
    parent: &str,
    delegation: &str,
    child: &str,
    policy: &DelegationWritePolicy,
    key: &str,
) -> Result<Vec<ReviewCustodySlot>, ApiError> {
    decode_review_slots_with_chain(records, parent, delegation, child, policy, key, true)
}

fn decode_review_slots_with_chain(
    records: &[MailboxIdempotencyRecord],
    parent: &str,
    delegation: &str,
    child: &str,
    policy: &DelegationWritePolicy,
    key: &str,
    validate_chain: bool,
) -> Result<Vec<ReviewCustodySlot>, ApiError> {
    let mut slots = Vec::new();
    for record in records {
        let slot: ReviewCustodySlot = decode_review_value(&record.body, key)?;
        if record.body.len() > MAX_MAILBOX_BODY_BYTES
            || slot.schema_version != 1
            || slot.kind != REVIEW_CUSTODY_SLOT_KIND
            || slot.parent_session_id != parent
            || slot.delegation_id != delegation
            || slot.child_session_id != child
            || slot.slot == 0
            || slot.slot > MAX_RETAINED_REVIEW_ATTEMPTS
            || slot.submission_attempt == 0
            || slot.submission_attempt > MAX_RETAINED_REVIEW_ATTEMPTS
            || record.target_session_id != parent
            || record.topic.as_deref() != Some(REVIEW_ATTEMPT_TOPIC)
            || record.idempotency_key != review_attempt_key(delegation, slot.slot)
            || record.state_stamp.as_deref() != Some(format!("{delegation}:{}", slot.slot).as_str())
            || slots
                .iter()
                .any(|existing: &ReviewCustodySlot| existing.slot == slot.slot)
        {
            return Err(review_attempt_corrupt());
        }
        if let Some(candidate) = &slot.candidate {
            let body = serde_json::to_string(candidate).map_err(|_| review_attempt_corrupt())?;
            let snapshot: ReviewAttemptSnapshot = decode_review_value(&body, key)?;
            validate_review_snapshot(
                &snapshot,
                parent,
                delegation,
                child,
                slot.submission_attempt,
                policy,
            )?;
            if slot.publication.as_ref().is_some_and(|proof| {
                proof.ordinal == 0
                    || proof.ordinal > MAX_RETAINED_REVIEW_ATTEMPTS
                    || review_candidate_id(candidate).ok().as_ref() != Some(&proof.result_id)
            }) {
                return Err(review_attempt_corrupt());
            }
        } else if slot.publication.is_some() {
            return Err(review_attempt_corrupt());
        }
        slots.push(slot);
    }
    if slots
        .iter()
        .filter(|slot| slot.candidate.is_some() && slot.publication.is_none())
        .count()
        > 1
    {
        return Err(review_attempt_corrupt());
    }
    if !validate_chain {
        return Ok(slots);
    }
    let mut proofs = slots
        .iter()
        .filter_map(|slot| slot.publication.as_ref())
        .collect::<Vec<_>>();
    proofs.sort_by_key(|proof| proof.ordinal);
    let mut predecessor = None;
    for (index, proof) in proofs.into_iter().enumerate() {
        if proof.ordinal as usize != index + 1
            || proof.predecessor_result_id.as_ref() != predecessor
        {
            return Err(review_attempt_corrupt());
        }
        predecessor = Some(&proof.result_id);
    }
    Ok(slots)
}

fn review_publication_head(slots: &[ReviewCustodySlot]) -> Option<&ReviewPublicationProof> {
    slots
        .iter()
        .filter_map(|slot| slot.publication.as_ref())
        .max_by_key(|proof| proof.ordinal)
}

fn review_capacity_error() -> ApiError {
    ApiError::conflict(
        "review retention capacity is exhausted; preserve the current result and use a new delegation",
    )
}

impl MailboxStore {
    // Caller owns delegation_review_recovery_guard, acquired before StateInner.
    // The same writer transaction checks the slot budget after inserting a row.
    fn append_review_custody_slot(
        &self,
        input: &MailboxAppendInput,
        delegation: &str,
    ) -> Result<()> {
        let existing =
            self.read_idempotent_message(&input.sender_session_id, &input.idempotency_key)?;
        let result = if existing.is_none() {
            Some(self.append_with_delivery_policy_inner(
                input,
                None,
                None,
                || Ok(()),
                |transaction, _| {
                    let _ = review_slot_rows_on(transaction, Some(&input.sender_session_id), delegation)?;
                    Ok(())
                },
                true,
            ))
        } else {
            None
        };
        // Resolve an ambiguous reply by the identical key and bytes. Finalize
        // every read-back, including a retry after a finalization failure.
        let saved =
            self.read_idempotent_message(&input.sender_session_id, &input.idempotency_key)?;
        let Some(saved) = saved else {
            return Err(result
                .and_then(Result::err)
                .unwrap_or_else(|| anyhow!("review custody append is unconfirmed")));
        };
        if saved.body != input.body
            || saved.target_session_id != input.target_session_id
            || saved.topic != input.topic
            || saved.state_stamp != input.state_stamp
        {
            bail!("review custody append conflicts with existing immutable routing or bytes");
        }
        self.record_initial_dispatch_outcome(&saved.message_id, "durableButNotWoken")?;
        Ok(())
    }

    fn replace_review_custody_slot(
        &self,
        saved: &MailboxIdempotencyRecord,
        old: &ReviewCustodySlot,
        new: &ReviewCustodySlot,
        policy: &DelegationWritePolicy,
        key: &str,
        expected_head: Option<&str>,
    ) -> Result<String, ApiError> {
        let body = encode_review_value(new, key)?;
        if body.len() > MAX_MAILBOX_BODY_BYTES {
            return Err(ApiError::conflict(
                "complete review candidate plus publication proof exceeds 256 KiB",
            ));
        }
        if old.publication.is_some() {
            return Err(review_attempt_corrupt());
        }
        if new.parent_session_id != old.parent_session_id
            || new.delegation_id != old.delegation_id
            || new.child_session_id != old.child_session_id
            || new.slot != old.slot
            || new.submission_attempt != old.submission_attempt
        {
            return Err(review_attempt_corrupt());
        }
        let _writer = self
            .lock_writer("publishing review custody")
            .map_err(mailbox_api_error)?;
        let mut connection = self.connection().map_err(mailbox_api_error)?;
        let transaction = begin_mailbox_write(&mut connection, "publishing review custody")
            .map_err(mailbox_api_error)?;
        let records = review_slot_rows_on(&transaction, Some(&old.child_session_id), &old.delegation_id)
            .map_err(mailbox_api_error)?;
        let slots = decode_review_slots(
            &records,
            &old.parent_session_id,
            &old.delegation_id,
            &old.child_session_id,
            policy,
            key,
        )?;
        if review_publication_head(&slots).map(|head| head.result_id.as_str()) != expected_head {
            return Err(ApiError::conflict(
                "review publication predecessor changed; retry the unchanged request",
            ));
        }
        if transaction
            .execute(
                "UPDATE mailbox_messages SET body=?3 WHERE id=?1 AND body=?2",
                rusqlite::params![saved.message_id, saved.body, body],
            )
            .map_err(|error| mailbox_api_error(error.into()))?
            != 1
        {
            return Err(ApiError::conflict(
                "review pending candidate changed; stale publication was refused",
            ));
        }
        let after = review_slot_rows_on(&transaction, Some(&new.child_session_id), &new.delegation_id)
            .map_err(mailbox_api_error)?;
        decode_review_slots(
            &after,
            &new.parent_session_id,
            &new.delegation_id,
            &new.child_session_id,
            policy,
            key,
        )?;
        let commit_error = transaction.commit().err().map(anyhow::Error::from);
        drop(connection);
        drop(_writer);
        #[cfg(test)]
        let lost_reply = if new.publication.is_some() {
            TEST_REVIEW_CUSTODY_AFTER_CONFIRM_COMMIT
                .with(|slot| slot.borrow_mut().take())
                .is_some_and(|hook| hook())
        } else if old.candidate.is_some() && new.candidate.is_none() {
            TEST_REVIEW_CUSTODY_AFTER_NEUTRALIZE_COMMIT
                .with(|slot| slot.borrow_mut().take())
                .is_some_and(|hook| hook())
        } else {
            false
        };
        #[cfg(test)]
        let commit_error = if lost_reply {
            Some(anyhow!(
                "injected lost reply after the real custody commit"
            ))
        } else {
            commit_error
        };
        if let Some(error) = commit_error {
            let resolved = self
                .read_idempotent_message(
                    &new.child_session_id,
                    &review_attempt_key(&new.delegation_id, new.slot),
                )
                .map_err(mailbox_api_error)?;
            if !resolved
                .is_some_and(|record| record.message_id == saved.message_id && record.body == body)
            {
                return Err(mailbox_api_error(error));
            }
        }
        Ok(body)
    }
}
