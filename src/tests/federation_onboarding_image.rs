// Owns the Windows running-image identity regression across rename-aside install.
// Does not replace the live host binary or install anything outside the test root.
// New subprocess fixture follows federation_onboarding_child.rs ownership.
use super::*;

struct ImageChild {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    waiter: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
}

impl ImageChild {
    fn start(binary: &FsPath, directory: &FsPath, mode: &str) -> Self {
        let test = format!(
            "{}::federation_onboarding_image_child",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = Command::new(binary);
        command
            .args(["--exact", &test, "--nocapture"])
            .env("TERMAL_TEST_O1_IMAGE_CHILD", mode)
            .current_dir(directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let output = child.take_stdout().unwrap();
        let (send, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                if send.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (send, exit) = mpsc::channel();
        let waiting = child.clone();
        let waiter = std::thread::spawn(move || {
            let _ = send.send(waiting.wait());
        });
        Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            waiter: Some(waiter),
            exit,
        }
    }

    fn receive(&self, prefix: &str) -> String {
        let deadline = Instant::now() + phase_sync::DEADLOCK_GUARD;
        loop {
            let line = self
                .lines
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            if let Some(value) = line.strip_prefix(prefix) {
                return value.to_owned();
            }
        }
    }

    fn measure(mut self) -> (String, FederationImageObservation) {
        writeln!(self.input.as_mut().unwrap(), "measure").unwrap();
        self.input.take();
        let hash = self.receive("IMAGE_HASH ");
        let observation = serde_json::from_str(&self.receive("IMAGE_OBSERVATION ")).unwrap();
        assert!(
            self.exit
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap()
                .unwrap()
                .success()
        );
        self.waiter.take().unwrap().join().unwrap();
        self.reader.take().unwrap().join().unwrap();
        (hash, observation)
    }
}

impl Drop for ImageChild {
    fn drop(&mut self) {
        self.input.take();
        if self.waiter.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn federation_onboarding_running_image_identity_survives_rename_replacement() {
    let _scope = TestTempRootDirectoryScope::repository_local();
    let directory = TestTempRoot::create("onboarding-image");
    let source = std::env::current_exe().unwrap();
    let expected = format!("{:x}", Sha256::digest(fs::read(&source).unwrap()));
    let mut observations = Vec::new();
    for mode in ["beforeFirstProfile", "afterFirstProfile"] {
        let folder = directory.path().join(mode);
        fs::create_dir(&folder).unwrap();
        let binary = folder.join("termal-image.exe");
        fs::copy(&source, &binary).unwrap();
        let original = fs::File::open(&binary).unwrap();
        let metadata = original.metadata().unwrap();
        let original_id = federation_image_file_identity(&original, &metadata).unwrap();
        drop(original);
        let child = ImageChild::start(&binary, &folder, mode);
        assert_eq!(child.receive("IMAGE_READY "), mode);
        fs::rename(&binary, folder.join("termal-image-running.exe")).unwrap();
        fs::copy(&source, &binary).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&binary)
            .unwrap()
            .write_all(b"TERMAL_TEST_REPLACEMENT_TRAILER")
            .unwrap();
        let installed = format!("{:x}", Sha256::digest(fs::read(&binary).unwrap()));
        assert_ne!(
            installed, expected,
            "replacement must have distinct valid image bytes"
        );
        let (hash, observation) = child.measure();
        observations.push((mode, hash, observation, original_id, metadata.len()));
    }
    for (mode, observed, observation, original_id, original_size) in observations {
        assert_eq!(
            observed, expected,
            "{mode}: onboarding must identify the original running image after replacement"
        );
        let FederationImageObservation::Known(image) = observation else {
            panic!("running image must be known");
        };
        assert_eq!(image.sha256, expected);
        assert_eq!(image.file_identity, original_id);
        assert_eq!(image.size, original_size);
        assert!(image.path_at_start.ends_with("termal-image.exe"));
    }
}

#[test]
fn federation_onboarding_image_child() {
    let Ok(mode) = std::env::var("TERMAL_TEST_O1_IMAGE_CHILD") else {
        return;
    };
    let directory = std::env::current_dir().unwrap();
    let io = FederationOnboardingIo::local(directory);
    if mode == "afterFirstProfile" {
        (io.profile)().unwrap();
    }
    println!("IMAGE_READY {mode}");
    std::io::stdout().flush().unwrap();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).unwrap() == 0 {
        return;
    }
    assert_eq!(input.trim(), "measure");
    let profile = (io.profile)().unwrap();
    println!("IMAGE_HASH {}", profile.binary_sha256);
    println!(
        "IMAGE_OBSERVATION {}",
        serde_json::to_string(&(io.image)()).unwrap()
    );
    std::io::stdout().flush().unwrap();
}
