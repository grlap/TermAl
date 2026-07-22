// Codex executable discovery regression tests.
//
// The npm launcher is a Node process that spawns the bundled native Codex
// executable. TermAl resolves that native executable directly so restarting
// the shared app-server cannot kill only the launcher and orphan its child.

use super::*;

struct TempCodexPackage {
    root: PathBuf,
}

impl TempCodexPackage {
    fn create() -> Self {
        let root = std::env::temp_dir().join(format!("termal-codex-package-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("temporary Codex package root should exist");
        Self { root }
    }
}

impl Drop for TempCodexPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn resolves_native_binary_from_current_codex_npm_layout() {
    let package = TempCodexPackage::create();
    let launcher = package.root.join("bin").join("codex.js");
    fs::create_dir_all(launcher.parent().expect("launcher should have a parent"))
        .expect("launcher directory should exist");
    fs::write(&launcher, "#!/usr/bin/env node\n").expect("launcher should be created");

    let target_triple = codex_target_triple().expect("test platform should support Codex");
    let binary_name = if cfg!(windows) { "codex.exe" } else { "codex" };
    let native_binary = package
        .root
        .join("node_modules")
        .join("@openai")
        .join("codex-test-platform")
        .join("vendor")
        .join(target_triple)
        .join("bin")
        .join(binary_name);
    fs::create_dir_all(
        native_binary
            .parent()
            .expect("native binary should have a parent"),
    )
    .expect("native binary directory should exist");
    fs::write(&native_binary, "native codex").expect("native binary should be created");

    let resolved = resolve_codex_native_binary(&launcher)
        .expect("current Codex package layout should resolve the native binary");
    let expected = fs::canonicalize(native_binary)
        .expect("native binary path should canonicalize after creation");

    assert_eq!(
        normalize_user_facing_path(&resolved),
        normalize_user_facing_path(&expected),
        "macOS may spell the same temporary path through /var or /private/var",
    );
}
