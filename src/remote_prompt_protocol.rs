//! Route-local direct-prompt codecs, not authority or transport.
//!
//! Decode the collected original bytes. Authentication must cover those bytes,
//! not a reserialized substitute. Successful decoding grants no permission and
//! an envelope's proof remains untrusted until the authenticated host validates
//! it against current native authority. No route is exposed by this module.
//! Provenance: recovered direct-prompt wire and at-rest value codecs.

use std::io::Read;
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};
use serde_json::value::{RawValue, to_raw_value};
use uuid::Uuid;

use super::remote_prompt_authority::{AuthorityBasis, DecisionIdentity, Intent, NativeRoot};

pub(super) const TEXT_BYTES: usize = 65_536;
pub(super) const REQUEST_BYTES: usize = 524_288;
pub(super) const METADATA_BYTES: usize = 32_768;
pub(super) const RESPONSE_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DecodeError {
    RequestTooLarge,
    TextTooLarge,
    MetadataTooLarge,
    ResponseTooLarge,
    InvalidJson,
    InvalidIdentity,
    UnsupportedVersion,
    InvalidUtf8,
    InputIo,
}

fn uuid(value: &str) -> Result<Uuid, DecodeError> {
    let id = Uuid::parse_str(value).map_err(|_| DecodeError::InvalidIdentity)?;
    if id.is_nil() || id.to_string() != value {
        return Err(DecodeError::InvalidIdentity);
    }
    Ok(id)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Root {
    host_id: String,
    host_epoch: u64,
    session_id: String,
    creation_id: String,
}

impl Root {
    fn native(self) -> Result<NativeRoot, DecodeError> {
        NativeRoot::new(
            uuid(&self.host_id)?,
            self.host_epoch,
            self.session_id,
            uuid(&self.creation_id)?,
        )
        .map_err(|_| DecodeError::InvalidIdentity)
    }

    fn from_native(root: &NativeRoot) -> Self {
        let (host, epoch, session, creation) = root.components();
        Self {
            host_id: host.to_string(),
            host_epoch: epoch,
            session_id: session.into(),
            creation_id: creation.to_string(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Decision {
    id: String,
    generation: u64,
}

impl Decision {
    fn identity(self) -> Result<DecisionIdentity, DecodeError> {
        DecisionIdentity::new(uuid(&self.id)?, self.generation)
            .map_err(|_| DecodeError::InvalidIdentity)
    }

    fn from_identity(identity: &DecisionIdentity) -> Self {
        let (id, generation) = identity.components();
        Self {
            id: id.to_string(),
            generation,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct UnverifiedProof {
    link_incarnation: String,
    nonce: String,
    expires_at: u64,
}

impl UnverifiedProof {
    pub(super) fn new(link: Uuid, nonce: Uuid, expires_at: u64) -> Result<Self, DecodeError> {
        let proof = Self {
            link_incarnation: link.to_string(),
            nonce: nonce.to_string(),
            expires_at,
        };
        proof.validate()?;
        Ok(proof)
    }

    fn validate(&self) -> Result<(), DecodeError> {
        uuid(&self.link_incarnation)?;
        uuid(&self.nonce)?;
        if self.expires_at == 0 {
            return Err(DecodeError::InvalidIdentity);
        }
        Ok(())
    }

    pub(super) fn components(&self) -> Result<(Uuid, Uuid, u64), DecodeError> {
        self.validate()?;
        Ok((
            uuid(&self.link_incarnation)?,
            uuid(&self.nonce)?,
            self.expires_at,
        ))
    }
}

// Without a custom deserializer serde treats a missing Option field as None.
// This field is deliberately mandatory: null and omission are not synonyms.
fn required_optional_generation<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReceiveRequest {
    protocol_version: u16,
    source: Root,
    target: Root,
    grant: Decision,
    consent: Decision,
    enrollment_generation: u64,
    #[serde(deserialize_with = "required_optional_generation")]
    placement_generation: Option<u64>,
    reference_id: String,
    idempotency_key: String,
    text: Box<RawValue>,
    proof: UnverifiedProof,
}

#[derive(Debug)]
pub(super) struct DecodedRequest {
    pub(super) intent: Intent,
    pub(super) proof: UnverifiedProof,
    pub(super) retained_wire_bytes: usize,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredIntent {
    schema_version: u8,
    source: Root,
    target: Root,
    grant: Decision,
    consent: Decision,
    enrollment_generation: u64,
    #[serde(deserialize_with = "required_optional_generation")]
    placement_generation: Option<u64>,
    reference_id: String,
    idempotency_key: String,
    text: Box<RawValue>,
}

impl StoredIntent {
    fn intent(self, bytes: usize) -> Result<Intent, DecodeError> {
        if self.schema_version != 1 {
            return Err(DecodeError::UnsupportedVersion);
        }
        let text = decoded_text(&self.text, bytes)?;
        let basis = AuthorityBasis {
            source: self.source.native()?,
            target: self.target.native()?,
            grant: self.grant.identity()?,
            consent: self.consent.identity()?,
            enrollment_generation: NonZeroU64::new(self.enrollment_generation)
                .ok_or(DecodeError::InvalidIdentity)?,
            placement_generation: self
                .placement_generation
                .map(|value| NonZeroU64::new(value).ok_or(DecodeError::InvalidIdentity))
                .transpose()?,
        };
        Intent::new(basis, uuid(&self.reference_id)?, self.idempotency_key, text)
            .map_err(|_| DecodeError::InvalidIdentity)
    }
}

/// A separate versioned at-rest record; transient nonce/link proof is never
/// retained as authority. Validation is identical to the route's durable fields.
pub(super) fn encode_stored_intent(intent: &Intent) -> Result<Vec<u8>, DecodeError> {
    let (basis, reference, key, text) = intent.components();
    let value = StoredIntent {
        schema_version: 1,
        source: Root::from_native(&basis.source),
        target: Root::from_native(&basis.target),
        grant: Decision::from_identity(&basis.grant),
        consent: Decision::from_identity(&basis.consent),
        enrollment_generation: basis.enrollment_generation.get(),
        placement_generation: basis.placement_generation.map(NonZeroU64::get),
        reference_id: reference.to_string(),
        idempotency_key: key.into(),
        text: to_raw_value(text).map_err(|_| DecodeError::InvalidJson)?,
    };
    let bytes = serde_json::to_vec(&value).map_err(|_| DecodeError::InvalidJson)?;
    decode_stored_intent(&bytes)?;
    Ok(bytes)
}

pub(super) fn decode_stored_intent(bytes: &[u8]) -> Result<Intent, DecodeError> {
    if bytes.len() > REQUEST_BYTES {
        return Err(DecodeError::RequestTooLarge);
    }
    let value: StoredIntent =
        serde_json::from_slice(bytes).map_err(|_| DecodeError::InvalidJson)?;
    value.intent(bytes.len())
}

fn decoded_text(raw: &RawValue, envelope_bytes: usize) -> Result<String, DecodeError> {
    // RawValue preserves the complete text-value token, including both quotes,
    // but not the whitespace before/after it. All that whitespace is metadata.
    let metadata = envelope_bytes
        .checked_sub(raw.get().len())
        .ok_or(DecodeError::InvalidJson)?;
    if metadata > METADATA_BYTES {
        return Err(DecodeError::MetadataTooLarge);
    }
    // Decode as String (not Value), strictly rejecting non-string values and
    // invalid Unicode escapes, including isolated UTF-16 surrogate halves.
    let text: String = serde_json::from_str(raw.get()).map_err(|_| DecodeError::InvalidJson)?;
    if text.len() > TEXT_BYTES {
        return Err(DecodeError::TextTooLarge);
    }
    Ok(text)
}

pub(super) fn decode_receive(bytes: &[u8]) -> Result<DecodedRequest, DecodeError> {
    if bytes.len() > REQUEST_BYTES {
        return Err(DecodeError::RequestTooLarge);
    }
    // Typed derives reject unknown and duplicate fields at every object depth;
    // do not deserialize through a map/Value which would erase duplicates.
    let request: ReceiveRequest =
        serde_json::from_slice(bytes).map_err(|_| DecodeError::InvalidJson)?;
    if request.protocol_version != 1 {
        return Err(DecodeError::UnsupportedVersion);
    }
    let text = decoded_text(&request.text, bytes.len())?;
    request.proof.validate()?;
    let enrollment_generation =
        NonZeroU64::new(request.enrollment_generation).ok_or(DecodeError::InvalidIdentity)?;
    let placement_generation = request
        .placement_generation
        .map(|value| NonZeroU64::new(value).ok_or(DecodeError::InvalidIdentity))
        .transpose()?;
    let basis = AuthorityBasis {
        source: request.source.native()?,
        target: request.target.native()?,
        grant: request.grant.identity()?,
        consent: request.consent.identity()?,
        enrollment_generation,
        placement_generation,
    };
    let intent = Intent::new(
        basis,
        uuid(&request.reference_id)?,
        request.idempotency_key,
        text,
    )
    .map_err(|_| DecodeError::InvalidIdentity)?;
    Ok(DecodedRequest {
        intent,
        proof: request.proof,
        retained_wire_bytes: bytes.len(),
    })
}

/// Pure serialization/validation, to be called before any transport counter,
/// MAC or network operation. The returned original bytes are what must be MACed.
pub(super) fn encode_receive(
    intent: &Intent,
    proof: &UnverifiedProof,
) -> Result<Vec<u8>, DecodeError> {
    let (basis, reference, key, text) = intent.components();
    if text.len() > TEXT_BYTES {
        return Err(DecodeError::TextTooLarge);
    }
    proof.validate()?;
    let request = ReceiveRequest {
        protocol_version: 1,
        source: Root::from_native(&basis.source),
        target: Root::from_native(&basis.target),
        grant: Decision::from_identity(&basis.grant),
        consent: Decision::from_identity(&basis.consent),
        enrollment_generation: basis.enrollment_generation.get(),
        placement_generation: basis.placement_generation.map(NonZeroU64::get),
        reference_id: reference.to_string(),
        idempotency_key: key.into(),
        text: to_raw_value(text).map_err(|_| DecodeError::InvalidJson)?,
        proof: proof.clone(),
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| DecodeError::InvalidJson)?;
    decode_receive(&bytes)?;
    Ok(bytes)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnRequest {
    target_reference: String,
    idempotency_key: String,
    text: Box<RawValue>,
}

#[derive(PartialEq, Eq)]
pub(super) struct OwnPrompt {
    pub(super) reference: Uuid,
    pub(super) idempotency_key: String,
    pub(super) text: String,
}

impl std::fmt::Debug for OwnPrompt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnPrompt")
            .field("reference", &self.reference)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

pub(super) fn validate_key(key: &str) -> Result<(), DecodeError> {
    if key.is_empty() || key.len() > 160 || key.chars().any(char::is_control) {
        return Err(DecodeError::InvalidIdentity);
    }
    Ok(())
}

pub(super) fn decode_own(bytes: &[u8]) -> Result<OwnPrompt, DecodeError> {
    if bytes.len() > REQUEST_BYTES {
        return Err(DecodeError::RequestTooLarge);
    }
    let request: OwnRequest =
        serde_json::from_slice(bytes).map_err(|_| DecodeError::InvalidJson)?;
    let reference = request
        .target_reference
        .strip_prefix("prompt-reference-")
        .ok_or(DecodeError::InvalidIdentity)?;
    let reference = uuid(reference)?;
    validate_key(&request.idempotency_key)?;
    Ok(OwnPrompt {
        reference,
        idempotency_key: request.idempotency_key,
        text: decoded_text(&request.text, bytes.len())?,
    })
}

pub(super) fn encode_own(prompt: &OwnPrompt) -> Result<Vec<u8>, DecodeError> {
    if prompt.reference.is_nil() {
        return Err(DecodeError::InvalidIdentity);
    }
    if prompt.text.len() > TEXT_BYTES {
        return Err(DecodeError::TextTooLarge);
    }
    validate_key(&prompt.idempotency_key)?;
    let bytes = serde_json::to_vec(&OwnRequest {
        target_reference: format!("prompt-reference-{}", prompt.reference),
        idempotency_key: prompt.idempotency_key.clone(),
        text: to_raw_value(&prompt.text).map_err(|_| DecodeError::InvalidJson)?,
    })
    .map_err(|_| DecodeError::InvalidJson)?;
    decode_own(&bytes)?;
    Ok(bytes)
}

fn read_bounded<R: Read>(
    reader: R,
    limit: usize,
    overflow: DecodeError,
) -> Result<Vec<u8>, DecodeError> {
    // The caller supplies only these fixed small limits. Stop after one extra
    // byte; neither stdin nor a chunked/lying-length response can grow unbounded.
    let mut bytes = Vec::with_capacity(limit.min(8192));
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| DecodeError::InputIo)?;
    if bytes.len() > limit {
        return Err(overflow);
    }
    Ok(bytes)
}

pub(super) fn read_stdin<R: Read>(reader: R) -> Result<String, DecodeError> {
    let bytes = read_bounded(reader, TEXT_BYTES, DecodeError::TextTooLarge)?;
    String::from_utf8(bytes).map_err(|_| DecodeError::InvalidUtf8)
}

pub(super) fn read_request<R: Read>(reader: R) -> Result<Vec<u8>, DecodeError> {
    read_bounded(reader, REQUEST_BYTES, DecodeError::RequestTooLarge)
}

/// A collection/parse/authentication failure after possible remote entry must
/// be mapped to unknown by the transport caller, never to a safe non-entry.
pub(super) fn read_response<R: Read>(reader: R) -> Result<Vec<u8>, DecodeError> {
    read_bounded(reader, RESPONSE_BYTES, DecodeError::ResponseTooLarge)
}

#[cfg(test)]
#[path = "tests/remote_prompt_protocol.rs"]
mod tests;
