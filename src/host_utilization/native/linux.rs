// Linux host counters: bounded proc/sysfs reads and statvfs, with no commands.
use super::*;
pub(super) struct Platform;
impl Platform {
    pub(super) fn new() -> Self {
        Self
    }
    pub(super) fn read(&mut self) -> Reading {
        let (logical, cpu) = bounded_read(FsPath::new("/proc/stat"), 256 * 1024)
            .and_then(|text| parse_cpu(&text))
            .map(|(n, cpu)| (Metric::available(n), Some(cpu)))
            .unwrap_or_else(|reason| (Metric::unavailable(reason), None));
        let memory = bounded_read(FsPath::new("/proc/meminfo"), 32 * 1024)
            .and_then(|text| parse_memory(&text))
            .unwrap_or_else(|_| Memory::unavailable());
        let network_observed = std::time::Instant::now();
        let network_at = utc_now();
        let network = match read_network() {
            Ok(rows) => Metric::available(rows),
            Err(reason) => Metric::unavailable(reason),
        };
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
    pub(super) fn volume(path: &FsPath) -> Result<Volume, Reason> {
        use std::os::unix::ffi::OsStrExt;
        let canonical = fs::canonicalize(path).map_err(|_| Reason::ProbeFailed)?;
        let cpath = std::ffi::CString::new(canonical.as_os_str().as_bytes())
            .map_err(|_| Reason::ProbeFailed)?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: the path is NUL-terminated and the output has the native layout.
        if unsafe { libc::statvfs(cpath.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(Reason::ProbeFailed);
        }
        let stat = unsafe { stat.assume_init() };
        let mounts = bounded_read(FsPath::new("/proc/self/mountinfo"), 1024 * 1024)?;
        let mount = mounts
            .lines()
            .filter_map(|line| line.split_whitespace().nth(4))
            .map(unescape_mount)
            .filter(|mount| canonical.starts_with(mount))
            .max_by_key(|mount| mount.as_os_str().len())
            .ok_or(Reason::ProbeFailed)?;
        let identity = mount
            .to_str()
            .filter(|s| valid_label(s))
            .ok_or(Reason::CollectionLimit)?
            .to_owned();
        let bytes = |blocks: u64| {
            blocks
                .checked_mul(stat.f_frsize as u64)
                .ok_or(Reason::InvalidCounters)
        };
        Ok(Volume {
            identity,
            total_bytes: Metric::available(bytes(stat.f_blocks as u64)?),
            free_bytes: Metric::available(bytes(stat.f_bfree as u64)?),
            available_to_caller_bytes: Metric::available(bytes(stat.f_bavail as u64)?),
        })
    }
}

fn unescape_mount(value: &str) -> PathBuf {
    // mountinfo escapes these four pathname bytes, not arbitrary backslashes.
    PathBuf::from(
        value
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\"),
    )
}
fn parse_cpu(text: &str) -> Result<(u64, CpuCounter), Reason> {
    let first = text.lines().next().ok_or(Reason::InvalidCounters)?;
    let mut parts = first.split_whitespace();
    if parts.next() != Some("cpu") {
        return Err(Reason::InvalidCounters);
    }
    let values = parts
        .take(8)
        .map(|part| part.parse::<u64>().map_err(|_| Reason::InvalidCounters))
        .collect::<Result<Vec<_>, _>>()?;
    if values.len() != 8 {
        return Err(Reason::InvalidCounters);
    }
    // guest and guest_nice are already included in user/nice. iowait is
    // classified as idle here; a decreasing counter invalidates the window.
    let total = values
        .iter()
        .try_fold(0u64, |sum, v| sum.checked_add(*v))
        .ok_or(Reason::InvalidCounters)?;
    let idle = values[3]
        .checked_add(values[4])
        .ok_or(Reason::InvalidCounters)?;
    let logical = text
        .lines()
        .filter(|line| {
            line.split_whitespace().next().is_some_and(|name| {
                name.strip_prefix("cpu")
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
            })
        })
        .count();
    if logical == 0 {
        return Err(Reason::InvalidCounters);
    }
    Ok((logical as u64, CpuCounter { total, idle }))
}
fn parse_memory(text: &str) -> Result<Memory, Reason> {
    let mut fields = HashMap::new();
    for line in text.lines() {
        let mut pieces = line.split_whitespace();
        let Some(key) = pieces.next() else { continue };
        let Some(value) = pieces.next() else { continue };
        if pieces.next() != Some("kB") {
            continue;
        }
        let bytes = value
            .parse::<u64>()
            .ok()
            .and_then(|v| v.checked_mul(1024))
            .ok_or(Reason::InvalidCounters)?;
        fields.insert(key.trim_end_matches(':'), bytes);
    }
    let field = |key| fields.get(key).copied().ok_or(Reason::NotExposedByPlatform);
    let total = field("MemTotal")?;
    let free = field("MemFree")?;
    // Exclude shared/tmpfs pages from Cached. This is a named accounting
    // category, not a claim that every cached page is immediately evictable.
    let cache = field("Buffers")?
        .checked_add(field("Cached")?)
        .and_then(|n| n.checked_add(fields.get("SReclaimable").copied()?))
        .and_then(|n| n.checked_sub(fields.get("Shmem").copied()?))
        .ok_or(Reason::InvalidCounters)?;
    let used = total
        .checked_sub(free)
        .and_then(|n| n.checked_sub(cache))
        .ok_or(Reason::InvalidCounters)?;
    let mut memory = Memory::unavailable();
    memory.total_bytes = Metric::available(total);
    memory.free_bytes = Metric::available(free);
    memory.reclaimable_cache_bytes = Metric::available(cache);
    memory.in_use_bytes = Metric::available(used);
    memory.available_bytes = field("MemAvailable")
        .map(Metric::available)
        .unwrap_or_else(|_| Metric::unsupported());
    memory.cache_definition =
        "buffers+cached+sReclaimable-shmem; not immediately evictable bytes".to_owned();
    Ok(memory)
}
fn read_network() -> Result<Vec<NetworkCounter>, Reason> {
    let text = bounded_read(FsPath::new("/proc/net/dev"), 64 * 1024)?;
    parse_network(&text, bounded_read)
}

fn parse_network(
    text: &str,
    mut read_sysfs: impl FnMut(&FsPath, usize) -> Result<String, Reason>,
) -> Result<Vec<NetworkCounter>, Reason> {
    let mut rows = Vec::new();
    for line in text.lines().skip(2) {
        let (name, values) = line.split_once(':').ok_or(Reason::InvalidCounters)?;
        let name = name.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(Reason::InvalidCounters);
        }
        let root = PathBuf::from("/sys/class/net").join(name);
        let flags = read_sysfs(&root.join("flags"), 64)?;
        let flags = u32::from_str_radix(flags.trim().trim_start_matches("0x"), 16)
            .map_err(|_| Reason::InvalidCounters)?;
        // sysfs flags expose dev->flags, without the volatile IFF_RUNNING.
        // UP or UNKNOWN operstate is its operational equivalent; UNKNOWN
        // includes loopback and drivers that do not implement operstate.
        if flags & libc::IFF_UP as u32 == 0 {
            continue;
        }
        let operstate = read_sysfs(&root.join("operstate"), 64)?;
        if !matches!(operstate.trim(), "up" | "unknown") {
            continue;
        }
        let index = read_sysfs(&root.join("ifindex"), 64)?;
        let index = index
            .trim()
            .parse::<u32>()
            .map_err(|_| Reason::InvalidCounters)?;
        let values = values
            .split_whitespace()
            .take(16)
            .map(|v| v.parse::<u64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| Reason::InvalidCounters)?;
        if values.len() != 16 {
            return Err(Reason::InvalidCounters);
        }
        rows.push(NetworkCounter {
            identity: format!("{index}:{name}"),
            received: values[0],
            sent: values[8],
        });
        if rows.len() > MAX_INTERFACES {
            return Err(Reason::CollectionLimit);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn network_uses_realistic_sysfs_flags_and_operstate_without_running_bit() {
        let interfaces = [
            ("eth0", "0x1003\n", "up\n"),
            ("lo", "0x9\n", "unknown\n"),
            ("admin_down", "0x1002\n", "up\n"),
            ("link_down", "0x1003\n", "down\n"),
            ("lower_down", "0x1003\n", "lowerlayerdown\n"),
            ("dormant", "0x1003\n", "dormant\n"),
            ("testing", "0x1003\n", "testing\n"),
            ("absent", "0x1003\n", "notpresent\n"),
        ];
        let mut text = String::from("Inter-| Receive | Transmit\n face |bytes |bytes\n");
        for (name, _, _) in &interfaces {
            text.push_str(&format!(
                "{name}: 1234 1 0 0 0 0 0 0 5678 2 0 0 0 0 0 0\n"
            ));
        }
        let rows = parse_network(&text, |path, limit| {
            assert_eq!(limit, 64);
            let name = path.parent().unwrap().file_name().unwrap().to_str().unwrap();
            let index = interfaces
                .iter()
                .position(|(candidate, _, _)| *candidate == name)
                .expect("only the proc-listed interface is probed");
            let (_, flags, operstate) = interfaces[index];
            Ok(match path.file_name().unwrap().to_str().unwrap() {
                "flags" => flags.to_owned(),
                "operstate" => operstate.to_owned(),
                "ifindex" => (index + 1).to_string(),
                _ => panic!("unexpected sysfs attribute"),
            })
        })
        .unwrap();
        assert_eq!(
            rows.len(),
            2,
            "up Ethernet and unknown-state loopback must survive"
        );
        assert_eq!(rows[0].identity, "1:eth0");
        assert_eq!(rows[1].identity, "2:lo");
        for row in rows {
            assert_eq!((row.received, row.sent), (1234, 5678));
        }
    }

    #[test]
    fn cpu_excludes_duplicate_guest_and_memory_excludes_shared_cache() {
        let (count, counter) =
            parse_cpu("cpu 10 2 3 40 5 6 7 8 100 200\ncpu0 0\ncpu1 0\n").unwrap();
        assert_eq!((count, counter.total, counter.idle), (2, 81, 45));
        let m = parse_memory("MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 400 kB\nBuffers: 20 kB\nCached: 200 kB\nSReclaimable: 30 kB\nShmem: 50 kB\n").unwrap();
        assert_eq!(m.reclaimable_cache_bytes.value, Some(200 * 1024));
        assert_eq!(m.in_use_bytes.value, Some(700 * 1024));
        assert_eq!(m.available_bytes.value, Some(400 * 1024));
    }
}
