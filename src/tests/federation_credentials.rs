// Owns credential-reference/material tests with a fake OS-store reader.
// Does not read real secrets or provision hosts. New tests beside federation_runtime.

use super::federation_link::peer_snapshot;
use super::*;

#[test]
fn federation_credential_reader_scopes_pair_and_pin_references_and_fingerprints_actual_material() {
    let root = TestTempRoot::create("federation-known-hosts");
    let path = root.path().join("pin with spaces");
    fs::write(&path, "pair-host-key ssh-ed25519 AAAATEST\n").unwrap();
    let snapshot = peer_snapshot(FederationRole::Central);
    let read = |kind: &str, reference: &str| match kind {
        "pair" => {
            assert_eq!(reference, snapshot.pair_capability_ref);
            Ok("a".repeat(64))
        }
        "known-hosts" => {
            assert_eq!(reference, snapshot.endpoint.known_hosts_ref);
            Ok(path.to_str().unwrap().to_owned())
        }
        _ => panic!("unexpected credential namespace"),
    };
    let first = resolve_federation_credentials_with(&snapshot, read).unwrap();
    assert_eq!(first.known_hosts.as_deref(), Some(path.as_path()));
    assert_eq!(
        first.material_fingerprint,
        resolve_federation_credentials_with(&snapshot, read)
            .unwrap()
            .material_fingerprint
    );
    fs::write(&path, "pair-host-key ssh-ed25519 BBBBTEST\n").unwrap();
    assert_ne!(
        first.material_fingerprint,
        resolve_federation_credentials_with(&snapshot, read)
            .unwrap()
            .material_fingerprint
    );
    let rotated = resolve_federation_credentials_with(&snapshot, |kind, reference| {
        if kind == "pair" {
            Ok("b".repeat(64))
        } else {
            read(kind, reference)
        }
    })
    .unwrap();
    assert_ne!(first.material_fingerprint, rotated.material_fingerprint);
}

#[test]
fn federation_credential_reader_refuses_missing_invalid_and_unbounded_material() {
    let satellite = peer_snapshot(FederationRole::Satellite);
    assert!(
        resolve_federation_credentials_with(&satellite, |_, _| Err(federation_credential_error()))
            .is_err()
    );
    assert!(
        resolve_federation_credentials_with(&satellite, |_, _| Ok(
            "reference-is-not-a-bearer".to_owned()
        ))
        .is_err()
    );
    let one = resolve_federation_credentials_with(&satellite, |kind, _| {
        assert_eq!(
            kind, "pair",
            "satellite must never resolve the central's SSH pins"
        );
        Ok("a".repeat(64))
    })
    .unwrap();
    assert!(one.known_hosts.is_none());
    let root = TestTempRoot::create("federation-bad-known-hosts");
    let path = root.path().join("pin");
    let central = peer_snapshot(FederationRole::Central);
    for contents in [Vec::new(), vec![b'a'; 64 * 1024 + 1]] {
        fs::write(&path, contents).unwrap();
        assert!(
            resolve_federation_credentials_with(&central, |kind, _| {
                if kind == "pair" {
                    Ok("a".repeat(64))
                } else {
                    Ok(path.to_str().unwrap().to_owned())
                }
            })
            .is_err()
        );
    }
}
