// Immutable identity/codec controls only; permission and provider behavior
// belong to the existing federation and PromptDelivery owners, not this module.
use super::*;

fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
fn root(host: u128, creation: u128) -> NativeRoot {
    NativeRoot::new(id(host), 1, format!("session-{creation}"), id(creation)).unwrap()
}
fn intent() -> Intent {
    Intent::new(
        AuthorityBasis {
            source: root(1, 11),
            target: root(2, 22),
            grant: DecisionIdentity::new(id(3), 1).unwrap(),
            consent: DecisionIdentity::new(id(4), 1).unwrap(),
            enrollment_generation: NonZeroU64::new(1).unwrap(),
            placement_generation: None,
        },
        id(5),
        "exact-operation-key".into(),
        "complete body\r\nwith trailing newline\n".into(),
    )
    .unwrap()
}

#[test]
fn remote_prompt_native_identity_refuses_nil_zero_and_invalid_session() {
    for (host, epoch, session, creation) in [
        (Uuid::nil(), 1, "session-1", id(2)),
        (id(1), 0, "session-1", id(2)),
        (id(1), 1, "session-1", Uuid::nil()),
        (id(1), 1, "", id(2)),
        (id(1), 1, "session-1\n", id(2)),
        (id(1), 1, "remote/session-1", id(2)),
    ] {
        assert_eq!(
            NativeRoot::new(host, epoch, session.into(), creation),
            Err(Refusal::InvalidIdentity)
        );
    }
    assert_eq!(
        DecisionIdentity::new(id(3), 0),
        Err(Refusal::InvalidIdentity)
    );
    assert_eq!(
        DecisionIdentity::new(Uuid::nil(), 1),
        Err(Refusal::InvalidIdentity)
    );
    assert_eq!(root(1, 11).components(), (id(1), 1, "session-11", id(11)));
}

#[test]
fn remote_prompt_intent_digest_keeps_exact_body_and_every_identity_component() {
    let original = intent();
    let mut variants = Vec::new();
    for field in 0..12 {
        let mut changed = original.clone();
        match field {
            0 => changed.basis.source.host = id(90),
            1 => changed.basis.source.epoch = NonZeroU64::new(2).unwrap(),
            2 => changed.basis.source.session.push('x'),
            3 => changed.basis.source.creation = id(90),
            4 => changed.basis.target.creation = id(90),
            5 => changed.basis.grant = DecisionIdentity::new(id(90), 1).unwrap(),
            6 => changed.basis.consent = DecisionIdentity::new(id(4), 2).unwrap(),
            7 => changed.basis.enrollment_generation = NonZeroU64::new(2).unwrap(),
            8 => changed.basis.placement_generation = NonZeroU64::new(2),
            9 => changed.reference = id(90),
            10 => changed.client_key.push('x'),
            11 => changed.text = changed.text.replace("\r\n", "\n"),
            _ => unreachable!(),
        }
        variants.push(changed);
    }
    for changed in variants {
        assert_ne!(changed.digest(), original.digest());
    }
    assert_eq!(original.clone().digest(), original.digest());
    assert!(!format!("{original:?}").contains("complete body"));
}

#[test]
fn remote_prompt_identity_refuses_same_host_and_invalid_key_without_granting_permission() {
    let original = intent();
    let mut basis = original.basis.clone();
    basis.target.host = basis.source.host;
    assert!(Intent::new(basis, id(5), "key".into(), "body".into()).is_err());
    for key in ["".to_owned(), "line\nkey".to_owned(), "x".repeat(161)] {
        assert!(Intent::new(original.basis.clone(), id(5), key, "body".into()).is_err());
    }
    assert!(
        Intent::new(
            original.basis.clone(),
            Uuid::nil(),
            "key".into(),
            "body".into()
        )
        .is_err()
    );
}

#[test]
fn remote_prompt_retained_identity_debug_omits_key_body_and_digest() {
    let original = intent();
    let retained = original.retained_identity();
    let debug = format!("{retained:?}");
    assert!(!debug.contains(original.components().2));
    assert!(!debug.contains(original.components().3));
    assert!(!debug.contains(&format!("{:?}", original.digest())));
    assert!(debug.contains("RetainedIdentity"));
}
