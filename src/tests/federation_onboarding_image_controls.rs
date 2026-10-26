// Owns unknown-image, lazy-hash and retained-descriptor failure controls.
// Does not replace the Windows install subprocess regression or provision hosts.
// Split from federation_onboarding_image.rs for portable image-state assertions.
use super::federation_onboarding_fixture::{Pair, io};
use super::*;

#[test]
fn federation_onboarding_real_profile_uses_federation_contract_not_storage_layout() {
    let measured = FederationOnboardingIo::local_with_image(
        std::env::current_dir().unwrap(),
        Arc::new(FederationProcessImage::capture(None)),
    );
    let actual = (measured.profile)().unwrap();
    assert_eq!(
        actual.protocol_version,
        u32::from(FEDERATION_PROTOCOL_VERSION)
    );
    assert_eq!(actual.schema_version, "2");
    assert_eq!(actual.schema_version, FEDERATION_PROFILE_SCHEMA_VERSION);
    assert_ne!(actual.schema_version, SQLITE_SCHEMA_VERSION);
}

#[test]
fn federation_onboarding_capture_and_disabled_io_do_not_hash_then_cache_once() {
    let app = federation_registry::domain_state();
    assert!(!app.inner.lock().unwrap().preferences.federation.enabled);
    let image = Arc::new(FederationProcessImage::capture(
        std::env::current_exe().ok(),
    ));
    let _io =
        FederationOnboardingIo::local_with_image(std::env::current_dir().unwrap(), image.clone());
    assert!(image.observation.get().is_none());
    assert_eq!(image.hash_reads.load(Ordering::SeqCst), 0);
    let first = image.observe();
    assert!(first.valid_identity());
    assert_eq!(image.observe(), first);
    assert_eq!(image.hash_reads.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn federation_onboarding_missing_image_is_diagnostic_and_does_not_gate_verification() {
    let image = Arc::new(FederationProcessImage::capture(None));
    let measured =
        FederationOnboardingIo::local_with_image(std::env::current_dir().unwrap(), image.clone());
    assert_eq!((measured.profile)().unwrap().binary_sha256, "");
    assert_eq!((measured.image)(), FederationImageObservation::Unknown);
    assert_eq!(image.hash_reads.load(Ordering::SeqCst), 0);
    // Keep the authenticated profile RPC successful, but give it an unknown
    // image; authenticated compatibility must not require image diagnostics.
    let mut satellite_io = io();
    satellite_io.image = measured.image;
    let pair = Pair::new(federation_registry::central(), satellite_io).await;
    let report = pair.verify().await.unwrap();
    assert_eq!(report.running_image, FederationImageObservation::Unknown);
    assert!(report.termal_complete());
    assert!(
        report
            .outcomes
            .iter()
            .any(|row| row.check == FederationOnboardingCheck::VersionSchema
                && row.passed
                && row.reason == "protocolSchemaMatched")
    );
    assert_eq!(
        pair.central
            .federation_peer_snapshot(&pair.id)
            .unwrap()
            .membership,
        FederationMembershipState::Verified
    );
    pair.finish().await;
}

#[test]
fn federation_onboarding_image_read_failure_is_unknown_without_path_fallback() {
    let image = Arc::new(FederationProcessImage::capture(
        std::env::current_exe().ok(),
    ));
    image.fail_hash.store(true, Ordering::SeqCst);
    let measured =
        FederationOnboardingIo::local_with_image(std::env::current_dir().unwrap(), image.clone());
    assert_eq!((measured.profile)().unwrap().binary_sha256, "");
    assert_eq!((measured.image)(), FederationImageObservation::Unknown);
    image.fail_hash.store(false, Ordering::SeqCst);
    assert_eq!(image.observe(), FederationImageObservation::Unknown);
    assert_eq!(image.hash_reads.load(Ordering::SeqCst), 1);
}
