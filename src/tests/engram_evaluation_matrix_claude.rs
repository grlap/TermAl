// Claude's full stdout-frame boundary, including host prompt ownership and
// the outstanding-work registry. Calling only the Bash parser misses this
// boundary: the foreground launch itself is outstanding during precheck.
use super::*;

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn claude_frame_check_a_edit_commit_check_b_same_turn_accepts_last_input() {
    let matrix = Matrix::new(Citation::BoundVerification, true);
    let mut frames =
        ClaudeFrames::begin(&matrix, "Run A, edit and commit, then run B in this turn.");
    let first = frames.check(&matrix);
    let original_results = fs::read(first.directory.join("results.json")).unwrap();
    frames.edit_commit(&matrix, "new intended input after A, before B\n");
    let last = frames.check(&matrix);
    assert_ne!(first.run_id, last.run_id);
    assert_ne!(
        first.results["input"]["commit"],
        last.results["input"]["commit"]
    );
    frames.end(&matrix);
    matrix.accept(&last, || {});
    assert!(fs::read(first.directory.join("results.json")).unwrap() == original_results,
        "the complete earlier result is unchanged");
}

struct ClaudeFrames {
    context: ClaudeRuntimeContext,
    frames: ClaudeReaderFrames,
    recorder: SessionRecorder,
    generation: u64,
    _input: mpsc::Receiver<ClaudeRuntimeCommand>,
}

impl ClaudeFrames {
    fn begin(matrix: &Matrix, text: &str) -> Self {
        let dispatch = dispatch_live_root(&matrix.fixture.live.state, &matrix.holder, text, None);
        deliver_turn_dispatch(&matrix.fixture.live.state, dispatch).unwrap();
        let Provider::Claude(input) = &matrix.provider else {
            panic!("real Claude fixture required");
        };
        let prompt = loop {
            if let ClaudeRuntimeCommand::Prompt(prompt) = input
                .recv_timeout(DEADLOCK_GUARD)
                .expect("production dispatch hands the actual host prompt to Claude")
            {
                break prompt;
            }
        };
        let (token, generation, cwd) = matrix.fixture.record(&matrix.holder, |record| {
            (
                record.runtime.runtime_token().unwrap(),
                record.active_turn_generation,
                record.session.workdir.clone(),
            )
        });
        assert_eq!(prompt.turn_generation, generation);
        let ownership = new_claude_turn_ownership();
        lock_claude_turn_ownership(&ownership).reserve(claude_host_prompt_owner(&prompt));
        let context = ClaudeRuntimeContext::new(
            matrix.fixture.live.state.clone(),
            matrix.holder.clone(),
            token,
            ownership,
            Arc::new(Mutex::new(None)),
            cwd,
        );
        let (input_tx, input_rx) = mpsc::channel();
        let frames = ClaudeReaderFrames::new(&context, input_tx, None);
        let mut driver = Self {
            context,
            frames,
            recorder: SessionRecorder::new(
                matrix.fixture.live.state.clone(),
                matrix.holder.clone(),
            ),
            generation,
            _input: input_rx,
        };
        driver.feed(json!({"type":"system", "subtype":"init", "session_id":"matrix-claude"}));
        driver.feed(json!({"type":"user", "message":{"role":"user",
            "content":claude_prompt_content(&prompt)}, "isReplay":true}));
        driver.feed(json!({"type":"assistant", "message":{"content":[{
            "type":"text", "text":"Working on the requested source and its real check."}]}}));
        assert!(matrix.fixture.record(&matrix.holder, |record| {
            record.engram.unadmitted_active_turn.is_some()
                && record.adopted_claude_turn_generation.is_none()
        }));
        driver
    }

    fn feed(&mut self, frame: Value) -> ClaudeFrameOutcome {
        let applied =
            apply_claude_frame(&self.context, &mut self.frames, &mut self.recorder, &frame);
        assert_eq!(
            applied.next,
            ClaudeFrameApplied::Continue,
            "actual frame failed: <frame withheld>"
        );
        applied
    }

    fn tool(&mut self, key: &str, name: &str, input: Value) {
        let applied = self.feed(json!({"type":"assistant", "message":{"content":[{
            "type":"tool_use", "id":key, "name":name, "input":input}]}}));
        assert_eq!(
            applied.plan.origin,
            ClaudeWorkOrigin::Attempt {
                turn_generation: self.generation
            },
            "the frame must carry the real host prompt's observation provenance"
        );
    }

    fn result(&mut self, key: &str, stdout: &str, stderr: &str, code: i32) {
        self.feed(json!({"type":"user", "message":{"role":"user", "content":[{
            "type":"tool_result", "tool_use_id":key, "is_error":code != 0,
            "content":format!("{stdout}\n{stderr}")}]},
            "tool_use_result":{"stdout":stdout,"stderr":stderr,"interrupted":false}}));
    }

    fn edit_commit(&mut self, matrix: &Matrix, content: &str) {
        let path = matrix.fixture.root.join("README.md");
        self.tool(
            "intended-write",
            "Write",
            json!({"file_path":path,"content":content}),
        );
        fs::write(&path, content).unwrap();
        self.feed(json!({"type":"user", "message":{"role":"user", "content":[{
            "type":"tool_result", "tool_use_id":"intended-write", "content":"File updated"}]},
            "tool_use_result":{"type":"update","filePath":path}}));
        // The provider is simulated; the two commands the Bash frame names
        // really execute. Their native stdout/stderr/exit are the result.
        self.tool(
            "intended-commit",
            "Bash",
            json!({"command":
            "git add -- README.md && git commit --quiet -m 'fixture intended source'"}),
        );
        let add = Command::new("git")
            .current_dir(&matrix.fixture.root)
            .args(["add", "--", "README.md"])
            .output()
            .unwrap();
        assert!(
            add.status.success(),
            "{}",
            String::from_utf8_lossy(&add.stderr)
        );
        let commit = Command::new("git")
            .current_dir(&matrix.fixture.root)
            .args(["commit", "--quiet", "-m", "fixture intended source"])
            .output()
            .unwrap();
        assert!(
            commit.status.success(),
            "{}",
            String::from_utf8_lossy(&commit.stderr)
        );
        self.result(
            "intended-commit",
            &format!(
                "{}{}",
                String::from_utf8_lossy(&add.stdout),
                String::from_utf8_lossy(&commit.stdout)
            ),
            &format!(
                "{}{}",
                String::from_utf8_lossy(&add.stderr),
                String::from_utf8_lossy(&commit.stderr)
            ),
            commit.status.code().unwrap(),
        );
        assert!(
            run_git_test_command_output(&matrix.fixture.root, &["status", "--porcelain"])
                .is_empty()
        );
    }

    fn check(&mut self, matrix: &Matrix) -> ActualCheck {
        let root = normalize_user_facing_path(&matrix.fixture.root)
            .to_string_lossy()
            .replace('\\', "/");
        let key = format!("foreground-registered-launcher-{}", Uuid::new_v4());
        self.tool(
            &key,
            "Bash",
            json!({"command":format!("pushd \"{root}\" && {LAUNCH}")}),
        );
        let entry = matrix.fixture.record(&matrix.holder, |record| {
            record
                .claude_outstanding
                .entries
                .iter()
                .find(|entry| entry.key == key)
                .cloned()
        });
        let entry =
            entry.expect("the actual frame registers its foreground Bash while precheck runs");
        assert_eq!(
            entry.origin,
            ClaudeWorkOrigin::Attempt {
                turn_generation: self.generation
            }
        );
        assert_eq!(entry.token, self.context.token);
        assert!(!entry.background && !entry.nested);
        matrix.fixture.wait_for_check_capture(&matrix.holder);
        let output = isolated_launcher(&matrix.fixture.root, &matrix.holder, &matrix.service)
            .output()
            .expect("actual foreground launcher process starts");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit = output.status.code().unwrap();
        self.result(&key, &stdout, &stderr, exit);
        assert!(
            matrix.fixture.record(&matrix.holder, |record| record
                .claude_outstanding
                .entries
                .iter()
                .all(|entry| entry.key != key)),
            "only the correlated real result retires the foreground launch"
        );
        matrix.fixture.wait_for_check_capture(&matrix.holder);
        let mut check = matrix.actual_check_output(output);
        assert_eq!(check.exit, 0, "{}", check.output);
        matrix.settle_check(&mut check);
        check
    }

    fn end(&mut self, matrix: &Matrix) {
        self.feed(json!({"type":"result","subtype":"success","is_error":false,"result":"done"}));
        assert_eq!(
            matrix
                .fixture
                .record(&matrix.holder, |record| record.session.status),
            SessionStatus::Idle
        );
        matrix.settle();
    }
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn ordinary_claude_frame_edit_commit_check_accepts_and_seals() {
    let matrix = Matrix::new(Citation::BoundVerification, true);
    let mut frames = ClaudeFrames::begin(
        &matrix,
        "Edit, commit and check in this same ordinary turn.",
    );
    frames.edit_commit(&matrix, "intended content written by the Claude turn\n");
    let check = frames.check(&matrix);
    frames.end(&matrix);
    matrix.accept(&check, || {});
}

#[test]
#[ignore = "requires reviewed TERMAL_TEST_LIVE_ENGRAM_BINARY and TERMAL_TEST_LIVE_ENGRAM_SHA256"]
fn claude_foreground_check_after_real_prior_work_preserves_precheck_order() {
    let matrix = Matrix::new(Citation::BoundVerification, true);
    // Prior ordinary work is a real registered check, not an empty turn to
    // create a baseline. The second turn must account for its own new edit.
    let mut prior = ClaudeFrames::begin(&matrix, "Check the existing input as ordinary work.");
    let original = prior.check(&matrix);
    prior.end(&matrix);
    assert!(!original.records.is_empty());
    let mut current = ClaudeFrames::begin(&matrix, "Now edit, commit and check the new input.");
    current.edit_commit(&matrix, "new content after earlier real checked work\n");
    let check = current.check(&matrix);
    current.end(&matrix);
    matrix.accept(&check, || {});
}
