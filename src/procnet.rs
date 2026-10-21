//! Read-only `/proc` parsing: per-protocol socket tables and fd→inode
//! ownership. Everything here is a pure function over bytes or over a
//! caller-supplied root so it can be tested against fixture trees.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;

use crate::error::{FlowError, Result};

/// TCP/UDP state "ESTABLISHED" as printed in /proc/net/tcp{,6} and as the
/// state a *connected* UDP socket shows in /proc/net/udp{,6}.
const STATE_ESTABLISHED: &str = "01";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SockRow {
    pub local: SocketAddr,
    pub state: String,
    pub inode: u32,
}

/// Parse the contents of a `/proc/net/tcp`-shaped table.
pub fn parse_tcp_table(text: &str) -> Result<Vec<SockRow>> {
    let mut rows = Vec::new();
    for line in text.lines().skip(1) {
        // Example v4 line:
        // "  0: 0100007F:B8DA 0100007F:1ED3 01 00000000:00000000 ..."
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 {
            continue;
        }
        let Some(local) = parse_hex_socket(cols[1]) else {
            continue;
        };
        let Ok(inode) = cols[9].parse::<u32>() else {
            continue;
        };
        rows.push(SockRow {
            local,
            state: cols[3].to_string(),
            inode,
        });
    }
    Ok(rows)
}

/// Decode "A1B2C3D4:PORT" into a socket address.
///
/// The kernel prints addresses with each 32-bit word in host byte order
/// (%pI4h-style): use `to_ne_bytes`, rather than assuming a little-endian
/// machine. For IPv6 each of the four native-order words is concatenated.
/// Ground truth: 127.0.0.1 → "0100007F", ::1 →
/// "00000000000000000000000001000000". IPv4-mapped IPv6 addresses are
/// canonicalized to plain IPv4 so both join sides compare equal (F04).
pub fn parse_hex_socket(field: &str) -> Option<SocketAddr> {
    let (addr_hex, port_hex) = field.split_once(':')?;
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    let ip = match addr_hex.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(addr_hex, 16).ok()?.to_ne_bytes(),
        )),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let chunk = addr_hex.get(i * 8..i * 8 + 8)?;
                let word = u32::from_str_radix(chunk, 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_ne_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes)).to_canonical()
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// Proc-side facts the aggregator joins against. Local endpoints map to a
/// *list* of socket inodes: several sockets can share one local endpoint
/// (same address+port, different remotes, TCP and UDP alike), and hiding
/// that ambiguity behind a single value mis-attributes processes (F02).
#[derive(Debug, Clone, Default)]
pub struct ProcSnapshot {
    /// established TCP local socket → socket inodes
    pub tcp_local: HashMap<SocketAddr, Vec<u32>>,
    /// connected-UDP local socket → socket inodes
    pub udp_local: HashMap<SocketAddr, Vec<u32>>,
    /// socket inode → owning pid + comm
    /// An inode may be visible through more than one process's fd table.
    /// Keep every owner so attribution can reject that ambiguity.
    pub inode_owners: HashMap<u32, Vec<ProcInfo>>,
    /// A production scan failure is distinct from an honestly empty table.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub comm: String,
    /// Linux `/proc/<pid>/stat` start-time ticks.  This distinguishes a
    /// recycled pid from the process that owned a cached socket earlier.
    pub start_time: Option<u64>,
}

/// Scan `/proc/net/{tcp,udp}{,6}` for established/connected local sockets.
/// `net/tcp` must be readable — a missing TCP table means we cannot do our
/// job and the caller must surface that instead of presenting emptiness as
/// fact (F11). Missing UDP tables degrade to empty.
pub fn scan_local_tables(proc_root: &Path) -> Result<ProcSnapshot> {
    let mut snap = ProcSnapshot::default();
    for (name, is_tcp) in [
        ("net/tcp", true),
        ("net/tcp6", true),
        ("net/udp", false),
        ("net/udp6", false),
    ] {
        let path = proc_root.join(name);
        let Ok(text) = fs::read_to_string(&path) else {
            if name == "net/tcp" {
                return Err(FlowError::ProcParse(format!(
                    "cannot read {} — /proc not usable in this root",
                    path.display()
                )));
            }
            continue;
        };
        let target = if is_tcp {
            &mut snap.tcp_local
        } else {
            &mut snap.udp_local
        };
        for row in parse_tcp_table(&text)? {
            if row.state == STATE_ESTABLISHED {
                target.entry(row.local).or_default().push(row.inode);
            }
        }
    }
    Ok(snap)
}

/// Map socket inodes to owning pids by reading `/proc/<pid>/fd/*` links.
///
/// Only same-uid, dumpable processes appear; entries for other uids or
/// non-dumpable same-uid processes are skipped silently — the aggregation
/// layer renders that as "owner-unreadable-or-gone", which does not claim
/// to know which of the two it was (F11).
pub fn inode_owners(proc_root: &Path) -> HashMap<u32, Vec<ProcInfo>> {
    let mut out: HashMap<u32, Vec<ProcInfo>> = HashMap::new();
    let Ok(pids) = fs::read_dir(proc_root) else {
        return out;
    };
    for entry in pids.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let fd_dir = proc_root.join(pid.to_string()).join("fd");
        let Ok(links) = fs::read_dir(&fd_dir) else {
            continue; // gone, another uid, or not dumpable
        };
        for link in links.flatten() {
            let Ok(target) = fs::read_link(link.path()) else {
                continue;
            };
            if let Some(inode) = parse_socket_link(&target.to_string_lossy()) {
                let owners = out.entry(inode).or_default();
                // A process can expose duplicate fd links to one socket; it
                // is still one owner. Distinct pids remain separate facts.
                if !owners.iter().any(|owner| owner.pid == pid) {
                    owners.push(ProcInfo {
                        pid,
                        comm: read_comm(proc_root, pid),
                        start_time: read_start_time(proc_root, pid),
                    });
                }
            }
        }
    }
    out
}

/// `"socket:[12345]"` → `Some(12345)`
pub fn parse_socket_link(link: &str) -> Option<u32> {
    let inner = link
        .strip_prefix("socket:[")
        .and_then(|s| s.strip_suffix(']'))?;
    inner.parse().ok()
}

pub fn read_comm(proc_root: &Path, pid: u32) -> String {
    fs::read_to_string(proc_root.join(pid.to_string()).join("comm"))
        .map(|s| s.trim_end_matches('\0').trim().to_string())
        .unwrap_or_else(|_| "?".to_string())
}

/// Read the parent pid from `/proc/<pid>/stat` (field 4).
///
/// The comm field is parenthesised and may contain spaces and parens, so we
/// split after the LAST ')' rather than naively tokenising the line.
pub fn read_ppid(proc_root: &Path, pid: u32) -> Option<u32> {
    let text = fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    let after = text.rsplit(')').next()?;
    // after == " S 1 2 3 ..." — state is token 0, ppid is token 1
    after.split_whitespace().nth(1)?.parse().ok()
}

pub fn proc_exists(proc_root: &Path, pid: u32) -> bool {
    proc_root.join(pid.to_string()).join("stat").exists()
}

/// Read field 22 (`starttime`) from `/proc/<pid>/stat`.
///
/// As with `read_ppid`, the executable name can contain spaces or `)`, so
/// fields are counted only after the final closing parenthesis.  The first
/// token there is field 3 (`state`), making starttime token 19.
pub fn read_start_time(proc_root: &Path, pid: u32) -> Option<u64> {
    let text = fs::read_to_string(proc_root.join(pid.to_string()).join("stat")).ok()?;
    text.rsplit(')')
        .next()?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// Convenience wrapper used by the real (non-test) path: tables plus owners.
pub fn snapshot(proc_root: &Path) -> Result<ProcSnapshot> {
    let mut snap = scan_local_tables(proc_root)?;
    snap.inode_owners = inode_owners(proc_root);
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP_FIXTURE: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n \
         0: 0100007F:B8DA 69A17E58:01BB 01 00000000:00000000 00:00000000 00000000  1000      0 1234567 1 0000000000000000 100 0 0 10 0\n \
         1: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0      0 7654321 1 0000000000000000 100 0 0 10 0\n \
         2: 0100007F:0050 0100007F:1ED3 01 00000000:00000000 00:00000000 00000000  1000      0 9999999 1 0000000000000000 20 0 0 10 0\n \
         3: 0100007F:0050 0100007F:1ED4 01 00000000:00000000 00:00000000 00000000  1000      0 8888888 1 0000000000000000 20 0 0 10 0\n";

    #[test]
    fn parses_v4_lines_and_states() {
        let rows = parse_tcp_table(TCP_FIXTURE).unwrap();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].local.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(rows[0].local.port(), 0xB8DA);
        assert_eq!(rows[0].state, "01");
        assert_eq!(rows[0].inode, 1234567);
        assert_eq!(rows[1].state, "0A"); // LISTEN kept, filtered later
    }

    #[test]
    fn same_local_endpoint_keeps_all_inodes() {
        // F02: two established sockets share 127.0.0.1:80 — both must survive
        let m = collect_established(TCP_FIXTURE);
        assert_eq!(
            m.get(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0x50)),
            Some(&vec![9_999_999, 8_888_888])
        );
    }

    fn collect_established(text: &str) -> HashMap<SocketAddr, Vec<u32>> {
        parse_tcp_table(text)
            .unwrap()
            .into_iter()
            .filter(|r| r.state == STATE_ESTABLISHED)
            .fold(HashMap::new(), |mut acc, r| {
                acc.entry(r.local).or_default().push(r.inode);
                acc
            })
    }

    #[test]
    fn hex_socket_decodes_v4_v6_and_mapped() {
        assert_eq!(
            parse_hex_socket("0100007F:B8DA").unwrap().ip(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        );
        assert_eq!(
            parse_hex_socket("00000000000000000000000001000000:0035")
                .unwrap()
                .ip(),
            IpAddr::V6(Ipv6Addr::LOCALHOST)
        );
        // ::ffff:127.0.0.1 canonicalizes to 127.0.0.1 (F04)
        assert_eq!(
            parse_hex_socket("0000000000000000FFFF00000100007F:B78E")
                .unwrap()
                .ip(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))
        );
        assert!(parse_hex_socket("nonsense").is_none());
        assert!(parse_hex_socket("1234:56").is_none()); // too short for v4/v6
    }

    #[test]
    fn socket_link_parsing() {
        assert_eq!(parse_socket_link("socket:[42]"), Some(42));
        assert_eq!(parse_socket_link("pipe:[42]"), None);
        assert_eq!(parse_socket_link("socket:[x]"), None);
    }

    #[test]
    fn fd_scan_and_ppid_over_fixture_tree() {
        let dir = std::env::temp_dir().join(format!("mihomo-top-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mk = |pid: u32| dir.join(pid.to_string());
        std::fs::create_dir_all(mk(100).join("fd")).unwrap();
        std::fs::create_dir_all(mk(200).join("fd")).unwrap();
        std::os::unix::fs::symlink("socket:[77]", mk(100).join("fd").join("3")).unwrap();
        std::os::unix::fs::symlink("socket:[bad", mk(200).join("fd").join("4")).unwrap();
        std::fs::write(mk(100).join("comm"), "curl\0").unwrap();
        std::fs::write(mk(100).join("stat"), "100 (cu rl) S 1 100 100 0 -1 0\n").unwrap();

        let owners = inode_owners(&dir);
        let owner = &owners.get(&77).unwrap()[0];
        assert_eq!(owner.pid, 100);
        assert_eq!(owner.comm, "curl");

        assert_eq!(read_ppid(&dir, 100), Some(1));
        assert_eq!(read_ppid(&dir, 999), None);
        assert!(proc_exists(&dir, 100));
        assert!(!proc_exists(&dir, 999));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inode_owners_dedups_fds_per_pid_but_keeps_distinct_pids() {
        let dir = std::env::temp_dir().join(format!("mihomo-top-owners-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for pid in [101u32, 202] {
            std::fs::create_dir_all(dir.join(pid.to_string()).join("fd")).unwrap();
        }
        std::os::unix::fs::symlink("socket:[88]", dir.join("101/fd/3")).unwrap();
        std::os::unix::fs::symlink("socket:[88]", dir.join("101/fd/4")).unwrap();
        std::os::unix::fs::symlink("socket:[88]", dir.join("202/fd/3")).unwrap();
        let owners = inode_owners(&dir);
        let owners = owners.get(&88).unwrap();
        assert_eq!(owners.len(), 2, "two fds from pid 101 are one owner");
        assert!(owners.iter().any(|owner| owner.pid == 101));
        assert!(owners.iter().any(|owner| owner.pid == 202));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tcp_and_udp_tables_from_fixture_root() {
        let dir = std::env::temp_dir().join(format!("mihomo-top-tcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("net")).unwrap();
        std::fs::write(dir.join("net/tcp"), TCP_FIXTURE).unwrap();
        std::fs::write(dir.join("net/udp"), TCP_FIXTURE).unwrap();
        let snap = scan_local_tables(&dir).unwrap();
        assert_eq!(snap.tcp_local.len(), 2);
        assert_eq!(snap.udp_local.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_tcp_table_is_an_error_not_empty_fact() {
        // F11: an unreadable /proc root must surface, not silently "no sockets"
        let dir = std::env::temp_dir().join(format!("mihomo-top-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap(); // no net/ at all
        assert!(scan_local_tables(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_udp_tables_degrade_to_empty() {
        let dir = std::env::temp_dir().join(format!("mihomo-top-noudp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("net")).unwrap();
        std::fs::write(dir.join("net/tcp"), TCP_FIXTURE).unwrap();
        let snap = scan_local_tables(&dir).unwrap();
        assert!(!snap.tcp_local.is_empty());
        assert!(snap.udp_local.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
