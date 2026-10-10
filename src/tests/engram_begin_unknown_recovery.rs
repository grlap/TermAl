//! The prepared turn_begin on the production admission path: its durable
//! acknowledgement before the send, the send withheld without one, and the
//! diagnostics a lost begin reply leaves on the retained hold. Does not own the
//! evaluate retry schedule. Registered by engram_admission_retry.rs, whose
//! clock and tick helpers it uses.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// One durability or wire boundary, in the order the writer acknowledged a
/// fence and the producer received a begin.
#[derive(Clone, Debug)]
enum Boundary {
    Acknowledged(Value),
    Sent(Value),
}

#[derive(Default)]
struct BoundaryLog(Mutex<Vec<Boundary>>);

impl BoundaryLog {
    fn push(&self, boundary: Boundary) {
        self.0.lock().unwrap().push(boundary);
    }

    fn snapshot(&self) -> Vec<Boundary> {
        self.0.lock().unwrap().clone()
    }
}

/// Drops one turn_begin reply after the stateful producer applied it, so an
/// exact replay of the same key can return the retained receipt. Models reply
/// loss on the control pipe, not a real sidecar or a real Engram store.
struct LostBeginReply {
    producer: Arc<StatefulEngramControlTransport>,
    lose_next_begin_reply: AtomicBool,
    log: Arc<BoundaryLog>,
}

impl EngramControlTransport for LostBeginReply {
    fn request(
        &self,
        connection: &EngramConnectionConfig,
        request: &EngramControlRequest,
        timeout: Duration,
    ) -> std::result::Result<Value, EngramTransportError> {
        if matches!(request, EngramControlRequest::TurnBegin { .. }) {
            self.log
                .push(Boundary::Sent(serde_json::to_value(request).unwrap()));
        }
        let reply = self.producer.request(connection, request, timeout)?;
        if matches!(request, EngramControlRequest::TurnBegin { .. })
            && reply["decision"] == "begin"
            && self.lose_next_begin_reply.swap(false, Ordering::SeqCst)
        {
            return Err(EngramTransportError::transport(LOST_BEGIN_REPLY));
        }
        Ok(reply)
    }

    fn shutdown_session(&self, session_id: &str) {
        self.producer.shutdown_session(session_id);
    }
}

const LOST_BEGIN_REPLY: &str =
    "control worker reached EOF after applying turn_begin; reply frame missing";

fn lost_begin_fixture(
    lose_reply: bool,
) -> (AppState, String, mpsc::Receiver<CodexRuntimeCommand>, Arc<LostBeginReply>) {
    let (state, session, receiver, _) = root_fixture([]);
    let transport = Arc::new(LostBeginReply {
        producer: StatefulEngramControlTransport::new(),
        lose_next_begin_reply: AtomicBool::new(lose_reply),
        log: Arc::default(),
    });
    state.install_control_test_transport(transport.clone());
    (state, session, receiver, transport)
}

fn begin_requests(transport: &LostBeginReply) -> Vec<Value> {
    transport
        .producer
        .requests()
        .into_iter()
        .filter(|recorded| recorded.request["operation"] == "turn_begin")
        .map(|recorded| recorded.request)
        .collect()
}

/// The prepared-begin record a fence's admission content carries for the
/// queued head, when it carries one.
fn prepared_begin(content: &Value) -> Option<&Value> {
    content["queue"]["engram_evaluate"]
        .get("prepared_begin")
        .filter(|prepared| prepared.is_object())
}

/// A durable writer that acknowledges every fence without writing, logging
/// each admission fence it acknowledges. With `fail_prepared`, it resolves the
/// first admission fence carrying a prepared begin with that result instead.
struct AcknowledgingWriter {
    stop: Arc<AtomicBool>,
    injected: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl AcknowledgingWriter {
    fn install(
        state: &mut AppState,
        log: Arc<BoundaryLog>,
        fail_prepared: Option<PersistFenceError>,
    ) -> Self {
        state.shutdown_persist_blocking();
        let (persist_tx, persist_rx) = mpsc::channel();
        state.persist_tx = persist_tx;
        let stop = Arc::new(AtomicBool::new(false));
        let injected = Arc::new(AtomicBool::new(false));
        let (writer_stop, writer_injected) = (stop.clone(), injected.clone());
        let handle = std::thread::spawn(move || {
            while let Ok(request) = persist_rx.recv() {
                if writer_stop.load(Ordering::SeqCst) {
                    break;
                }
                let PersistRequest::Fence(fence) = request else {
                    continue;
                };
                let content = match &fence.target {
                    PersistFenceTarget::EngramAdmission { content, .. } => Some(content.clone()),
                    _ => None,
                };
                let carries_prepared = content.as_ref().and_then(prepared_begin).is_some();
                if carries_prepared
                    && fail_prepared.is_some()
                    && !writer_injected.swap(true, Ordering::SeqCst)
                {
                    match fail_prepared.clone().unwrap() {
                        // A writer that stops abandons the fence unfinished.
                        PersistFenceError::WorkerStopped => drop(fence),
                        fault => fence.finish(Err(fault)),
                    }
                    continue;
                }
                if let Some(content) = content {
                    log.push(Boundary::Acknowledged(content));
                }
                fence.finish(Ok(()));
            }
        });
        Self {
            stop,
            injected,
            handle,
        }
    }

    fn injected(&self) -> bool {
        self.injected.load(Ordering::SeqCst)
    }

    fn finish(self, state: &AppState) {
        self.stop.store(true, Ordering::SeqCst);
        state.persist_tx.send(PersistRequest::Delta).unwrap();
        self.handle.join().unwrap();
    }
}

/// The newest begin card's causal failure, as the durable writer sees it.
fn begin_cause(state: &AppState, session: &str) -> Option<Value> {
    with_record(state, session, |record| {
        record.session.messages.iter().rev().find_map(|message| match message {
            Message::EngramControl { card, .. } => card
                .causal_failure
                .as_ref()
                .filter(|cause| cause.operation == "turn_begin")
                .map(|cause| serde_json::to_value(cause).unwrap()),
            _ => None,
        })
    })
}

#[test]
fn begin_unknown_recovery_healthy_delivery_control() {
    let (state, session, receiver, transport) = lost_begin_fixture(false);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(transport.producer.grant_state(&session).1.is_some(), "begun");
    assert_eq!(begin_requests(&transport).len(), 1);
    assert_eq!(abort_retry::prompts_received(&receiver), 1);
    let (queued, uncertain) = with_record(&state, &session, |record| {
        (record.queued_prompts.len(), record.engram.uncertain_grant_id.clone())
    });
    assert_eq!(queued, 0);
    assert_eq!(uncertain, None);
}

/// Before the first turn_begin leaves, the queued head's retained evaluate
/// carries the exact begin it is about to send (key string, grant, delivery
/// tokens, expiry basis, prepared phase), and the durable writer has
/// acknowledged that content: the acknowledgement precedes the send.
#[test]
fn begin_unknown_recovery_prepared_begin_is_durable_before_send() {
    let (mut state, session, receiver, transport) = lost_begin_fixture(false);
    let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), None);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    writer.finish(&state);
    assert_eq!(abort_retry::prompts_received(&receiver), 1, "healthy delivery");

    let log = transport.log.snapshot();
    let sent_at = log
        .iter()
        .position(|boundary| matches!(boundary, Boundary::Sent(_)))
        .expect("one begin reached the producer");
    let Boundary::Sent(sent) = &log[sent_at] else {
        unreachable!()
    };
    let acknowledged = log[..sent_at]
        .iter()
        .rev()
        .find_map(|boundary| match boundary {
            Boundary::Acknowledged(content) => {
                prepared_begin(content).map(|prepared| (content, prepared))
            }
            Boundary::Sent(_) => None,
        });
    let Some((content, prepared)) = acknowledged else {
        panic!("the prepared begin must be durably acknowledged before the send: {log:?}");
    };
    assert_eq!(prepared["idempotency_key"], sent["idempotency_key"]);
    assert!(prepared["idempotency_key"].is_string());
    assert_eq!(prepared["grant_id"], sent["grant_id"]);
    assert_eq!(prepared["delivery_tokens"], sent["delivery_tokens"]);
    assert_eq!(prepared["phase"], "prepared");
    let issued = prepared["issued_no_later_than"]
        .as_str()
        .expect("the grant's expiry basis is the local time it had been issued by");
    assert!(chrono::DateTime::parse_from_rfc3339(issued).is_ok());
    let evaluate = &content["queue"]["engram_evaluate"];
    assert_eq!(evaluate["connection"]["session_id"], session.as_str());
    assert!(evaluate["settings"].is_object(), "project/store authority");
    assert_eq!(evaluate["request"]["operation"], "turn_evaluate");
    assert!(
        content["queue"]["pending_prompt"]["id"].is_string(),
        "head identity"
    );
}

/// A failed or ambiguous acknowledgement of the prepared begin withholds the
/// send. The head stays retained as a local-persistence hold that names the
/// unsent begin; it is not scheduled for automatic replay, and it keeps no
/// prepared record that would later read as a possibly sent begin.
#[test]
fn begin_unknown_recovery_unacknowledged_preparation_withholds_the_send() {
    for (label, fault) in [
        (
            "write failure",
            PersistFenceError::WriteFailed("controlled prepared-begin write failure".to_owned()),
        ),
        ("ambiguous deadline", PersistFenceError::Deadline),
        ("stopped writer", PersistFenceError::WorkerStopped),
    ] {
        let (mut state, session, receiver, transport) = lost_begin_fixture(false);
        let writer = AcknowledgingWriter::install(&mut state, transport.log.clone(), Some(fault));
        let _ = deliver_turn_dispatch(&state, root_dispatch(&state, &session, false));
        let injected = writer.injected();
        tick_past_due(&state, &session);
        tick_past_due(&state, &session);
        writer.finish(&state);
        assert!(
            injected,
            "{label}: the prepared-begin fence is requested before the send"
        );
        assert!(begin_requests(&transport).is_empty(), "{label}: send withheld");
        assert!(receiver.try_recv().is_err(), "{label}: no provider handoff");
        let (retained, uncertain, retry) = with_record(&state, &session, |record| {
            (
                record.queued_prompts.len(),
                record.engram.uncertain_grant_id.clone(),
                record.engram.admission_retry.clone(),
            )
        });
        assert_eq!(retained, 1, "{label}: the head is retained");
        assert_eq!(uncertain, None, "{label}: nothing can have begun");
        assert!(retry.is_none(), "{label}: a local-persistence hold is not replayed");
        let cause = begin_cause(&state, &session).expect("the hold names the unsent begin");
        assert_eq!(cause["failureClass"], "local_state", "{label}");
        assert_eq!(cause["originalCode"], "begin_preparation_unacknowledged");
        assert_eq!(cause["remoteApplication"], "not_started", "{label}");
        let prepared = with_record(&state, &session, |record| {
            record.queued_prompts[0]
                .engram_evaluate
                .as_ref()
                .and_then(|evaluate| evaluate.prepared_begin.clone())
        });
        assert_eq!(prepared, None, "{label}: the withheld begin leaves no prepared record");
        assert!(
            !transport.log.snapshot().iter().any(|boundary| matches!(
                boundary,
                Boundary::Acknowledged(content) if prepared_begin(content).is_some()
            )),
            "{label}: no acknowledged content carries the withheld begin"
        );
    }
}

/// The begin-unknown hold keeps the original raw transport error kind and
/// message and the attempt key. A transport that captured no control process
/// says so explicitly instead of inventing process evidence.
#[test]
fn begin_unknown_recovery_hold_records_raw_error_and_unavailable_sidecar() {
    let (state, session, receiver, transport) = lost_begin_fixture(true);
    deliver_turn_dispatch(&state, root_dispatch(&state, &session, false)).unwrap();
    assert!(receiver.try_recv().is_err());
    let key = begin_requests(&transport)[0]["idempotency_key"].clone();
    let cause = begin_cause(&state, &session).expect("the begin-unknown hold has a cause");
    assert_eq!(cause["failureClass"], "transport");
    assert_eq!(cause["message"], LOST_BEGIN_REPLY);
    assert_eq!(cause["attemptId"], key);
    assert_eq!(cause["remoteApplication"], "unknown");
    assert_eq!(
        cause["controlProcess"],
        json!({"identity": "unavailable", "exitState": "unavailable"}),
        "uncaptured sidecar evidence is explicitly unavailable"
    );
}
