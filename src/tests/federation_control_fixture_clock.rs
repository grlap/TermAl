// Owns the witness that the shared fixture's attestation of plain channels does
// not race wall time. Does not own attestation expiry itself, which
// federation_onboarding_deadlines tests by advancing a scripted clock.
// Included by federation_control_fixture.rs.

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_fixture_attestation_does_not_depend_on_elapsed_wall_time() {
    // Plain channels, as establish and the link, RPC and listener tests build
    // them. A short window and a longer real delay: on a loaded machine the
    // real 10 s window is exceeded the same way.
    let window = Duration::from_millis(200);
    let mut central = federation_link::channel(FederationRole::Central, 'a');
    let mut satellite = federation_link::channel(FederationRole::Satellite, 'a');
    central.control.attest_timeout = window;
    satellite.control.attest_timeout = window;
    let _proof = attest_after_satellite_delay(&central, &satellite, window * 5).await;
    // The proof is installed in the callers' own link state.
    assert_eq!(
        central.security_context().unwrap().incarnation,
        satellite.security_context().unwrap().incarnation,
    );
}
