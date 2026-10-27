// Durable metadata-only native notifications and the joined relay wake worker.
// Home messages/cursors stay at home; local ACK/handoff flags govern wakes only.

#[cfg(test)]
std::thread_local! {
    static TEST_MAILBOX_RELAY_BEFORE_REDRIVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    static TEST_MAILBOX_RELAY_AFTER_FIRST_ADMISSION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

struct MailboxRelayNotification {
    wake: MailboxRelayWake,
    peer: FederationPeerSnapshot,
    acknowledged: bool,
    handed_off: bool,
    queued: bool,
}

impl MailboxStore {
    fn note_live_mailbox_relay_queued(&self, actor: &MailboxRelayPrincipal, mailbox: &str, through: u64) {
        let mut queued = self.relay_live_queued.lock().expect("relay live admission mutex poisoned");
        let sequence = queued.entry((actor.participant_key(), mailbox.to_owned())).or_default();
        *sequence = (*sequence).max(through);
    }

    fn live_mailbox_relay_queued(&self, actor: &MailboxRelayPrincipal, mailbox: &str, through: u64) -> bool {
        self.relay_live_queued.lock().expect("relay live admission mutex poisoned")
            .get(&(actor.participant_key(), mailbox.to_owned())).is_some_and(|sequence| *sequence >= through)
    }

    fn reopen_live_mailbox_relay_queued(&self, actor: &MailboxRelayPrincipal, mailbox: &str, through: u64) {
        let mut queued = self.relay_live_queued.lock().expect("relay live admission mutex poisoned");
        let key = (actor.participant_key(), mailbox.to_owned());
        if queued.get(&key).is_some_and(|sequence| *sequence <= through) { queued.remove(&key); }
    }

    fn receive_mailbox_relay_notification<G>(
        &self,
        wake: &MailboxRelayWake,
        peer: &FederationPeerSnapshot,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<MailboxRelayNotification> {
        wake.validate()
            .map_err(|error| relay_store_error(&error.message))?;
        validate_mailbox_relay_wake_peer(wake, peer, false)?;
        let _writer = self.lock_writer_with_budget(
            "retaining native relay notification",
            Duration::from_secs(2),
        )?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "retaining native relay notification")?;
        if mailbox_relay_store_id(&transaction)? != wake.target.store_id {
            return Err(relay_store_error(
                "native relay notification store was replaced",
            ));
        }
        let target = wake.target.participant_key();
        let acknowledged_through:u64=transaction.query_row(
            "SELECT acknowledged_through FROM mailbox_relay_associations WHERE pair_key=?1 AND mailbox_id=?2",
            rusqlite::params![wake.conversation.pair_key(),wake.mailbox_id],|row|row.get(0))?;
        // A home-authenticated zero-unread boundary is also a cursor proof.
        // It still owes the continuation: acknowledgement is not cancellation.
        let already_acknowledged = wake.sequence <= acknowledged_through || wake.unread_count == 0;
        let prior: Option<(String,bool,bool,bool,String)> = transaction.query_row(
            "SELECT wake_json,acknowledged,handed_off,queued,peer_json FROM mailbox_relay_notifications WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence=?3",
            rusqlite::params![wake.mailbox_id,target,wake.sequence],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        let (acknowledged, handed_off, queued) = if let Some((
            json,
            acknowledged,
            handed_off,
            queued,
            original_peer,
        )) = prior
        {
            if !same_mailbox_relay_enrollment(&serde_json::from_str(&original_peer)?, peer) {
                return Err(relay_store_error(
                    "native relay notification enrollment changed; no rebasing",
                ));
            }
            let mut expected: MailboxRelayWake = serde_json::from_str(&json)?;
            expected.unread_count = wake.unread_count;
            if expected != *wake {
                return Err(relay_store_error(
                    "native relay notification identity changed",
                ));
            }
            let next_json = serde_json::to_string(wake)?;
            if json != next_json {
                transaction.execute("UPDATE mailbox_relay_notifications SET wake_json=?4 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence=?3",
                    rusqlite::params![wake.mailbox_id,target,wake.sequence,next_json])?;
            }
            if already_acknowledged && !acknowledged {
                transaction.execute("UPDATE mailbox_relay_notifications SET acknowledged=1 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence=?3",
                    rusqlite::params![wake.mailbox_id,target,wake.sequence])?;
            }
            (acknowledged || already_acknowledged, handed_off, queued)
        } else {
            transaction.execute("INSERT INTO mailbox_relay_notifications(mailbox_id,target_key,through_sequence,record_version,wake_json,acknowledged,peer_json) VALUES(?1,?2,?3,1,?4,?5,?6)",
                rusqlite::params![wake.mailbox_id,target,wake.sequence,serde_json::to_string(wake)?,already_acknowledged,serde_json::to_string(peer)?])?;
            (already_acknowledged, false, false)
        };
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(MailboxRelayNotification {
            wake: wake.clone(),
            peer: peer.clone(),
            acknowledged,
            handed_off,
            queued: queued || self.live_mailbox_relay_queued(&wake.target, &wake.mailbox_id, wake.sequence),
        })
    }

    fn mailbox_relay_notification(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox: &str,
        sequence: u64,
    ) -> Result<Option<MailboxRelayNotification>> {
        let connection = self.connection()?;
        let row: Option<(String,bool,bool,bool,String)> = connection.query_row(
            "SELECT wake_json,acknowledged,handed_off,queued,peer_json FROM mailbox_relay_notifications WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence=?3",
            rusqlite::params![mailbox,actor.participant_key(),sequence],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        row.map(|(json, acknowledged, handed_off, queued, peer)| {
            let wake: MailboxRelayWake = serde_json::from_str(&json)?;
            wake.validate()
                .map_err(|error| relay_store_error(&error.message))?;
            if wake.target != *actor {
                return Err(relay_store_error(
                    "native relay notification membership changed",
                ));
            }
            let peer = serde_json::from_str(&peer)?;
            validate_mailbox_relay_wake_peer(&wake, &peer, false)?;
            Ok(MailboxRelayNotification {
                wake,
                peer,
                acknowledged,
                handed_off,
                queued,
            })
        })
        .transpose()
    }

    fn mark_mailbox_relay_notification<G>(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox: &str,
        through: u64,
        handoff: bool,
        authorize: impl FnOnce() -> Result<G>,
    ) -> Result<()> {
        let _writer = self.lock_writer_with_budget(
            "settling native relay wake metadata",
            Duration::from_secs(2),
        )?;
        let mut connection = self.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "settling native relay wake metadata")?;
        if !handoff {
            let json: Option<String> = transaction
                .query_row(
                    "SELECT conversation_json FROM mailbox_relay_associations WHERE mailbox_id=?1",
                    [mailbox],
                    |row| row.get(0),
                )
                .optional()?;
            let conversation: MailboxRelayConversation =
                serde_json::from_str(&json.ok_or_else(|| {
                    relay_store_error("native relay acknowledgement association missing")
                })?)?;
            conversation
                .require_member(actor)
                .map_err(|error| relay_store_error(&error.message))?;
            if actor.store_id != mailbox_relay_store_id(&transaction)? {
                return Err(relay_store_error(
                    "native relay acknowledgement store changed",
                ));
            }
            transaction.execute("UPDATE mailbox_relay_associations SET acknowledged_through=MAX(acknowledged_through,?2) WHERE mailbox_id=?1",
                rusqlite::params![mailbox,through])?;
        }
        let query = if handoff {
            "UPDATE mailbox_relay_notifications SET handed_off=1,queued=1 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence<=?3"
        } else {
            "UPDATE mailbox_relay_notifications SET acknowledged=1 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence<=?3"
        };
        transaction.execute(
            query,
            rusqlite::params![mailbox, actor.participant_key(), through],
        )?;
        let _authority = authorize()?;
        transaction.commit()?;
        Ok(())
    }
}

fn mailbox_relay_unread_wake(
    notification: &MailboxRelayNotification,
) -> Result<MailboxUnreadWakeup, ApiError> {
    let wake = &notification.wake;
    Ok(MailboxUnreadWakeup {
        mailbox_id: MailboxRelayLocator::encode(&wake.conversation.home, &wake.mailbox_id, false)?,
        message_id: MailboxRelayLocator::encode(&wake.conversation.home, &wake.message_id, true)?,
        sequence: wake.sequence,
        unread_count: if notification.acknowledged {
            0
        } else {
            wake.unread_count
        },
        sender_session_id: wake.sender.participant_key(),
        sender_name: wake.sender_name.clone(),
        topic: wake.topic.clone(),
    })
}

impl AppState {
    fn queue_native_mailbox_relay_wake(
        &self,
        wake: &MailboxRelayWake,
        lease: Option<&FederationLinkLease>,
    ) -> Result<bool, ApiError> {
        let actual = self.native_mailbox_relay_descriptor(&wake.target.native_session_id)?;
        if actual.principal != wake.target {
            return Err(ApiError::conflict("native mailbox wake target changed"));
        }
        let peer = if wake.target.host == wake.conversation.home.host {
            self.mailbox_relay_home_wake_peer(wake)?
        } else {
            match lease {
                Some(lease) => lease.with_current(self, |_, peer| Ok(peer.clone()))?,
                None => self.mailbox_relay_peer(&wake.target, &wake.conversation)?.2,
            }
        };
        let authority = self.mailbox_relay_dispatch_authority(wake, peer.clone(), lease)?;
        let notification = self
            .mailbox_store
            .receive_mailbox_relay_notification(wake, &peer, || authority.lock())
            .map_err(mailbox_api_error)?;
        if notification.handed_off {
            return Ok(true);
        }
        if notification.queued {
            // A retained carrier may have reached an idle queue during a link
            // outage. Re-drive that exact head after current authority returns;
            // a duplicate must not start an unrelated successor or recreate a
            // carrier removed by Cancel.
            let owner = {
                let inner = self.inner.lock().expect("state mutex poisoned");
                inner
                    .find_session_index(&wake.target.native_session_id)
                    .and_then(|index| MailboxRelayDrainOwner::capture(
                        &inner.sessions[index], wake, self.federation_executor_wake_signal.clone(),
                    ))
            };
            if let Some(owner) = owner.filter(|_| !self.federation_executor_wake_signal.is_stopping()) {
                #[cfg(test)]
                if let Some(hook) = TEST_MAILBOX_RELAY_BEFORE_REDRIVE.with(|hook| hook.borrow_mut().take()) { hook(); }
                if let Some(dispatch) = self
                    .dispatch_next_queued_turn_for_mailbox_relay(&wake.target.native_session_id, owner)
                    .map_err(mailbox_api_error)?
                {
                    let _ = deliver_turn_dispatch(self, dispatch);
                }
            }
            return Ok(true);
        }
        let source = mailbox_relay_unread_wake(&notification)?;
        let outcome = self
            .queue_mailbox_wakeups_for_native_outcome(
                &wake.target.native_session_id,
                vec![source],
                MailboxWakeupRecovery::NeverWoken,
                false,
                None,
                Some((&wake.target, authority.lease.as_ref(), &peer)),
            )
            .map_err(mailbox_api_error)?;
        if outcome.accepted && !self.federation_executor_wake_signal.is_stopping() {
            self.mark_native_mailbox_relay_queued(&wake.target, &wake.mailbox_id, wake.sequence)
                .map_err(mailbox_api_error)?;
            #[cfg(test)]
            if let Some(hook) = TEST_MAILBOX_RELAY_AFTER_FIRST_ADMISSION.with(|hook| hook.borrow_mut().take()) { hook(); }
            if let Some(owner) = outcome.relay_owner {
                if let Some(dispatch) = self
                    .dispatch_next_queued_turn_for_mailbox_relay(&wake.target.native_session_id, owner)
                    .map_err(mailbox_api_error)?
                {
                    let _ = deliver_turn_dispatch(self, dispatch);
                }
            }
        }
        Ok(outcome.accepted)
    }

    fn mark_native_mailbox_relay_queued(
        &self,
        actor: &MailboxRelayPrincipal,
        mailbox: &str,
        through: u64,
    ) -> Result<()> {
        let _writer = self.mailbox_store.lock_writer_with_budget(
            "retaining native relay queue admission",
            Duration::from_secs(2),
        )?;
        let mut connection = self.mailbox_store.connection()?;
        let transaction =
            begin_mailbox_write(&mut connection, "retaining native relay queue admission")?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, &actor.native_session_id)
            .map_err(|error| anyhow!(error.message))?;
        if root.federation_native_creation_id != actor.native_creation_id
            || inner.federation.local.as_ref() != Some(&actor.host)
        {
            return Err(relay_store_error("native relay queue owner changed"));
        }
        transaction.execute("UPDATE mailbox_relay_notifications SET queued=1 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence<=?3",
            rusqlite::params![mailbox,actor.participant_key(),through])?;
        transaction.commit()?;
        Ok(())
    }

    fn read_native_mailbox_relay_revalidation(
        &self,
        session: &str,
        head: FrontMailboxHead,
        locator: &MailboxRelayLocator,
    ) -> Result<Option<FrontMailboxRevalidation>> {
        let descriptor = self
            .native_mailbox_relay_descriptor(session)
            .map_err(|error| anyhow!(error.message))?;
        let Some(notification) = self.mailbox_store.mailbox_relay_notification(
            &descriptor.principal,
            &locator.id,
            head.sequence,
        )?
        else {
            // Missing/replaced metadata is neither delivery authority nor proof
            // that the home-owned wake disappeared. Preserve it without handoff.
            return Err(relay_store_error(
                "native relay wake metadata missing; delivery remains withheld",
            ));
        };
        if notification.wake.conversation.home != locator.home {
            return Err(relay_store_error("native relay wake home changed"));
        }
        let authority = self
            .mailbox_relay_dispatch_authority(&notification.wake, notification.peer.clone(), None)
            .map_err(|error| anyhow!(error.message))?;
        let _guard = authority.lock()?;
        let (wakeup, boundary) = if notification.handed_off || notification.acknowledged {
            (
                None,
                Some(MailboxWakeBoundary {
                    sender_session_id: notification.wake.sender.participant_key(),
                    sender_name: notification.wake.sender_name,
                    topic: notification.wake.topic,
                    delivered: notification.handed_off,
                }),
            )
        } else {
            (
                Some(
                    mailbox_relay_unread_wake(&notification)
                        .map_err(|error| anyhow!(error.message))?,
                ),
                None,
            )
        };
        Ok(Some(FrontMailboxRevalidation {
            head,
            wakeup,
            boundary,
        }))
    }

    fn requeue_native_mailbox_relay_notification(
        &self,
        notification: &MailboxNotificationDelivery,
        known_rejected: bool,
    ) -> Result<bool> {
        let Some(locator) = MailboxRelayLocator::decode(&notification.mailbox_id, false)
            .map_err(|error| anyhow!(error.message))?
        else {
            return Ok(false);
        };
        let actor = self
            .native_mailbox_relay_descriptor(&notification.session_id)
            .map_err(|error| anyhow!(error.message))?
            .principal;
        let Some(retained) = self.mailbox_store.mailbox_relay_notification(
            &actor,
            &locator.id,
            notification.through_sequence,
        )?
        else {
            return Ok(false);
        };
        if retained.wake.conversation.home != locator.home {
            return Err(relay_store_error("native relay recovery home changed"));
        }
        if retained.acknowledged && (!known_rejected || retained.handed_off) {
            return Ok(false);
        }
        // Failure recovery owns the old boundary, not a successor's accepted
        // turn. Reset its handoff marker while holding the same native guard
        // used for the reset decision, before a queued retry can revalidate it.
        let _writer = self
            .mailbox_store
            .lock_writer_with_budget("retaining failed relay wake", Duration::from_secs(2))?;
        let mut connection = self.mailbox_store.connection()?;
        let transaction = begin_mailbox_write(&mut connection, "retaining failed relay wake")?;
        let inner = self.inner.lock().expect("state mutex poisoned");
        let root = federation_mailbox_root_locked(&inner, &notification.session_id)
            .map_err(|error| anyhow!(error.message))?;
        if root.federation_native_creation_id != actor.native_creation_id
            || inner.federation.local.as_ref() != Some(&actor.host)
            || root
                .active_turn_mailbox_notification
                .as_ref()
                .is_some_and(|active| {
                    active.mailbox_id == notification.mailbox_id
                        && active.through_sequence >= notification.through_sequence
                })
        {
            return Ok(false);
        }
        // ACK does not cancel an owed first continuation, but only the exact
        // known-rejected enqueue path can reopen an acknowledged boundary.
        // Generic failure/unknown outcome and an accepted handoff cannot.
        transaction.execute("UPDATE mailbox_relay_notifications SET handed_off=0,queued=0 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence<=?3 AND (acknowledged=0 OR (?4=1 AND handed_off=0))",
            rusqlite::params![locator.id,actor.participant_key(),notification.through_sequence,known_rejected])?;
        transaction.commit()?;
        self.mailbox_store.reopen_live_mailbox_relay_queued(&actor, &locator.id, notification.through_sequence);
        drop(inner);
        drop(connection);
        drop(_writer);
        let Some(retained) = self.mailbox_store.mailbox_relay_notification(
            &actor,
            &locator.id,
            notification.through_sequence,
        )?
        else {
            return Ok(false);
        };
        if retained.acknowledged && (!known_rejected || retained.handed_off) {
            return Ok(false);
        }
        let authority = self
            .mailbox_relay_dispatch_authority(&retained.wake, retained.peer.clone(), None)
            .map_err(|error| anyhow!(error.message))?;
        let outcome = self.queue_mailbox_wakeups_for_native_outcome(
            &notification.session_id,
            vec![mailbox_relay_unread_wake(&retained).map_err(|error| anyhow!(error.message))?],
            MailboxWakeupRecovery::NeverWoken,
            true,
            None,
            Some((&actor, authority.lease.as_ref(), &retained.peer)),
        )?;
        if outcome.accepted {
            self.mark_native_mailbox_relay_queued(
                &actor,
                &locator.id,
                notification.through_sequence,
            )?;
        }
        Ok(outcome.changed)
    }

    fn mark_native_mailbox_relay_handoff(
        &self,
        notification: &MailboxNotificationDelivery,
    ) -> Result<()> {
        let Some(locator) = MailboxRelayLocator::decode(&notification.mailbox_id, false)
            .map_err(|error| anyhow!(error.message))?
        else {
            return Ok(());
        };
        let actor = self
            .native_mailbox_relay_descriptor(&notification.session_id)
            .map_err(|error| anyhow!(error.message))?
            .principal;
        self.mailbox_store.mark_mailbox_relay_notification(
            &actor,
            &locator.id,
            notification.through_sequence,
            true,
            || Ok(()),
        )
    }

    fn refresh_native_mailbox_relay_acknowledgement(
        &self,
        session: &str,
        locator: &MailboxRelayLocator,
        through: u64,
    ) -> Result<()> {
        let actor = self
            .native_mailbox_relay_descriptor(session)
            .map_err(|error| anyhow!(error.message))?
            .principal;
        self.mailbox_store.mark_mailbox_relay_notification(
            &actor,
            &locator.id,
            through,
            false,
            || Ok(()),
        )?;
        let mailbox = MailboxRelayLocator::encode(&locator.home, &locator.id, false)
            .map_err(|error| anyhow!(error.message))?;
        let covered = {
            let inner = self.inner.lock().expect("state mutex poisoned");
            let root = federation_mailbox_root_locked(&inner, session)
                .map_err(|error| anyhow!(error.message))?;
            root.queued_prompts
                .iter()
                .filter(|queued| {
                    !queued.is_engram_retained() && !queued_prompt_kept_standalone(root, queued)
                })
                .filter_map(|queued| {
                    let source = queued.pending_prompt.source.as_ref()?.mailbox.as_ref()?;
                    (source.mailbox_id == mailbox && source.sequence <= through).then(|| {
                        (
                            queued.pending_prompt.id.clone(),
                            source.message_id.clone(),
                            source.sequence,
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        for (prompt, message, sequence) in covered {
            let Some(notification) =
                self.mailbox_store
                    .mailbox_relay_notification(&actor, &locator.id, sequence)?
            else {
                continue;
            };
            if notification.handed_off || notification.wake.conversation.home != locator.home {
                continue;
            }
            let boundary = MailboxWakeBoundary {
                sender_session_id: notification.wake.sender.participant_key(),
                sender_name: notification.wake.sender_name,
                topic: notification.wake.topic,
                delivered: false,
            };
            let mut inner = self.inner.lock().expect("state mutex poisoned");
            let Some(index) = inner.find_session_index(session) else {
                break;
            };
            let root = &mut inner.sessions[index];
            if root.federation_native_creation_id != actor.native_creation_id {
                break;
            }
            let holds = &root.queued_prompt_durability_holds;
            let Some(queued) = root.queued_prompts.iter_mut().find(|queued| {
                queued.pending_prompt.id == prompt
                    && !queued.is_engram_retained()
                    && !holds.contains_key(&prompt)
                    && queued
                        .pending_prompt
                        .source
                        .as_ref()
                        .and_then(|source| source.mailbox.as_ref())
                        .is_some_and(|source| {
                            source.mailbox_id == mailbox
                                && source.message_id == message
                                && source.sequence == sequence
                        })
            }) else {
                continue;
            };
            if rewrite_queued_wake_as_continuation(queued, &mailbox, &message, sequence, boundary) {
                sync_pending_prompts(root);
                self.commit_locked(&mut inner)?;
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct MailboxRelayBootRecovery {
    after: i64,
    through: Option<i64>,
    scan_complete: bool,
    retry: VecDeque<i64>,
}

impl MailboxRelayBootRecovery {
    fn complete(&self) -> bool {
        self.scan_complete && self.retry.is_empty()
    }
}

#[derive(Default)]
struct MailboxRelayWakeWorker {
    task: Option<tokio::task::JoinHandle<()>>,
    pending: Option<tokio::sync::oneshot::Receiver<(i64, i64, MailboxRelayBootRecovery, bool)>>,
    observed_kick: u64,
    home_after: i64,
    native_after: i64,
    boot: MailboxRelayBootRecovery,
}

impl MailboxRelayWakeWorker {
    async fn tick(&mut self, app: &AppState, fallback: bool) {
        let mut more = false;
        if let Some(pending) = &mut self.pending {
            match pending.try_recv() {
                Ok((home, native, boot, boot_more)) => {
                    self.home_after = home;
                    self.native_after = native;
                    self.boot = boot;
                    more = home != 0 || native != 0 || boot_more;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => return,
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {}
            }
            self.pending = None;
            if let Some(task) = self.task.take() {
                if let Err(error) = task.await {
                    eprintln!("native relay wake worker failed: {error}");
                }
            }
        }
        if app.federation_executor_wake_signal.is_stopping() {
            return;
        }
        let enabled = app
            .inner
            .lock()
            .expect("state mutex poisoned")
            .preferences
            .federation
            .enabled;
        if !enabled {
            return;
        }
        let generation = app
            .federation_executor_wake_signal
            .generation
            .load(Ordering::SeqCst);
        if !fallback && !more && generation == self.observed_kick {
            return;
        }
        self.observed_kick = generation;
        let app = app.clone();
        let (home, native) = (self.home_after, self.native_after);
        let mut boot = std::mem::take(&mut self.boot);
        let (send, receive) = tokio::sync::oneshot::channel();
        self.task = Some(tokio::spawn(async move {
            let boot_more = if boot.complete() {
                false
            } else {
                match app
                    .reconcile_mailbox_relay_notifications_after_boot(&mut boot)
                    .await
                {
                    Ok(()) => !boot.scan_complete,
                    Err(error) => {
                        eprintln!(
                            "native relay boot reconciliation deferred ({}): {:?}",
                            error.status, error.message
                        );
                        false
                    }
                }
            };
            // An unavailable home cannot stall unrelated committed wakes.
            // Failed boot boundaries retry on the next kick/fallback, not on
            // this task's completion signal, and successful rows are not reset
            // a second time after a new handoff in this process.
            let (home, native) = app.run_mailbox_relay_wake_batch(home, native).await;
            let _ = send.send((home, native, boot, boot_more));
            // Completion is not a new generation: the supervisor harvests the
            // owned result and drains another bounded page only if needed.
            app.federation_executor_wake_signal.notify.notify_one();
        }));
        self.pending = Some(receive);
    }

    async fn shutdown(self) {
        if let Some(task) = self.task {
            let _ = task.await;
        }
    }
}

impl AppState {
    async fn reconcile_mailbox_relay_notifications_after_boot(
        &self,
        recovery: &mut MailboxRelayBootRecovery,
    ) -> Result<(), ApiError> {
        let app = self.clone();
        let after = recovery.after;
        let through = recovery.through;
        let retrying = recovery.scan_complete;
        let retry_ids = recovery.retry.iter().take(8).copied().collect::<Vec<_>>();
        let (through, rows): (i64, Vec<(i64, Option<String>)>) = run_blocking_api(move || {
            let connection = app.mailbox_store.connection().map_err(mailbox_api_error)?;
            // Only boundaries retained before this process's boot scan belong
            // to recovery. Later handoffs must not be reset by a slow scan.
            let through = match through {
                Some(through) => through,
                None => connection.query_row("SELECT coalesce(max(rowid),0) FROM mailbox_relay_notifications", [], |row|row.get(0))
                    .map_err(|error| mailbox_api_error(error.into()))?,
            };
            if retrying {
                let rows = retry_ids.into_iter().map(|id| {
                    let json = connection.query_row(
                        "SELECT wake_json FROM mailbox_relay_notifications WHERE rowid=?1 AND acknowledged=0",
                        [id], |row| row.get(0)).optional().map_err(|error| mailbox_api_error(error.into()))?;
                    Ok((id, json))
                }).collect::<Result<Vec<_>,ApiError>>()?;
                return Ok((through, rows));
            }
            let mut statement = connection.prepare(
                "SELECT rowid,wake_json FROM mailbox_relay_notifications WHERE rowid>?1 AND rowid<=?2 AND acknowledged=0 ORDER BY rowid LIMIT 8"
            ).map_err(|error| mailbox_api_error(error.into()))?;
            let rows = statement.query_map([after, through], |row| Ok((row.get(0)?, Some(row.get::<_, String>(1)?))))
                .map_err(|error| mailbox_api_error(error.into()))?;
            let rows = rows.collect::<std::result::Result<Vec<_>,_>>().map_err(|error| mailbox_api_error(error.into()))?;
            Ok((through, rows))
        }).await?;
        recovery.through = Some(through);
        let started = Instant::now();
        let mut processed = 0;
        for (id, json) in &rows {
            if self.federation_executor_wake_signal.is_stopping()
                || started.elapsed() >= Duration::from_secs(2)
            {
                break;
            }
            if retrying {
                recovery.retry.pop_front();
            } else {
                recovery.after = *id;
            }
            processed += 1;
            let mut context = String::new();
            let result = match json {
                Some(json) => match serde_json::from_str::<MailboxRelayWake>(json) {
                    Ok(wake) => {
                        context = format!(
                            " session={:?} mailbox={:?} sequence={}",
                            wake.target.native_session_id, wake.mailbox_id, wake.sequence
                        );
                        self.reconcile_mailbox_relay_boot_boundary(wake).await
                    }
                    Err(_) => Err(ApiError::conflict("invalid native relay boot metadata")),
                },
                None => Ok(()),
            };
            if let Err(error) = result {
                eprintln!(
                    "native relay boot boundary {id} deferred ({}){context}: {:?}",
                    error.status, error.message
                );
                recovery.retry.push_back(*id);
            }
        }
        if !retrying && rows.len() < 8 && processed == rows.len() {
            recovery.scan_complete = true;
        }
        Ok(())
    }

    async fn reconcile_mailbox_relay_boot_boundary(
        &self,
        wake: MailboxRelayWake,
    ) -> Result<(), ApiError> {
        // The home cursor, not a per-process handoff flag, decides whether
        // unread mail remains. Read never advances or fabricates a cursor.
        let page: MailboxReadResponse = self
            .call_native_mailbox_home(
                &wake.target,
                &wake.conversation,
                MailboxRelayOperation::Read {
                    actor: wake.target.clone(),
                    conversation: wake.conversation.clone(),
                    mailbox_id: wake.mailbox_id.clone(),
                    after_sequence: None,
                    limit: 1,
                    issue_receipt: false,
                },
            )
            .await?;
        let app = self.clone();
        run_blocking_api(move || {
                let _writer = app.mailbox_store.lock_writer_with_budget("reconciling relay boot boundary", Duration::from_secs(2)).map_err(mailbox_api_error)?;
                let mut connection = app.mailbox_store.connection().map_err(mailbox_api_error)?;
                let transaction = begin_mailbox_write(&mut connection, "reconciling relay boot boundary").map_err(mailbox_api_error)?;
                if mailbox_relay_store_id(&transaction).map_err(mailbox_api_error)? != wake.target.store_id {
                    return Err(ApiError::conflict("relay boot target store changed"));
                }
                let inner = app.inner.lock().expect("state mutex poisoned");
                let root = federation_mailbox_root_locked(&inner, &wake.target.native_session_id)?;
                if root.federation_native_creation_id != wake.target.native_creation_id || inner.federation.local.as_ref() != Some(&wake.target.host) {
                    return Err(ApiError::conflict("relay boot target changed"));
                }
                let mailbox = MailboxRelayLocator::encode(&wake.conversation.home, &wake.mailbox_id, false)?;
                let covered = root.active_turn_mailbox_notification.as_ref().is_some_and(|active|
                    active.mailbox_id == mailbox && active.through_sequence >= wake.sequence)
                    || root.queued_prompts.iter().any(|queued| queued.pending_prompt.source.as_ref()
                        .and_then(|source| source.mailbox.as_ref()).is_some_and(|source|
                            source.mailbox_id == mailbox && source.sequence >= wake.sequence));
                if page.processed_through >= wake.sequence {
                    transaction.execute("UPDATE mailbox_relay_notifications SET acknowledged=1 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence<=?3",
                        rusqlite::params![wake.mailbox_id, wake.target.participant_key(), page.processed_through]).map_err(|error|mailbox_api_error(error.into()))?;
                } else {
                    // No payload replay: rearm only an unacknowledged metadata
                    // boundary not already owned by a restored queue/runtime.
                    transaction.execute("UPDATE mailbox_relay_notifications SET handed_off=0,queued=?4 WHERE mailbox_id=?1 AND target_key=?2 AND through_sequence=?3 AND acknowledged=0",
                        rusqlite::params![wake.mailbox_id, wake.target.participant_key(), wake.sequence, covered]).map_err(|error|mailbox_api_error(error.into()))?;
                }
                transaction.commit().map_err(|error|mailbox_api_error(error.into()))?;
                if page.processed_through < wake.sequence && !covered {
                    // Only authoritative boot recovery or a proven rejected
                    // handoff may release the process-local admission witness.
                    app.mailbox_store.reopen_live_mailbox_relay_queued(&wake.target, &wake.mailbox_id, wake.sequence);
                }
                Ok(())
            }).await?;
        Ok(())
    }

    async fn run_mailbox_relay_wake_batch(&self, home_after: i64, native_after: i64) -> (i64, i64) {
        let started = Instant::now();
        let worker = self.clone();
        let rows = run_blocking_api(move || {
            let connection=worker.mailbox_store.connection().map_err(mailbox_api_error)?;
            let mut statement=connection.prepare("SELECT rowid,wake_json FROM mailbox_relay_wakes WHERE rowid>?1 AND delivered=0 ORDER BY rowid LIMIT 8").map_err(|error|mailbox_api_error(error.into()))?;
            let rows=statement.query_map([home_after],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?))).map_err(|error|mailbox_api_error(error.into()))?;
            rows.collect::<std::result::Result<Vec<_>,_>>().map_err(|error|mailbox_api_error(error.into()))
        }).await.unwrap_or_default();
        let mut home_next = home_after;
        let mut home_attempted = 0;
        for (row, json) in &rows {
            if self.federation_executor_wake_signal.is_stopping()
                || started.elapsed() >= Duration::from_secs(2)
            {
                break;
            }
            home_next = *row;
            home_attempted += 1;
            let Ok(mut wake) = serde_json::from_str::<MailboxRelayWake>(json) else {
                continue;
            };
            let worker = self.clone();
            let observed = wake.clone();
            let status = run_blocking_api(move || {
                observed.validate()?;
                let peer = worker.mailbox_relay_home_wake_peer(&observed)?;
                if !worker
                    .native_mailbox_relay_home()
                    .is_ok_and(|home| home == observed.conversation.home)
                {
                    return Err(ApiError::conflict(
                        "native relay home changed during wake recovery",
                    ));
                }
                let unread = worker
                    .mailbox_store
                    .unread_wakeup_for_mailbox_through(
                        &observed.target.participant_key(),
                        &observed.mailbox_id,
                        observed.sequence,
                    )
                    .map_err(mailbox_api_error)?;
                Ok((unread, peer))
            })
            .await;
            let outcome = match status {
                Ok((unread, original_peer)) => {
                    wake.unread_count = unread.map_or(0, |unread| unread.unread_count);
                    if wake.target.host == wake.conversation.home.host {
                        let worker = self.clone();
                        let wake = wake.clone();
                        run_blocking_api(move || {
                            worker.queue_native_mailbox_relay_wake(&wake, None)
                        })
                        .await
                    } else if let Some(handle) =
                        self.federation_peer_handle(&wake.target.host.host_id)
                    {
                        match handle.channel.current_lease() {
                            Ok(lease) => match lease.with_current(self, |_, current| {
                                if !same_mailbox_relay_enrollment(&original_peer, current) {
                                    return Err(federation_stale_link());
                                }
                                Ok(())
                            }) {
                                Ok(()) => {
                                    self.request_mailbox_relay::<bool>(
                                        &handle,
                                        &lease,
                                        MailboxRelayOperation::Wake { wake: wake.clone() },
                                    )
                                    .await
                                }
                                Err(error) => Err(error),
                            },
                            Err(error) => Err(error),
                        }
                    } else {
                        Err(ApiError::conflict("native relay target host has no connected peer"))
                    }
                }
                Err(error) => Err(error),
            };
            let queued = match outcome {
                Ok(true) => {
                    self.mailbox_store.relay_home_wake_refusals.clear(&wake.message_id);
                    true
                }
                Ok(false) => {
                    self.note_mailbox_relay_home_wake_refusal(&wake, StatusCode::CONFLICT, "wake not admitted by its target");
                    false
                }
                Err(error) => {
                    self.note_mailbox_relay_home_wake_refusal(&wake, error.status, &error.message);
                    false
                }
            };
            if queued {
                let worker = self.clone();
                let id = wake.message_id;
                let _ = run_blocking_api(move || {
                    let _writer = worker
                        .mailbox_store
                        .lock_writer_with_budget(
                            "settling relay notification",
                            Duration::from_secs(2),
                        )
                        .map_err(mailbox_api_error)?;
                    worker
                        .mailbox_store
                        .connection()
                        .map_err(mailbox_api_error)?
                        .execute(
                            "UPDATE mailbox_relay_wakes SET delivered=1 WHERE message_id=?1",
                            [id],
                        )
                        .map_err(|error| mailbox_api_error(error.into()))?;
                    Ok(())
                })
                .await;
            }
        }
        if rows.len() < 8 && home_attempted == rows.len() {
            home_next = 0;
        }
        let worker = self.clone();
        let native=run_blocking_api(move || {
            let connection=worker.mailbox_store.connection().map_err(mailbox_api_error)?;
            let mut statement=connection.prepare("SELECT rowid,wake_json FROM mailbox_relay_notifications WHERE rowid>?1 AND handed_off=0 ORDER BY rowid LIMIT 8")
                .map_err(|error|mailbox_api_error(error.into()))?;
            let rows=statement.query_map([native_after],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?)))
                .map_err(|error|mailbox_api_error(error.into()))?.collect::<std::result::Result<Vec<_>,_>>()
                .map_err(|error|mailbox_api_error(error.into()))?;
            drop(statement);drop(connection);
            let mut next=0;
            for (id,json) in &rows {
                if worker.federation_executor_wake_signal.is_stopping() { break; }
                next=*id;
                if let Ok(wake)=serde_json::from_str::<MailboxRelayWake>(json) {
                    if let Err(error)=worker.queue_native_mailbox_relay_wake(&wake,None) {
                        // At most eight rows per bounded pass; never log bodies,
                        // opaque locators or credentials from a failed attempt.
                        eprintln!("native relay notification {id} deferred ({})",error.status);
                    }
                }
            }
            Ok(if rows.len()==8 { next } else { 0 })
        }).await.unwrap_or(0);
        (home_next, native)
    }
}

// A refused home wake keeps its durable obligation and is retried on every
// worker pass. These process-local, metadata-only records make the refusal
// visible without reading message bodies or the receiver's store.
const MAILBOX_RELAY_HOME_WAKE_REFUSALS: usize = 256;
const MAILBOX_RELAY_HOME_WAKE_REASON_BYTES: usize = 200;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct MailboxRelayHomeWakeRefusal {
    message_id: String,
    mailbox_id: String,
    target_host_id: String,
    target_session_id: String,
    attempts: u64,
    status: u16,
    reason: String,
    first_refused_at: String,
    last_refused_at: String,
}

#[derive(Default)]
struct MailboxRelayHomeWakeRefusals {
    refused: Mutex<BTreeMap<String, MailboxRelayHomeWakeRefusal>>,
    /// Refused attempts not kept because the bound was full. It counts
    /// attempts, not distinct wakes.
    untracked: AtomicU64,
}

impl MailboxRelayHomeWakeRefusals {
    /// Records one refused attempt. True when the refusal is new or its
    /// reason changed, so a caller logs once per change, not once per pass.
    fn note(&self, wake: &MailboxRelayWake, status: StatusCode, reason: &str) -> bool {
        let reason = mailbox_relay_bounded_reason(reason);
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut refused = self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = refused.get_mut(&wake.message_id) {
            let changed = existing.status != status.as_u16() || existing.reason != reason;
            existing.attempts = existing.attempts.saturating_add(1);
            existing.status = status.as_u16();
            existing.reason = reason;
            existing.last_refused_at = now;
            return changed;
        }
        if refused.len() >= MAILBOX_RELAY_HOME_WAKE_REFUSALS {
            self.untracked.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        refused.insert(wake.message_id.clone(), MailboxRelayHomeWakeRefusal {
            message_id: wake.message_id.clone(),
            mailbox_id: wake.mailbox_id.clone(),
            target_host_id: wake.target.host.host_id.clone(),
            target_session_id: wake.target.native_session_id.clone(),
            attempts: 1,
            status: status.as_u16(),
            reason,
            first_refused_at: now.clone(),
            last_refused_at: now,
        });
        true
    }

    fn clear(&self, message_id: &str) {
        self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(message_id);
    }

    fn snapshot(&self) -> (Vec<MailboxRelayHomeWakeRefusal>, u64) {
        let refused = self.refused.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (refused.values().cloned().collect(), self.untracked.load(Ordering::Relaxed))
    }
}

impl AppState {
    fn note_mailbox_relay_home_wake_refusal(&self, wake: &MailboxRelayWake, status: StatusCode, reason: &str) {
        if self.mailbox_store.relay_home_wake_refusals.note(wake, status, reason) {
            // Metadata only: no body, locator or credential.
            eprintln!(
                "native relay home wake held for {}/{} ({}): {}",
                wake.target.host.host_id,
                wake.target.native_session_id,
                status.as_u16(),
                mailbox_relay_bounded_reason(reason),
            );
        }
    }
}

/// A peer's refusal text, cut at a character boundary within the bound, with
/// control characters replaced, so it cannot split or style a log line.
fn mailbox_relay_bounded_reason(reason: &str) -> String {
    let mut end = reason.len().min(MAILBOX_RELAY_HOME_WAKE_REASON_BYTES);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    reason[..end].chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

/// Undelivered home wake obligations and this process's recorded refusals.
/// Reads one count from the coordination store; takes no state lock.
async fn get_mailbox_relay_wake_diagnostics(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let store = state.mailbox_store.clone();
    let pending = run_blocking_api(move || {
        store.connection().map_err(mailbox_api_error)?
            .query_row("SELECT count(*) FROM mailbox_relay_wakes WHERE delivered=0", [], |row| row.get::<_, u64>(0))
            .map_err(|error| mailbox_api_error(error.into()))
    })
    .await?;
    let (refused, untracked) = state.mailbox_store.relay_home_wake_refusals.snapshot();
    Ok(Json(json!({
        "pendingHomeWakes": pending,
        "refusedHomeWakes": refused,
        "untrackedRefusals": untracked,
    })))
}
