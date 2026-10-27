// Owns the real proof-to-heartbeat RPC startup race regression.
// Does not bypass readiness, synthesize success or change protocol deadlines.

use super::*;
use super::super::federation_control_fixture::{attest, heartbeat_request, heartbeat_round_trip};
use super::super::federation_link::{allowed, channel};

fn sender_state(context: &FederationSecurityContext) -> (u64, usize, bool) {
    let sender = context.sender.lock().unwrap();
    (sender.next, sender.outstanding.len(), context.watchdog_started.load(Ordering::SeqCst))
}

#[tokio::test]
async fn federation_security_satellite_rpc_waits_for_current_central_heartbeat() {
    let mut central = channel(FederationRole::Central, 'a');
    let mut satellite = channel(FederationRole::Satellite, 'a');
    central.control.clock = FederationControlClock::scripted_io();
    satellite.control.clock = FederationControlClock::scripted_io();
    let _old_guards = attest(&central, &satellite).await;
    let old_context = satellite.security_context().unwrap();
    assert!(old_context.outgoing_rpc_ready.load(Ordering::SeqCst));
    let old_heartbeat = heartbeat_request(&central.security_context().unwrap(), 2);

    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = requests.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let router = federation_router(central.clone(), allowed()).layer(
        axum::middleware::from_fn(move |request: axum::extract::Request, next: axum::middleware::Next| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                next.run(request).await
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (events_tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    satellite.control.observed = Arc::new(move |event, sequence| {
        let _ = events_tx.send((event, sequence));
    });
    let (c, s) = tokio::io::duplex(FEDERATION_FRAME_LIMIT);
    let mut c: FederationControlIo = Box::new(c);
    let mut s: FederationControlIo = Box::new(s);
    let receiving = satellite.clone();
    let receiver = tokio::spawn(async move {
        let mut preface = vec![0; FEDERATION_CONTROL_PREFACE.len()];
        s.read_exact(&mut preface).await.unwrap();
        assert_eq!(preface, FEDERATION_CONTROL_PREFACE);
        federation_control_satellite(receiving, s).await;
    });
    // Stop central after receiving the real final proof, BEFORE central M1.
    c.write_all(FEDERATION_CONTROL_PREFACE).await.unwrap();
    let hello = federation_read_frame(&mut c).await.unwrap();
    let owner = central.next_security_owner().unwrap();
    let central_context = tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        federation_prove_central(&central, &mut c, owner, hello)).await.unwrap().unwrap();
    let context = satellite.security_context().unwrap();
    let lease = satellite.current_lease().unwrap();
    assert_ne!(context.incarnation, old_context.incarnation);
    assert_ne!(central.security_context().unwrap().incarnation, context.incarnation);
    let client = FederationRpcClient::new(port, satellite.clone(), allowed()).unwrap();
    let pending: Result<FederationHealth, ApiError> = client.request(
        FederationRpcRoute::Health(lease.clone())).await;
    assert_eq!(pending.err().expect("pending readiness must refuse locally").status,
        StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(requests.load(Ordering::SeqCst), 0, "no pre-install HTTP request");
    assert_eq!(sender_state(&context), (1, 0, false));
    assert!(satellite.security_current(&context).is_ok());

    // Neither a genuine old-context frame nor a corrupt new-context frame
    // can unlock this successor. Its receive loop must remain usable.
    let mut invalid = heartbeat_request(&central_context, 2);
    *invalid.last_mut().unwrap() ^= 1;
    for frame in [old_heartbeat, invalid] {
        federation_write_frame(&mut c, &frame).await.unwrap();
        let event = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, events.recv())
            .await.unwrap().unwrap();
        assert_eq!(event, ("requestRefused", 2));
        assert!(!context.outgoing_rpc_ready.load(Ordering::SeqCst));
        assert!(satellite.security_current(&context).is_ok());
    }
    let _central_guard = central.install_security(central_context.clone()).await.unwrap();
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD,
        heartbeat_round_trip(&mut c, &central_context, 1)).await.unwrap();
    let event = tokio::time::timeout(phase_sync::DEADLOCK_GUARD, events.recv())
        .await.unwrap().unwrap();
    assert_eq!(event, ("requestAccepted", 1));
    let health = client.health(&lease).await;
    let retained = satellite.current_lease().unwrap().incarnation;
    drop(c);
    tokio::time::timeout(phase_sync::DEADLOCK_GUARD, receiver).await.unwrap().unwrap();
    server.abort();
    assert!(health.is_ok());
    assert_eq!(retained, lease.incarnation, "pending RPC did not replace the link");
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(sender_state(&context), (2, 0, true));
}
