// Host-owned admission and public status of the local Codex account operation.
// Credential storage and process shutdown belong to their existing owners.

use super::*;

mod http;
mod operation;
#[cfg(test)]
mod tests;
mod wire;

pub(crate) use http::codex_account_action;
pub(crate) use wire::{CodexAccountSignal, CodexAccountWire};

/// Shown beside the account view after a managed runtime reports
/// Unauthorized. Codex's app-server folds every refresh failure into that one
/// code, so it is a hint, not an account state: admission is unchanged.
const MANAGED_UNAUTHORIZED_NOTICE: &str =
    "Codex account state is unknown — sign in again with /login.";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
enum AccountMode {
    #[default]
    Standalone,
    Enrolling,
    Managed,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum AccountPhase {
    Standalone,
    Pending,
    NeedsLogin,
    SignedIn,
    SignedOut,
    Unknown,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountView {
    mode: AccountMode,
    phase: AccountPhase,
    message: &'static str,
    can_use_standalone: bool,
    /// An informational overlay, never an account phase: it does not gate
    /// dispatch or launches. The next account operation replaces it.
    #[serde(skip_serializing_if = "Option::is_none")]
    notice: Option<&'static str>,
}

impl Default for AccountView {
    fn default() -> Self {
        Self {
            mode: AccountMode::Standalone,
            phase: AccountPhase::Standalone,
            message: "TermAl uses standalone Codex sign-in.",
            can_use_standalone: false,
            notice: None,
        }
    }
}

#[derive(Default)]
struct AccountAdmission {
    operation: Option<AccountOperation>,
    launches: usize,
    retiring: Option<SharedCodexRuntime>,
    retiring_shared: HashMap<String, Arc<SharedChild>>,
    held_workflow: HashMap<String, QueuedDrainOwner>,
    view: AccountView,
    /// Advanced, and saved with the account mode, before every account
    /// action that can change which account Codex uses. A Codex thread
    /// records the generation it was saved under.
    generation: u64,
}

struct AccountOperation {
    id: Uuid,
    signal: Sender<CodexAccountSignal>,
}

#[derive(Default)]
pub(crate) struct CodexAccountControl {
    admission: Mutex<AccountAdmission>,
    #[cfg(test)]
    standalone_home: Mutex<Option<PathBuf>>,
}

/// Counts preparation/spawn through publication to the shared slot or owned
/// registry. The operation refuses while one is counted, without waiting under
/// the host state lock.
pub(crate) struct CodexLaunchAdmission(Arc<CodexAccountControl>);

impl Drop for CodexLaunchAdmission {
    fn drop(&mut self) {
        self.0
            .admission
            .lock()
            .expect("Codex account mutex")
            .launches -= 1;
    }
}

impl CodexAccountControl {
    fn permit_turn(admission: &AccountAdmission) -> Result<(), ApiError> {
        if admission.operation.is_some() {
            return Err(ApiError::conflict(
                "Codex account change is in progress. Wait for the account panel before sending.",
            ));
        }
        match admission.view.phase {
            AccountPhase::Standalone | AccountPhase::SignedIn => Ok(()),
            _ => Err(ApiError::conflict(admission.view.message)),
        }
    }

    pub(crate) fn check_dispatch(&self) -> Result<(), ApiError> {
        Self::permit_turn(&self.admission.lock().expect("Codex account mutex"))
    }

    /// Remember an already triggered workflow activation at the account
    /// fence. The opener drains the same owner after releasing this lock.
    pub(crate) fn workflow_dispatch_is_held(
        &self,
        record: &SessionRecord,
        queued: &QueuedPromptRecord,
        owner: Option<QueuedDrainOwner>,
    ) -> bool {
        let mut admission = self.admission.lock().expect("Codex account mutex");
        if Self::permit_turn(&admission).is_ok() {
            return false;
        }
        if record.is_local_session()
            && matches!(
                queued.source,
                QueuedPromptSource::Mailbox | QueuedPromptSource::Orchestrator
            )
            && !queued_prompt_held_for_durability(record, queued)
            && let Some(owner) =
                owner.or_else(|| QueuedTurnOwner::capture(record).map(QueuedDrainOwner::Activation))
        {
            admission
                .held_workflow
                .insert(record.session.id.clone(), owner);
        }
        true
    }

    pub(crate) fn admit_launch(self: &Arc<Self>) -> Result<CodexLaunchAdmission> {
        let mut admission = self.admission.lock().expect("Codex account mutex");
        Self::permit_turn(&admission).map_err(|error| anyhow!(error.message))?;
        admission.launches += 1;
        Ok(CodexLaunchAdmission(self.clone()))
    }

    /// Test-only: an account action in progress, closing the account fence
    /// until the returned receiver's operation is cleared.
    #[cfg(test)]
    pub(crate) fn hold_operation_for_test(&self) -> mpsc::Receiver<CodexAccountSignal> {
        let (signal, signals) = mpsc::channel();
        self.admission
            .lock()
            .expect("Codex account mutex")
            .operation = Some(AccountOperation {
            id: Uuid::new_v4(),
            signal,
        });
        signals
    }

    pub(crate) fn account_generation(&self) -> u64 {
        self.admission
            .lock()
            .expect("Codex account mutex")
            .generation
    }

    pub(crate) fn uses_standalone(&self) -> bool {
        self.admission
            .lock()
            .expect("Codex account mutex")
            .view
            .mode
            == AccountMode::Standalone
    }

    #[cfg(test)]
    pub(crate) fn fixture_standalone_home(&self) -> Option<PathBuf> {
        self.standalone_home
            .lock()
            .expect("fixture Codex home mutex")
            .clone()
    }

    fn view(&self) -> AccountView {
        self.admission
            .lock()
            .expect("Codex account mutex")
            .view
            .clone()
    }

    pub(crate) fn cancel_on_shutdown(&self) {
        if let Some(operation) = &self
            .admission
            .lock()
            .expect("Codex account mutex")
            .operation
        {
            let _ = operation.signal.send(CodexAccountSignal::Cancel);
        }
    }

    /// Publish before the shared manager removes its slot. The child waiter
    /// removes this handle after observing the exact process exit.
    pub(crate) fn retain_shared_exit(&self, runtime: &SharedCodexRuntime) {
        let mut admission = self.admission.lock().expect("Codex account mutex");
        if runtime.process.try_wait().ok().flatten().is_none() {
            admission
                .retiring_shared
                .insert(runtime.runtime_id.clone(), runtime.process.clone());
        }
    }

    /// A current managed runtime reported Unauthorized. Only a signed-in
    /// managed view outside an account operation gains the notice: every
    /// runtime still current then was launched after the last operation.
    pub(crate) fn note_managed_unauthorized(&self) {
        let mut admission = self.admission.lock().expect("Codex account mutex");
        if admission.operation.is_none()
            && admission.view.mode == AccountMode::Managed
            && matches!(admission.view.phase, AccountPhase::SignedIn)
        {
            admission.view.notice = Some(MANAGED_UNAUTHORIZED_NOTICE);
        }
    }

    pub(crate) fn finish_shared_exit(&self, runtime_id: &str) {
        self.admission
            .lock()
            .expect("Codex account mutex")
            .retiring_shared
            .remove(runtime_id);
    }
}

impl AppState {
    fn begin_codex_account_operation(
        &self,
        operation: AccountOperation,
        restoring: bool,
    ) -> Result<AccountView, ApiError> {
        if self.host_is_shutting_down() {
            return Err(ApiError::conflict(
                "TermAl is shutting down. Reopen the local UI before changing the Codex account.",
            ));
        }
        // All session admissions inspect the same account gate while holding
        // this lock. An already prepared send still owns an Active session.
        let inner = self.inner.lock().expect("state mutex poisoned");
        let mut admission = self
            .codex_account
            .admission
            .lock()
            .expect("Codex account mutex");
        if admission.operation.is_some() {
            return Err(ApiError::conflict(
                "A Codex account action is already running.",
            ));
        }
        if !restoring && matches!(admission.view.phase, AccountPhase::Pending) {
            return Err(ApiError::conflict(
                "TermAl is checking the saved Codex account. Try again after startup.",
            ));
        }
        let has_codex_work = inner.sessions.iter().any(|record| {
            record.session.agent == Agent::Codex
                && matches!(record.remote_proxy_identity(), Ok(None))
                && (matches!(
                    record.session.status,
                    SessionStatus::Active | SessionStatus::Approval | SessionStatus::Stopping
                ) || record.runtime_stop_in_progress)
        });
        if admission.launches != 0 || self.owned_codex_servers.has_unfinished() || has_codex_work {
            return Err(ApiError::conflict(
                "Local Codex work is active or starting. Let it finish before changing accounts.",
            ));
        }
        // Shutdown cancels the running operation under this mutex. Checking
        // the flag here, just before publishing, means shutdown either sees
        // this operation or this reservation sees shutdown.
        if self.host_is_shutting_down() {
            return Err(ApiError::conflict(
                "TermAl is shutting down. Reopen the local UI before changing the Codex account.",
            ));
        }
        let previous = admission.view.clone();
        admission.view.phase = AccountPhase::Pending;
        admission.view.message = "Codex account action is in progress.";
        admission.view.can_use_standalone = false;
        admission.view.notice = None;
        admission.operation = Some(operation);
        Ok(previous)
    }

    pub(crate) fn prepare_codex_account_restore(&self) {
        let saved = operation::load_saved_account(self);
        // A generation never falls below one a saved thread already records;
        // without a readable setting, every saved thread predates the next.
        let stamped = self
            .inner
            .lock()
            .expect("state mutex poisoned")
            .sessions
            .iter()
            .filter_map(|record| record.codex_thread_account_generation)
            .max()
            .unwrap_or(0);
        {
            let mut admission = self
                .codex_account
                .admission
                .lock()
                .expect("Codex account mutex");
            admission.generation = match &saved {
                Ok(saved) => saved.generation.max(stamped),
                Err(_) => stamped.saturating_add(1),
            };
            admission.view = match saved.map(|saved| saved.mode) {
                Ok(AccountMode::Standalone) => AccountView::default(),
                // An unconfirmed first sign-in never strands standalone Codex.
                Ok(AccountMode::Enrolling) => AccountView {
                    mode: AccountMode::Enrolling,
                    phase: AccountPhase::NeedsLogin,
                    message: "Complete a TermAl /login before using Codex or signing out.",
                    can_use_standalone: true,
                    notice: None,
                },
                Ok(AccountMode::Managed) => AccountView {
                    mode: AccountMode::Managed,
                    phase: AccountPhase::Pending,
                    message: "TermAl is checking the saved Codex account.",
                    can_use_standalone: false,
                    notice: None,
                },
                Err(_) => AccountView {
                    mode: AccountMode::Enrolling,
                    phase: AccountPhase::Unknown,
                    message: "TermAl cannot read its Codex account setting. Use /login to establish a managed account.",
                    can_use_standalone: false,
                    notice: None,
                },
            };
        }
    }

    pub(crate) fn restore_codex_account(&self) {
        if !matches!(self.codex_account.view().phase, AccountPhase::Pending) {
            return;
        }
        if operation::restore_on_boot(self).is_err() {
            let mut admission = self
                .codex_account
                .admission
                .lock()
                .expect("Codex account mutex");
            admission.view.phase = AccountPhase::Unknown;
            admission.view.message =
                "TermAl could not start checking the saved Codex account. Open /login to recover.";
        }
    }
}
