// Owns isolated journal rows and HTTP observations for operator diagnostics.
// Does not run dispatch, use live enrollment, or retain runtime payloads in output.
use super::*;

pub(super) struct JournalFixture {
    pub(super) state: AppState,
}

impl JournalFixture {
    pub(super) fn new(is_central: bool) -> Self {
        let mut state = if is_central {
            central()
        } else {
            domain_state()
        };
        state.mailbox_store = Arc::new(
            MailboxStore::open(&state.persistence_path.with_file_name("coordination.sqlite"))
                .unwrap(),
        );
        Self { state }
    }

    pub(super) fn local(&self) -> FederationHostIdentity {
        self.state
            .inner
            .lock()
            .unwrap()
            .federation
            .local
            .clone()
            .unwrap()
    }

    pub(super) fn operation(&self, key: &str, remote: bool) -> FederationExecutorOperation {
        let local = self.local();
        FederationExecutorOperation {
            central_host_id: local.host_id.clone(),
            central_epoch: local.host_generation,
            operation_key: key.to_owned(),
            target_host_id: if remote {
                "22222222-2222-4222-8222-222222222222".into()
            } else {
                local.host_id
            },
            enrollment_generation: 3,
            session_id: "journal-root".into(),
            creation_id: "journal-creation".into(),
            placement_generation: 5,
            command_kind: "wake".into(),
            payload: "PRIVATE_JOURNAL_PAYLOAD_CANARY".into(),
        }
    }

    pub(super) fn receive(&self, operation: &FederationExecutorOperation) {
        self.state
            .mailbox_store
            .admit_federation_executor_operation(operation, || Ok(()))
            .unwrap();
    }

    pub(super) fn evidence(
        &self,
        operation: &FederationExecutorOperation,
        evidence: FederationExecutorEvidence,
    ) {
        self.state
            .mailbox_store
            .record_federation_executor_evidence(&self.local().host_id, operation, evidence, || {
                Ok(())
            })
            .unwrap();
    }

    pub(super) fn queued(&self, operation: &FederationExecutorOperation) {
        self.evidence(
            operation,
            FederationExecutorEvidence::Queued {
                queue_entry_id: "PRIVATE_QUEUE_CANARY".into(),
                delivery_attempt_id: "PRIVATE_ATTEMPT_CANARY".into(),
            },
        );
    }

    pub(super) async fn get(&self, query: &str, expected: StatusCode) -> Value {
        operator::operator_request(
            &self.state,
            "GET",
            &format!("/api/federation/journal{query}"),
            None,
            expected,
        )
        .await
    }

    pub(super) fn target(&self, operation: &FederationExecutorOperation) -> Value {
        json!({
            "central": {"hostId": operation.central_host_id,
                "hostGeneration": operation.central_epoch},
            "identity": {"hostId": operation.target_host_id,
                "sessionId": operation.session_id, "creationId": operation.creation_id},
            "placementGeneration": operation.placement_generation,
        })
    }

    // Counts use the SAME connection that the HTTP read will use. A different
    // observer connection's total_changes would miss route-side writes.
    pub(super) fn snapshot(&self) -> (Value, i64, Vec<(String, String)>) {
        let image = operator::current_image(&self.state);
        let connection = self.state.mailbox_store.connection().unwrap();
        let changes = connection
            .query_row("SELECT total_changes()", [], |row| row.get(0))
            .unwrap();
        let schema = connection
            .prepare("SELECT name,COALESCE(sql,'') FROM sqlite_master ORDER BY type,name")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        (image, changes, schema)
    }

    pub(super) fn wake(&self, key: &str) -> (String, FederationExecutorOperation) {
        let operation = self.operation(key, true);
        let target: FederationMailboxBinding =
            serde_json::from_value(self.target(&operation)).unwrap();
        self.wake_for(key, target)
    }

    pub(super) fn wake_for(
        &self,
        key: &str,
        target: FederationMailboxBinding,
    ) -> (String, FederationExecutorOperation) {
        let mut operation = self.operation(key, true);
        operation.central_host_id = target.central.host_id.clone();
        operation.central_epoch = target.central.host_generation;
        operation.target_host_id = target.identity.host_id.clone();
        operation.session_id = target.identity.session_id.clone();
        operation.creation_id = target.identity.creation_id.clone();
        operation.placement_generation = target.placement_generation;
        let central = self.local();
        let delivery = FederationDelivery {
            version: 1,
            sender: FederationMailboxBinding {
                central: central.clone(),
                identity: FederationSessionIdentity {
                    host_id: central.host_id,
                    session_id: "journal-sender".into(),
                    creation_id: "journal-sender-creation".into(),
                },
                placement_generation: 1,
            },
            target,
            key: key.to_owned(),
            message: "PRIVATE_WAKE_BODY_CANARY".into(),
            topic: Some("PRIVATE_WAKE_TOPIC_CANARY".into()),
            state_stamp: Some("PRIVATE_WAKE_STAMP_CANARY".into()),
        };
        let result = self
            .state
            .mailbox_store
            .append_with_delivery(
                &delivery.input(
                    "PRIVATE_SENDER_CANARY".into(),
                    "PRIVATE_TARGET_CANARY".into(),
                ),
                Some(&delivery),
                || Ok(()),
            )
            .unwrap();
        operation.operation_key = format!("mailbox-wake:{}", result.receipt.message_id);
        (result.receipt.message_id, operation)
    }

    pub(super) fn seed_opaque_payload(&self) {
        let operation = self.operation("opaque", false);
        self.state
            .mailbox_store
            .connection()
            .unwrap()
            .execute(
                "INSERT INTO federation_executor_operations
             (central_host_id,operation_key,central_epoch,target_host_id,
              enrollment_generation,session_id,creation_id,placement_generation,
              command_kind,payload_digest,payload,payload_bytes,stage,effect_state)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'wake',?9,CAST(x'ff' AS TEXT),1,
                    'received','not_started')",
                rusqlite::params![
                    operation.central_host_id,
                    operation.operation_key,
                    operation.central_epoch,
                    operation.target_host_id,
                    operation.enrollment_generation,
                    operation.session_id,
                    operation.creation_id,
                    operation.placement_generation,
                    sha256_hex(&[0xff])
                ],
            )
            .unwrap();
    }
}

pub(super) async fn assert_journal_absent_from_pair_listener() {
    use super::super::super::{federation_control_fixture as control, federation_link as link};
    let receiver = link::channel(FederationRole::Satellite, 'a');
    let sender = link::channel(FederationRole::Central, 'a');
    let _attested = control::attest(&sender, &receiver).await;
    let router = federation_router(receiver, link::allowed());
    let health = control::signed(
        &sender,
        1,
        "GET",
        "/federation/v1/health",
        Default::default(),
        &[],
    );
    assert_eq!(
        router.clone().oneshot(health).await.unwrap().status(),
        StatusCode::OK,
        "the pair listener must accept a current authenticated control request"
    );
    let local_path = Request::builder()
        .uri("/api/federation/journal")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        router.clone().oneshot(local_path).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let transport_path = control::signed(
        &sender,
        2,
        "GET",
        "/federation/v1/journal",
        Default::default(),
        &[],
    );
    assert_eq!(
        router.oneshot(transport_path).await.unwrap().status(),
        StatusCode::NOT_FOUND,
        "even an authenticated pair must not expose the local journal diagnostic"
    );
}

pub(super) fn entries(page: &Value) -> &Vec<Value> {
    page["entries"].as_array().expect("journal page entries")
}

pub(super) fn sorted_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<_> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

pub(super) fn continuation(page: &Value, stream: &str) -> String {
    let cursor = page["nextCursor"].as_str().expect("bounded continuation");
    assert!(cursor.len() <= 2048, "cursor must stay bounded");
    assert!(
        cursor
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "cursor must be URL-safe without interpreting caller text"
    );
    format!("?stream={stream}&limit=100&cursor={cursor}")
}
