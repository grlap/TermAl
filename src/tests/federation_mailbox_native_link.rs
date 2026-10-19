// Owns attested setup/teardown for native satellite delivery fixtures.
// Does not fabricate a connected lease or place sessions before proof.
// Extracted from the two-process mailbox fixture for the delivery-test carry.
use super::federation_mailbox_process::{Configuration, NativeSession, Satellite};
use super::*;

pub(super) fn channel(
    central: &AppState,
    native: &NativeSession,
    port: u16,
) -> (FederationPeerRecord, FederationChannel) {
    let mut record = federation_registry::peer(port);
    record.identity = native.host.clone();
    record.endpoint.satellite_federation_port = native.control_port;
    record.endpoint.satellite_reverse_forward_port = port;
    central.enroll_federation_peer(record.clone()).unwrap();
    let peer = central
        .federation_peer_snapshot(&native.host.host_id)
        .unwrap();
    let channel = FederationChannel::for_app(
        peer,
        FederationPairCapability::new("a".repeat(64)).unwrap(),
        central.clone(),
    );
    (record, channel)
}

pub(super) struct Control {
    stop: tokio::sync::watch::Sender<bool>,
    task: Option<tokio::task::JoinHandle<Result<(), ApiError>>>,
}

impl Control {
    pub async fn finish(mut self) {
        self.stop.send_replace(true);
        // A child shutdown or an explicit retirement test may have closed the
        // stream already. Always join the task; its guard owns exact teardown.
        let _ = self.task.take().unwrap().await.unwrap();
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub(super) async fn bind(
    central: &AppState,
    channel: &FederationChannel,
    mut satellite: Satellite,
    native: &NativeSession,
    record: FederationPeerRecord,
    port: u16,
) -> (Satellite, u16, Control) {
    let config = Configuration {
        central: channel.snapshot.local.clone(),
        record,
        port,
    };
    let (next, satellite_port) = tokio::task::spawn_blocking(move || {
        let port = satellite.configure(&config);
        (satellite, port)
    })
    .await
    .unwrap();
    satellite = next;
    let mut stream: FederationControlIo = Box::new(
        tokio::net::TcpStream::connect(("127.0.0.1", native.control_port))
            .await
            .unwrap(),
    );
    let (context, guard) = tokio::time::timeout(
        phase_sync::DEADLOCK_GUARD,
        federation_attest_central(channel, &mut stream),
    )
    .await
    .unwrap()
    .unwrap();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let (ready, received) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(federation_control_central(
        channel.clone(),
        stream,
        context,
        guard,
        stopped,
        ready,
    ));
    let control = Control {
        stop,
        task: Some(task),
    };
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, received)
        .await
        .unwrap()
        .unwrap();
    let connected = central
        .federation_peer_snapshot(&native.host.host_id)
        .unwrap();
    central
        .verify_federation_peer(&connected, federation_registry::checks())
        .unwrap();
    let verified = central
        .federation_peer_snapshot(&native.host.host_id)
        .unwrap();
    central
        .transition_federation_peer(&verified, FederationMembershipState::Active)
        .unwrap();
    let peer = central
        .federation_peer_snapshot(&native.host.host_id)
        .unwrap();
    let placement = central
        .reserve_federation_placement(
            &peer,
            "native-delivery-fixture".to_owned(),
            native.session.clone(),
            0,
            0,
        )
        .unwrap();
    let binding = FederationMailboxBinding {
        central: peer.local,
        identity: placement.identity,
        placement_generation: placement.placement_generation,
    };
    let (satellite, bound) = tokio::task::spawn_blocking(move || {
        let result = satellite.command(json!({"command":"bind", "binding":binding}));
        (satellite, result)
    })
    .await
    .unwrap();
    assert_eq!(bound, json!({"bound":true}));
    (satellite, satellite_port, control)
}
