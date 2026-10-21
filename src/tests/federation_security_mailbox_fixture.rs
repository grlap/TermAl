// Owns real application mailbox setup and a response-controlling HTTP relay.
// Does not create proof keys or classify outcomes. Split from security request tests.

use super::super::federation_onboarding_fixture::{Pair as HostPair, io};
use super::*;

pub(super) struct MailboxCase {
    pub pair: HostPair,
    pub request: FederationMailboxRequest,
}

impl MailboxCase {
    pub async fn new() -> Self {
        Self::with_writer_timeout(MAILBOX_WRITER_ADMISSION_TIMEOUT).await
    }

    pub async fn with_writer_timeout(timeout: Duration) -> Self {
        let mut central = super::super::federation_mailboxes::central_store();
        Arc::get_mut(&mut central.mailbox_store).unwrap().write_admission_timeout = timeout;
        let (_, target) = super::super::federation_mailboxes::local_participant(&central, "Target");
        let pair = HostPair::new(central, io()).await;
        let connected = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .verify_federation_peer(&connected, super::super::federation_registry::checks())
            .unwrap();
        let verified = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        pair.central
            .transition_federation_peer(&verified, FederationMembershipState::Active)
            .unwrap();
        let current = pair.central.federation_peer_snapshot(&pair.id).unwrap();
        let placement = pair
            .central
            .reserve_federation_placement(
                &current,
                "remote-mailbox".to_owned(),
                "session-remote".to_owned(),
                0,
                0,
            )
            .unwrap();
        let actor = FederationMailboxActor {
            binding: FederationMailboxBinding {
                central: current.local.clone(),
                identity: placement.identity,
                placement_generation: placement.placement_generation,
            },
            name: "Remote".to_owned(),
        };
        pair.central
            .execute_federation_mailbox(
                FederationMailboxRequest {
                    protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
                    schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
                    central: current.local.clone(),
                    actor: Some(actor.clone()),
                    operation: FederationMailboxOperation::Register,
                },
                Some(pair.lease.clone()),
            )
            .unwrap();
        let request = FederationMailboxRequest {
            protocol_version: FEDERATION_MAILBOX_WIRE_VERSION,
            schema_version: FEDERATION_MAILBOX_SCHEMA_VERSION.to_owned(),
            central: current.local,
            actor: Some(actor),
            operation: FederationMailboxOperation::Send {
                target: target.session_id,
                message: "one durable central append".to_owned(),
                key: "same-application-key".to_owned(),
                topic: None,
                state_stamp: None,
            },
        };
        Self { pair, request }
    }

    pub fn client(&self, port: u16) -> FederationRpcClient {
        let channel = self.pair.satellite_channel.clone();
        let checked = channel.clone();
        FederationRpcClient::new(
            port,
            channel,
            Arc::new(move || checked.current_authority().map(|_| ())),
        )
        .unwrap()
    }

    pub fn central_port(&self) -> u16 {
        self.pair
            .satellite_channel
            .snapshot
            .endpoint
            .central_federation_port
    }

    pub fn message_count(&self) -> u64 {
        self.pair
            .central
            .mailbox_store
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM mailbox_messages", [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

pub(super) struct ReceiptRelay {
    pub port: u16,
    pub committed: tokio::sync::mpsc::UnboundedReceiver<(StatusCode, Value)>,
    pub release: tokio::sync::mpsc::UnboundedSender<()>,
    stop: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ReceiptRelay {
    pub async fn new(target: u16, replay: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (committed_tx, committed) = tokio::sync::mpsc::unbounded_channel();
        let (release, released) = tokio::sync::mpsc::unbounded_channel();
        let released = Arc::new(tokio::sync::Mutex::new(released));
        let first = Arc::new(AtomicBool::new(true));
        let client = reqwest::Client::builder()
            .no_proxy()
            .no_gzip()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let router = Router::new().fallback(move |request: axum::extract::Request| {
            let client = client.clone();
            let committed = committed_tx.clone();
            let released = released.clone();
            let first = first.clone();
            async move {
                let (parts, body) = request.into_parts();
                let body = axum::body::to_bytes(body, FEDERATION_MAILBOX_REQUEST_BYTES)
                    .await
                    .unwrap();
                let target = format!("http://127.0.0.1:{target}{}", parts.uri);
                let forward = || {
                    client
                        .request(parts.method.clone(), target.clone())
                        .headers(parts.headers.clone())
                        .body(body.clone())
                        .send()
                };
                let response = forward().await.unwrap();
                let status = response.status();
                let headers = response.headers().clone();
                let bytes = response.bytes().await.unwrap();
                if first.swap(false, Ordering::SeqCst) {
                    committed
                        .send((status, serde_json::from_slice(&bytes).unwrap()))
                        .unwrap();
                    if replay {
                        // Suppress the committed receipt, deliver the byte-identical
                        // signed request again, and return only its signed refusal.
                        let duplicate = forward().await.unwrap();
                        let status = duplicate.status();
                        let headers = duplicate.headers().clone();
                        let body = duplicate.bytes().await.unwrap();
                        return (status, headers, body).into_response();
                    }
                    released.lock().await.recv().await.unwrap();
                }
                (status, headers, bytes).into_response()
            }
        });
        let (stop, mut rx) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    federation_wait_stop(&mut rx).await;
                })
                .await
                .unwrap();
        });
        Self {
            port,
            committed,
            release,
            stop,
            task,
        }
    }

    pub async fn finish(self) {
        self.stop.send_replace(true);
        self.task.await.unwrap();
    }
}
