// Owns per-peer pressure/retry regression tests, not enrollment or real SSH.
// New coverage for federation transport; no source was split from another file.

use super::*;

#[test]
fn federation_rpc_limit_releases_capacity_after_cancellation() {
    let bound = FederationRpcBound::default();
    let other_peer = FederationRpcBound::default();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(bound.try_acquire().unwrap());
    }
    assert_eq!(
        bound.try_acquire().unwrap_err().status,
        StatusCode::TOO_MANY_REQUESTS
    );
    let independent = other_peer.try_acquire().unwrap();
    drop(held.pop());
    let replacement = bound.try_acquire().unwrap();
    assert!(bound.try_acquire().is_err());
    drop(replacement);
    drop(held);
    drop(independent);
    assert!(bound.try_acquire().is_ok());
}

#[test]
fn federation_retry_exponent_jitter_cap_and_reset() {
    let mut retry = FederationBackoff::default();
    for attempt in 0..24_u32 {
        let low = retry.delay_for_entropy(0);
        let high = retry.delay_for_entropy(u64::MAX);
        let upper = Duration::from_millis((1000_u64 << (attempt + 1).min(9)).min(300_000));
        assert!(low >= Duration::from_secs(1));
        assert!(low <= high);
        assert!(high <= upper);
        assert_eq!(high, upper);
        assert_ne!(
            low, high,
            "retries need jitter, including the first attempt"
        );
        retry.record_failure();
    }
    retry.reset();
    assert_eq!(retry.delay_for_entropy(0), Duration::from_secs(1));
    assert_eq!(retry.delay_for_entropy(u64::MAX), Duration::from_secs(2));
}

#[test]
fn federation_first_retry_has_jitter_without_delaying_below_one_second() {
    let retry = FederationBackoff::default();
    assert!(retry.delay_for_entropy(0) >= Duration::from_secs(1));
    assert!(retry.delay_for_entropy(u64::MAX) > retry.delay_for_entropy(0));
}
