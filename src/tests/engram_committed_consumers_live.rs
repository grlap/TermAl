//! Real consumer commits through the compiled router, maintained launcher and
//! pinned Engram. Sources are read-only object donors; all changes are confined
//! to fresh disposable clones. No result, receipt, evaluation or seal is minted
//! by this fixture. Provider events are the existing simulated runtime ingress.
use super::*;

struct Consumer {
    name: &'static str,
    commit: &'static str,
    children: &'static [(&'static str, &'static str)],
}

const QUESTLY: Consumer = Consumer {
    name: "questly",
    commit: "75250b57407bddbaadd1fb4cdf5c69e9cafa6973",
    children: &[("rincon-common", "b951c661957e8888840cdd857c246cf3047b62c3")],
};
const PHOENIX: Consumer = Consumer {
    name: "phoenix",
    commit: "74ae7b25d18bd920d91e5fbbe3ec2432dec14d28",
    children: &[
        (
            "external/fsharp",
            "5928e91b5f701586690562ce10bd639357fff50b",
        ),
        (
            "external/roslyn",
            "e7680f251b8fc52a0ba7108906c0e40ff3263a32",
        ),
    ],
};

fn source_path(value: &Value) -> PathBuf {
    let path = PathBuf::from(
        value
            .as_str()
            .expect("manifest supplies an absolute object donor"),
    );
    assert!(path.is_absolute() && path.is_dir(), "{}", path.display());
    let canonical = fs::canonicalize(path).unwrap();
    PathBuf::from(identity_git_path(&canonical).expect("object donor has a Git-compatible path"))
}

fn consumer_fixture(consumer: &Consumer) -> CompletionFixture {
    let manifest_path = PathBuf::from(
        std::env::var_os("TERMAL_TEST_SUBMODULE_CONSUMERS")
            .expect("reviewed consumer object-donor manifest is required"),
    );
    assert!(manifest_path.is_absolute());
    let bytes = fs::read(&manifest_path).unwrap();
    let manifest: Value = serde_json::from_slice(&bytes).unwrap();
    let donors = &manifest[consumer.name];
    let source = source_path(&donors["root"]);
    let fixture = completion_fixture_for_repository(|root| {
        // --shared reads committed objects only, never the donor's dirty files.
        // Donors must remain retained until all rows using them have finished.
        run_git_test_command(
            root,
            &[
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                "--",
                source.to_str().unwrap(),
                ".",
            ],
        );
        run_git_test_command(root, &["config", "core.longpaths", "true"]);
        run_git_test_command(root, &["checkout", "--quiet", "--detach", consumer.commit]);
        run_git_test_command(root, &["config", "user.name", "Consumer fixture"]);
        run_git_test_command(root, &["config", "user.email", "fixture@example.invalid"]);
        let mappings = run_git_test_command_output(
            root,
            &[
                "config",
                "--file",
                ".gitmodules",
                "--get-regexp",
                r"^submodule\..*\.path$",
            ],
        );
        for (path, pin) in consumer.children {
            let key = mappings
                .lines()
                .find_map(|line| {
                    let (key, mapped) = line.split_once(' ')?;
                    (mapped == *path)
                        .then(|| key.strip_suffix(".path").unwrap().to_owned() + ".url")
                })
                .expect("the actual consumer maps each expected direct child");
            let child_source = source_path(&donors["children"][*path]);
            run_git_test_command(root, &["config", &key, child_source.to_str().unwrap()]);
            run_git_test_command(
                root,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "-c",
                    "core.longpaths=true",
                    "submodule",
                    "update",
                    "--init",
                    "--",
                    path,
                ],
            );
            run_git_test_command(&root.join(path), &["config", "core.longpaths", "true"]);
            assert_eq!(
                run_git_test_command_output(&root.join(path), &["rev-parse", "HEAD"]).trim(),
                *pin
            );
        }
        // A consumer's tracked project binding is preserved byte-for-byte. If
        // absent, the fixture's own binding is tooling, ignored in Git metadata.
        if !root.join(".engram-project").exists() {
            let exclude =
                run_git_test_command_output(root, &["rev-parse", "--git-path", "info/exclude"]);
            let exclude = root.join(exclude.trim());
            if let Some(parent) = exclude.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut content = fs::read(&exclude).unwrap_or_default();
            content.extend_from_slice(b"\n/.engram-project\n");
            fs::write(exclude, content).unwrap();
            fs::write(
                root.join(".engram-project"),
                format!("consumer-fixture-{}\n", Uuid::new_v4()),
            )
            .unwrap();
        }
        assert_eq!(
            run_git_test_command_output(root, &["rev-parse", "HEAD"]).trim(),
            consumer.commit
        );
        assert!(
            run_git_test_command_output(
                root,
                &[
                    "-c",
                    "status.showUntrackedFiles=all",
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignore-submodules=none"
                ]
            )
            .is_empty()
        );
    });
    eprintln!(
        "CONSUMER_FIXTURE {}",
        json!({"consumer":consumer.name,"commit":consumer.commit,
        "tree":run_git_test_command_output(&fixture.root, &["rev-parse", "HEAD^{tree}"]).trim(),
        "children":consumer.children,"manifestSha256":sha256_hex(&bytes),"root":fixture.root})
    );
    fixture
}

const DRIVER: &str = r#"
import { pathToFileURL } from 'node:url';
const [root, launcher, probe] = process.argv.slice(2);
const { createRun, executeRun, summarize } = await import(pathToFileURL(launcher).href);
const runDir = await createRun({ root, automaticInput: true,
  stages: [{ name: 'focused', command: process.execPath, args: ['--test', probe] }] });
console.log('RUN ' + runDir);
const result = await executeRun(runDir);
console.log(await summarize(runDir));
process.exitCode = result.exitCode;
"#;

fn launch_consumer(matrix: &Matrix, commit: &str, children: &[(&str, &str)]) -> ActualCheck {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let launcher = source.join("scripts/test-launcher.mjs");
    let probe = source.join("scripts/committed-consumer-probe.test.mjs");
    assert!(launcher.is_file() && probe.is_file());
    let driver = matrix.fixture.home.join("consumer-launch.mjs");
    fs::write(&driver, DRIVER).unwrap();
    let temporary = matrix.fixture.home.join("consumer-command-tmp");
    fs::create_dir_all(&temporary).unwrap();
    let expected = json!({"commit":commit,
        "children":children.iter().map(|(p,c)| ((*p).to_owned(), json!(c))).collect::<serde_json::Map<_,_>>()});
    let mut command = Command::new(if cfg!(windows) { "node.exe" } else { "node" });
    command.env_clear();
    for key in ["PATH", "SystemRoot", "WINDIR", "COMSPEC", "PATHEXT"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .env("HOME", &matrix.fixture.home)
        .env("USERPROFILE", &matrix.fixture.home)
        .env("TMPDIR", &temporary)
        .env("TMP", &temporary)
        .env("TEMP", &temporary)
        .env("TERMAL_SESSION_ID", &matrix.holder)
        .env("TERMAL_BASE_URL", &matrix.service.url)
        .env("TERMAL_TEST_CONSUMER_PIN_PROBE", expected.to_string())
        .current_dir(&matrix.fixture.root)
        .arg(&driver)
        .arg(&matrix.fixture.root)
        .arg(&launcher)
        .arg(&probe);
    let key = format!("consumer-check-{}", Uuid::new_v4());
    let mut events = matrix
        .provider
        .events(&matrix.fixture, &matrix.holder, &key, false);
    // Report the actual library driver invocation. It is not the CLI's
    // script-relative focused form, so this deliberately earns no invented
    // ordinary Test/Build observation or durable association.
    events.command_line(&format!(
        "node \"{}\" \"{}\" \"{}\" \"{}\"",
        driver.display(),
        matrix.fixture.root.display(),
        launcher.display(),
        probe.display()
    ));
    events.start();
    let output = command
        .output()
        .expect("real consumer launcher driver starts");
    let mut check = matrix.actual_check_output(output);
    assert_eq!(check.exit, 0, "{}", check.output);
    assert_eq!(check.results["input"]["eligible"], true);
    let (before, after) = test_run_source_endpoints(&check.results)
        .expect("the real launcher retained complete owner source endpoints");
    assert_eq!(before, after);
    assert_eq!(before.commit, commit);
    assert_eq!(check.results["input"]["commit"], commit);
    assert_eq!(check.results["stages"][0]["tests"]["passed"], 1);
    matrix.receive_unplaced_check(events, &mut check);
    assert!(
        !check.records.is_empty(),
        "actual Engram Received receipt has verification records"
    );
    check
}

fn immutable_bytes(matrix: &Matrix) -> Vec<(String, Vec<u8>)> {
    let database = matrix.fixture.store().database_path;
    assert!(database.starts_with(&matrix.fixture.home));
    let connection =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut query = connection
        .prepare("SELECT object_id, canonical_json FROM objects ORDER BY object_id")
        .unwrap();
    let rows = query
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    rows
}

fn assert_immutable_bytes(matrix: &Matrix, originals: &[(String, Vec<u8>)]) {
    let current: BTreeMap<_, _> = immutable_bytes(matrix).into_iter().collect();
    for (id, bytes) in originals {
        assert_eq!(current.get(id), Some(bytes), "immutable object {id}");
    }
}

fn clean_consumer(consumer: &Consumer) {
    let matrix = Matrix::from_fixture(
        consumer_fixture(consumer),
        Citation::BoundVerification,
        false,
    );
    matrix.begin("Check the exact real consumer commit without altering its source.");
    let check = launch_consumer(&matrix, consumer.commit, consumer.children);
    matrix.close();
    matrix.settle();
    let originals = immutable_bytes(&matrix);
    matrix.accept(&check, || {});
    assert_immutable_bytes(&matrix, &originals);
    assert_eq!(
        run_git_test_command_output(&matrix.fixture.root, &["rev-parse", "HEAD"]).trim(),
        consumer.commit
    );
}

#[derive(Clone, Copy, Debug)]
enum Change {
    TrackedChild,
    UntrackedChild,
    HiddenUntracked,
    OffPin,
    Uninitialized,
    CleanFilter,
    SmudgeFilter,
    ProcessFilter,
    Nested,
    DirtyRoot,
    NewRootCommit,
    MissingMapping,
}

fn tracked_path(root: &FsPath) -> PathBuf {
    let listing = run_git_test_command_output(root, &["ls-files", "-z"]);
    listing
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(|p| root.join(p))
        .find(|p| fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() < 65536))
        .expect("real repository supplies a small tracked regular file")
}

fn mutate(matrix: &Matrix, consumer: &Consumer, change: Change) {
    let root = &matrix.fixture.root;
    let (path, _) = consumer.children[0];
    let child = root.join(path);
    match change {
        Change::TrackedChild => {
            let file = tracked_path(&child);
            let mut bytes = fs::read(&file).unwrap();
            bytes.extend_from_slice(b"\nconsumer unchecked input\n");
            fs::write(file, bytes).unwrap();
        }
        Change::UntrackedChild | Change::HiddenUntracked => {
            if matches!(change, Change::HiddenUntracked) {
                run_git_test_command(&child, &["config", "status.showUntrackedFiles", "no"]);
            }
            let file = format!("untracked-{}.fixture-input", Uuid::new_v4());
            fs::write(child.join(&file), "unchecked child input\n").unwrap();
            assert!(
                !run_git_test_command_output(
                    &child,
                    &[
                        "-c",
                        "status.showUntrackedFiles=all",
                        "status",
                        "--porcelain=v1",
                        "--untracked-files=all",
                        "--",
                        &file
                    ]
                )
                .is_empty(),
                "the negative fixture's new file must be nonignored"
            );
        }
        Change::OffPin => run_git_test_command(
            &child,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "different child commit",
            ],
        ),
        Change::Uninitialized => {
            run_git_test_command(root, &["submodule", "deinit", "--force", "--", path])
        }
        Change::CleanFilter | Change::SmudgeFilter | Change::ProcessFilter => {
            let driver = match change {
                Change::CleanFilter => "clean",
                Change::SmudgeFilter => "smudge",
                _ => "process",
            };
            run_git_test_command(
                &child,
                &[
                    "config",
                    &format!("filter.consumer_fixture.{driver}"),
                    "must-not-execute",
                ],
            );
        }
        Change::Nested => {
            let donor = matrix.fixture.home.join("nested-donor");
            fs::create_dir_all(&donor).unwrap();
            run_git_test_command(&donor, &["init", "--quiet", "--template="]);
            fs::write(donor.join("nested.txt"), "nested pinned input\n").unwrap();
            run_git_test_command(&donor, &["add", "--", "nested.txt"]);
            run_git_test_command(
                &donor,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "nested input",
                ],
            );
            let donor = identity_git_path(&fs::canonicalize(donor).unwrap()).unwrap();
            run_git_test_command(
                &child,
                &[
                    "-c",
                    "core.autocrlf=false",
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    &donor,
                    "nested-fixture",
                ],
            );
            run_git_test_command(
                &child.join("nested-fixture"),
                &["config", "core.autocrlf", "false"],
            );
            run_git_test_command(
                &child,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "-am",
                    "pin nested input",
                ],
            );
            run_git_test_command(root, &["add", "--", path]);
            run_git_test_command(root, &["commit", "--quiet", "-m", "pin nested child"]);
        }
        Change::DirtyRoot => {
            let file = tracked_path(root);
            let mut bytes = fs::read(&file).unwrap();
            bytes.extend_from_slice(b"\nunchecked root input\n");
            fs::write(file, bytes).unwrap();
        }
        Change::NewRootCommit => run_git_test_command(
            root,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "-m",
                "different root commit",
            ],
        ),
        Change::MissingMapping => {
            fs::write(root.join(".gitmodules"), "").unwrap();
            run_git_test_command(root, &["add", "--", ".gitmodules"]);
            run_git_test_command(root, &["commit", "--quiet", "-m", "remove child mappings"]);
        }
    }
}

fn refuse_consumer_done(matrix: &Matrix, check: &ActualCheck, consumer: &Consumer, change: Change) {
    let expected = match change {
        Change::OffPin => "HEAD differs from its pin",
        Change::Uninitialized => "uninitialized",
        Change::CleanFilter | Change::SmudgeFilter | Change::ProcessFilter => {
            "configured Git filters are unsupported"
        }
        Change::Nested => {
            unreachable!("admitted rows use the positive consumer path")
        }
        Change::MissingMapping => {
            let output = matrix.done_output(check);
            assert!(!output.status.success());
            let body: Value = serde_json::from_slice(&output.stdout)
                .or_else(|_| serde_json::from_slice(&output.stderr))
                .unwrap();
            assert_eq!(
                body["error"]["code"], "work_completion_refused",
                "<body withheld>"
            );
            assert_eq!(
                body["error"]["details"]["work_ref"], matrix.work,
                "<body withheld>"
            );
            let reason = body["error"]["details"]["reason"].as_str().unwrap();
            assert!(
                reason.contains("no submodule mapping found")
                    && reason.contains(consumer.children[0].0),
                "<body withheld>"
            );
            let core = matrix
                .fixture
                .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
            assert!(core["run"]["completion_seal"].is_null(), "<core withheld>");
            eprintln!(
                "MATRIX_DONE_REFUSAL exit={:?} stdout_bytes={} stderr_bytes={}",
                output.status.code(),
                output.stdout.len(),
                output.stderr.len()
            );
            return;
        }
        Change::TrackedChild
        | Change::UntrackedChild
        | Change::HiddenUntracked
        | Change::DirtyRoot
        | Change::NewRootCommit => {
            matrix.refuse_done(check, true);
            return;
        }
    };
    let output = matrix.done_output(check);
    eprintln!(
        "MATRIX_DONE_REFUSAL exit={:?} stdout_bytes={} stderr_bytes={}",
        output.status.code(),
        output.stdout.len(),
        output.stderr.len()
    );
    assert!(
        !output.status.success(),
        "an unsupported child cannot complete"
    );
    let body: Value = serde_json::from_slice(&output.stdout)
        .or_else(|_| serde_json::from_slice(&output.stderr))
        .expect("ordinary done returns its typed refusal");
    assert_eq!(
        body["error"]["code"], "work_completion_refused",
        "<body withheld>"
    );
    assert_eq!(
        body["error"]["details"]["work_ref"], matrix.work,
        "<body withheld>"
    );
    let reason = body["error"]["details"]["reason"]
        .as_str()
        .expect("the completion refusal names its actual cause");
    let suffix = format!(
        " is unavailable: submodule '{}': {expected}; restore access and retry",
        consumer.children[0].0
    );
    let refused_root = reason
        .strip_prefix("registered root ")
        .and_then(|value| value.strip_suffix(&suffix))
        .unwrap_or_else(|| {
            panic!("expected this row's '{expected}' guard refusal: <body withheld>")
        });
    assert_eq!(
        fs::canonicalize(refused_root).unwrap(),
        fs::canonicalize(&matrix.fixture.root).unwrap(),
        "the guard refusal must name this consumer's root"
    );
    let core = matrix
        .fixture
        .work(&matrix.holder, &["core", "inspect", &matrix.work, "--json"]);
    assert!(core["run"]["completion_seal"].is_null(), "<core withheld>");
}

fn changed_consumer(consumer: &Consumer, change: Change) {
    let matrix = Matrix::from_fixture(
        consumer_fixture(consumer),
        Citation::BoundVerification,
        false,
    );
    if matches!(change, Change::Nested) {
        mutate(&matrix, consumer, change);
        let commit = run_git_test_command_output(&matrix.fixture.root, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        let pins: Vec<(String, String)> = consumer
            .children
            .iter()
            .map(|(path, _)| {
                (
                    (*path).to_owned(),
                    run_git_test_command_output(
                        &matrix.fixture.root.join(path),
                        &["rev-parse", "HEAD"],
                    )
                    .trim()
                    .to_owned(),
                )
            })
            .collect();
        let pins: Vec<(&str, &str)> = pins.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
        assert!(matches!(source_revision(&matrix.fixture.root, true),
            Ok(SourceRevision::Taken { revision, .. }) if revision == format!("git-commit:{commit}")));
        matrix
            .begin("Check clean nested pinned input derived inside the disposable consumer clone.");
        let check = launch_consumer(&matrix, &commit, &pins);
        matrix.close();
        matrix.settle();
        let originals = immutable_bytes(&matrix);
        matrix.accept(&check, || {});
        assert_immutable_bytes(&matrix, &originals);
        eprintln!(
            "CONSUMER_ADMITTED {}",
            json!({"consumer":consumer.name,"change":format!("{change:?}"),"commit":commit})
        );
        return;
    }
    matrix.begin("Check and independently evaluate the real clean consumer input.");
    let check = launch_consumer(&matrix, consumer.commit, consumer.children);
    matrix.close();
    matrix.settle();
    let (evaluation_id, _) = matrix.evaluate(&check, || {});
    let originals = immutable_bytes(&matrix);
    let request_bytes = fs::read(check.directory.join("request.json")).unwrap();
    let result_bytes = fs::read(check.directory.join("results.json")).unwrap();
    mutate(&matrix, consumer, change);
    let current = source_revision(&matrix.fixture.root, true);
    if matches!(change, Change::NewRootCommit) {
        let SourceRevision::Taken { revision, .. } = current.unwrap() else {
            panic!("new commit is readable");
        };
        assert_ne!(revision, format!("git-commit:{}", consumer.commit));
    } else {
        assert!(
            !matches!(current, Ok(SourceRevision::Taken { .. })),
            "{change:?} must fail fresh admission"
        );
    }
    // Both decisions use the original valid declared revision. No new closing
    // report is supplied to teach either product about this actual change.
    matrix.refuse(&check, || {});
    refuse_consumer_done(&matrix, &check, consumer, change);
    assert_immutable_bytes(&matrix, &originals);
    assert_eq!(
        fs::read(check.directory.join("request.json")).unwrap(),
        request_bytes
    );
    assert_eq!(
        fs::read(check.directory.join("results.json")).unwrap(),
        result_bytes
    );
    eprintln!(
        "CONSUMER_NEGATIVE {}",
        json!({"consumer":consumer.name,
        "change":format!("{change:?}"),"run":check.run_id,"priorEvaluation":evaluation_id,
        "originalObjects":originals.len(),"originalRunBytesPreserved":true})
    );
}

macro_rules! consumer_row {
    ($name:ident, $consumer:expr) => {
        #[test]
        #[ignore = "requires reviewed Engram binary pins and real consumer object-donor manifest"]
        fn $name() {
            clean_consumer(&$consumer);
        }
    };
    ($name:ident, $consumer:expr, $change:ident) => {
        #[test]
        #[ignore = "requires reviewed Engram binary pins and real consumer object-donor manifest"]
        fn $name() {
            changed_consumer(&$consumer, Change::$change);
        }
    };
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn repository_adapter_smoke_reaches_real_evaluation_and_seal() {
    let fixture = completion_fixture_for_repository(|root| {
        run_git_test_command(root, &["init", "--quiet", "--template="]);
        run_git_test_command(root, &["config", "user.name", "Fixture"]);
        run_git_test_command(root, &["config", "user.email", "test@example.invalid"]);
        fs::write(root.join("README.md"), "Actual adapter smoke input\n").unwrap();
        fs::create_dir_all(root.join(".git/info")).unwrap();
        fs::write(root.join(".git/info/exclude"), "/.engram-project\n").unwrap();
        fs::write(
            root.join(".engram-project"),
            format!("adapter-smoke-{}\n", Uuid::new_v4()),
        )
        .unwrap();
        run_git_test_command(root, &["add", "--", "README.md"]);
        run_git_test_command(root, &["commit", "--quiet", "-m", "adapter smoke input"]);
    });
    let commit = run_git_test_command_output(&fixture.root, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let matrix = Matrix::from_fixture(fixture, Citation::BoundVerification, false);
    matrix.begin("Exercise the actual library root adapter before consumer pins are available.");
    let check = launch_consumer(&matrix, &commit, &[]);
    matrix.close();
    matrix.settle();
    let originals = immutable_bytes(&matrix);
    matrix.accept(&check, || {});
    assert_immutable_bytes(&matrix, &originals);
}

consumer_row!(real_questly_clean_check_evaluate_done, QUESTLY);
consumer_row!(real_phoenix_clean_check_evaluate_done, PHOENIX);
consumer_row!(real_questly_tracked_child_refuses, QUESTLY, TrackedChild);
consumer_row!(
    real_questly_untracked_child_refuses,
    QUESTLY,
    UntrackedChild
);
consumer_row!(
    real_questly_hidden_untracked_refuses,
    QUESTLY,
    HiddenUntracked
);
consumer_row!(real_questly_off_pin_refuses, QUESTLY, OffPin);
consumer_row!(real_questly_uninitialized_refuses, QUESTLY, Uninitialized);
consumer_row!(real_questly_clean_filter_refuses, QUESTLY, CleanFilter);
consumer_row!(real_questly_smudge_filter_refuses, QUESTLY, SmudgeFilter);
consumer_row!(real_questly_process_filter_refuses, QUESTLY, ProcessFilter);
consumer_row!(real_questly_nested_at_pin_admitted, QUESTLY, Nested);
consumer_row!(real_questly_dirty_root_refuses, QUESTLY, DirtyRoot);
consumer_row!(real_questly_new_root_commit_refuses, QUESTLY, NewRootCommit);
consumer_row!(
    real_questly_missing_mapping_refuses,
    QUESTLY,
    MissingMapping
);
consumer_row!(real_phoenix_tracked_child_refuses, PHOENIX, TrackedChild);
consumer_row!(
    real_phoenix_untracked_child_refuses,
    PHOENIX,
    UntrackedChild
);
consumer_row!(
    real_phoenix_hidden_untracked_refuses,
    PHOENIX,
    HiddenUntracked
);
consumer_row!(real_phoenix_off_pin_refuses, PHOENIX, OffPin);
consumer_row!(real_phoenix_uninitialized_refuses, PHOENIX, Uninitialized);
consumer_row!(real_phoenix_clean_filter_refuses, PHOENIX, CleanFilter);
consumer_row!(real_phoenix_smudge_filter_refuses, PHOENIX, SmudgeFilter);
consumer_row!(real_phoenix_process_filter_refuses, PHOENIX, ProcessFilter);
consumer_row!(real_phoenix_nested_at_pin_admitted, PHOENIX, Nested);
consumer_row!(real_phoenix_dirty_root_refuses, PHOENIX, DirtyRoot);
consumer_row!(real_phoenix_new_root_commit_refuses, PHOENIX, NewRootCommit);
consumer_row!(
    real_phoenix_missing_mapping_refuses,
    PHOENIX,
    MissingMapping
);
