// Final local disposition for completed generic runs whose commands have no
// native check classification. Reuses registration custody and requester
// feedback; never grants test credit, sends a producer record or adopts a
// declaration that was not captured on the original native execution.

use super::*;

pub(crate) fn unclassified_verification_detail(stages: &[String]) -> String {
    let names = stages
        .iter()
        .take(8)
        .map(|stage| {
            stage
                .chars()
                .take(48)
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                        ch
                    } else {
                        '?'
                    }
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(", ");
    let more = stages.len().saturating_sub(8);
    let suffix = if more == 0 {
        String::new()
    } else {
        format!(" (+{more} more)")
    };
    format!(
        "unclassified_stages: completed run has no recognized check for {names}{suffix}; \
        native verification was not granted. Use a recognized test/build command, or the \
        declared-command native route with its original host-captured declaration and test \
        artifact. A stage label or later declaration cannot credit this run; it will not be sent."
    )
}

#[derive(Debug)]
pub(crate) struct EngramLocalRefusalPresentation {
    pub(crate) registration: TestRunRegistration,
    notice_done: bool,
    mirror_done: bool,
}

impl EngramLocalRefusalPresentation {
    pub(crate) fn finished(&self) -> bool {
        self.notice_done && self.mirror_done
    }
}

impl EngramVerificationRecordCursor {
    pub(crate) fn repair_local_refusal(
        &mut self,
        state: &AppState,
        registration: &TestRunRegistration,
    ) -> bool {
        let presentation = self
            .local_refusals
            .entry(registration.run_id.clone())
            .or_insert_with(|| EngramLocalRefusalPresentation {
                registration: registration.clone(),
                notice_done: false,
                mirror_done: false,
            });
        if presentation.registration != *registration {
            *presentation = EngramLocalRefusalPresentation {
                registration: registration.clone(),
                notice_done: false,
                mirror_done: false,
            };
        }
        state.repair_unclassified_verification_presentation(registration, presentation)
    }
}

impl AppState {
    /// Called only for terminal preparation's UnclassifiedStages. The original
    /// registration is checked again before the final disposition: an arriving
    /// native end, a held receipt or another owner cannot be overwritten by an
    /// earlier snapshot. A failed local persistence enqueue leaves it retryable.
    pub(crate) fn reject_unclassified_verification_record(
        &self,
        registration: &TestRunRegistration,
        stages: &[String],
    ) -> bool {
        if registration.declared_native.is_some()
            || stages.is_empty()
            || registration.verification_record.is_some()
        {
            return false;
        }
        // A retained body may already have been sent without a conclusive
        // answer. Preserve that exact retry/reconciliation custody even if
        // someone subsequently changed the run's classification artifacts.
        if !matches!(fs::symlink_metadata(FsPath::new(&registration.run_directory)
            .join(ENGRAM_VERIFICATION_RECORD_BODY_FILE)),
            Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            return false;
        }
        // Re-read the run off-lock. A replaced request, a now-recognized stage
        // or nonterminal results must not inherit the earlier refusal.
        if !matches!(engram_verification_record_prepare(registration),
            EngramVerificationRecordPrepared::Pending(
                EngramVerificationRecordPending::UnclassifiedStages { stages: current }
            ) if current == stages)
        {
            return false;
        }
        let detail = unclassified_verification_detail(stages);
        {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !inner.test_runs.registered.contains(&registration.run_id)
                || inner.test_run_registrations.get(&registration.run_id) != Some(registration)
            {
                return false;
            }
            inner
                .test_run_registrations
                .update_unindexed_fields(&registration.run_id, |row| {
                    row.verification_record = Some(TestRunVerificationAck::LocalUnclassified {
                        detail: detail.clone(),
                    });
                });
            if let Err(error) = self.persist_internal_locked(&inner) {
                inner
                    .test_run_registrations
                    .update_unindexed_fields(&registration.run_id, |row| {
                        row.verification_record = None;
                    });
                eprintln!(
                    "engram> run={} local verification refusal not persisted: {error:#}",
                    registration.run_id
                );
                return false;
            }
            // When it became final, for a run wait on a run the index has not
            // listed (test_run_waits.rs).
            let generation = inner.test_run_scan_generation;
            inner
                .test_run_record_final_at_scan
                .entry(registration.run_id.clone())
                .or_insert(generation);
        }
        true
    }

    /// Informational mirrors are retryable independently of the final refusal.
    /// The sender cursor remembers success only in this process, so reload
    /// requeues the original requester's notice even if the sidecar survived.
    fn repair_unclassified_verification_presentation(
        &self,
        registration: &TestRunRegistration,
        presentation: &mut EngramLocalRefusalPresentation,
    ) -> bool {
        let Some(TestRunVerificationAck::LocalUnclassified { detail }) =
            &registration.verification_record
        else {
            return false;
        };
        {
            let inner = self.inner.lock().expect("state mutex poisoned");
            if !inner.test_runs.registered.contains(&registration.run_id)
                || inner.test_run_registrations.get(&registration.run_id) != Some(registration)
            {
                return false;
            }
            // Deleting an original session ends its presentation work; never
            // redirect the notice or retry a vanished recipient forever.
            if inner
                .find_session_index(&registration.owning_session_id)
                .is_none()
            {
                presentation.notice_done = true;
                presentation.mirror_done = true;
                return true;
            }
        }
        let run_dir = FsPath::new(&registration.run_directory);
        // Worktree cleanup removes its Git-admin run directory. An absent
        // destination ends mirror work; other filesystem errors stay retryable.
        let absent = match fs::metadata(run_dir) {
            Ok(metadata) => !metadata.is_dir(),
            Err(error) => error.kind() == io::ErrorKind::NotFound,
        };
        let progress = EngramVerificationRecordProgress::Rejected {
            detail: detail.clone(),
        };
        let mirrored = presentation.mirror_done || absent;
        let mirrored = if mirrored {
            true
        } else {
            engram_verification_record_set_progress(
                run_dir,
                &registration.run_id,
                progress.clone(),
            );
            engram_verification_record_progress(run_dir) == Some(progress)
        };
        let line = format!(
            "{ENGRAM_CHECK_CREDIT_LINE_PREFIX} registered launcher run {}: {detail}",
            registration.run_id
        );
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        if !inner.test_runs.registered.contains(&registration.run_id)
            || inner.test_run_registrations.get(&registration.run_id) != Some(registration)
        {
            return false;
        }
        // A missing original session cannot redirect this notice elsewhere.
        let Some(index) = inner.find_session_index(&registration.owning_session_id) else {
            presentation.notice_done = true;
            presentation.mirror_done = true;
            return true;
        };
        let queued = !presentation.notice_done;
        if queued {
            inner.sessions[index]
                .engram
                .set_pending_source_root_line(line.clone());
            presentation.notice_done = true;
        }
        presentation.mirror_done = mirrored;
        drop(inner);
        if queued {
            eprintln!("engram> session={} {line}", registration.owning_session_id);
        }
        presentation.finished()
    }
}
