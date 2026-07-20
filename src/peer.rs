//! Passive caller attribution: resolve the local process behind a loopback
//! connection to a toll listener, from its peer socket address.
//!
//! This is what toll *observed* about who called, as opposed to the `client`
//! column, which is what the caller *declared* via `x-toll-client` /
//! `User-Agent`. It needs no cooperation from the caller — the value most
//! callers never set — so it covers traffic that would otherwise be anonymous.
//!
//! toll's listeners bind `127.0.0.1` only (DESIGN.md invariant 9), so every
//! peer is IPv4 loopback and appears in `/proc/net/tcp`; `tcp6` is never
//! consulted. Resolution is best-effort and **expensive** (it scans every
//! process's fds), so it runs only inside the detached record-write task, off
//! the forward path (invariant 2). Any missing file, exited process, or
//! cross-user socket yields `None`, never an error.

use std::net::SocketAddr;

/// Absolute path of the executable behind `peer`, or `None` when it cannot be
/// resolved: non-Linux, the process already exited, the socket is owned by
/// another user toll cannot read, or its path is not UTF-8. Never panics.
///
/// Best-effort: resolution runs after the response, so it reads the socket's
/// owner *then*, and under PID/inode reuse can occasionally name a different
/// process. Treat the result as a hint, not proof.
///
/// Cost: one `/proc/net/tcp` read plus a scan of `/proc/<pid>/fd` across every
/// readable process — O(system-wide fds), not toll's call rate — per call.
#[cfg(target_os = "linux")]
pub fn resolve_peer_exe(peer: SocketAddr) -> Option<String> {
    // Listeners are IPv4-only, so a V6 peer cannot occur; refuse rather than
    // parse /proc/net/tcp6 for a case that never arises.
    let SocketAddr::V4(v4) = peer else {
        return None;
    };
    let inode = socket_inode(v4)?;
    let pid = pid_owning_inode(inode)?;
    std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()?
        .to_str()
        .map(str::to_owned)
}

#[cfg(not(target_os = "linux"))]
pub fn resolve_peer_exe(_peer: SocketAddr) -> Option<String> {
    None
}

/// Inode of the socket whose local endpoint is `peer`, from `/proc/net/tcp`.
/// The kernel prints the local address as `IIIIIIII:PPPP` with the IPv4 word
/// in native byte order (`%08X` of the in-memory address) and the port
/// big-endian; `u32::from_ne_bytes` reproduces that layout on any host, so we
/// format `peer` the same way and match the column verbatim.
#[cfg(target_os = "linux")]
fn socket_inode(peer: std::net::SocketAddrV4) -> Option<u64> {
    let addr = u32::from_ne_bytes(peer.ip().octets());
    let want = format!("{addr:08X}:{:04X}", peer.port());
    let tcp = std::fs::read_to_string("/proc/net/tcp").ok()?;
    for line in tcp.lines().skip(1) {
        // field 1 = local_address; field 9 = inode.
        let mut fields = line.split_whitespace();
        if fields.nth(1) == Some(want.as_str()) {
            return fields.nth(7)?.parse().ok();
        }
    }
    None
}

/// PID holding an fd to the socket with `inode`, found by scanning
/// `/proc/<pid>/fd/*` for a `socket:[inode]` symlink. Bad items — non-numeric
/// `/proc` entries, processes that exited or belong to another user — are
/// skipped, not fatal: an unprivileged toll resolves its own-user callers and
/// silently misses the rest.
#[cfg(target_os = "linux")]
fn pid_owning_inode(inode: u64) -> Option<u32> {
    let want = format!("socket:[{inode}]");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path())
                .ok()
                .as_deref()
                .and_then(|p| p.to_str())
                == Some(want.as_str())
            {
                return Some(pid);
            }
        }
    }
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    #[test]
    fn resolves_own_process_exe() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // Hold the client socket open so it stays ESTABLISHED in
        // /proc/net/tcp for the duration of resolution — no timing race.
        let _client = TcpStream::connect(addr).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        // `peer` is the client end, opened by this test process, so its exe
        // is our own binary.
        let exe = resolve_peer_exe(peer).expect("own-process peer should resolve");
        let want = std::env::current_exe().unwrap();
        assert_eq!(exe, want.to_str().unwrap());
    }

    #[test]
    fn unresolvable_peer_is_none() {
        // Loopback local port 1 has no live client socket; resolution must
        // degrade to None, not error.
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert!(resolve_peer_exe(addr).is_none());
    }
}
