// Owns the worker transport basis and cancellation-independent settlement lifetime.
// Does not grant authority or own persistence/SSH. Split from federation_link.rs
// and federation_runtime.rs so detached completions retain the same worker fence.

use super::*;

pub(crate) struct FederationTransportBasis {
    pub(crate) snapshot: RwLock<FederationPeerSnapshot>,
    pub(crate) settling: AtomicU64,
}

impl FederationTransportBasis {
    pub(crate) fn new(snapshot: FederationPeerSnapshot) -> Self {
        Self {
            snapshot: RwLock::new(snapshot),
            settling: AtomicU64::new(0),
        }
    }

    pub(crate) fn read(
        &self,
    ) -> std::sync::LockResult<std::sync::RwLockReadGuard<'_, FederationPeerSnapshot>> {
        self.snapshot.read()
    }

    pub(crate) fn write(
        &self,
    ) -> std::sync::LockResult<std::sync::RwLockWriteGuard<'_, FederationPeerSnapshot>> {
        self.snapshot.write()
    }

    pub(crate) fn is_settling(&self) -> bool {
        self.settling.load(Ordering::Acquire) != 0
    }

    pub(crate) fn begin_settlement(self: &Arc<Self>) -> FederationBasisSettlement {
        self.settling.fetch_add(1, Ordering::AcqRel);
        FederationBasisSettlement(self.clone())
    }
}

pub(crate) struct FederationBasisSettlement(pub(crate) Arc<FederationTransportBasis>);

impl Drop for FederationBasisSettlement {
    fn drop(&mut self) {
        self.0.settling.fetch_sub(1, Ordering::AcqRel);
    }
}
