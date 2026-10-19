// Owns the isolated native satellite test process and its phase protocol.
// Does not launch SSH or touch a live store. New fixture for the two-process
// central mailbox proof; loopback sockets stand in for L1's enrolled forwards.
use super::*;

pub(super) const CHILD_ENV: &str = "TERMAL_TEST_MAILBOX_SATELLITE";

#[derive(Deserialize, Serialize)]
pub(super) struct NativeSession {
    pub host: FederationHostIdentity,
    pub session: String,
    pub creation: String,
    pub control_port: u16,
}

#[derive(Deserialize, Serialize)]
pub(super) struct Configuration {
    pub central: FederationHostIdentity,
    pub record: FederationPeerRecord,
    pub port: u16,
}

pub(super) struct Satellite {
    child: Arc<SharedChild>,
    input: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
    exit: mpsc::Receiver<std::io::Result<std::process::ExitStatus>>,
    waiter: Option<std::thread::JoinHandle<()>>,
}

impl Satellite {
    pub fn spawn() -> (Self, NativeSession) {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "tests::federation_mailbox_child::satellite_child",
                "--nocapture",
            ])
            .env(CHILD_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = Arc::new(host_command::spawn_shared(&mut command).unwrap());
        let input = child.take_stdin().unwrap();
        let output = child.take_stdout().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let (tx, exit) = mpsc::channel();
        let waiting = child.clone();
        let waiter = std::thread::spawn(move || {
            let _ = tx.send(waiting.wait());
        });
        let process = Self {
            child,
            input: Some(input),
            lines,
            reader: Some(reader),
            exit,
            waiter: Some(waiter),
        };
        let native = serde_json::from_str(&process.receive("M2_NATIVE ")).unwrap();
        (process, native)
    }

    fn receive(&self, prefix: &str) -> String {
        let began = Instant::now();
        loop {
            let timeout = phase_sync::DEADLOCK_GUARD
                .checked_sub(began.elapsed())
                .expect("satellite phase deadline");
            let line = self
                .lines
                .recv_timeout(timeout)
                .expect("satellite must report its phase");
            if let Some((_, value)) = line.split_once(prefix) {
                return value.to_owned();
            }
        }
    }

    pub fn configure(&mut self, configuration: &Configuration) -> u16 {
        writeln!(
            self.input.as_mut().unwrap(),
            "{}",
            serde_json::to_string(configuration).unwrap()
        )
        .unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        self.receive("M2_STARTED ").parse().unwrap()
    }

    pub fn command(&mut self, command: Value) -> Value {
        writeln!(self.input.as_mut().unwrap(), "{command}").unwrap();
        self.input.as_mut().unwrap().flush().unwrap();
        serde_json::from_str(&self.receive("M2_RESULT ")).unwrap()
    }

    pub fn finish(mut self) {
        assert_eq!(
            self.command(json!({"command":"stop"})),
            json!({"stopped":true})
        );
        self.input.take();
        assert!(
            self.exit
                .recv_timeout(phase_sync::DEADLOCK_GUARD)
                .unwrap()
                .unwrap()
                .success()
        );
        self.join();
    }

    fn join(&mut self) {
        if let Some(waiter) = self.waiter.take() {
            waiter.join().unwrap();
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

impl Drop for Satellite {
    fn drop(&mut self) {
        self.input.take();
        if self.waiter.is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.join();
        }
    }
}
