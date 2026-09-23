//! Contracts of the Engram test fixtures themselves, independent of the
//! adapter behaviour they stand in for: today, the control fixture's refusal
//! to write anywhere but a TermAl test temp root (tm-47g6).
//!
//! Owns fixture self-protection tests. Does not own adapter conformance, the
//! fixtures' behaviour under a valid home, or `real_engram_control_fixture_path`
//! (the parent's readiness and compaction suites import it). New module beside
//! `src/tests/engram_host_adapter.rs`, created instead of growing that file.

use super::*;

/// Removes the outside-the-test-root home even when an assertion fails, so a
/// regressed guard cannot leave fixture output behind in `target/`, where the
/// test temp sweep never looks.
struct OutsideHome(PathBuf);

impl Drop for OutsideHome {
    fn drop(&mut self) {
        remove_test_directory(&self.0);
    }
}

#[test]
fn engram_control_fixture_refuses_a_home_outside_the_test_temp_root() {
    // The fixture writes a placeholder store and marker files under --home.
    // Every TermAl test root lives below <temp>/termal/tests; a home anywhere
    // else (the developer's real ~/.engram, once, tm-47g6) must be refused
    // before any write, with an exit code no other fixture exit uses.
    // The containment wrapper redirects TEMP into the run root, so a home
    // under std::env::temp_dir() would carry the marker; use the crate's
    // default target/ directory instead (git-ignored, and a harmless landing
    // spot if the guard ever regressed; with CARGO_TARGET_DIR set elsewhere it
    // is simply an empty ignored directory).
    let outside = OutsideHome(
        FsPath::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("termal-fixture-guard-{}", Uuid::new_v4())),
    );
    fs::create_dir_all(&outside.0).expect("outside home should exist");
    let project_file = outside.0.join(".engram-project");
    fs::write(&project_file, "fixture-ready\n").expect("fixture mode should write");
    // A path that enters a termal/tests directory and climbs back out resolves
    // to the same outside home, and an unrelated directory can spell the
    // termal/tests pair itself; a substring check alone would accept both.
    let traversal_alias = outside.0.join("termal").join("tests").join("..").join("..");
    let unrelated_root = outside.0.join("termal").join("tests").join("home");
    // A sibling sharing the root's prefix is what the component boundary in
    // the containment check exists to reject; a plain prefix match would
    // accept it. The fixture derives the root from the same environment as
    // test_temp_dir(), so the sibling is placed beside the real root and is
    // never created here: it must not exist afterwards either.
    let user_temp = std::env::var_os("TERMAL_TEST_USER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let sibling = OutsideHome(
        user_temp
            .join("termal")
            .join(format!("tests-sibling-{}", Uuid::new_v4())),
    );
    // A home that starts with the real root and then climbs out of it passes
    // a lexical prefix match; only the `..` refusal (or, on Windows, the
    // normalisation before the containment check) stops it. The guard owns
    // the resolved location the fixture would create if that rule regressed.
    let escape = OutsideHome(
        user_temp
            .join("termal")
            .join(format!("tests-escape-{}", Uuid::new_v4())),
    );
    let escape_spelling = user_temp
        .join("termal")
        .join("tests")
        .join("..")
        .join(escape.0.file_name().expect("escape home has a name"));
    for (label, home) in [
        ("direct", outside.0.clone()),
        ("traversal alias", traversal_alias),
        ("unrelated root spelling termal/tests", unrelated_root),
        ("same-prefix sibling of the test root", sibling.0.clone()),
        ("escape from inside the test root", escape_spelling),
    ] {
        let output = engram_command(&real_engram_control_fixture_path())
            .arg("--project-file")
            .arg(&project_file)
            .arg("--home")
            .arg(&home)
            .args(["readiness", "--json"])
            .output()
            .expect("fixture should run");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(4),
            "{label}: fixture must refuse {}; stdout={stdout} stderr={stderr}",
            home.display()
        );
        assert!(
            !sibling.0.exists(),
            "{label}: the fixture must not create a sibling of the test root"
        );
        assert!(
            !escape.0.exists(),
            "{label}: the fixture must not create the escape home beside the test root"
        );
        assert!(
            stderr.contains("refusing --home outside a TermAl test temp root"),
            "{label}: {stderr}"
        );
        let mut entries = fs::read_dir(&outside.0)
            .expect("outside home should be listable")
            .map(|entry| {
                entry
                    .expect("entry should be readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        assert_eq!(
            entries,
            vec![".engram-project".to_owned()],
            "{label}: the fixture must not write anything outside the test root"
        );
    }
}
