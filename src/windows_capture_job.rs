//! Capture-only completion custody. Terminal launches retain their existing
//! kill-on-close policy; owned captures require an actual job-zero certificate.
use super::*;
use std::time::Instant;
use windows_sys::Win32::System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatus};
use windows_sys::Win32::System::SystemServices::JOB_OBJECT_MSG_NEW_PROCESS;

pub(crate) struct CaptureJob {
    job: OwnedHandle,
    port: Arc<OwnedHandle>,
    members: Mutex<CaptureMembers>,
    #[cfg(test)]
    probe: Option<Arc<CleanupProbe>>,
}

struct CaptureMembers {
    // Events count creation occurrences, not distinct PIDs. The retained
    // leader is a wait witness; its notification counts once in this total.
    notifications: usize,
    handles: Vec<Option<Arc<OwnedHandle>>>,
}

impl PreparedLaunch {
    // Must be installed while the leader is suspended, before any descendants
    // can start. The private duplicate preserves the exact job through cleanup.
    pub(crate) fn capture_job(&self) -> io::Result<Option<Arc<CaptureJob>>> {
        let job = self.job.lock().unwrap();
        let Some(job) = job.as_ref() else {
            return Ok(None);
        };
        let mut duplicate = null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                job.as_raw_handle(),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let job = owned(duplicate)?;
        let port =
            owned(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, null_mut(), 0, 1) })?;
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: null_mut(),
            CompletionPort: port.as_raw_handle(),
        };
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectAssociateCompletionPortInformation,
                &association as *const _ as *const c_void,
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // Any incomplete suspended-leader cleanup is now retained with this
        // job. PreparedLaunch::drop must not add an unbounded process wait.
        self.capture_cleanup_owned
            .store(true, std::sync::atomic::Ordering::Release);
        let mut leader = null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.process.handle.as_raw_handle(),
                GetCurrentProcess(),
                &mut leader,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(Arc::new(CaptureJob {
            job,
            port: Arc::new(port),
            members: Mutex::new(CaptureMembers {
                notifications: 0,
                handles: vec![Some(Arc::new(owned(leader)?))],
            }),
            #[cfg(test)]
            probe: self.capture_cleanup_probe.clone(),
        })))
    }
}

impl CaptureJob {
    #[cfg(test)]
    pub(crate) fn diagnostic_counts(&self) -> String {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(), JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0 {
            return format!("capture job counts unavailable: {}", io::Error::last_os_error());
        }
        let members = self.members.lock().unwrap();
        format!("active={} total={} notifications={} pending_wait_handles={}",
            accounting.ActiveProcesses, accounting.TotalProcesses,
            members.notifications, members.handles.iter().flatten().count())
    }

    #[cfg(test)]
    fn witness_count(&self) -> usize {
        self.members.lock().unwrap().notifications
    }

    #[cfg(test)]
    fn wait_target_count(&self) -> usize {
        self.members
            .lock()
            .unwrap()
            .handles
            .iter()
            .flatten()
            .count()
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        if unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn complete(&self) -> io::Result<bool> {
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe {
            QueryInformationJobObject(
                self.job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                &mut accounting as *mut _ as *mut c_void,
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut members = self.members.lock().unwrap();
        for handle in &mut members.handles {
            if let Some(process) = handle {
                match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
                    WAIT_OBJECT_0 => {
                        *handle = None;
                    }
                    WAIT_TIMEOUT => return Ok(false),
                    _ => return Err(io::Error::last_os_error()),
                }
            }
        }
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            if probe.blocked.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(false);
            }
        }
        // A best-effort notification stream is never assumed complete. The
        // native total must account for every retained member witness. A gap
        // preserves custody rather than treating an empty job as full proof.
        Ok(accounting.ActiveProcesses == 0
            && members.notifications == accounting.TotalProcesses as usize)
    }

    fn observe_member(&self, message: u32, identity: usize) -> io::Result<()> {
        if message != JOB_OBJECT_MSG_NEW_PROCESS {
            return Ok(());
        }
        let pid =
            u32::try_from(identity).map_err(|_| io::Error::other("invalid capture member id"))?;
        let mut members = self.members.lock().unwrap();
        // The PID is a job-issued discovery hint, never termination authority.
        // Retain the opened object and wait it. Even PID reuse can only delay
        // certification; no process outside the private job is signalled.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                // The job-issued process identity no longer exists.
                members.notifications += 1;
                return Ok(());
            }
            return Err(error);
        }
        let handle = owned(handle)?;
        let mut in_job = 0;
        if unsafe {
            IsProcessInJob(
                handle.as_raw_handle(),
                self.job.as_raw_handle(),
                &mut in_job,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // A non-member with this recycled PID is unrelated to the old event;
        // it must not become a wait target or termination authority.
        if in_job != 0 {
            members.handles.push(Some(Arc::new(handle)));
        }
        members.notifications += 1;
        Ok(())
    }

    pub(crate) fn finish(&self, deadline: Option<Instant>) -> io::Result<bool> {
        #[cfg(test)]
        if let Some(probe) = &self.probe {
            probe.finish_entries.lock().unwrap().push((deadline, Instant::now()));
            *probe.port.lock().unwrap() = Some(self.port.clone());
        }
        loop {
            // Drain already-issued member discovery before consulting totals.
            loop {
                let mut message = 0;
                let mut key = 0;
                let mut identity = null_mut();
                if unsafe {
                    GetQueuedCompletionStatus(
                        self.port.as_raw_handle(),
                        &mut message,
                        &mut key,
                        &mut identity,
                        0,
                    )
                } == 0
                {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(WAIT_TIMEOUT as i32) {
                        return Err(error);
                    }
                    break;
                }
                self.observe_member(message, identity as usize)?;
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    break;
                }
            }
            if self.complete()? {
                return Ok(true);
            }
            #[cfg(test)]
            if deadline.is_some()
                && self
                    .probe
                    .as_ref()
                    .is_some_and(|probe| probe.blocked.load(std::sync::atomic::Ordering::Acquire))
            {
                return Ok(false);
            }
            let millis = match deadline {
                Some(deadline) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return self.complete();
                    }
                    (remaining.as_millis() + u128::from(remaining.subsec_nanos() % 1_000_000 != 0))
                        .min(u128::from(INFINITE - 1)) as u32
                }
                None => INFINITE,
            };
            let member = self
                .members
                .lock()
                .unwrap()
                .handles
                .iter()
                .flatten()
                .next()
                .cloned();
            if let Some(member) = member {
                match unsafe { WaitForSingleObject(member.as_raw_handle(), millis) } {
                    WAIT_OBJECT_0 | WAIT_TIMEOUT => continue,
                    _ => return Err(io::Error::last_os_error()),
                }
            }
            let mut bytes = 0;
            let mut key = 0;
            let mut overlapped = null_mut();
            let ok = unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle(),
                    &mut bytes,
                    &mut key,
                    &mut overlapped,
                    millis,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(WAIT_TIMEOUT as i32) {
                    return Err(error);
                }
            } else {
                self.observe_member(bytes, overlapped as usize)?;
            }
            // Job notifications are wake hints, not completion certificates;
            // their delivery is not guaranteed. Query again even on timeout.
        }
    }
}

#[cfg(test)]
mod pid_reuse_tests {
    use super::*;

    fn notification_control(check: impl FnOnce(&Arc<CaptureJob>, u32)) {
        let spec = LaunchSpec::new(std::env::current_exe().unwrap());
        let launch = prepare(&spec).unwrap();
        let process = launch.process();
        let job = launch
            .capture_job()
            .unwrap()
            .expect("contained native fixture");
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&job, process.id())));
        // The fixture stays suspended; retire its exact native objects even
        // when the RED assertion fails. Never wait on a substituted PID.
        job.terminate().unwrap();
        process
            .wait_timeout(Duration::from_secs(30))
            .unwrap()
            .expect("fixture exited");
        drop(job);
        drop(launch);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    #[test]
    fn capture_recycled_member_pid_counts_each_creation_notification() {
        notification_control(|job, pid| {
            // Inject the two queued creation events that survive when an
            // earlier member's PID is recycled into a later same-job member.
            job.observe_member(JOB_OBJECT_MSG_NEW_PROCESS, pid as usize)
                .unwrap();
            job.observe_member(JOB_OBJECT_MSG_NEW_PROCESS, pid as usize)
                .unwrap();
            assert_eq!(
                job.witness_count(),
                2,
                "a repeated PID must not erase a distinct creation witness"
            );
        });
    }

    #[test]
    fn capture_recycled_nonmember_pid_never_becomes_a_wait_target() {
        notification_control(|job, _| {
            let before = job.wait_target_count();
            // This test process is live outside the fixture's private job.
            // Substituting its PID models reuse after the old member exited.
            job.observe_member(JOB_OBJECT_MSG_NEW_PROCESS, unsafe { GetCurrentProcessId() }
                as usize)
                .unwrap();
            assert_eq!(
                job.wait_target_count(),
                before,
                "a recycled nonmember PID must not retain an unrelated process"
            );
        });
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(super) struct CleanupProbe {
    blocked: std::sync::atomic::AtomicBool,
    port: Mutex<Option<Arc<OwnedHandle>>>,
    finish_entries: Mutex<Vec<(Option<Instant>, Instant)>>,
}
#[cfg(test)]
impl CleanupProbe {
    pub(super) fn blocked() -> Arc<Self> {
        Arc::new(Self {
            blocked: std::sync::atomic::AtomicBool::new(true),
            port: Mutex::new(None),
            finish_entries: Mutex::new(Vec::new()),
        })
    }
    pub(super) fn finish_entries(&self) -> Vec<(Option<Instant>, Instant)> {
        self.finish_entries.lock().unwrap().clone()
    }
    pub(super) fn release(&self) {
        self.blocked
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(port) = self.port.lock().unwrap().take() {
            unsafe {
                windows_sys::Win32::System::IO::PostQueuedCompletionStatus(
                    port.as_raw_handle(),
                    0,
                    0,
                    null(),
                )
            };
        }
    }
}
