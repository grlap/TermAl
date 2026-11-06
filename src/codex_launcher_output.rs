// Bounded, transient RUN-locator ingress for an already recognised foreground
// launcher. Native thread/turn/item routing and the original check supply
// identity; output never starts/ends a check or supplies verification credit.
// All artifact/path validation stays in test_run_artifacts.rs.

struct CodexLauncherOutput {
    check: EngramTurnCheck,
    line: String,
    skipping_line: bool,
    locator: Option<PathBuf>,
    invalid: bool,
}

impl CodexLauncherOutput {
    fn same_start(&self, check: &EngramTurnCheck) -> bool {
        self.check.host_turn_ref == check.host_turn_ref
            && self.check.sequence == check.sequence && self.check.key == check.key
            && self.check.started_at == check.started_at && self.check.command == check.command
            && self.check.target.root == check.target.root
            && Arc::ptr_eq(&self.check.start_basis, &check.start_basis)
    }

    fn append(&mut self, delta: &str) {
        if self.invalid { return; }
        if delta.len() > 256 * 1024 { self.invalid = true; return; }
        for character in delta.chars() {
            if character == '\n' {
                if !self.skipping_line {
                    let line = self.line.trim();
                    let path = line.strip_prefix("RUN ").or_else(|| line.strip_prefix("STARTED "))
                        .map(PathBuf::from)
                        .or_else(|| line.strip_prefix("results: ").and_then(|path| {
                            let path = FsPath::new(path);
                            (path.file_name()?.to_str()? == "results.json")
                                .then(|| path.parent().map(FsPath::to_path_buf)).flatten()
                        }));
                    if let Some(path) = path {
                        if !path.is_absolute() || path.to_string_lossy().len() > TEST_RUN_PATH_MAX_BYTES
                            || engram_network_path(&path.to_string_lossy())
                            || self.locator.as_ref().is_some_and(|previous| previous != &path)
                        {
                            self.invalid = true;
                        } else {
                            self.locator = Some(path);
                        }
                    }
                }
                self.line.clear();
                self.skipping_line = false;
            } else if !self.skipping_line {
                if self.line.len() + character.len_utf8() > TEST_RUN_PATH_MAX_BYTES + 16 {
                    if self.line.starts_with("RUN ") || self.line.starts_with("STARTED ")
                        || self.line.starts_with("results: ") { self.invalid = true; }
                    self.line.clear();
                    self.skipping_line = true;
                } else {
                    self.line.push(character);
                }
            }
        }
    }
}

impl AppState {
    fn capture_codex_launcher_output(
        &self, session: &str, runtime: &RuntimeToken, generation: Option<u64>, key: &str,
    ) -> Option<CodexLauncherOutput> {
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session)?];
        if record.runtime.runtime_token().as_ref() != Some(runtime)
            || generation != Some(record.active_turn_generation) { return None; }
        let turn = runtime_observation_check_owner(record)?;
        let mut checks = record.engram.active_turn_checks.iter().filter(|check|
            check.key == key && check.host_turn_ref == turn && check.end.is_none());
        let check = checks.next()?;
        if checks.next().is_some() || check.keyed_by_line || check.runtime_output_cut
            || !EngramHost::is_nonterminal_launcher_reply(
                &check.command.normalized, Some(EngramCommandExit::NotFinished)) { return None; }
        Some(CodexLauncherOutput { check: check.clone(), line: String::new(),
            skipping_line: false, locator: None, invalid: false })
    }

    fn observe_codex_launcher_output(
        &self, session: &str, runtime: &RuntimeToken, generation: Option<u64>,
        output: &mut CodexLauncherOutput, delta: &str,
    ) {
        output.append(delta);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session) else { return; };
            let record = &mut inner.sessions[index];
            if record.runtime.runtime_token().as_ref() != Some(runtime)
                || generation != Some(record.active_turn_generation) { return; }
            let turn = runtime_observation_check_owner(record).map(str::to_owned);
            let Some(check) = record.engram.active_turn_checks.iter_mut().find(|check|
                output.same_start(check) && check.end.is_none()
                    && turn.as_deref() == Some(check.host_turn_ref.as_str())) else { return; };
            if output.invalid { check.runtime_output_cut = true; }
            if check.runtime_output_cut { return; }
        }
        let Some(path) = output.locator.as_ref() else { return; };
        self.observe_test_command_artifact(session, &EngramObservationProvenance::Ambient,
            &output.check.key, &output.check.command.normalized,
            &format!("RUN {}", test_run_display_path(path)), false);
    }
}
