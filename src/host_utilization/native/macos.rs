// macOS Mach/sysctl counters and statfs. File-backed and compressor pages
// retain their native meanings; there is no invented MemAvailable equivalent.
use super::*;
pub(super) struct Platform {
    host: libc::mach_port_t,
}
unsafe extern "C" {
    fn mach_port_deallocate(
        task: libc::mach_port_t,
        name: libc::mach_port_t,
    ) -> libc::kern_return_t;
}
impl Drop for Platform {
    #[allow(deprecated)]
    fn drop(&mut self) {
        // SAFETY: mach_host_self gave this owner one send right.
        unsafe {
            mach_port_deallocate(libc::mach_task_self(), self.host);
        }
    }
}
impl Platform {
    pub(super) fn new() -> Self {
        Self {
            host: unsafe { libc::mach_host_self() },
        }
    }
    #[allow(deprecated)]
    pub(super) fn read(&mut self) -> Reading {
        let mut cpu = std::mem::MaybeUninit::<libc::host_cpu_load_info>::uninit();
        let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
        // SAFETY: the flavor, count and native output structure agree.
        let cpu = if unsafe {
            libc::host_statistics(
                self.host,
                libc::HOST_CPU_LOAD_INFO,
                cpu.as_mut_ptr().cast(),
                &mut count,
            )
        } == 0
            && count == libc::HOST_CPU_LOAD_INFO_COUNT
        {
            let ticks = unsafe { cpu.assume_init() }.cpu_ticks;
            Some(CpuCounter {
                total: ticks.iter().map(|n| u64::from(*n)).sum(),
                idle: u64::from(ticks[libc::CPU_STATE_IDLE as usize]),
            })
        } else {
            None
        };
        let logical = sysctl_value::<u32>(b"hw.logicalcpu\0")
            .filter(|n| *n > 0)
            .map(|n| Metric::available(u64::from(n)))
            .unwrap_or_else(|| Metric::unavailable(Reason::ProbeFailed));
        let memory = self.memory().unwrap_or_else(|_| Memory::unavailable());
        let network_observed = std::time::Instant::now();
        let network_at = utc_now();
        let network = read_network()
            .map(Metric::available)
            .unwrap_or_else(Metric::unavailable);
        Reading {
            logical,
            cpu,
            busy: None,
            load: load_average(),
            memory,
            network,
            network_observed,
            network_at,
        }
    }
    #[allow(deprecated)]
    fn memory(&self) -> Result<Memory, Reason> {
        let total = sysctl_value::<u64>(b"hw.memsize\0").ok_or(Reason::ProbeFailed)?;
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 {
            return Err(Reason::ProbeFailed);
        }
        let mut stat = std::mem::MaybeUninit::<libc::vm_statistics64>::uninit();
        let mut count = libc::HOST_VM_INFO64_COUNT;
        if unsafe {
            libc::host_statistics64(
                self.host,
                libc::HOST_VM_INFO64,
                stat.as_mut_ptr().cast(),
                &mut count,
            )
        } != 0
            || count < libc::HOST_VM_INFO64_COUNT
        {
            return Err(Reason::ProbeFailed);
        }
        let stat = unsafe { stat.assume_init() };
        let bytes = |n: u32| {
            u64::from(n)
                .checked_mul(page as u64)
                .ok_or(Reason::InvalidCounters)
        };
        // XNU includes speculative pages in free_count already. The external
        // pageable count is file-backed memory, not a promise of clean pages.
        let free = bytes(stat.free_count)?;
        let file_backed = bytes(stat.external_page_count)?;
        let used = total
            .checked_sub(free)
            .and_then(|n| n.checked_sub(file_backed))
            .ok_or(Reason::InvalidCounters)?;
        let mut memory = Memory::unavailable();
        memory.total_bytes = Metric::available(total);
        memory.free_bytes = Metric::available(free);
        memory.reclaimable_cache_bytes = Metric::available(file_backed);
        memory.in_use_bytes = Metric::available(used);
        memory.available_bytes = Metric::unsupported();
        memory.compressed_bytes = Metric::available(bytes(stat.compressor_page_count)?);
        memory.cache_definition =
            "file-backed pageable pages; not immediately evictable bytes".to_owned();
        Ok(memory)
    }
    pub(super) fn volume(path: &FsPath) -> Result<Volume, Reason> {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| Reason::ProbeFailed)?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(Reason::ProbeFailed);
        }
        let stat = unsafe { stat.assume_init() };
        let identity = unsafe { std::ffi::CStr::from_ptr(stat.f_mntonname.as_ptr()) }
            .to_str()
            .map_err(|_| Reason::InvalidCounters)?
            .to_owned();
        let bytes = |n: u64| {
            n.checked_mul(stat.f_bsize as u64)
                .ok_or(Reason::InvalidCounters)
        };
        Ok(Volume {
            identity,
            total_bytes: Metric::available(bytes(stat.f_blocks)?),
            free_bytes: Metric::available(bytes(stat.f_bfree)?),
            available_to_caller_bytes: Metric::available(bytes(stat.f_bavail)?),
        })
    }
}

fn sysctl_value<T: Copy>(name: &[u8]) -> Option<T> {
    let mut value = std::mem::MaybeUninit::<T>::uninit();
    let mut size = std::mem::size_of::<T>();
    // SAFETY: static NUL-terminated name, exact typed output, no write request.
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            value.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } == 0
        && size == std::mem::size_of::<T>()
    {
        Some(unsafe { value.assume_init() })
    } else {
        None
    }
}
fn read_network() -> Result<Vec<NetworkCounter>, Reason> {
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut len = 0usize;
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return Err(Reason::ProbeFailed);
    }
    if len > 1024 * 1024 {
        return Err(Reason::CollectionLimit);
    }
    let mut bytes = vec![0u8; len];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            bytes.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || len > bytes.len()
    {
        return Err(Reason::ProbeFailed);
    }
    bytes.truncate(len);
    let mut offset = 0;
    let mut rows = Vec::new();
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        if remaining.len() < 4 {
            return Err(Reason::InvalidCounters);
        }
        let length = u16::from_ne_bytes([remaining[0], remaining[1]]) as usize;
        if length < 4 || length > remaining.len() {
            return Err(Reason::InvalidCounters);
        }
        if remaining[3] as i32 == libc::RTM_IFINFO2 {
            if length < std::mem::size_of::<libc::if_msghdr2>() {
                return Err(Reason::InvalidCounters);
            }
            // SAFETY: length checked above; a byte buffer need not be aligned.
            let msg =
                unsafe { std::ptr::read_unaligned(remaining.as_ptr().cast::<libc::if_msghdr2>()) };
            if msg.ifm_flags & (libc::IFF_UP | libc::IFF_RUNNING)
                == (libc::IFF_UP | libc::IFF_RUNNING)
            {
                let index = msg.ifm_index;
                let data = msg.ifm_data;
                rows.push(NetworkCounter {
                    identity: index.to_string(),
                    received: data.ifi_ibytes,
                    sent: data.ifi_obytes,
                });
                if rows.len() > MAX_INTERFACES {
                    return Err(Reason::CollectionLimit);
                }
            }
        }
        offset += length;
    }
    Ok(rows)
}
