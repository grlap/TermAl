// Private evidence readers require complete identities. These fixtures reuse
// the existing native/bound wire shape and generate opaque ids for each read.
use super::*;

pub(super) fn record_id(legacy: bool) -> String {
    let current = Uuid::new_v4().simple().to_string();
    if legacy {
        format!("{current}{}", Uuid::new_v4().simple())
    } else {
        current
    }
}

pub(super) fn fingerprint() -> String {
    use sha2::Digest as _;
    format!("{:x}", sha2::Sha256::digest(Uuid::new_v4().as_bytes()))
}

const VERIFICATION: &str = "/resolution/satisfaction/verification";
const RECORD_FIELDS: [&str; 7] = [
    "/definition",
    "/triggering_observation",
    "/resolution/record",
    "/resolution/satisfaction/verification/record",
    "/resolution/satisfaction/verification/producer/record",
    "/resolution/satisfaction/verification/bound/verification",
    "/resolution/satisfaction/verification/bound/sighting",
];

fn closure(bound: bool, legacy: Option<bool>) -> (Value, Value, Value) {
    let (show, full, mut page) = if bound {
        super::bound::bound_receipts()
    } else {
        evidence_selection::canonical_binding_receipts()
    };
    let obligation = &mut page["rows"][1]["binding"]["obligation"];
    for (index, pointer) in RECORD_FIELDS.iter().enumerate() {
        if !bound && pointer.contains("/bound/") {
            continue;
        }
        *obligation
            .pointer_mut(pointer)
            .expect("fixture record field") = json!(record_id(legacy.unwrap_or(index % 2 == 0)));
    }
    *obligation
        .pointer_mut(&format!("{VERIFICATION}/check_fingerprint"))
        .expect("the fixture has a fingerprint") = json!(fingerprint());
    (show, full, page)
}

#[test]
fn canonical_closures_keep_current_legacy_and_mixed_record_ids() {
    for bound in [false, true] {
        for legacy in [Some(false), Some(true), None] {
            let (show, full, page) = closure(bound, legacy);
            let record = page["rows"][1]["binding"]["obligation"]
                .pointer(&format!("{VERIFICATION}/record"))
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let task = super::bound::apply(show, full, page)
                .expect("complete native or bound record identities are accepted");
            assert!(
                task.criterion_evidence.iter().any(|association| {
                    association.criterion == 2 && association.locators.contains(&record)
                }),
                "the original closure is associated with its criterion"
            );
            assert!(
                task.indexed_evidence
                    .iter()
                    .any(|entry| entry.locator == record)
            );
            // A short public window locator remains valid citation context.
            assert!(
                task.evidence
                    .iter()
                    .any(|entry| entry.locator == "ffffffff6666")
            );
        }
    }
}

#[test]
fn canonical_closures_refuse_partial_or_malformed_record_identities() {
    let id = record_id(false);
    let invalid = [
        String::new(),
        id[..12].to_owned(),
        format!("{id}12345678"),
        format!("A{}", &id[1..]),
        Uuid::new_v4().to_string(),
    ];
    for bound in [false, true] {
        for pointer in RECORD_FIELDS {
            if !bound && pointer.contains("/bound/") {
                continue;
            }
            for value in &invalid {
                let (show, full, mut page) = closure(bound, None);
                let field = page["rows"][1]["binding"]["obligation"]
                    .pointer_mut(pointer)
                    .expect("fixture record field");
                *field = json!(value);
                assert!(
                    super::bound::apply(show, full, page).is_err(),
                    "a malformed full record identity cannot close the criterion: {pointer}"
                );
            }
        }
    }
}

#[test]
fn canonical_closures_refuse_record_ids_and_prefixes_as_fingerprints() {
    let digest = fingerprint();
    for bound in [false, true] {
        for invalid in [
            digest[..12].to_owned(),
            record_id(false),
            digest[..40].to_owned(),
            digest[..63].to_owned(),
            format!("{digest}0"),
            format!("G{}", &digest[1..]),
        ] {
            let (show, full, mut page) = closure(bound, None);
            *page["rows"][1]["binding"]["obligation"]
                .pointer_mut(&format!("{VERIFICATION}/check_fingerprint"))
                .unwrap() = json!(invalid);
            assert!(
                super::bound::apply(show, full, page).is_err(),
                "only a complete SHA-256 fingerprint can describe the closing check"
            );
        }
    }
}
