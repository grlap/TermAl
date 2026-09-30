//! Native Windows launch for finite reads and terminal commands. The job lease
//! belongs to the command supervisor, never to a detached process waiter.
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString, c_void};
use std::fs::File;
use std::io::{self, Read};
use std::mem::size_of;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Globalization::CompareStringOrdinal;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFullName;
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetWindowsDirectoryW};
#[cfg(test)]
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_QUERY;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE;

/// Deliberately supports ordinary argv and inherited environment with edits.
/// No conversion from Command: its getters lose raw_arg and env_clear intent.
#[derive(Debug)]
pub(crate) struct LaunchSpec {
    program: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, Option<OsString>)>,
    cwd: Option<PathBuf>,
    flags: u32,
}

impl LaunchSpec {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().into(),
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            flags: 0,
        }
    }
    pub(crate) fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().into());
        self
    }
    pub(crate) fn args(&mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> &mut Self {
        for arg in args {
            self.arg(arg);
        }
        self
    }
    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.edit_env(key.as_ref(), Some(value.as_ref().into()));
        self
    }
    pub(crate) fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        self.edit_env(key.as_ref(), None);
        self
    }
    fn edit_env(&mut self, key: &OsStr, value: Option<OsString>) {
        if let Some(entry) = self
            .env
            .iter_mut()
            .find(|(k, _)| compare_env(k, key) == Ordering::Equal)
        {
            entry.1 = value;
        } else {
            self.env.push((key.into(), value));
        }
        self.env.sort_by(|a, b| compare_env(&a.0, &b.0));
    }
    pub(crate) fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.cwd = Some(path.as_ref().into());
        self
    }
    pub(crate) fn creation_flags(&mut self, flags: u32) -> &mut Self {
        self.flags = flags;
        self
    }
    pub(crate) fn get_program(&self) -> &OsStr {
        &self.program
    }
    pub(crate) fn get_args(&self) -> impl Iterator<Item = &OsStr> {
        self.args.iter().map(OsString::as_os_str)
    }
    #[cfg(test)]
    pub(crate) fn get_envs(&self) -> impl Iterator<Item = (&OsStr, Option<&OsStr>)> {
        self.env.iter().map(|(k, v)| (k.as_os_str(), v.as_deref()))
    }
    #[cfg(test)]
    pub(crate) fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    // Existing command-construction tests execute this same specification.
    // Keep their assertions intact while exercising the native transport.
    #[cfg(test)]
    pub(crate) fn output(&mut self) -> io::Result<std::process::Output> {
        let launch = prepare(self)?;
        let process = launch.process();
        let mut stdout = process.take_stdout().unwrap();
        let mut stderr = process.take_stderr().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes).map(|_| bytes)
        });
        let err = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).map(|_| bytes)
        });
        launch.resume_after_attach(&process)?;
        let status = process.wait()?;
        launch.cleanup_after_shell_exit(&process, "test output")?;
        Ok(std::process::Output {
            status,
            stdout: out
                .join()
                .map_err(|_| io::Error::other("stdout reader panicked"))??,
            stderr: err
                .join()
                .map_err(|_| io::Error::other("stderr reader panicked"))??,
        })
    }
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(crate) enum ContainmentStatus {
    Contained,
    ContainedWithPackagedIdentityChanged { package_full_name: String },
    Unavailable { reason: String },
}

pub(crate) struct PipeReader(File);
impl Read for PipeReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(0),
            result => result,
        }
    }
}

pub(crate) struct WindowsChild {
    handle: OwnedHandle,
    id: u32,
    stdout: Mutex<Option<PipeReader>>,
    stderr: Mutex<Option<PipeReader>>,
    #[cfg(test)]
    deny_root_termination: std::sync::atomic::AtomicBool,
}
impl WindowsChild {
    pub(crate) fn id(&self) -> u32 {
        self.id
    }
    pub(crate) fn take_stdout(&self) -> Option<PipeReader> {
        self.stdout.lock().unwrap().take()
    }
    pub(crate) fn take_stderr(&self) -> Option<PipeReader> {
        self.stderr.lock().unwrap().take()
    }
    pub(crate) fn wait(&self) -> io::Result<ExitStatus> {
        self.wait_millis(INFINITE)?
            .ok_or_else(|| io::Error::other("infinite process wait timed out"))
    }
    pub(crate) fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.wait_millis(0)
    }
    pub(crate) fn wait_timeout(&self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        // Avoid mapping a finite duration to INFINITE; round up partial millis.
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let millis =
                remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0);
            let result = self.wait_millis(millis.min(u128::from(INFINITE - 1)) as u32)?;
            if result.is_some() || std::time::Instant::now() >= deadline {
                return Ok(result);
            }
        }
    }
    fn wait_millis(&self, millis: u32) -> io::Result<Option<ExitStatus>> {
        // The retained process handle is identity. No mutex is held while waiting.
        unsafe {
            match WaitForSingleObject(self.handle.as_raw_handle(), millis) {
                WAIT_OBJECT_0 => {
                    let mut code = 0;
                    if GetExitCodeProcess(self.handle.as_raw_handle(), &mut code) == 0 {
                        return Err(io::Error::last_os_error());
                    }
                    // 259 is a valid final code; only handle signaling establishes exit.
                    Ok(Some(ExitStatus::from_raw(code)))
                }
                WAIT_TIMEOUT => Ok(None),
                _ => Err(io::Error::last_os_error()),
            }
        }
    }
    pub(crate) fn kill(&self) -> io::Result<()> {
        #[cfg(test)]
        if self
            .deny_root_termination
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
        }
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 {
            let error = io::Error::last_os_error();
            let observed = self.try_wait();
            eprintln!(
                "windows launch> root TerminateProcess refused for pid={}: {error}; before=None after={observed:?}",
                self.id()
            );
            // Match std's Windows kill path: ACCESS_DENIED can mean that
            // termination is pending, before the handle becomes signaled.
            // A successful handle query suffices; other errors remain errors.
            if error.raw_os_error() != Some(ERROR_ACCESS_DENIED as i32) || observed.is_err() {
                return Err(error);
            }
        }
        Ok(())
    }
}

/// A prepared launch owns its primary thread until one resume. Dropping an
/// unresumed launch also terminates an uncontained process, not just its job.
pub(crate) struct PreparedLaunch {
    process: Arc<WindowsChild>,
    thread: Mutex<Option<OwnedHandle>>,
    job: Mutex<Option<OwnedHandle>>,
    status: ContainmentStatus,
    #[cfg(test)]
    refused_job_observer: Option<OwnedHandle>,
}
impl PreparedLaunch {
    pub(crate) fn process(&self) -> Arc<WindowsChild> {
        self.process.clone()
    }
    pub(crate) fn containment(&self) -> &ContainmentStatus {
        &self.status
    }
    pub(crate) fn resume_after_attach(&self, _process: &Arc<WindowsChild>) -> io::Result<()> {
        let mut thread = self.thread.lock().unwrap();
        let handle = thread
            .as_ref()
            .ok_or_else(|| io::Error::other("launch already resumed"))?;
        let previous = unsafe { ResumeThread(handle.as_raw_handle()) };
        if previous == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        if previous != 1 {
            return Err(io::Error::other(format!(
                "unexpected primary thread suspend count: {previous}"
            )));
        }
        thread.take();
        Ok(())
    }
    pub(crate) fn cleanup_after_shell_exit(
        &self,
        _process: &Arc<WindowsChild>,
        _label: &str,
    ) -> io::Result<()> {
        // This is the only job lease. KILL_ON_JOB_CLOSE ends remaining members.
        self.job.lock().unwrap().take();
        Ok(())
    }
    pub(crate) fn kill(&self, process: &Arc<WindowsChild>, _label: &str) -> io::Result<()> {
        if let Some(job) = self.job.lock().unwrap().take() {
            // Creation/assignment succeeded before this lease was retained.
            // Closing it already initiates termination of the root and every
            // member. A second TerminateProcess races that asynchronous exit
            // and may return ACCESS_DENIED while the handle is not yet signaled.
            drop(job);
            Ok(())
        } else {
            process.kill()
        }
    }
}
impl Drop for PreparedLaunch {
    fn drop(&mut self) {
        let had_job = self.job.get_mut().unwrap().take().is_some();
        if self.thread.get_mut().unwrap().is_some() {
            if !had_job {
                let _ = self.process.kill();
            }
            let _ = self.process.wait();
        }
    }
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<u16> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in launch value",
        ));
    }
    result.push(0);
    Ok(result)
}
// Resolution, ordinary argv quoting and environment ordering follow Rust's
// Windows Command semantics (library/std/src/sys/process/windows.rs and
// sys/args/windows.rs, Rust 1.90.0). Keep differential tests when changing them.
fn compare_env(a: &OsStr, b: &OsStr) -> Ordering {
    let a: Vec<_> = a.encode_wide().collect();
    let b: Vec<_> = b.encode_wide().collect();
    // Matches Command's Windows environment key ordering, including Unicode.
    let result =
        unsafe { CompareStringOrdinal(a.as_ptr(), a.len() as i32, b.as_ptr(), b.len() as i32, 1) };
    match result {
        1 => Ordering::Less,
        2 => Ordering::Equal,
        3 => Ordering::Greater,
        _ => a.cmp(&b),
    }
}
fn environment(spec: &LaunchSpec) -> io::Result<Option<Vec<u16>>> {
    if spec.env.is_empty() {
        return Ok(None);
    }
    let mut entries: Vec<_> = std::env::vars_os().collect();
    for (key, value) in &spec.env {
        let existing = entries
            .iter()
            .position(|(existing, _)| compare_env(existing, key) == Ordering::Equal);
        if let Some(index) = existing {
            if let Some(value) = value {
                entries[index].1 = value.clone();
            } else {
                entries.remove(index);
            }
        } else if let Some(value) = value {
            entries.push((key.clone(), value.clone()));
        }
    }
    entries.sort_by(|a, b| compare_env(&a.0, &b.0));
    let mut block = Vec::new();
    for (key, value) in entries {
        let key = wide(&key)?;
        let value = wide(&value)?;
        block.extend_from_slice(&key[..key.len() - 1]);
        block.push(b'=' as u16);
        block.extend(value);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(Some(block))
}
fn command_line(spec: &LaunchSpec) -> io::Result<Vec<u16>> {
    let program = wide(&spec.program)?;
    let mut line = vec![b'"' as u16];
    line.extend_from_slice(&program[..program.len() - 1]);
    line.push(b'"' as u16);
    for arg in &spec.args {
        let value = wide(arg)?;
        let value = &value[..value.len() - 1];
        let quoted = value.is_empty()
            || value
                .iter()
                .any(|c| *c == b' ' as u16 || *c == b'\t' as u16);
        line.push(b' ' as u16);
        if quoted {
            line.push(b'"' as u16);
        }
        let mut slashes = 0;
        for &unit in value {
            if unit == b'\\' as u16 {
                slashes += 1;
            } else {
                if unit == b'"' as u16 {
                    line.extend(std::iter::repeat_n(b'\\' as u16, slashes + 1));
                }
                slashes = 0;
            }
            line.push(unit);
        }
        if quoted {
            line.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            line.push(b'"' as u16);
        }
    }
    if line.len() >= 32767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows command line too long",
        ));
    }
    line.push(0);
    Ok(line)
}

fn user_path(path: &Path, cwd: bool) -> io::Result<Vec<u16>> {
    let original = wide(path.as_os_str())?;
    let prefix: Vec<_> = r"\\?\".encode_utf16().collect();
    if original.starts_with(&prefix) {
        let candidate = if original[4..].starts_with(&"UNC\\".encode_utf16().collect::<Vec<_>>()) {
            [vec![b'\\' as u16, b'\\' as u16], original[8..].to_vec()].concat()
        } else {
            original[4..].to_vec()
        };
        // Strip verbatim syntax only if ordinary resolution is byte-identical.
        if cwd || original.len() <= 260 {
            let mut full = vec![0; 32768];
            let len = unsafe {
                GetFullPathNameW(
                    candidate.as_ptr(),
                    full.len() as u32,
                    full.as_mut_ptr(),
                    null_mut(),
                )
            } as usize;
            if len > 0 && len < full.len() && full[..len] == candidate[..candidate.len() - 1] {
                return Ok(candidate);
            }
        }
    }
    Ok(original)
}
fn exists(path: &Path) -> bool {
    user_path(path, false)
        .is_ok_and(|wide| unsafe { GetFileAttributesW(wide.as_ptr()) != INVALID_FILE_ATTRIBUTES })
}
fn resolve_program(spec: &LaunchSpec) -> io::Result<Vec<u16>> {
    let application = resolve_program_path(spec)?;
    // Windows ordinary path resolution removes trailing dots and spaces.
    // Check the final file name before using ordinary executable argv quoting.
    let mut full = vec![0; 32768];
    let len = unsafe {
        GetFullPathNameW(
            application.as_ptr(),
            full.len() as u32,
            full.as_mut_ptr(),
            null_mut(),
        )
    } as usize;
    if len == 0 || len >= full.len() {
        return Err(io::Error::last_os_error());
    }
    let normalized = OsString::from_wide(&full[..len]);
    if Path::new(&normalized).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native launch does not support batch shims",
        ));
    }
    Ok(application)
}
fn resolve_program_path(spec: &LaunchSpec) -> io::Result<Vec<u16>> {
    let program = &spec.program;
    let text = program.to_string_lossy();
    if program.is_empty() || text.ends_with(['/', '\\']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program has no file name",
        ));
    }
    let path = Path::new(program);
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("bat") || ext.eq_ignore_ascii_case("cmd"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native launch does not support batch shims",
        ));
    }
    wide(program)?;
    if text.contains(['/', '\\', ':']) {
        if !text.to_ascii_lowercase().ends_with(".exe") {
            let mut with_exe = program.clone();
            with_exe.push(".exe");
            if exists(Path::new(&with_exe)) {
                return user_path(Path::new(&with_exe), false);
            }
        }
        return user_path(path, false);
    }
    let mut paths = Vec::new();
    if let Some((_, Some(child_path))) = spec
        .env
        .iter()
        .find(|(key, _)| compare_env(key, OsStr::new("PATH")) == Ordering::Equal)
    {
        paths.extend(std::env::split_paths(child_path).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            paths.push(parent.into());
        }
    }
    for get in [GetSystemDirectoryW, GetWindowsDirectoryW] {
        let mut buffer = vec![0; 32768];
        let len = unsafe { get(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if len > 0 && len < buffer.len() {
            paths.push(PathBuf::from(OsString::from_wide(&buffer[..len])));
        }
    }
    if let Some(parent_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&parent_path).filter(|p| !p.as_os_str().is_empty()));
    }
    for mut dir in paths {
        dir.push(program);
        if !text.contains('.') {
            dir.set_extension("exe");
        }
        if exists(&dir) {
            return user_path(&dir, false);
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "program not found"))
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}
fn pipe(security: &SECURITY_ATTRIBUTES) -> io::Result<(PipeReader, OwnedHandle)> {
    let mut read = null_mut();
    let mut write = null_mut();
    if unsafe { CreatePipe(&mut read, &mut write, security, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let read = owned(read)?;
    let write = owned(write)?;
    if unsafe { SetHandleInformation(read.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((PipeReader(File::from(read)), write))
}
fn create_job() -> io::Result<OwnedHandle> {
    let job = owned(unsafe { CreateJobObjectW(null(), null()) })?;
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}
struct Attributes {
    storage: Vec<usize>,
}
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        if unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { storage })
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn add<T>(&mut self, kind: usize, value: &[T]) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                kind,
                value.as_ptr().cast(),
                std::mem::size_of_val(value),
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.ptr());
        }
    }
}

fn package(handle: HANDLE) -> io::Result<Option<String>> {
    let mut len = 0;
    let result = unsafe { GetPackageFullName(handle, &mut len, null_mut()) };
    if result == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if result != ERROR_INSUFFICIENT_BUFFER {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let mut buffer = vec![0; len as usize];
    let result = unsafe { GetPackageFullName(handle, &mut len, buffer.as_mut_ptr()) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    Ok(Some(String::from_utf16_lossy(
        &buffer[..len.saturating_sub(1) as usize],
    )))
}

pub(crate) fn prepare(spec: &LaunchSpec) -> io::Result<PreparedLaunch> {
    prepare_inner(spec, false, false)
}
fn membership_failure(job: &mut Option<OwnedHandle>, process: HANDLE) -> Option<String> {
    let lease = job.as_ref()?;
    let mut member = 0;
    let queried = unsafe { IsProcessInJob(process, lease.as_raw_handle(), &mut member) };
    if queried != 0 && member != 0 {
        return None;
    }
    let reason = if queried == 0 {
        format!(
            "job membership query failed: {}",
            io::Error::last_os_error()
        )
    } else {
        "root is not a member of the launch job".to_owned()
    };
    // Without membership, closing this lease cannot replace root kill.
    job.take();
    Some(reason)
}
fn prepare_inner(
    spec: &LaunchSpec,
    force_unavailable: bool,
    force_atomic_refusal: bool,
) -> io::Result<PreparedLaunch> {
    prepare_with_hook(spec, force_unavailable, force_atomic_refusal, |_| {})
}
fn prepare_with_hook(
    spec: &LaunchSpec,
    force_unavailable: bool,
    force_atomic_refusal: bool,
    before_create: impl FnOnce(&[HANDLE]),
) -> io::Result<PreparedLaunch> {
    let mut before_create = Some(before_create);
    let application = resolve_program(spec)?;
    let line = command_line(spec)?;
    let environment = environment(spec)?;
    let cwd = spec
        .cwd
        .as_deref()
        .map(|cwd| user_path(cwd, true))
        .transpose()?;
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 0,
    };
    let stdin = owned(unsafe {
        CreateFileW(
            wide(OsStr::new("NUL"))?.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &security,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        )
    })?;
    let (stdout, stdout_write) = pipe(&security)?;
    let (stderr, stderr_write) = pipe(&security)?;
    let handles = [
        stdin.as_raw_handle(),
        stdout_write.as_raw_handle(),
        stderr_write.as_raw_handle(),
    ];
    let (mut job, mut reason) = match if force_unavailable {
        Err(io::Error::other("forced job setup unavailable"))
    } else {
        create_job()
    } {
        Ok(job) => (Some(job), None),
        Err(error) => (None, Some(format!("job setup: {error}"))),
    };
    let policy = [PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_DISABLE_PROCESS_TREE];
    let jobs = job.as_ref().map(|job| [job.as_raw_handle()]);
    let mut create =
        |job_list: Option<&[HANDLE]>, use_policy: bool| -> io::Result<PROCESS_INFORMATION> {
            let atomic = job_list.is_some();
            let mut attributes = Attributes::new(1 + u32::from(atomic) + u32::from(use_policy))?;
            if use_policy {
                attributes.add(PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize, &policy)?;
            }
            if atomic {
                attributes.add(PROC_THREAD_ATTRIBUTE_JOB_LIST as usize, job_list.unwrap())?;
            }
            let inheritance_guard = crate::host_command::inheritance_lock();
            // Originals are never inheritable. Temporary child duplicates live
            // only inside the same window used by every standard host spawn.
            let mut child_handles = Vec::with_capacity(handles.len());
            for &handle in &handles {
                let mut duplicate = null_mut();
                if unsafe {
                    DuplicateHandle(
                        GetCurrentProcess(),
                        handle,
                        GetCurrentProcess(),
                        &mut duplicate,
                        0,
                        1,
                        DUPLICATE_SAME_ACCESS,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                child_handles.push(owned(duplicate)?);
            }
            let handles: Vec<_> = child_handles
                .iter()
                .map(AsRawHandle::as_raw_handle)
                .collect();
            attributes.add(PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize, &handles)?;
            if let Some(hook) = before_create.take() {
                hook(&handles);
            }
            let mut startup = STARTUPINFOEXW::default();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = handles[0];
            startup.StartupInfo.hStdOutput = handles[1];
            startup.StartupInfo.hStdError = handles[2];
            startup.lpAttributeList = attributes.ptr();
            let mut line = line.clone();
            let mut information = PROCESS_INFORMATION::default();
            let result = unsafe {
                CreateProcessW(
                    application.as_ptr(),
                    line.as_mut_ptr(),
                    null(),
                    null(),
                    1,
                    spec.flags
                        | CREATE_SUSPENDED
                        | CREATE_UNICODE_ENVIRONMENT
                        | EXTENDED_STARTUPINFO_PRESENT,
                    environment
                        .as_ref()
                        .map_or(null(), |block| block.as_ptr().cast()),
                    cwd.as_ref().map_or(null(), |cwd| cwd.as_ptr()),
                    &startup.StartupInfo,
                    &mut information,
                )
            };
            let error = (result == 0).then(io::Error::last_os_error);
            drop(child_handles);
            drop(inheritance_guard);
            if let Some(error) = error {
                Err(error)
            } else {
                Ok(information)
            }
        };
    let atomic = job.is_some();
    #[cfg(test)]
    let mut refused_job_observer = None;
    let atomic_result = if atomic && force_atomic_refusal {
        Err(io::Error::from_raw_os_error(ERROR_INVALID_PARAMETER as i32))
    } else {
        create(jobs.as_ref().map(|jobs| jobs.as_slice()), true)
    };
    let (information, assigned_at_creation, policy_applied) = match atomic_result {
        Ok(info) => (info, atomic, true),
        Err(atomic_error) => {
            if atomic {
                // A refused packaged atomic launch can leave this job in a
                // hierarchy that rejects post-create assignment. Never reuse
                // that lease for the suspended fallback; create a fresh one.
                eprintln!(
                    "windows launch> atomic job creation refused: {atomic_error}; preparing fresh suspended-assignment lease"
                );
                #[cfg(test)]
                if force_atomic_refusal {
                    // A query-only witness lets the regression compare kernel
                    // job identities; it never exists on a production path.
                    let mut observer = null_mut();
                    if unsafe {
                        DuplicateHandle(
                            GetCurrentProcess(),
                            job.as_ref().unwrap().as_raw_handle(),
                            GetCurrentProcess(),
                            &mut observer,
                            JOB_OBJECT_QUERY,
                            0,
                            0,
                        )
                    } == 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    refused_job_observer = Some(owned(observer)?);
                }
                job.take();
                match create_job() {
                    Ok(fresh) => job = Some(fresh),
                    Err(error) => reason = Some(format!("fallback job setup: {error}")),
                }
            }
            match create(None, true) {
                Ok(info) => (info, false, true),
                Err(error) if matches!(error.raw_os_error(), Some(50 | 87)) => {
                    reason = Some(format!(
                        "desktop app policy unavailable: {error}; first attempt: {atomic_error}"
                    ));
                    (create(None, false)?, false, false)
                }
                Err(error) => return Err(error),
            }
        }
    };
    let process = Arc::new(WindowsChild {
        handle: owned(information.hProcess)?,
        id: information.dwProcessId,
        stdout: Mutex::new(Some(stdout)),
        stderr: Mutex::new(Some(stderr)),
        #[cfg(test)]
        deny_root_termination: std::sync::atomic::AtomicBool::new(false),
    });
    let thread = owned(information.hThread)?;
    if let Some(lease) = &job {
        if !assigned_at_creation
            && unsafe {
                AssignProcessToJobObject(lease.as_raw_handle(), process.handle.as_raw_handle())
            } == 0
        {
            reason = Some(format!(
                "suspended job assignment: {}",
                io::Error::last_os_error()
            ));
            job.take();
        }
    }
    if let Some(failure) = membership_failure(&mut job, process.handle.as_raw_handle()) {
        reason = Some(failure);
    }
    let status = match (reason, package(process.handle.as_raw_handle())) {
        (Some(reason), _) => ContainmentStatus::Unavailable { reason },
        (None, Err(error)) => ContainmentStatus::Unavailable {
            reason: format!("package identity query: {error}"),
        },
        (None, Ok(Some(package_full_name))) if policy_applied => {
            ContainmentStatus::ContainedWithPackagedIdentityChanged { package_full_name }
        }
        (None, _) => ContainmentStatus::Contained,
    };
    let launch = PreparedLaunch {
        process,
        thread: Mutex::new(Some(thread)),
        job: Mutex::new(job),
        status,
        #[cfg(test)]
        refused_job_observer,
    };
    eprintln!(
        "windows launch> pid={} {}",
        launch.process.id(),
        serde_json::to_string(launch.containment()).unwrap()
    );
    Ok(launch)
}

#[cfg(test)]
#[path = "tests/windows_launch.rs"]
mod tests;
