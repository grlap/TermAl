// Owns the witness that this fixture's attestation does not race wall time.
// Does not own attestation expiry itself, which federation_onboarding_deadlines
// tests by advancing a scripted clock past attest_timeout.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_attestation_does_not_depend_on_elapsed_wall_time() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::new(false).await;
    // A short window and a longer real delay: on a loaded machine the real
    // 10 s window is exceeded the same way.
    let window = Duration::from_millis(200);
    let (mut central, mut satellite) = (fixture.channel.clone(), fixture.satellite_channel.clone());
    central.control.attest_timeout = window;
    satellite.control.attest_timeout = window;
    let proof = federation_control_fixture::attest_after_satellite_delay(
        &central, &satellite, window * 5,
    )
    .await;
    let old = fixture.attested.lock().unwrap().replace(proof);
    drop(old);
    fixture.rpc.health(&fixture.channel.current_lease().unwrap()).await
        .expect("the re-attested link is current");
    fixture.finish().await;
}
