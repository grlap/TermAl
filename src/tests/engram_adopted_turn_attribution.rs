//! A runtime-started (adopted) Claude turn's own top-level foreground
//! command is that turn's own: the observation keeps its source change and
//! its measured sighting. Other work restricting the turn is tested beside the
//! outstanding-work registry (`engram_claude_outstanding_work.rs`).
//!
//! Owns the end-to-end report shape of an adopted turn that ran its own Bash.
//! Does not own the hazard rule itself or the reporting fixture, which live
//! in `engram_unadmitted_runtime_reporting.rs`. New file.
use super::*;

#[test]
fn an_adopted_turns_own_foreground_bash_keeps_its_source_change_and_sighting() {
    let mut fixture = ReportingTurn::new("adopted-own-bash", true, ReplyMode::Recorded, false);
    fixture.turn.change_readme("changed by the adopted turn's own command\n");
    // A top-level foreground Bash of the adopted turn, ended by its own result.
    fixture.check("adopted-own-bash", false);
    fixture.turn.record(|record| {
        assert!(record.engram.unadmitted_active_turn.as_ref()
            .is_some_and(|trace| !trace.source_attribution_uncertain),
            "the turn's own command does not make its attribution uncertain");
    });
    fixture.finish();
    let turn = fixture.observation();
    assert_eq!(occurrence(&turn)["kind"], "unadmitted_turn");
    let change = &occurrence(&turn)["source_change"];
    assert_eq!(change["detection"], "content_comparison",
        "the adopted turn keeps its source change: {change}");
    assert_eq!(occurrence(&turn)["observed_checks"].as_array().unwrap().len(), 1,
        "its own check is reported with the turn");
    assert_eq!(turn.receipt["accounting"]["kind"], "source_change");
    let sighting = fixture.observation();
    assert_eq!(occurrence(&sighting)["kind"], "source_sighting",
        "a certain turn is followed by its measured sighting");
    assert_eq!(occurrence(&sighting)["source_basis"]["source_revision"],
        change["sighting"]["source_basis"]["source_revision"],
        "the sighting is the closing measurement of the same turn");
    fixture.assert_no_granted_activity();
}
