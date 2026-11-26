//! Resend custody after a definitive binding refusal. Engram decides lineage;
//! this host only checks that the original holder and workspace are available.

use super::*;

impl AppState {
    pub(super) fn send_engram_verification_record_until(
        &self,
        registration: &TestRunRegistration,
        frozen: EngramVerificationRecordFrozen,
        deadline: std::time::Instant,
    ) -> EngramVerificationRecordStep {
        let route = self.engram_verification_record_route_until(registration, deadline);
        let clock = self.engram_budget_clock();
        let mut armed_replay = None;
        let link = route
            .as_ref()
            .map(EngramVerificationRecordRoute::link)
            .map_err(Clone::clone)
            .and_then(|mut link| {
                if let Some(TestRunVerificationAck::BindingRetry {
                    wait: Some(field),
                    body_digest,
                }) = &registration.verification_record
                {
                    let support = link
                        .adapter
                        .verification_record_route_support(link.connection);
                    if !support.is_supported() {
                        return Err(EngramVerificationRecordPending::CapabilityUnsupported {
                            support,
                        });
                    }
                    if !binding_ready(registration, &link, clock.now(), deadline) {
                        return Err(EngramVerificationRecordPending::BindingDeferred {
                            field: *field,
                        });
                    }
                    armed_replay = Some(self.confirm_verification_binding_resend(
                        registration,
                        body_digest,
                        link.connection,
                        clock.clone(),
                        deadline,
                    )?);
                } else if matches!(
                    registration.verification_record,
                    Some(TestRunVerificationAck::BindingRetry { wait: None, .. })
                ) && !self.confirm_verification_replay_snapshot(
                    registration,
                    link.connection,
                    clock.clone(),
                    deadline,
                ) {
                    return Err(EngramVerificationRecordPending::PersistFailed);
                }
                link.timeout = link
                    .timeout
                    .min(deadline.saturating_duration_since(clock.now()));
                if link.timeout.is_zero() {
                    return Err(EngramVerificationRecordPending::TransportFailed {
                        error_kind: format!("{:?}", EngramTransportErrorKind::Deadline),
                    });
                }
                Ok(link)
            });
        let mut may_have_dispatched = false;
        let step = engram_verification_record_send_reporting_dispatch(
            frozen,
            link,
            &mut may_have_dispatched,
        );
        if let Some(replay) = armed_replay
            && !may_have_dispatched
        {
            self.restore_verification_binding_wait(registration, &replay, clock);
        }
        step
    }

    /// A crash after the resend may lose its answer. Before sending, persist
    /// that a replay is allowed even if authority moves after acceptance.
    fn confirm_verification_binding_resend(
        &self,
        registration: &TestRunRegistration,
        body_digest: &str,
        connection: &EngramConnectionConfig,
        clock: EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> Result<TestRunRegistration, EngramVerificationRecordPending> {
        let failed = EngramVerificationRecordPending::PersistFailed;
        let mut replay = registration.clone();
        replay.verification_record = Some(TestRunVerificationAck::BindingRetry {
            wait: None,
            body_digest: body_digest.to_owned(),
        });
        let written = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !verification_retry_route_current(&inner, registration, connection)
                || inner.test_run_registrations.get(&registration.run_id) != Some(registration)
                || clock.now() >= deadline
            {
                return Err(failed);
            }
            inner
                .test_run_registrations
                .update_unindexed_fields(&registration.run_id, |row| {
                    row.verification_record = replay.verification_record.clone();
                });
            self.persist_internal_locked(&inner).is_ok()
        };
        if !written
            || !self.confirm_verification_replay_snapshot(
                &replay,
                connection,
                clock.clone(),
                deadline,
            )
        {
            self.restore_verification_binding_wait(registration, &replay, clock);
            return Err(failed);
        }
        Ok(replay)
    }

    fn confirm_verification_replay_snapshot(
        &self,
        replay: &TestRunRegistration,
        connection: &EngramConnectionConfig,
        clock: EngramBudgetClock,
        deadline: std::time::Instant,
    ) -> bool {
        // A failed rollback may have left the replay state only in memory.
        // Establish its durable custody before another possible delivery.
        if clock.now() >= deadline
            || self
                .confirm_engram_fence_target_until(
                    PersistFenceTarget::TestRunRegistrationReadback(replay.clone()),
                    clock.clone(),
                    deadline,
                )
                .is_err()
        {
            return false;
        }
        let inner = self.inner.lock().expect("state mutex poisoned");
        inner.test_run_registrations.get(&replay.run_id) == Some(replay)
            && verification_retry_route_current(&inner, replay, connection)
            && clock.now() < deadline
    }

    fn restore_verification_binding_wait(
        &self,
        registration: &TestRunRegistration,
        replay: &TestRunRegistration,
        clock: EngramBudgetClock,
    ) {
        // Only undo this attempt's marker. A prior uncertain delivery and
        // another row's receipt or refusal retain their own custody. An
        // artifact scan may have appended completion or diagnostics.
        let (restored, write) = {
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if !inner
                .test_run_registrations
                .get(&registration.run_id)
                .is_some_and(|row| row.matches_registration_snapshot(replay))
            {
                return;
            }
            let restored = inner
                .test_run_registrations
                .update_unindexed_fields(&registration.run_id, |row| {
                    row.verification_record = registration.verification_record.clone();
                    row.clone()
                })
                .expect("the registration was checked under the same lock");
            (restored, self.persist_internal_locked(&inner))
        };
        // Rollback has a deadline of its own, even after the send budget
        // expires. The next pass follows the wait only after confirmation.
        let deadline = clock.now() + ENGRAM_VERIFICATION_RECORD_TICK_BUDGET;
        let durable = write.map_err(|error| format!("{error:#}")).and_then(|()| {
            self.confirm_engram_fence_target_until(
                PersistFenceTarget::TestRunRegistrationReadback(restored.clone()),
                clock,
                deadline,
            )
        });
        if let Err(error) = durable {
            eprintln!(
                "engram> run={} verification binding wait durability unconfirmed: {error}",
                registration.run_id
            );
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            if inner
                .test_run_registrations
                .get(&registration.run_id)
                .is_some_and(|row| row.matches_registration_snapshot(&restored))
            {
                inner
                    .test_run_registrations
                    .update_unindexed_fields(&registration.run_id, |row| {
                        row.verification_record = replay.verification_record.clone();
                    });
                if let Err(error) = self.persist_internal_locked(&inner) {
                    eprintln!(
                        "engram> run={} verification replay state not persisted: {error:#}",
                        registration.run_id
                    );
                }
            }
        }
    }
}

fn verification_retry_route_current(
    inner: &StateInner,
    registration: &TestRunRegistration,
    connection: &EngramConnectionConfig,
) -> bool {
    AppState::engram_source_root_target_locked(inner, &registration.initiating_session_id)
        .ok()
        .flatten()
        .is_some_and(|target| {
            &target.connection == connection
                && target.settings.authority_store_key == registration.authority_store
        })
}

fn binding_ready(
    registration: &TestRunRegistration,
    link: &EngramVerificationRecordLink<'_>,
    now: std::time::Instant,
    deadline: std::time::Instant,
) -> bool {
    let (Some(store), Some(binding), Some(named)) = (
        &registration.authority_store,
        &registration.work_binding,
        &registration.named_root,
    ) else {
        return false;
    };
    let timeout = link.timeout.min(deadline.saturating_duration_since(now));
    if timeout.is_zero() || link.connection.session_id != registration.initiating_session_id {
        return false;
    }
    let Ok(proof) = link
        .adapter
        .request(
            link.connection,
            &EngramControlRequest::NamedRootRead {
                run_id: binding.run_id.clone(),
                claim_id: binding.claim_id.clone(),
            },
            timeout,
        )
        .and_then(parse_engram_result::<EngramNamedRootReadResponse>)
    else {
        return false;
    };
    proof.validate_authority(store, binding).is_ok()
        && matches!(proof.run.state.as_str(), "claimed" | "active")
        && matches!(
            &proof.named_root,
            EngramNamedRootState::Bound { workspace_id, generation, .. }
                if workspace_id == &named.workspace_id
                    && u64::try_from(*generation).is_ok_and(|current| current >= named.generation)
        )
        && proof.claim.is_some_and(|claim| {
            claim.state == "active"
                && claim.holder.as_deref() == Some(registration.initiating_session_id.as_str())
                && claim
                    .fence
                    .is_some_and(|fence| fence >= binding.claim_fence)
                && claim
                    .expires_at
                    .as_deref()
                    .and_then(|expires| chrono::DateTime::parse_from_rfc3339(expires).ok())
                    .is_some_and(|expires| expires > chrono::Utc::now())
        })
}
