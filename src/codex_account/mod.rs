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
}

impl Default for AccountView {
    fn default() -> Self {
        Self {
            mode: AccountMode::Standalone,
            phase: AccountPhase::Standalone,
            message: "TermAl uses standalone Codex sign-in.",
            can_use_standalone: false,
        }
    }
}

#[derive(Default)]
struct AccountAdmission {
    operation: Option<AccountOperation>,
    launches: usize,
    retiring: Option<SharedCodexRuntime>,
    retiring_shared: HashMap<String, Arc<SharedChild>>,
    view: AccountView,
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

    pub(crate) fn admit_launch(self: &Arc<Self>) -> Result<CodexLaunchAdmission> {
        let mut admission = self.admission.lock().expect("Codex account mutex");
        Self::permit_turn(&admission).map_err(|error| anyhow!(error.message))?;
        admission.launches += 1;
        Ok(CodexLaunchAdmission(self.clone()))
    }

    pub(crate) fn uses_standalone(&self) -> bool {
        self.admission
            .lock()
            .expect("Codex account mutex")
            .view
            .mode
            == AccountMode::Standalone
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
        let previous = admission.view.clone();
        admission.view.phase = AccountPhase::Pending;
        admission.view.message = "Codex account action is in progress.";
        admission.view.can_use_standalone = false;
        admission.operation = Some(operation);
        Ok(previous)
    }

    pub(crate) fn prepare_codex_account_restore(&self) {
        let mode = operation::load_account_mode(self);
        {
            let mut admission = self
                .codex_account
                .admission
                .lock()
                .expect("Codex account mutex");
            admission.view = match mode {
                Ok(AccountMode::Standalone) => AccountView::default(),
                Ok(AccountMode::Enrolling) => AccountView {
                    mode: AccountMode::Enrolling,
                    phase: AccountPhase::NeedsLogin,
                    message: "Complete a TermAl /login before using Codex or signing out.",
                    can_use_standalone: false,
                },
                Ok(AccountMode::Managed) => AccountView {
                    mode: AccountMode::Managed,
                    phase: AccountPhase::Pending,
                    message: "TermAl is checking the saved Codex account.",
                    can_use_standalone: false,
                },
                Err(_) => AccountView {
                    mode: AccountMode::Enrolling,
                    phase: AccountPhase::Unknown,
                    message: "TermAl cannot read its Codex account setting. Use /login to establish a managed account.",
                    can_use_standalone: false,
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
