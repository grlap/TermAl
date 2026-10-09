// Publishes causal details only after the exact message's durable write.
// Generic card visibility and all admission/recovery decisions stay unchanged.
//
// Publication never blocks the caller. With the background writer, the exact
// message's fence carries a completion hook, and the writer runs it in the
// tick that acknowledges that content. With the synchronous fallback, the
// message already committed while StateInner was held, so the caller publishes
// after releasing it. Lock order: the hook runs on the writer thread after
// COMMIT and read-back, with the fence's completion mutex released and no
// other lock held. It then takes StateInner, the same lock the writer already
// takes after each write, so this adds no lock-order edge. Nothing here retries
// or sets a timer: on Deadline, WriteFailed, Shutdown or WorkerStopped the
// hook is dropped unrun and the details stay hidden. The hook keeps an
// AppState clone until its fence resolves, which bounds it by the fence
// deadline or the writer's shutdown.

fn engram_causal_unpublished(cause: &Option<EngramCausalFailure>) -> bool {
    cause.as_ref().is_none_or(|cause| cause.publication_pending)
}

struct EngramCausalPublication {
    session_id: String,
    message_id: String,
    cause: EngramCausalFailure,
}

impl AppState {
    /// Returns a publication only for the synchronous fallback; the caller
    /// passes it to `finish_engram_causal_publication` after releasing
    /// StateInner. A background write publishes from its own ACK instead.
    fn prepare_engram_causal_publication_locked(
        &self,
        inner: &StateInner,
        session_id: &str,
        message_index: usize,
        dispatch: PersistDispatch,
    ) -> Option<EngramCausalPublication> {
        let index = inner.find_session_index(session_id)?;
        let message = inner.sessions[index].session.messages.get(message_index)?;
        let Message::EngramControl { card, .. } = message else { return None; };
        let cause = card.causal_failure.as_ref()?.clone();
        if !cause.publication_pending { return None; }
        let publication = EngramCausalPublication {
            session_id: session_id.to_owned(),
            message_id: message.id().to_owned(),
            cause,
        };
        if dispatch != PersistDispatch::BackgroundQueued {
            // The successful synchronous fallback committed this unchanged
            // normalized message while StateInner was still held.
            return Some(publication);
        }
        let mut persisted = message.clone();
        if let Message::EngramControl { card, .. } = &mut persisted {
            card.causal_failure.as_mut()?.publication_pending = false;
        }
        let target = PersistFenceTarget::EngramCausalCard {
            session_id: session_id.to_owned(),
            message_id: publication.message_id.clone(),
            content: serde_json::to_value(persisted).ok()?,
        };
        let clock = inner.engram_budget_clock_snapshot();
        // Nobody waits on this fence; the writer resolves it, or its deadline
        // or drop does. A failed send drops the fence and its hook unrun.
        let (fence, _unwaited) = PersistFence::new_with_clock(
            target, clock.now() + Duration::from_secs(30), clock,
        );
        let state = self.clone();
        let fence = fence.on_acknowledged(move || state.publish_engram_causal_publication(publication));
        let _ = self.persist_tx.send(PersistRequest::Fence(Box::new(fence)));
        None
    }

    // Called with StateInner released; never waits on the writer.
    fn finish_engram_causal_publication(&self, publication: Option<EngramCausalPublication>) {
        if let Some(publication) = publication {
            self.publish_engram_causal_publication(publication);
        }
    }

    fn publish_engram_causal_publication(&self, publication: EngramCausalPublication) {
        let mut inner = self.inner.lock().expect("state mutex poisoned");
        let Some(index) = inner.find_session_index(&publication.session_id) else { return; };
        let Some(record) = inner.session_mut_by_index(index) else { return; };
        let Some(message_index) = record.session.messages.iter()
            .position(|message| message.id() == publication.message_id) else { return; };
        let Message::EngramControl { card, .. } = &mut record.session.messages[message_index] else { return; };
        let Some(cause) = &mut card.causal_failure else { return; };
        // A park update or replacement must obtain its own exact write ACK.
        if *cause != publication.cause { return; }
        cause.publication_pending = false;
        let updates = message_updated_delta_parts_for_indices(record, vec![message_index]);
        if let Ok(revision) = self.commit_delta_locked(&mut inner) {
            self.publish_message_updated_delta_parts(&inner, revision, updates);
        }
    }
}
