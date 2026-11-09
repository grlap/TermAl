//! Immutable direct-prompt operation identity values only.
//! Derived from the existing federation/session owner; no permission, registry,
//! session lifecycle, runtime, handoff or acceptance machine lives here.
//! Provenance: recovered value-only direct-prompt foundation.

use std::num::NonZeroU64;

use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct NativeRoot {
    host: Uuid,
    epoch: NonZeroU64,
    session: String,
    creation: Uuid,
}

impl NativeRoot {
    pub(super) fn components(&self) -> (Uuid, u64, &str, Uuid) {
        (self.host, self.epoch.get(), &self.session, self.creation)
    }

    pub(super) fn new(
        host: Uuid,
        epoch: u64,
        session: String,
        creation: Uuid,
    ) -> Result<Self, Refusal> {
        if host.is_nil()
            || creation.is_nil()
            || session.is_empty()
            || session.len() > 160
            || session
                .chars()
                .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '/')
        {
            return Err(Refusal::InvalidIdentity);
        }
        Ok(Self {
            host,
            epoch: NonZeroU64::new(epoch).ok_or(Refusal::InvalidIdentity)?,
            session,
            creation,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DecisionIdentity {
    id: Uuid,
    generation: NonZeroU64,
}

impl DecisionIdentity {
    pub(super) fn components(&self) -> (Uuid, u64) {
        (self.id, self.generation.get())
    }

    pub(super) fn new(id: Uuid, generation: u64) -> Result<Self, Refusal> {
        if id.is_nil() {
            return Err(Refusal::InvalidIdentity);
        }
        Ok(Self {
            id,
            generation: NonZeroU64::new(generation).ok_or(Refusal::InvalidIdentity)?,
        })
    }
}

/// Durable identity excludes link incarnation, proof nonce and expiry. A
/// reconnect can refresh those transport facts, never these permission facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AuthorityBasis {
    pub(super) source: NativeRoot,
    pub(super) target: NativeRoot,
    pub(super) grant: DecisionIdentity,
    pub(super) consent: DecisionIdentity,
    pub(super) enrollment_generation: NonZeroU64,
    pub(super) placement_generation: Option<NonZeroU64>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Intent {
    basis: AuthorityBasis,
    reference: Uuid,
    client_key: String,
    text: String,
}

impl std::fmt::Debug for Intent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Intent")
            .field("basis", &self.basis)
            .field("reference", &self.reference)
            .field("text_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}

impl Intent {
    pub(super) fn components(&self) -> (&AuthorityBasis, Uuid, &str, &str) {
        (&self.basis, self.reference, &self.client_key, &self.text)
    }

    pub(super) fn new(
        basis: AuthorityBasis,
        reference: Uuid,
        client_key: String,
        text: String,
    ) -> Result<Self, Refusal> {
        if reference.is_nil()
            || basis.source.host == basis.target.host
            || client_key.is_empty()
            || client_key.len() > 160
            || client_key.chars().any(char::is_control)
        {
            return Err(Refusal::InvalidIdentity);
        }
        Ok(Self {
            basis,
            reference,
            client_key,
            text,
        })
    }

    /// Logical identity, not an authentication MAC. Length-prefixed components
    /// preserve exact decoded UTF-8, including CR/LF and trailing whitespace.
    pub(super) fn digest(&self) -> [u8; 32] {
        fn field(hash: &mut Sha256, bytes: &[u8]) {
            hash.update((bytes.len() as u64).to_be_bytes());
            hash.update(bytes);
        }
        fn root(hash: &mut Sha256, root: &NativeRoot) {
            field(hash, root.host.as_bytes());
            field(hash, &root.epoch.get().to_be_bytes());
            field(hash, root.session.as_bytes());
            field(hash, root.creation.as_bytes());
        }
        let mut hash = Sha256::new();
        field(&mut hash, b"termal.direct-prompt.intent.v1");
        root(&mut hash, &self.basis.source);
        root(&mut hash, &self.basis.target);
        for decision in [&self.basis.grant, &self.basis.consent] {
            field(&mut hash, decision.id.as_bytes());
            field(&mut hash, &decision.generation.get().to_be_bytes());
        }
        field(
            &mut hash,
            &self.basis.enrollment_generation.get().to_be_bytes(),
        );
        match self.basis.placement_generation {
            Some(generation) => {
                field(&mut hash, b"placement");
                field(&mut hash, &generation.get().to_be_bytes());
            }
            None => field(&mut hash, b"no-placement"),
        }
        field(&mut hash, self.reference.as_bytes());
        field(&mut hash, self.client_key.as_bytes());
        field(&mut hash, self.text.as_bytes());
        hash.finalize().into()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    InvalidIdentity,
}

#[cfg(test)]
#[path = "tests/remote_prompt_authority.rs"]
mod tests;
