// Exact direct-prompt association owned by the existing primary receipt.
// No queue, lifecycle, retry loop or retirement authority is owned here.

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemotePromptPrimaryAssociation {
    identity: remote_prompt_authority::RetainedIdentity,
}

impl Serialize for RemotePromptPrimaryAssociation {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        let bytes = remote_prompt_protocol::encode_stored_identity(&self.identity)
            .map_err(|_| serde::ser::Error::custom("invalid retained prompt identity"))?;
        let raw = serde_json::value::RawValue::from_string(
            String::from_utf8(bytes)
                .map_err(|_| serde::ser::Error::custom("invalid retained prompt encoding"))?,
        )
        .map_err(serde::ser::Error::custom)?;
        raw.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RemotePromptPrimaryAssociation {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        // Keep the original object bytes so duplicate keys cannot be erased
        // by an intermediate Value before the strict codec checks them.
        let raw = Box::<serde_json::value::RawValue>::deserialize(deserializer)?;
        let identity = remote_prompt_protocol::decode_stored_identity(raw.get().as_bytes())
            .map_err(|_| serde::de::Error::custom("invalid retained prompt identity"))?;
        Ok(Self { identity })
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl RemotePromptPrimaryAssociation {
    fn new(intent: &remote_prompt_authority::Intent) -> Result<Self> {
        remote_prompt_protocol::encode_stored_intent(intent)
            .map_err(|_| remote_prompt_invalid_record())?;
        Ok(Self {
            identity: intent.retained_identity(),
        })
    }

    fn operation_id(&self) -> Uuid {
        let (basis, _, key, _) = self.identity.components();
        remote_prompt_operation_id_for_source(&basis.source, key)
    }

    fn prompt_id(&self) -> String {
        remote_prompt_stable_id(
            b"termal.direct-prompt.queue.v1",
            &[self.operation_id().as_bytes()],
        )
        .to_string()
    }
}

// Called only while the primary owner holds StateInner, after current native
// authority admission. A retained association is immutable across receipt
// settlement, display pruning and reload; it never recreates a missing entry.
#[cfg_attr(not(test), allow(dead_code))]
fn associate_remote_prompt_receipt(
    record: &mut SessionRecord,
    intent: &remote_prompt_authority::Intent,
) -> Result<bool> {
    let association = RemotePromptPrimaryAssociation::new(intent)?;
    let (basis, _, _, text) = intent.components();
    let (_, _, target, creation) = basis.target.components();
    if record.session.id != target || record.federation_native_creation_id != creation.to_string() {
        return Err(remote_prompt_journal_conflict());
    }
    let prompt_id = association.prompt_id();
    let delivery = record
        .prompt_deliveries
        .get(&prompt_id)
        .ok_or_else(remote_prompt_journal_conflict)?;
    if let Some(existing) = &delivery.remote_prompt {
        return if existing == &association {
            Ok(false)
        } else {
            Err(remote_prompt_journal_conflict())
        };
    }
    if delivery.federation_operation.is_some()
        || delivery.disposition != PromptDeliveryDisposition::Pending
        || delivery.attempt != 0
        || delivery.turn_generation.is_some()
        || !record
            .queued_prompt_durability_holds
            .get(&prompt_id)
            .is_some_and(|hold| {
                record
                    .queued_prompts
                    .iter()
                    .find(|queued| queued.pending_prompt.id == prompt_id)
                    .is_some_and(|queued| hold.content == queued_prompt_durability_content(queued))
            })
        || !record.queued_prompts.iter().any(|queued| {
            queued.pending_prompt.id == prompt_id
                && queued.pending_prompt.text == text
                && queued.pending_prompt.expanded_text.is_none()
                && queued.attachments.is_empty()
                && queued.pending_prompt.attachments.is_empty()
                && queued.source == QueuedPromptSource::Orchestrator
        })
    {
        return Err(remote_prompt_journal_conflict());
    }
    record
        .prompt_deliveries
        .get_mut(&prompt_id)
        .expect("receipt checked")
        .remote_prompt = Some(association);
    Ok(true)
}
