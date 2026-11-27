//! Peer evidence for the SSH child that owns a healthy remote route.
//! Socket and interface inspection runs outside application state locks.

use std::{
    collections::{HashMap, HashSet},
    fs, io,
    net::SocketAddr,
    path::Path,
    process::Child,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crate::{engram_network_path, engram_path_key};

#[cfg(any(windows, test))]
use std::net::IpAddr;
#[cfg(all(windows, not(test)))]
use std::sync::{Arc, Mutex};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProxyWorktree {
    Local(String),
    Outside,
    Inconclusive,
}

const PROXY_PATH_PROBE_BUDGET: Duration = Duration::from_millis(150);
const MAX_PROXY_PATH_PROBES: usize = 8;
static ACTIVE_PROXY_PATH_PROBES: AtomicUsize = AtomicUsize::new(0);

struct ProxyPathProbeSlot;

impl Drop for ProxyPathProbeSlot {
    fn drop(&mut self) {
        ACTIVE_PROXY_PATH_PROBES.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Limits the wait for the whole batch. A stalled volume retains its worker
/// slot until the OS call returns. Callers treat missing results as unknown.
pub(crate) fn probe_proxy_worktrees(paths: HashSet<String>) -> HashMap<String, ProxyWorktree> {
    probe_proxy_worktrees_with(paths, probe_proxy_worktree)
}

fn probe_proxy_worktrees_with(
    paths: HashSet<String>,
    lookup: impl Fn(&Path) -> ProxyWorktree + Send + 'static,
) -> HashMap<String, ProxyWorktree> {
    if paths.is_empty()
        || ACTIVE_PROXY_PATH_PROBES
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < MAX_PROXY_PATH_PROBES).then_some(active + 1)
            })
            .is_err()
    {
        return HashMap::new();
    }
    let slot = ProxyPathProbeSlot;
    let deadline = Instant::now() + PROXY_PATH_PROBE_BUDGET;
    let (sender, receiver) = mpsc::channel();
    if std::thread::Builder::new()
        .name("engram-proxy-worktrees".to_owned())
        .spawn(move || {
            let _slot = slot;
            for path in paths {
                if Instant::now() >= deadline {
                    break;
                }
                let result = lookup(Path::new(&path));
                if sender.send((path, result)).is_err() {
                    break;
                }
            }
        })
        .is_err()
    {
        return HashMap::new();
    }
    let mut results = HashMap::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok((path, result)) = receiver.recv_timeout(remaining) else {
            break;
        };
        results.insert(path, result);
    }
    results
}

fn probe_proxy_worktree(workdir: &Path) -> ProxyWorktree {
    if engram_network_path(&workdir.to_string_lossy()) {
        return ProxyWorktree::Inconclusive;
    }
    if !workdir.is_absolute() {
        // A foreign-format path cannot be rooted at the Central process cwd.
        return ProxyWorktree::Outside;
    }
    for ancestor in workdir.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return ProxyWorktree::Inconclusive,
            Ok(_) => {}
        }
        let Ok(rooted) = fs::canonicalize(ancestor) else {
            return ProxyWorktree::Inconclusive;
        };
        if engram_network_path(&rooted.to_string_lossy()) {
            return ProxyWorktree::Inconclusive;
        }
        for path in rooted.ancestors() {
            match fs::symlink_metadata(path.join(".git")) {
                Ok(_) => return ProxyWorktree::Local(engram_path_key(path)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => return ProxyWorktree::Inconclusive,
            }
        }
        return ProxyWorktree::Outside;
    }
    // No ancestor was accessible, for example an unavailable drive.
    ProxyWorktree::Inconclusive
}

/// Whether the admitted child owns one established outward TCP socket
/// matching the endpoint of OpenSSH's direct connection,
/// with a peer on the configured SSH port and an address foreign to this
/// host. Missing evidence leaves the proxy a possible writer here.
pub(crate) fn has_direct_foreign_peer(
    child: &Child,
    forward_port: u16,
    ssh_port: u16,
    direct_peer: Option<SocketAddr>,
) -> bool {
    #[cfg(windows)]
    {
        tcp_connections().is_some_and(|connections| {
            direct_foreign_peer(
                &connections,
                child.id(),
                forward_port,
                ssh_port,
                direct_peer,
                address_may_be_local,
            )
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (child, forward_port, ssh_port, direct_peer);
        false
    }
}

/// OpenSSH logs the numeric endpoint when attempting a direct connect and
/// acknowledges the successful socket before beginning the SSH handshake.
/// A forwarded socket alone does not supply either of those observations.
#[cfg(any(windows, test))]
fn direct_ssh_peer_from_startup(log: &[u8]) -> Option<SocketAddr> {
    let end = log.iter().rposition(|byte| *byte == b'\n')?;
    let log = std::str::from_utf8(&log[..=end]).ok()?;
    let mut attempt = None;
    let mut direct = None;
    for line in log.lines() {
        if line.starts_with("debug1: Executing proxy ") || line.starts_with("debug1: auto-mux:") {
            return None;
        }
        if let Some(connecting) = line.strip_prefix("debug1: Connecting to ") {
            if direct.is_some() {
                return None;
            }
            attempt = connecting
                .rsplit_once(" [")
                .and_then(|(_, rest)| rest.split_once("] port "))
                .and_then(|(ip, port)| {
                    Some(SocketAddr::new(
                        ip.parse().ok()?,
                        port.strip_suffix('.')?.parse().ok()?,
                    ))
                });
        }
        if line == "debug1: Connection established." {
            if direct.is_some() {
                return None;
            }
            direct = Some(attempt.take()?);
        }
    }
    direct
}

#[cfg(all(windows, not(test)))]
pub(crate) struct SshStartupDiagnostics {
    prefix: Arc<Mutex<Vec<u8>>>,
}

#[cfg(all(windows, not(test)))]
impl SshStartupDiagnostics {
    const LIMIT: usize = 32 * 1024;

    pub(crate) fn drain(mut stderr: std::process::ChildStderr) -> Option<Self> {
        use std::io::Read;

        let prefix = Arc::new(Mutex::new(Vec::new()));
        let collected = prefix.clone();
        std::thread::Builder::new()
            .spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = stderr.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut collected = collected.lock().expect("SSH diagnostics mutex poisoned");
                    let remaining = (Self::LIMIT + 1).saturating_sub(collected.len());
                    collected.extend_from_slice(&chunk[..n.min(remaining)]);
                }
            })
            .ok()?;
        Some(Self { prefix })
    }

    pub(crate) fn direct_peer(&self) -> Option<SocketAddr> {
        let prefix = self.prefix.lock().ok()?;
        (prefix.len() <= Self::LIMIT)
            .then(|| direct_ssh_peer_from_startup(&prefix))
            .flatten()
    }

    pub(crate) fn failure_suffix(&self) -> String {
        let Ok(prefix) = self.prefix.lock() else {
            return String::new();
        };
        let log = String::from_utf8_lossy(&prefix);
        let Some(error) = log
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("debug"))
        else {
            return String::new();
        };
        format!(": {error}")
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy)]
struct TcpConnection {
    pid: u32,
    local: SocketAddr,
    remote: SocketAddr,
    listening: bool,
}

#[cfg(any(windows, test))]
fn normalized_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}

#[cfg(any(windows, test))]
fn direct_foreign_peer(
    connections: &[TcpConnection],
    pid: u32,
    forward_port: u16,
    ssh_port: u16,
    direct_peer: Option<SocketAddr>,
    address_may_be_local: impl FnOnce(SocketAddr) -> bool,
) -> bool {
    let Some(direct_peer) = direct_peer else {
        return false;
    };
    let mut outbound = connections.iter().filter(|socket| {
        socket.pid == pid
            && !socket.listening
            && !(socket.local.port() == forward_port
                && normalized_ip(socket.local.ip()).is_loopback()
                && normalized_ip(socket.remote.ip()).is_loopback())
    });
    let Some(socket) = outbound.next() else {
        return false;
    };
    let ip = normalized_ip(socket.remote.ip());
    outbound.next().is_none()
        // An established row has no direction. A local port used by the
        // child to accept connections cannot identify its outbound peer.
        && socket.local.port() != forward_port
        && !connections.iter().any(|listener| {
            listener.pid == pid
                && listener.listening
                && listener.local.port() == socket.local.port()
        })
        && socket.remote.port() == ssh_port
        && direct_peer.port() == ssh_port
        && normalized_ip(direct_peer.ip()) == ip
        && !ip.is_loopback()
        && !ip.is_unspecified()
        && ip != normalized_ip(socket.local.ip())
        // Scope IDs are unnecessary for a positive proof. A scoped peer or
        // link-local address stays conservative even if a bind would fail.
        && !matches!(socket.remote, SocketAddr::V6(address)
            if address.scope_id() != 0 || address.ip().is_unicast_link_local())
        && !address_may_be_local(socket.remote)
}

#[cfg(windows)]
fn address_may_be_local(peer: SocketAddr) -> bool {
    let address = SocketAddr::new(normalized_ip(peer.ip()), 0);
    bind_may_be_local(std::net::UdpSocket::bind(address).map(drop))
}

#[cfg(any(windows, test))]
fn bind_may_be_local(result: std::io::Result<()>) -> bool {
    // Only an address unavailable on this host is affirmative evidence.
    !matches!(result, Err(error) if error.kind() == std::io::ErrorKind::AddrNotAvailable)
}

#[cfg(windows)]
fn tcp_v6_address(bytes: [u8; 16], port: u32, scope: u32) -> SocketAddr {
    SocketAddr::V6(std::net::SocketAddrV6::new(
        bytes.into(),
        u16::from_be(port as u16),
        0,
        u32::from_be(scope),
    ))
}

#[cfg(windows)]
fn tcp_connections() -> Option<Vec<TcpConnection>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_TCP_STATE_ESTAB, MIB_TCP_STATE_LISTEN, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
    };

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    let port = |raw: u32| u16::from_be(raw as u16);
    let v4 = tcp_table::<MIB_TCPROW_OWNER_PID>(AF_INET)?;
    let v6 = tcp_table::<MIB_TCP6ROW_OWNER_PID>(AF_INET6)?;
    let mut connections = Vec::new();
    for row in v4 {
        let listening = row.dwState == MIB_TCP_STATE_LISTEN as u32;
        if listening || row.dwState == MIB_TCP_STATE_ESTAB as u32 {
            connections.push(TcpConnection {
                pid: row.dwOwningPid,
                local: SocketAddr::from((row.dwLocalAddr.to_ne_bytes(), port(row.dwLocalPort))),
                remote: SocketAddr::from((row.dwRemoteAddr.to_ne_bytes(), port(row.dwRemotePort))),
                listening,
            });
        }
    }
    for row in v6 {
        let listening = row.dwState == MIB_TCP_STATE_LISTEN as u32;
        if listening || row.dwState == MIB_TCP_STATE_ESTAB as u32 {
            connections.push(TcpConnection {
                pid: row.dwOwningPid,
                local: tcp_v6_address(row.ucLocalAddr, row.dwLocalPort, row.dwLocalScopeId),
                remote: tcp_v6_address(row.ucRemoteAddr, row.dwRemotePort, row.dwRemoteScopeId),
                listening,
            });
        }
    }
    Some(connections)
}

// The native table is a count followed by aligned rows of the requested
// family. Retry when connections appear between the sizing and read calls.
#[cfg(windows)]
fn tcp_table<Row: Copy>(family: u32) -> Option<Vec<Row>> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_ALL,
    };

    let mut size = 0u32;
    for _ in 0..4 {
        let words = (size as usize).div_ceil(8) + 1;
        let mut buffer = vec![0u64; words];
        let mut available = u32::try_from(words * 8).ok()?;
        // SAFETY: buffer is aligned and writable for the input byte count.
        let result = unsafe {
            GetExtendedTcpTable(
                buffer.as_mut_ptr().cast(),
                &mut available,
                0,
                family,
                TCP_TABLE_OWNER_PID_ALL,
                0,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            size = available;
            continue;
        }
        let available = available as usize;
        if result != NO_ERROR || !(4..=words * 8).contains(&available) {
            return None;
        }
        let bytes = buffer.as_ptr().cast::<u8>();
        // SAFETY: a successful call supplies the initialized count.
        let count = unsafe { bytes.cast::<u32>().read() } as usize;
        let offset = std::mem::size_of::<u32>().next_multiple_of(std::mem::align_of::<Row>());
        let end = offset.checked_add(count.checked_mul(std::mem::size_of::<Row>())?)?;
        if end > available {
            return None;
        }
        // SAFETY: callers supply the native integer-only row type for the
        // family, and every row is aligned and within the returned table.
        let rows = unsafe { std::slice::from_raw_parts(bytes.add(offset).cast::<Row>(), count) };
        return Some(rows.to_vec());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stuck_proxy_path_probe_does_not_hold_its_caller() {
        let (entered, observed) = mpsc::channel();
        let (release, blocked) = mpsc::channel::<()>();
        let (done, results) = mpsc::channel();
        let caller = std::thread::spawn(move || {
            let result =
                probe_proxy_worktrees_with(HashSet::from(["slow-path".to_owned()]), move |_| {
                    entered.send(()).unwrap();
                    let _ = blocked.recv();
                    ProxyWorktree::Outside
                });
            done.send(result).unwrap();
        });
        let guard = Duration::from_secs(5);
        observed.recv_timeout(guard).expect("probe entered");
        let result = results.recv_timeout(guard);
        drop(release);
        caller.join().unwrap();
        assert!(
            result
                .expect("caller returned while the probe was held")
                .is_empty()
        );
    }

    fn connection(pid: u32, local: &str, remote: &str) -> TcpConnection {
        TcpConnection {
            pid,
            local: local.parse().unwrap(),
            remote: remote.parse().unwrap(),
            listening: false,
        }
    }

    #[test]
    fn only_one_foreign_ssh_socket_of_the_admitted_pid_proves_elsewhere() {
        let direct = connection(7, "192.0.2.10:50000", "192.0.2.20:2222");
        let forward = connection(7, "[::ffff:127.0.0.1]:8877", "[::1]:50123");
        let other_process = connection(8, "192.0.2.10:50001", "192.0.2.20:2222");
        let extra_socket = connection(7, "192.0.2.10:50002", "192.0.2.30:2222");
        let loopback = connection(7, "[::1]:50000", "[::ffff:127.0.0.1]:2222");
        let other_port = connection(7, "192.0.2.10:50000", "192.0.2.20:8080");

        assert!(direct_foreign_peer(
            &[direct, forward, other_process],
            7,
            8877,
            2222,
            Some(direct.remote),
            |_| false
        ));
        for sockets in [
            vec![forward, other_process],
            vec![direct, extra_socket],
            vec![loopback],
            vec![other_port],
        ] {
            assert!(!direct_foreign_peer(
                &sockets,
                7,
                8877,
                2222,
                Some(direct.remote),
                |_| false
            ));
        }
        assert!(!direct_foreign_peer(
            &[direct],
            7,
            8877,
            2222,
            Some(direct.remote),
            |_| true
        ));
    }

    #[test]
    fn accepted_forwards_do_not_prove_an_outbound_peer() {
        let own_forward = connection(7, "192.0.2.10:8877", "192.0.2.20:2222");
        let configured_forward = connection(7, "192.0.2.10:8878", "192.0.2.20:2222");
        let listener = TcpConnection {
            listening: true,
            ..connection(7, "[::]:8878", "[::]:0")
        };
        for sockets in [vec![own_forward], vec![configured_forward, listener]] {
            let peer = sockets[0].remote;
            assert!(!direct_foreign_peer(
                &sockets,
                7,
                8877,
                2222,
                Some(peer),
                |_| { panic!("an accepted socket cannot reach the bind proof") }
            ));
        }
    }

    #[test]
    fn only_an_address_not_available_locally_is_affirmative_foreign_evidence() {
        use std::io::ErrorKind;
        assert!(bind_may_be_local(Ok(())));
        assert!(bind_may_be_local(Err(ErrorKind::PermissionDenied.into())));
        assert!(!bind_may_be_local(Err(ErrorKind::AddrNotAvailable.into())));
    }

    #[test]
    fn ambiguous_ipv6_scope_and_own_address_are_not_foreign_proof() {
        for row in [
            connection(7, "[fe80::1%13]:50000", "[fe80::2%13]:2222"),
            connection(7, "[fe80::1]:50000", "[fe80::2]:2222"),
            connection(7, "[2001:db8::1]:50000", "[2001:db8::2%13]:2222"),
            connection(7, "192.0.2.10:50000", "192.0.2.10:2222"),
        ] {
            assert!(!direct_foreign_peer(
                &[row],
                7,
                8877,
                2222,
                Some(row.remote),
                |_| { panic!("an ambiguous or own address cannot reach the bind proof") }
            ));
        }
    }

    #[test]
    fn a_reverse_forward_socket_cannot_supply_direct_ssh_evidence() {
        let socket = connection(7, "192.0.2.10:50000", "192.0.2.20:2222");
        let direct = b"debug1: Connecting to target.example [192.0.2.20] port 2222.\r\ndebug1: Connection established.\r\n";
        let proxied = b"debug1: Executing proxy command: exec configured-transport\r\ndebug1: Connecting to target.example [192.0.2.20] port 2222.\r\ndebug1: Connection established.\r\n";
        let direct_peer = direct_ssh_peer_from_startup(direct);
        assert_eq!(direct_peer, Some(socket.remote));
        assert!(direct_foreign_peer(
            &[socket],
            7,
            8877,
            2222,
            direct_peer,
            |_| false
        ));
        for trace in [proxied.as_slice(), b"debug1: Connection established.\n"] {
            assert!(!direct_foreign_peer(
                &[socket],
                7,
                8877,
                2222,
                direct_ssh_peer_from_startup(trace),
                |_| {
                    panic!("a socket uncorroborated by a direct SSH connection is insufficient")
                }
            ));
        }
        let different_peer = Some("192.0.2.30:2222".parse().unwrap());
        assert!(!direct_foreign_peer(
            &[socket],
            7,
            8877,
            2222,
            different_peer,
            |_| false
        ));
    }

    #[cfg(windows)]
    #[test]
    fn native_tcp_table_decodes_a_live_owned_connection() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert_native_owned_connection(listener);
    }

    #[cfg(windows)]
    #[test]
    fn native_tcp6_table_decodes_a_live_owned_connection() {
        match std::net::TcpListener::bind("[::1]:0") {
            Ok(listener) => assert_native_owned_connection(listener),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::AddrNotAvailable
                ) =>
            {
                eprintln!("IPv6 loopback unavailable: {err}");
            }
            Err(err) => panic!("bind IPv6 loopback: {err}"),
        }
    }

    #[cfg(windows)]
    fn assert_native_owned_connection(listener: std::net::TcpListener) {
        use std::net::TcpStream;

        let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_accepted, _) = listener.accept().unwrap();
        let local = stream.local_addr().unwrap();
        let peer = stream.peer_addr().unwrap();
        let rows = tcp_connections().expect("the Windows TCP tables are readable");
        assert!(
            rows.iter().any(|row| !row.listening
                && row.pid == std::process::id()
                && row.local == local
                && row.remote == peer),
            "the actual established outbound socket must decode with its owner and exact endpoints"
        );
        assert!(
            rows.iter()
                .any(|row| row.listening && row.pid == std::process::id() && row.local == peer),
            "the actual listening socket must decode with its owner and local address"
        );
        assert!(address_may_be_local(peer));
    }
}
