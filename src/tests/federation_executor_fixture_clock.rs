// Owns the witness that the executor delivery fixture's attestation of app
// channels does not race wall time. Does not own attestation expiry itself.
// Included by federation_executor_delivery.rs.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn executor_fixture_attestation_does_not_depend_on_elapsed_wall_time() {
    let mut fixture = Fixture::new();
    // A short window and a longer real delay: on a loaded machine the real
    // 10 s window is exceeded the same way.
    let window = Duration::from_millis(200);
    let (mut outbound, mut channel) = (fixture.outbound.clone(), fixture.channel.clone());
    outbound.control.attest_timeout = window;
    channel.control.attest_timeout = window;
    let proof =
        federation_control_fixture::attest_after_satellite_delay(&outbound, &channel, window * 5)
            .await;
    drop(std::mem::replace(&mut fixture._proof, proof));
    let (status, body) = fixture.receive_raw(&fixture.request).await;
    assert_eq!(status, StatusCode::ACCEPTED, "the re-attested link admits the wake: {body}");
}
