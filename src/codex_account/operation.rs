use super::wire::{
    AccountIdentity, Completion, account_rpc, check_account_config, read_account, run_browser_login,
};
use super::*;
use tokio::sync::oneshot;

const NEEDS_ENROLLMENT: &str =
    "This Codex sign-in may come from standalone Codex. Use TermAl /login once before /logout.";

#[derive(Clone, Copy)]
pub(super) enum Action {
    Login,
    Logout,
    Reconcile,
    UseStandalone,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BrowserChallenge {
    pub(super) attempt_id: Uuid,
    pub(super) authorization_url: String,
}

pub(super) type StartReply = Result<Option<BrowserChallenge>, &'static str>;

pub(super) fn start_operation(
    state: &AppState,
    action: Action,
) -> Result<oneshot::Receiver<StartReply>, ApiError> {
    let (run, reply) = reserve_operation(state, action)?;
    tokio::task::spawn_blocking(run);
    Ok(reply)
}

pub(super) fn restore_on_boot(state: &AppState) -> Result<(), ApiError> {
    let (run, _reply) = reserve_operation(state, Action::Reconcile)?;
    run();
    Ok(())
}

fn reserve_operation(
    state: &AppState,
    action: Action,
) -> Result<
    (
        impl FnOnce() + Send + 'static,
        oneshot::Receiver<StartReply>,
    ),
    ApiError,
> {
    let id = Uuid::new_v4();
    let (signal, signals) = mpsc::channel();
    let previous = state.begin_codex_account_operation(
        AccountOperation {
            id,
            signal: signal.clone(),
        },
        matches!(action, Action::Reconcile),
    )?;
    let state = state.clone();
    let (reply, receiver) = oneshot::channel();
    let run = move || {
        let mut reply = Some(reply);
        let mut rejected = None;
        let view = match change_account(&state, action, &previous, id, signal, signals, &mut reply)
        {
            Ok(view) => view,
            Err(error) => {
                rejected = Some(error);
                let mode = state.codex_account.view().mode;
                let has_unreaped_server = {
                    let admission = state
                        .codex_account
                        .admission
                        .lock()
                        .expect("Codex account mutex");
                    admission.retiring.is_some() || !admission.retiring_shared.is_empty()
                };
                if !has_unreaped_server
                    && (error == NEEDS_ENROLLMENT
                        || matches!(action, Action::UseStandalone)
                        || mode == AccountMode::Standalone)
                {
                    AccountView {
                        message: error,
                        ..previous
                    }
                } else {
                    AccountView {
                        mode,
                        phase: if mode == AccountMode::Enrolling {
                            AccountPhase::NeedsLogin
                        } else {
                            AccountPhase::Unknown
                        },
                        message: error,
                        can_use_standalone: false,
                    }
                }
            }
        };
        let mut admission = state
            .codex_account
            .admission
            .lock()
            .expect("Codex account mutex");
        admission.view = view;
        admission.operation = None;
        if let Some(reply) = reply {
            let _ = reply.send(rejected.map_or(Ok(None), Err));
        }
    };
    Ok((run, receiver))
}

fn account_mode_path(state: &AppState) -> PathBuf {
    state.persistence_path.with_extension("codex-account.json")
}

pub(super) fn load_account_mode(state: &AppState) -> Result<AccountMode> {
    match fs::read(account_mode_path(state)) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(AccountMode::Standalone),
        Err(error) => Err(error.into()),
    }
}

fn save_account_mode(state: &AppState, mode: AccountMode) -> Result<(), &'static str> {
    let path = account_mode_path(state);
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, &mode)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return Err("TermAl could not save the Codex account setting. Retry the account action.");
    }
    state
        .codex_account
        .admission
        .lock()
        .expect("Codex account mutex")
        .view
        .mode = mode;
    Ok(())
}

fn profiles() -> &'static [SharedCodexProfile] {
    if cfg!(windows) {
        &SharedCodexProfile::ALL
    } else {
        &SharedCodexProfile::ALL[..1]
    }
}

fn change_account(
    state: &AppState,
    action: Action,
    previous: &AccountView,
    attempt: Uuid,
    signal: Sender<CodexAccountSignal>,
    signals: mpsc::Receiver<CodexAccountSignal>,
    reply: &mut Option<oneshot::Sender<StartReply>>,
) -> Result<AccountView, &'static str> {
    if matches!(action, Action::Logout) && previous.mode != AccountMode::Managed {
        return Err(NEEDS_ENROLLMENT);
    }
    if matches!(action, Action::UseStandalone) && !previous.can_use_standalone {
        return Err("Sign out with TermAl /logout before returning to standalone Codex sign-in.");
    }
    if !matches!(action, Action::UseStandalone) {
        refuse_aliased_homes(state)?;
    }
    retire_session_servers(state)?;
    if matches!(action, Action::UseStandalone) {
        save_account_mode(state, AccountMode::Standalone)?;
        state.owned_codex_servers.invalidate_prepared_homes();
        return Ok(AccountView::default());
    }
    // Check every destination's effective storage before the canonical grant
    // can change. A non-file profile must never receive a file mirror.
    for &profile in &profiles()[1..] {
        with_account_server(state, profile, CodexAccountWire::default(), |server| {
            check_account_config(state, server)
        })?;
    }
    let completion = if matches!(action, Action::Reconcile) {
        None
    } else {
        let wire = if matches!(action, Action::Login) {
            CodexAccountWire::for_login(signal)
        } else {
            CodexAccountWire::default()
        };
        Some(with_account_server(
            state,
            SharedCodexProfile::Default,
            wire,
            |server| {
                check_account_config(state, server)?;
                read_account(state, server)?;
                match action {
                    Action::Login => {
                        if previous.mode == AccountMode::Standalone {
                            save_account_mode(state, AccountMode::Enrolling)?;
                        }
                        run_browser_login(state, server, attempt, signals, reply)
                    }
                    Action::Logout => {
                        let result = account_rpc(state, server, "account/logout", Value::Null);
                        Ok(if result.is_ok() {
                            Completion::LogoutAcknowledged
                        } else {
                            Completion::Unconfirmed
                        })
                    }
                    _ => unreachable!("only login and logout mutate the provider"),
                }
            },
        )?)
    };
    let canonical = verify_profile(state, SharedCodexProfile::Default)?;
    if let Some(Completion::LoginConfirmed(selected)) = &completion
        && canonical.as_ref() != Some(selected)
    {
        return Err(
            "A fresh Codex process did not load the account selected by /login. Retry sign-in.",
        );
    }
    let signed_in = canonical.is_some();
    let canonical_home = codex_profile_home(state, SharedCodexProfile::Default);
    if signed_in
        != managed_codex_home_has_grant(&canonical_home).map_err(
            |_| "TermAl could not verify the managed Codex file store. Retry the account action.",
        )?
    {
        return Err(
            "Codex account readback disagrees with its managed file store. Retry the account action.",
        );
    }
    let mut mode = state.codex_account.view().mode;
    if mode == AccountMode::Enrolling {
        if !matches!(completion, Some(Completion::LoginConfirmed(_))) || !signed_in {
            return Err("TermAl sign-in was not confirmed. Use /login to finish enrollment.");
        }
        save_account_mode(state, AccountMode::Managed)?;
        mode = AccountMode::Managed;
    }
    for &profile in &profiles()[1..] {
        reconcile_managed_codex_home(
            &canonical_home,
            &codex_profile_home(state, profile),
            signed_in,
        )
        .map_err(|_| "Codex profile synchronization did not finish. Retry the account action.")?;
        if verify_profile(state, profile)? != canonical {
            return Err(
                "A Codex profile disagrees with the default account. Retry the account action.",
            );
        }
    }
    let message = match (action, completion, signed_in) {
        (Action::Login, Some(Completion::Cancelled), true) => {
            "Login was cancelled. Codex reports that it is signed in."
        }
        (Action::Login, Some(Completion::Cancelled), false) => {
            "Login was cancelled. Codex reports that it is signed out."
        }
        (Action::Login, Some(Completion::Unconfirmed), true) => {
            "The new login was not confirmed. Codex reports that it is signed in. Retry /login to switch."
        }
        (Action::Login, _, false) => "Codex sign-in was not confirmed. Use /login to try again.",
        (Action::Logout, _, true) => "Codex is still signed in. Retry /logout.",
        (Action::Logout, Some(Completion::Unconfirmed), false) => {
            "Codex did not acknowledge logout. Fresh TermAl profiles report that they are signed out."
        }
        (_, _, true) => "Codex is signed in across TermAl profiles.",
        (_, _, false) => "Codex is signed out. Use /login before sending a Codex prompt.",
    };
    Ok(AccountView {
        mode,
        phase: if signed_in {
            AccountPhase::SignedIn
        } else {
            AccountPhase::SignedOut
        },
        message,
        can_use_standalone: !signed_in,
    })
}

fn retire_session_servers(state: &AppState) -> Result<(), &'static str> {
    let previous = state
        .codex_account
        .admission
        .lock()
        .expect("Codex account mutex")
        .retiring
        .take();
    if let Some(server) = previous {
        reap_account_server(state, server)?;
    }
    for &profile in profiles() {
        if let Some(server) = state.running_shared_codex_runtime(profile) {
            let retired = state.handle_shared_codex_runtime_exit(&server.runtime_id, None);
            reap_account_server(state, server)?;
            retired.map_err(|_| "TermAl could not retire its idle Codex session attachments.")?;
        }
    }
    let retiring = state
        .codex_account
        .admission
        .lock()
        .expect("Codex account mutex")
        .retiring_shared
        .clone();
    for (runtime_id, process) in retiring {
        match wait_for_shared_child_exit_timeout(
            &process,
            Duration::from_secs(5),
            "retiring Codex runtime",
        ) {
            Ok(Some(_)) => state.codex_account.finish_shared_exit(&runtime_id),
            _ => {
                return Err(
                    "A previous Codex app-server is still retiring. Retry the account action when it exits.",
                );
            }
        }
    }
    Ok(())
}

fn refuse_aliased_homes(state: &AppState) -> Result<(), &'static str> {
    #[cfg(test)]
    let standalone = state
        .codex_account
        .standalone_home
        .lock()
        .expect("fixture Codex home mutex")
        .clone()
        .unwrap_or_else(|| PathBuf::from(&state.default_workdir).join("fixture-standalone"));
    #[cfg(not(test))]
    let standalone = resolve_source_codex_home_dir()
        .map_err(|_| "TermAl could not resolve the standalone Codex home.")?;
    validate_managed_codex_homes(
        &standalone,
        profiles()
            .iter()
            .map(|profile| codex_profile_home(state, *profile)),
    )
}

fn with_account_server<T>(
    state: &AppState,
    profile: SharedCodexProfile,
    wire: CodexAccountWire,
    run: impl FnOnce(&SharedCodexRuntime) -> Result<T, &'static str>,
) -> Result<T, &'static str> {
    let server = spawn_codex_account_runtime(state, profile, wire)
        .map_err(|_| "TermAl could not start Codex for the account action. Retry the action.")?;
    let result = run(&server);
    reap_account_server(state, server)?;
    result
}

fn reap_account_server(state: &AppState, server: SharedCodexRuntime) -> Result<(), &'static str> {
    let _ = server.kill();
    match wait_for_shared_child_exit_timeout(
        &server.process,
        Duration::from_secs(5),
        "Codex account runtime",
    ) {
        Ok(Some(_)) => Ok(()),
        _ => {
            state
                .codex_account
                .admission
                .lock()
                .expect("Codex account mutex")
                .retiring = Some(server);
            Err("A Codex process has not closed. Retry the account action before sending work.")
        }
    }
}

fn verify_profile(
    state: &AppState,
    profile: SharedCodexProfile,
) -> Result<Option<AccountIdentity>, &'static str> {
    with_account_server(state, profile, CodexAccountWire::default(), |server| {
        check_account_config(state, server)?;
        read_account(state, server)
    })
}
