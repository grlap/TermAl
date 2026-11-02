// Ordinary source-root diagnostics share the existing bounded line owner.
// Historical batch records have no decision, retry or send executor.
use super::*;

#[test]
fn a_batchs_member_lines_merge_and_never_push_a_bind_or_name_line_out() {
    let mut engram = EngramSessionState::default();
    let named = "[TermAl] The source root for the launching item is .tmp/wt-batch.".to_owned();
    engram.set_pending_source_root_line(named.clone());
    for member in ["a", "b", "c", "d", "e", "f"] {
        engram.set_pending_source_root_line(engram_batch_not_recorded_line(
            &format!("member-{member}"),
            "its claim moved",
        ));
    }
    engram.set_pending_source_root_line(engram_batch_member_lost_line("member-g", "before it settled"));
    let pending = engram.pending_source_root_line.clone().unwrap_or_default();
    assert!(pending.contains(&named), "{pending}");
    assert!(pending.contains("member-g"), "{pending}");
    assert_eq!(pending.lines().count(), 2, "{pending}");
}
