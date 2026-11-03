// Owns the bounded reason a source-root capture is unconfirmed, and its
// sentence in the turn-opening notice. It does not decide confirmation:
// `engram_root_capture_explained_locked` (engram_named_root_binding.rs) and
// `engram_authority_work_unresolved_reason` (engram_root_authority.rs) remain
// the only judges, and every variant names exactly one of their branches.
// Sentences name a condition only: no path, store, id or secret.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EngramUnconfirmedRootReason {
    SessionMissing,
    StoreIdentityMissing,
    WorkHistoryMissing,
    WorkHistoryEpoch,
    WorkProofMissing,
    WorkRunsUnresolved,
    WorkTransitionUnpublished,
    WorkJournalReconciliation,
    ClaimProofMissing,
    NamedRootUnknown,
    JournalPending,
    JournalReconciliationUnsettled,
    JournalRetired,
    BoundRootMismatch,
    ConfirmedEventRetired,
    LocalEntryWithoutEvent,
    OpeningWithoutRoot,
    OpeningCapturedUnconfirmed,
}

impl EngramUnconfirmedRootReason {
    /// Why this claim's own root journal needs reconciliation, if it does:
    /// exactly `EngramNamedRootJournal::requires_reconciliation`, by branch.
    fn of_journal(journal: &EngramNamedRootJournal) -> Option<Self> {
        if journal.pending.is_some() {
            return Some(Self::JournalPending);
        }
        if journal.reconciliation.as_ref().is_some_and(|read| !read.settled) {
            return Some(Self::JournalReconciliationUnsettled);
        }
        (journal.obsolete
            && !matches!(
                journal.retirement,
                Some(EngramRootRetirement::Unbound | EngramRootRetirement::Displaced)
            ))
        .then_some(Self::JournalRetired)
    }

    fn sentence(self) -> &'static str {
        match self {
            Self::SessionMissing => "the session is not in the host's state",
            Self::StoreIdentityMissing => "the project has no verified Engram store identity",
            Self::WorkHistoryMissing => "the host has no naming history for this work",
            Self::WorkHistoryEpoch => "the work's naming history is from another history epoch",
            Self::WorkProofMissing => "the work's naming history holds no authority proof",
            Self::WorkRunsUnresolved => {
                "an earlier run's root authority for this work is still unresolved"
            }
            Self::WorkTransitionUnpublished => {
                "the work's latest root-authority transition was not published"
            }
            Self::WorkJournalReconciliation => {
                "a root journal naming this work still needs reconciliation"
            }
            Self::ClaimProofMissing => "no authority proof covers this claim's run",
            Self::NamedRootUnknown => "the session's root state is unknown after a recovery guard",
            Self::JournalPending => "this claim's root journal holds an unsettled naming intent",
            Self::JournalReconciliationUnsettled => {
                "this claim's root journal holds an unsettled canonical read"
            }
            Self::JournalRetired => "this claim's root journal was retired without a settled end",
            Self::BoundRootMismatch => {
                "the session's bound root differs from the claim's selected root"
            }
            Self::ConfirmedEventRetired => "this claim's confirmed naming event was retired",
            Self::LocalEntryWithoutEvent => {
                "the claim's root has a local selection but no confirmed naming event"
            }
            Self::OpeningWithoutRoot => "this turn opened without a captured root",
            Self::OpeningCapturedUnconfirmed => "this turn's root was unconfirmed when it opened",
        }
    }
}
