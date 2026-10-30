// Windows whole-host PDH, performance memory, IP Helper and volume APIs.
// All handles stay on the single sampler thread; no request invokes a probe.
use super::*;
use windows_sys::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Storage::FileSystem::{
    DISK_SPACE_INFORMATION, GetDiskFreeSpaceExW, GetVolumePathNameW,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Performance::*;
use windows_sys::Win32::System::ProcessStatus::{GetPerformanceInfo, PERFORMANCE_INFORMATION};
use windows_sys::Win32::System::Threading::GetActiveProcessorCount;

// PDH_FMT_NOSCALE in the Windows SDK Pdh.h; windows-sys 0.60.2 omits it.
const PDH_NO_SCALE: u32 = 0x00001000;

pub(super) struct Platform {
    query: PDH_HQUERY,
    counters: [PDH_HCOUNTER; 5],
    last_collect: Option<std::time::Instant>,
}
impl Drop for Platform {
    fn drop(&mut self) {
        if !self.query.is_null() {
            // SAFETY: this owner alone created and uses the query on its thread.
            unsafe {
                PdhCloseQuery(self.query);
            }
        }
    }
}
impl Platform {
    pub(super) fn new() -> Self {
        let mut this = Self {
            query: std::ptr::null_mut(),
            counters: [std::ptr::null_mut(); 5],
            last_collect: None,
        };
        // SAFETY: PDH writes a query handle into a valid output pointer.
        if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut this.query) } != 0 {
            return this;
        }
        for (index, name) in [
            r"\Processor Information(_Total)\% Processor Time",
            r"\Memory\Standby Cache Core Bytes",
            r"\Memory\Standby Cache Normal Priority Bytes",
            r"\Memory\Standby Cache Reserve Bytes",
            r"\Memory\Free & Zero Page List Bytes",
        ]
        .iter()
        .enumerate()
        {
            let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            // English names are resolved natively on localized installations.
            if unsafe {
                PdhAddEnglishCounterW(this.query, name.as_ptr(), 0, &mut this.counters[index])
            } != 0
            {
                this.counters[index] = std::ptr::null_mut();
            }
        }
        this
    }
    fn value(&self, index: usize, format: u32) -> Result<PDH_FMT_COUNTERVALUE, Reason> {
        if self.counters[index].is_null() {
            return Err(Reason::NotExposedByPlatform);
        }
        let mut value = PDH_FMT_COUNTERVALUE::default();
        if unsafe {
            PdhGetFormattedCounterValue(
                self.counters[index],
                format,
                std::ptr::null_mut(),
                &mut value,
            )
        } != 0
            || !matches!(value.CStatus, PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA)
        {
            return Err(Reason::ProbeFailed);
        }
        Ok(value)
    }
    fn bytes(&self, index: usize) -> Metric<u64> {
        match self.value(index, PDH_FMT_LARGE | PDH_NO_SCALE) {
            Ok(value) => {
                // SAFETY: PDH_FMT_LARGE selects this union member.
                let bytes = unsafe { value.Anonymous.largeValue };
                u64::try_from(bytes)
                    .map(Metric::available)
                    .unwrap_or_else(|_| Metric::unavailable(Reason::InvalidCounters))
            }
            Err(Reason::NotExposedByPlatform) => Metric::unsupported(),
            Err(reason) => Metric::unavailable(reason),
        }
    }
    pub(super) fn read(&mut self) -> Reading {
        let now = std::time::Instant::now();
        let collected = !self.query.is_null() && unsafe { PdhCollectQueryData(self.query) } == 0;
        let prior = if collected {
            self.last_collect.replace(now)
        } else {
            None
        };
        let window = prior
            .map(|prior| Metric::available(millis(now.saturating_duration_since(prior))))
            .unwrap_or_else(|| Metric::unavailable(Reason::FirstSample));
        let busy = if !collected {
            Metric::unavailable(Reason::ProbeFailed)
        } else if prior.is_none() {
            Metric::unavailable(Reason::FirstSample)
        } else {
            match self.value(0, PDH_FMT_DOUBLE | PDH_NO_SCALE) {
                Ok(value) => {
                    // SAFETY: PDH_FMT_DOUBLE selects this union member.
                    let percent = unsafe { value.Anonymous.doubleValue };
                    if percent.is_finite() && (0.0..=100.0).contains(&percent) {
                        Metric::available(percent)
                    } else {
                        Metric::unavailable(Reason::InvalidCounters)
                    }
                }
                Err(Reason::NotExposedByPlatform) => Metric::unsupported(),
                Err(reason) => Metric::unavailable(reason),
            }
        };
        // ALL_PROCESSOR_GROUPS: unlike available_parallelism/GetSystemTimes,
        // this is host-wide even on machines with multiple processor groups.
        let logical = unsafe { GetActiveProcessorCount(0xffff) };
        let logical = if logical == 0 {
            Metric::unavailable(Reason::ProbeFailed)
        } else {
            Metric::available(u64::from(logical))
        };
        let mut memory = Memory::unavailable();
        memory.commit_used_bytes = Metric::unavailable(Reason::ProbeFailed);
        memory.commit_limit_bytes = Metric::unavailable(Reason::ProbeFailed);
        memory.cache_definition =
            "standby core+normal+reserve; excludes system working set".to_owned();
        let mut info = PERFORMANCE_INFORMATION::default();
        info.cb = std::mem::size_of::<PERFORMANCE_INFORMATION>() as u32;
        let size = info.cb;
        if unsafe { GetPerformanceInfo(&mut info, size) } != 0 {
            let bytes = |pages: usize| {
                (pages as u64)
                    .checked_mul(info.PageSize as u64)
                    .map(Metric::available)
                    .unwrap_or_else(|| Metric::unavailable(Reason::InvalidCounters))
            };
            memory.total_bytes = bytes(info.PhysicalTotal);
            memory.available_bytes = bytes(info.PhysicalAvailable);
            memory.in_use_bytes = info
                .PhysicalTotal
                .checked_sub(info.PhysicalAvailable)
                .map(bytes)
                .unwrap_or_else(|| Metric::unavailable(Reason::InvalidCounters));
            memory.commit_used_bytes = bytes(info.CommitTotal);
            memory.commit_limit_bytes = bytes(info.CommitLimit);
        }
        if collected {
            let standby = [self.bytes(1), self.bytes(2), self.bytes(3)];
            memory.reclaimable_cache_bytes =
                if standby.iter().any(|v| v.state == Status::Unsupported) {
                    Metric::unsupported()
                } else {
                    standby
                        .iter()
                        .try_fold(0u64, |sum, v| sum.checked_add(v.value?))
                        .map(Metric::available)
                        .unwrap_or_else(|| Metric::unavailable(Reason::ProbeFailed))
                };
            memory.free_bytes = self.bytes(4);
        }
        let network_observed = std::time::Instant::now();
        let network_at = utc_now();
        let network = read_network()
            .map(Metric::available)
            .unwrap_or_else(Metric::unavailable);
        Reading {
            logical,
            cpu: None,
            busy: Some((busy, window)),
            load: Metric::unsupported(),
            memory,
            network,
            network_observed,
            network_at,
        }
    }
    pub(super) fn volume(path: &FsPath) -> Result<Volume, Reason> {
        use std::os::windows::ffi::OsStrExt;
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut mount = [0u16; 32768];
        // SAFETY: all output buffers have their stated capacity and inputs are terminated.
        if unsafe { GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0
        {
            return Err(Reason::ProbeFailed);
        }
        let end = mount
            .iter()
            .position(|c| *c == 0)
            .ok_or(Reason::InvalidCounters)?;
        let identity = String::from_utf16(&mount[..end]).map_err(|_| Reason::InvalidCounters)?;
        // The quota-independent API was added in Windows 10 1809. Resolve it
        // from the already-loaded system module, so older hosts can still
        // report native free bytes without a fabricated physical total.
        type DiskSpace = unsafe extern "system" fn(*const u16, *mut DISK_SPACE_INFORMATION) -> i32;
        let module = unsafe { GetModuleHandleW(windows_sys::core::w!("kernel32.dll")) };
        let function = if module.is_null() {
            None
        } else {
            unsafe { GetProcAddress(module, windows_sys::core::s!("GetDiskSpaceInformationW")) }
        };
        let Some(function) = function else {
            let (mut free, mut available) = (0, 0);
            if unsafe {
                GetDiskFreeSpaceExW(
                    mount.as_ptr(),
                    &mut available,
                    std::ptr::null_mut(),
                    &mut free,
                )
            } == 0
            {
                return Err(Reason::ProbeFailed);
            }
            return Ok(Volume {
                identity,
                total_bytes: Metric::unsupported(),
                free_bytes: Metric::available(free),
                available_to_caller_bytes: Metric::available(available),
            });
        };
        // SAFETY: the resolved system function's documented ABI is DiskSpace.
        let function: DiskSpace = unsafe { std::mem::transmute(function) };
        let mut info = std::mem::MaybeUninit::<DISK_SPACE_INFORMATION>::uninit();
        if unsafe { function(mount.as_ptr(), info.as_mut_ptr()) } != 0 {
            return Err(Reason::ProbeFailed);
        }
        let info = unsafe { info.assume_init() };
        let unit = u64::from(info.SectorsPerAllocationUnit)
            .checked_mul(u64::from(info.BytesPerSector))
            .filter(|n| *n > 0)
            .ok_or(Reason::InvalidCounters)?;
        let bytes = |n: u64| n.checked_mul(unit).ok_or(Reason::InvalidCounters);
        Ok(Volume {
            identity,
            total_bytes: Metric::available(bytes(info.ActualTotalAllocationUnits)?),
            free_bytes: Metric::available(bytes(info.ActualAvailableAllocationUnits)?),
            available_to_caller_bytes: Metric::available(bytes(
                info.CallerAvailableAllocationUnits,
            )?),
        })
    }
}
fn read_network() -> Result<Vec<NetworkCounter>, Reason> {
    struct Table(*mut MIB_IF_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            unsafe {
                FreeMibTable(self.0.cast());
            }
        }
    }
    let mut table = std::ptr::null_mut();
    if unsafe { GetIfTable2(&mut table) } != 0 || table.is_null() {
        return Err(Reason::ProbeFailed);
    }
    let table = Table(table);
    // SAFETY: GetIfTable2 allocates this native variable-length table, kept
    // alive by the guard. Bound the scan as well as the serialized collection.
    let count = unsafe { (*table.0).NumEntries as usize };
    if count > 4096 {
        return Err(Reason::CollectionLimit);
    }
    let entries = unsafe { std::slice::from_raw_parts((*table.0).Table.as_ptr(), count) };
    let mut rows = Vec::new();
    for row in entries
        .iter()
        .filter(|row| row.OperStatus == IfOperStatusUp)
    {
        let guid = row.InterfaceGuid;
        let identity = format!(
            "{:08x}-{:04x}-{:04x}-{}",
            guid.data1,
            guid.data2,
            guid.data3,
            guid.data4
                .iter()
                .map(|n| format!("{n:02x}"))
                .collect::<String>()
        );
        rows.push(NetworkCounter {
            identity,
            received: row.InOctets,
            sent: row.OutOctets,
        });
        if rows.len() > MAX_INTERFACES {
            return Err(Reason::CollectionLimit);
        }
    }
    Ok(rows)
}
