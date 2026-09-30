//! Serializes the Windows inheritable-handle window for every host launch.
#[cfg(not(windows))]
pub(crate) use std::process::Command;

#[cfg(windows)]
use std::sync::{Mutex, MutexGuard};
#[cfg(windows)]
static INHERITANCE_LOCK: Mutex<()> = Mutex::new(());
#[cfg(windows)]
pub(crate) fn inheritance_lock() -> MutexGuard<'static, ()> {
    let guard = INHERITANCE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    guard
}
#[cfg(all(windows, test))]
pub(crate) fn inheritance_is_locked() -> bool {
    INHERITANCE_LOCK.try_lock().is_err()
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct Command {
    inner: std::process::Command,
    explicit_stdio: [bool; 3],
}

#[cfg(windows)]
impl Command {
    pub(crate) fn new(program: impl AsRef<std::ffi::OsStr>) -> Self {
        Self {
            inner: std::process::Command::new(program),
            explicit_stdio: [false; 3],
        }
    }
    pub(crate) fn arg(&mut self, arg: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.inner.arg(arg);
        self
    }
    pub(crate) fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        self.inner.args(args);
        self
    }
    pub(crate) fn env(
        &mut self,
        key: impl AsRef<std::ffi::OsStr>,
        value: impl AsRef<std::ffi::OsStr>,
    ) -> &mut Self {
        self.inner.env(key, value);
        self
    }
    pub(crate) fn env_remove(&mut self, key: impl AsRef<std::ffi::OsStr>) -> &mut Self {
        self.inner.env_remove(key);
        self
    }
    pub(crate) fn current_dir(&mut self, dir: impl AsRef<std::path::Path>) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }
    pub(crate) fn stdin(&mut self, io: std::process::Stdio) -> &mut Self {
        self.explicit_stdio[0] = true;
        self.inner.stdin(io);
        self
    }
    pub(crate) fn stdout(&mut self, io: std::process::Stdio) -> &mut Self {
        self.explicit_stdio[1] = true;
        self.inner.stdout(io);
        self
    }
    pub(crate) fn stderr(&mut self, io: std::process::Stdio) -> &mut Self {
        self.explicit_stdio[2] = true;
        self.inner.stderr(io);
        self
    }
    pub(crate) fn creation_flags(&mut self, flags: u32) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.inner.creation_flags(flags);
        self
    }
    fn defaults(&mut self, capture: bool) {
        use std::process::Stdio;
        if !self.explicit_stdio[0] {
            self.inner.stdin(if capture {
                Stdio::null()
            } else {
                Stdio::inherit()
            });
        }
        if !self.explicit_stdio[1] {
            self.inner.stdout(if capture {
                Stdio::piped()
            } else {
                Stdio::inherit()
            });
        }
        if !self.explicit_stdio[2] {
            self.inner.stderr(if capture {
                Stdio::piped()
            } else {
                Stdio::inherit()
            });
        }
    }
    pub(crate) fn spawn(&mut self) -> std::io::Result<std::process::Child> {
        self.defaults(false);
        let _guard = inheritance_lock();
        self.inner.spawn()
    }
    pub(crate) fn output(&mut self) -> std::io::Result<std::process::Output> {
        self.defaults(true);
        let child = {
            let _guard = inheritance_lock();
            self.inner.spawn()?
        };
        // Waiting under the launch lock can deadlock a child calling the host.
        child.wait_with_output()
    }
    #[cfg(test)]
    pub(crate) fn status(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.spawn()?.wait()
    }
}

// Read-only access preserves the standard inspection APIs without exposing a
// mutable Command through which a caller could bypass guarded creation.
#[cfg(windows)]
impl std::ops::Deref for Command {
    type Target = std::process::Command;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(any(not(windows), test))]
pub(crate) fn spawn_shared(command: &mut Command) -> std::io::Result<shared_child::SharedChild> {
    #[cfg(windows)]
    {
        command.defaults(false);
        let _guard = inheritance_lock();
        shared_child::SharedChild::spawn(&mut command.inner)
    }
    #[cfg(not(windows))]
    shared_child::SharedChild::spawn(command)
}

#[cfg(all(windows, test))]
pub(crate) fn reference_output(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    // These two differential fixtures use the standard capture defaults.
    // Call std directly for quoting/environment/resolution, never hold the
    // inheritance lock while collecting even a supposedly short output.
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let child = {
        let _guard = inheritance_lock();
        command.spawn()?
    };
    child.wait_with_output()
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_windows_process_creation_cannot_bypass_the_shared_wrapper() {
        fn inspect(directory: &std::path::Path, violations: &mut Vec<String>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    inspect(&path, violations);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                if path
                    == std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/host_command.rs")
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                let mut references = 0;
                for (line, text) in source.lines().enumerate() {
                    let text = text.trim();
                    if text.starts_with("//") {
                        continue;
                    }
                    // Two parity references and one bridge regression run
                    // through reference_output; no production bypass is allowed.
                    let reference = path.ends_with("tests/windows_launch.rs")
                        && matches!(
                            text,
                            "let mut command = std::process::Command::new(\"node.exe\");"
                                | "let mut command = std::process::Command::new(&program);"
                        );
                    if reference {
                        references += 1;
                    }
                    let import_bypass = text.contains("use std::process")
                        && !matches!(
                            text,
                            "use std::process::{Child, Stdio};" | "use std::process::ExitStatus;"
                        );
                    if !reference
                        && (text.contains("std::process::Command")
                            || import_bypass
                            || text.contains("SharedChild::spawn"))
                    {
                        violations.push(format!("{}:{}: {}", path.display(), line + 1, text));
                    }
                }
                if path.ends_with("tests/windows_launch.rs") && references != 3 {
                    violations.push(format!(
                        "{}: expected exactly3 allowlisted standard references, found{references}",
                        path.display()
                    ));
                }
            }
        }
        let mut violations = Vec::new();
        inspect(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut violations,
        );
        assert!(
            violations.is_empty(),
            "raw host creation bypasses:\n{}",
            violations.join("\n")
        );
    }
}
