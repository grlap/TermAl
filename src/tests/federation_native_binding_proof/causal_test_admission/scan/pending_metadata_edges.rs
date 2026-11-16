// Additional corruption boundaries for the pending metadata owner. The original
// failing pending_metadata tests remain unchanged; this exercises a damaged
// prefix larger than one selection page and invalid SQLite retry-order types.
use super::*;

#[derive(Clone, Copy, Debug)]
enum MetadataEdge {
    Json,
    NullOrder,
    TextOrder,
    BlobOrder,
    RealOrder,
}

fn metadata_edge_rows(
    fixture: &Fixture,
    include_metadata: bool,
) -> Vec<Vec<rusqlite::types::Value>> {
    let connection = fixture.central.mailbox_store.connection().unwrap();
    let columns = if include_metadata {
        "id,test_request_json,test_request_scope_json,test_request_pending_json"
    } else {
        "id,test_request_json,test_request_scope_json"
    };
    let mut statement = connection.prepare(&format!(
        "SELECT {columns} FROM mailbox_messages WHERE idempotency_key LIKE 'edge-waiting-%' ORDER BY rowid"
    )).unwrap();
    let count = if include_metadata { 4 } else { 3 };
    let rows = statement
        .query_map([], |row| {
            (0..count)
                .map(|column| row.get(column))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

async fn metadata_edge_progress(edge: MetadataEdge) {
    let _temp_scope = TestTempRootDirectoryScope::repository_local();
    let fixture = Fixture::production_shape().await;
    let (mut server, handles) =
        super::super::super::causal_test_completion::connect(&fixture).await;
    let (input, transport, scope) = destination(fixture.central.clone(), &fixture.native);
    for number in 0..40 {
        local_request(&fixture, &input, &format!("edge-waiting-{number}"));
    }
    let mut expected = Vec::new();
    for key in ["edge-neighbor", "edge-tail"] {
        let (run, directory, selector) = original(&fixture, key, input.clone()).await;
        finish(
            &fixture.satellite,
            &run,
            &directory,
            native_test_completion::Verdict::Pass,
            source(),
            source(),
        );
        expected.push((run, selector));
    }
    let (column, value) = match edge {
        MetadataEdge::Json => (
            "test_request_pending_json",
            rusqlite::types::Value::Text("{private-metadata-edge".into()),
        ),
        MetadataEdge::NullOrder => ("test_request_pending_order", rusqlite::types::Value::Null),
        MetadataEdge::TextOrder => (
            "test_request_pending_order",
            rusqlite::types::Value::Text("private-metadata-edge".into()),
        ),
        MetadataEdge::BlobOrder => (
            "test_request_pending_order",
            rusqlite::types::Value::Blob(b"private-metadata-edge".to_vec()),
        ),
        MetadataEdge::RealOrder => (
            "test_request_pending_order",
            rusqlite::types::Value::Real(4096.5),
        ),
    };
    assert_eq!(fixture.central.mailbox_store.connection().unwrap().execute(
        &format!("UPDATE mailbox_messages SET {column}=?1 WHERE idempotency_key LIKE 'edge-waiting-%'"),
        [&value]).unwrap(), 40);
    let raw = metadata_edge_rows(&fixture, true);
    let custody = metadata_edge_rows(&fixture, false);
    let diagnostic = get_mailbox_relay_wake_diagnostics(State(fixture.central.clone()))
        .await
        .ok()
        .map(|response| response.0);
    let diagnostic_ok = diagnostic.as_ref().is_some_and(|value| {
        value["causalCompletionPending"]["records"]
            .as_array()
            .is_some_and(|records| {
                records.len() <= CAUSAL_PENDING_DIAGNOSTICS
                    && records
                        .iter()
                        .filter(|record| {
                            record["table"] == "mailbox_messages"
                                && record["state"]["reason"] == "pending_metadata_decode"
                        })
                        .count()
                        == 40
            })
            && !value.to_string().contains("private-metadata-edge")
            && !value.to_string().contains("edge-waiting-")
            && !value.to_string().contains("legacyCustodyDigest")
    });
    // The first bounded selection is entirely damaged. It must commit its
    // rotations even though it produces no plan, exposing the healthy tail.
    let first = fixture.central.mailbox_store.causal_test_completion_plans();
    let rotated_without_plans = first.as_ref().is_ok_and(|plans| plans.is_empty());
    let raw_preserved_on_rotation = metadata_edge_rows(&fixture, true) == raw;
    let accepted = receipt(None);
    for _ in &expected {
        script(
            &transport,
            &scope,
            &fixture.native,
            Ok(accepted.clone()),
            true,
        );
    }
    let mut recovery = CausalTestCompletionRecovery::default();
    for _ in 0..4 {
        recovery.tick(&fixture.central, true).await;
        recovery
            .task
            .take()
            .expect("one bounded recovery task")
            .await
            .unwrap();
        if expected.iter().all(|(_, selector)| {
            fixture
                .central
                .mailbox_store
                .causal_test_settlement(selector, Some(&scope))
                .unwrap()
                .is_some_and(|saved| saved.complete())
        }) {
            break;
        }
    }
    recovery.shutdown().await;
    let settled = expected.iter().all(|(run, selector)| {
        fixture
            .central
            .mailbox_store
            .causal_test_settlement(selector, Some(&scope))
            .unwrap()
            .is_some_and(|saved| {
                saved.complete()
                    && saved.completion.run_id == *run
                    && saved.admissions[0].receipt == Some(accepted.clone())
            })
    });
    let sent = writes(&transport);
    let actual_runs = sent
        .iter()
        .map(|write| {
            write.request["completion"]["run_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<std::collections::BTreeSet<_>>();
    let expected_runs = expected
        .into_iter()
        .map(|(run, _)| run)
        .collect::<std::collections::BTreeSet<_>>();
    let custody_preserved = metadata_edge_rows(&fixture, false) == custody;
    let no_local_run = fixture
        .central
        .inner
        .lock()
        .unwrap()
        .test_run_registrations
        .is_empty();
    let observation = (
        diagnostic_ok,
        rotated_without_plans,
        raw_preserved_on_rotation,
        settled,
        sent.len(),
        custody_preserved,
        no_local_run,
    );
    drop(handles);
    server.finish().await;
    fixture.finish().await;
    assert_eq!(
        observation,
        (true, true, true, true, 2, true, true),
        "edge={edge:?}"
    );
    assert_eq!(
        actual_runs, expected_runs,
        "only the original healthy completions are admitted"
    );
}

#[tokio::test]
async fn damaged_pending_prefix_and_retry_order_types_do_not_starve_healthy_originals() {
    for edge in [
        MetadataEdge::Json,
        MetadataEdge::NullOrder,
        MetadataEdge::TextOrder,
        MetadataEdge::BlobOrder,
        MetadataEdge::RealOrder,
    ] {
        metadata_edge_progress(edge).await;
    }
}
