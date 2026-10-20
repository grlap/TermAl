// Owns disposable launcher-shaped history, including real failure diagnostics.
// Does not prefill the parse cache, registration map, or retention rows.

struct HistorySeed {
    directories: Vec<PathBuf>,
    ended: Vec<String>,
    digests: HashMap<String, String>,
    failures: HashMap<String, String>,
    bytes: usize,
}

impl HistorySeed {
    fn new(root: &FsPath, owner: &str, count: usize) -> Self {
        assert_eq!(count % 16, 0);
        let started = std::time::Instant::now();
        let common = root.join(".git");
        let mut seed = Self {
            directories: Vec::new(), ended: Vec::new(), digests: HashMap::new(),
            failures: HashMap::new(), bytes: 0,
        };
        for ordinal in 0..8 {
            let admin = common.join("worktrees").join(format!("history-{ordinal}"));
            fs::create_dir_all(&admin).unwrap();
            fs::write(admin.join("commondir"), "../..").unwrap();
        }
        for ordinal in 0..count {
            let admin = if ordinal < count / 2 {
                common.clone()
            } else {
                common.join("worktrees")
                    .join(format!("history-{}", (ordinal - count / 2) / (count / 16)))
            };
            let id = format!("test-history-{ordinal:04}");
            let directory = admin.join("review-runs").join(&id);
            fs::create_dir_all(&directory).unwrap();
            // Most history predates cards. Four recent specimens exercise actual
            // card creation/publication without manufacturing thousands of cards.
            let timestamp = if ordinal < 4 {
                (chrono::Utc::now() + chrono::TimeDelta::seconds(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            } else { "2026-01-01T00:00:00.000Z".to_owned() };
            let request = json!({
                "runId":id, "root":test_run_display_path(root), "owner":owner,
                "full":false, "stages":[{"name":"focused","command":"cargo",
                    "args":["test","history-fixture"]}], "started":timestamp,
                "creatorPid":std::process::id()
            }).to_string();
            fs::write(directory.join("request.json"), &request).unwrap();
            seed.bytes += request.len();
            seed.directories.push(directory);
            seed.ended.push((chrono::DateTime::parse_from_rfc3339(&timestamp).unwrap()
                + chrono::TimeDelta::seconds(10)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
        seed.write_results(0);
        eprintln!("history seed: directories={} linkedAdmins=8 bytes={} wall={:?}",
            count, seed.bytes, started.elapsed());
        seed
    }

    fn write_results(&mut self, generation: usize) {
        self.digests.clear();
        self.failures.clear();
        for (ordinal, directory) in self.directories.iter().enumerate() {
            let id = directory.file_name().unwrap().to_str().unwrap().to_owned();
            let failed = ordinal % 2 == 0;
            let marker = format!("history-failure-{ordinal:04}-generation-{generation}");
            // Generation changes lengths too, so a coarse mtime cannot hide a
            // changed result. The log and embedded excerpt describe the same run.
            let log = format!("{marker}\n{}\n", "diagnostic context\n".repeat(240 + generation));
            fs::write(directory.join("focused.log"), &log).unwrap();
            let results = json!({
                "runId":id, "state":if failed {"failed"} else {"passed"},
                "exitCode":if failed {1} else {0}, "ended":self.ended[ordinal],
                "stages":[{"name":"focused", "state":if failed {"failed"} else {"passed"},
                    "code":if failed {101} else {0}, "log":"focused.log",
                    "diagnostics":{"text":log,"truncated":false}}]
            }).to_string();
            fs::write(directory.join("results.json"), &results).unwrap();
            self.bytes += results.len() + log.len();
            self.digests.insert(id.clone(), file_content_hash(results.as_bytes()));
            if failed { self.failures.insert(id, marker); }
        }
    }
}
