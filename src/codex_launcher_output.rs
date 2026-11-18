// Bounded, transient RUN-locator ingress for an already recognised foreground
// launcher. Native thread/turn/item routing and the original observation
// supply identity. A mediated foreground check keeps its own output fence.
// Output never starts/ends a check or supplies verification credit.
// All artifact/path validation stays in test_run_artifacts.rs.

struct CodexLauncherOutput {
    runtime: RuntimeToken,
    generation: u64,
    check: Option<EngramTurnCheck>,
    observation: Option<TestRunCommandObservation>,
    command: EngramCheckCommand,
    key: String,
    reported_cwd: Option<String>,
    line: String,
    skipping_line: bool,
    locator: Option<PathBuf>,
    invalid: bool,
}

impl CodexLauncherOutput {
    fn same_start(&self, check: &EngramTurnCheck) -> bool {
        self.check.as_ref().is_some_and(|original| {
            original.host_turn_ref == check.host_turn_ref
                && original.sequence == check.sequence
                && original.key == check.key
                && original.started_at == check.started_at
                && original.command == check.command
                && original.target.root == check.target.root
                && Arc::ptr_eq(&original.start_basis, &check.start_basis)
        })
    }

    fn same_origin(&self, other: &Self) -> bool {
        self.runtime == other.runtime
            && self.generation == other.generation
            && self.command == other.command
            && self.key == other.key
            // A replay must agree with the first routed start. Comparing only
            // the retained observation would compare its original cwd to itself.
            && self.reported_cwd == other.reported_cwd
            && match (&self.check, &other.check) {
                (Some(_), Some(check)) => self.same_start(check),
                (None, None) => true,
                _ => false,
            }
            && match (&self.observation, &other.observation) {
                (Some(original), Some(current)) => original.same_start(current),
                (None, None) => true,
                _ => false,
            }
    }

    fn append(&mut self, delta: &str) {
        if self.invalid {
            return;
        }
        if delta.len() > 256 * 1024 {
            self.invalid = true;
            return;
        }
        for character in delta.chars() {
            if character == '\n' {
                if !self.skipping_line {
                    let line = self.line.trim();
                    let path = line
                        .strip_prefix("RUN ")
                        .or_else(|| line.strip_prefix("STARTED "))
                        .map(PathBuf::from)
                        .or_else(|| {
                            line.strip_prefix("results: ").and_then(|path| {
                                let path = FsPath::new(path);
                                (path.file_name()?.to_str()? == "results.json")
                                    .then(|| path.parent().map(FsPath::to_path_buf))
                                    .flatten()
                            })
                        });
                    if let Some(path) = path {
                        if !path.is_absolute()
                            || path.to_string_lossy().len() > TEST_RUN_PATH_MAX_BYTES
                            || engram_network_path(&path.to_string_lossy())
                            || self
                                .locator
                                .as_ref()
                                .is_some_and(|previous| previous != &path)
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
                    if self.line.starts_with("RUN ")
                        || self.line.starts_with("STARTED ")
                        || self.line.starts_with("results: ")
                    {
                        self.invalid = true;
                    }
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
        &self,
        session: &str,
        runtime: &RuntimeToken,
        generation: Option<u64>,
        key: &str,
        line: &str,
        cwd: Option<&str>,
    ) -> Option<CodexLauncherOutput> {
        if cwd.is_some_and(|cwd| cwd.len() > TEST_RUN_PATH_MAX_BYTES) {
            return None;
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        let record = &inner.sessions[inner.find_session_index(session)?];
        if record.runtime.runtime_token().as_ref() != Some(runtime)
            || generation != Some(record.active_turn_generation)
        {
            return None;
        }
        let command = EngramHost::recognized_check(line)?;
        if !command.simple
            || command.literal_pipe
            || EngramHost::launcher_arguments(&command).is_none()
        {
            return None;
        }
        let turn = runtime_observation_check_owner(record);
        let mut checks = record.engram.active_turn_checks.iter().filter(|check| {
            check.key == key && Some(check.host_turn_ref.as_str()) == turn && check.end.is_none()
        });
        let check = checks.next();
        if let Some(check) = check
            && (checks.next().is_some()
                || check.command != command
                || check.keyed_by_line
                || check.runtime_output_cut
                || !EngramHost::is_nonterminal_launcher_reply(
                    &check.command.normalized,
                    Some(EngramCommandExit::NotFinished),
                ))
        {
            return None;
        }
        // Both branches use only the original runtime/turn/item observation
        // for artifact association. A check may still need its output fence
        // when there is no attachable observation.
        let provenance = EngramObservationProvenance::Runtime {
            token: runtime.clone(),
            turn_generation: record.active_turn_generation,
        };
        let runtime_identity = test_run_runtime_identity(record, &provenance);
        let tool = sha256_hex(key.as_bytes());
        let fingerprint = EngramHost::check_fingerprint(&command);
        let execution_id = test_run_execution_id(
            session,
            &runtime_identity,
            record.active_turn_generation,
            &tool,
        );
        let observation = inner
            .test_run_observations
            .get(&execution_id)
            .filter(|row| {
                row.summary.owner_session_id.as_deref() == Some(session)
                    && row.runtime_identity == runtime_identity
                    && row.runtime_identity.is_some()
                    && row.turn_generation == record.active_turn_generation
                    && row.tool_identity == tool
                    && row.command_fingerprint == fingerprint
                    && row.summary.ended_at.is_none()
                    && row.placement_refusal.is_none()
            })
            .cloned();
        if check.is_none() && observation.is_none() {
            return None;
        }
        Some(CodexLauncherOutput {
            runtime: runtime.clone(),
            generation: record.active_turn_generation,
            check: check.cloned(),
            observation,
            command,
            key: key.to_owned(),
            reported_cwd: cwd.map(str::to_owned),
            line: String::new(),
            skipping_line: false,
            locator: None,
            invalid: false,
        })
    }

    fn observe_codex_launcher_output(
        &self,
        session: &str,
        runtime: &RuntimeToken,
        generation: Option<u64>,
        output: &mut CodexLauncherOutput,
        delta: &str,
    ) {
        // Rejected runtime/turn ingress cannot modify even the transient line
        // buffer belonging to a different original start.
        if &output.runtime != runtime || generation != Some(output.generation) {
            return;
        }
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session) else {
                return;
            };
            let record = &mut inner.sessions[index];
            if record.runtime.runtime_token().as_ref() != Some(runtime)
                || generation != Some(record.active_turn_generation)
            {
                return;
            }
            if output.check.is_some() {
                let turn = runtime_observation_check_owner(record).map(str::to_owned);
                let Some(check) = record.engram.active_turn_checks.iter_mut().find(|check| {
                    output.same_start(check)
                        && check.end.is_none()
                        && turn.as_deref() == Some(check.host_turn_ref.as_str())
                }) else {
                    return;
                };
                output.append(delta);
                if output.invalid {
                    check.runtime_output_cut = true;
                }
                if check.runtime_output_cut {
                    return;
                }
            } else {
                let Some(original) = output.observation.as_ref() else {
                    return;
                };
                if output.invalid
                    || inner
                        .test_run_observations
                        .get(&original.execution_id)
                        .is_none_or(|row| {
                            !original.same_start(row)
                                || row.summary.ended_at.is_some()
                                || row.placement_refusal.is_some()
                        })
                {
                    return;
                }
                output.append(delta);
                if output.invalid {
                    return;
                }
            }
        }
        let (Some(path), Some(observation)) =
            (output.locator.as_ref(), output.observation.as_ref())
        else {
            return;
        };
        let provenance = EngramObservationProvenance::Runtime {
            token: runtime.clone(),
            turn_generation: output.generation,
        };
        self.associate_test_command_artifact(
            session,
            &provenance,
            &output.key,
            &output.command.normalized,
            &format!("RUN {}", test_run_display_path(path)),
            Some(&TestRunArtifactStreamStart {
                runtime,
                observation,
            }),
        );
    }
}
