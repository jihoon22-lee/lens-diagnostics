use std::collections::{HashMap, HashSet};
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use crate::model::*;
use lens_core::error::{LensError, Result};

// NOTE: /proc/net/* prints addresses as the host-endian interpretation of the
// in-memory word, so `to_ne_bytes` is intentional and correct on every host —
// the file format itself is host-endianness dependent, not fixed little-endian.
pub fn parse_ipv4_hex(hex_str: &str) -> Option<String> {
    if hex_str.len() != 8 {
        return None;
    }
    let num = u32::from_str_radix(hex_str, 16).ok()?;
    let bytes = num.to_ne_bytes();
    Some(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string())
}

pub fn parse_ipv6_hex(hex_str: &str) -> Option<String> {
    if hex_str.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for i in 0..4 {
        let chunk = &hex_str[i * 8..(i + 1) * 8];
        let num = u32::from_str_radix(chunk, 16).ok()?;
        let chunk_bytes = num.to_ne_bytes();
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&chunk_bytes);
    }
    Some(Ipv6Addr::from(bytes).to_string())
}

pub fn parse_port_hex(hex_str: &str) -> Option<u16> {
    u16::from_str_radix(hex_str, 16).ok()
}

pub fn parse_addr_port(entry: &str) -> (String, u16) {
    if let Some((addr_hex, port_hex)) = entry.split_once(':') {
        let addr = if addr_hex.len() == 32 {
            parse_ipv6_hex(addr_hex).unwrap_or_else(|| addr_hex.to_string())
        } else if addr_hex.len() == 8 {
            parse_ipv4_hex(addr_hex).unwrap_or_else(|| addr_hex.to_string())
        } else {
            addr_hex.to_string()
        };
        let port = parse_port_hex(port_hex).unwrap_or(0);
        (addr, port)
    } else {
        (entry.to_string(), 0)
    }
}

pub fn parse_proc_net_tcp(content: &str, kind: SocketKind) -> Vec<SocketEntry> {
    let mut entries = Vec::new();

    for line in content.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let (local_address, local_port) = parse_addr_port(parts[1]);
        let (remote_address, remote_port) = parse_addr_port(parts[2]);
        let state = TcpState::from_hex(parts[3]);

        let (tx_queue, rx_queue) = if let Some((tx, rx)) = parts[4].split_once(':') {
            (
                u64::from_str_radix(tx, 16).unwrap_or(0),
                u64::from_str_radix(rx, 16).unwrap_or(0),
            )
        } else {
            (0, 0)
        };

        let uid = parts[7].parse::<u32>().unwrap_or(0);
        let inode = parts[9].parse::<u64>().unwrap_or(0);

        entries.push(SocketEntry {
            kind,
            local_address,
            local_port,
            remote_address,
            remote_port,
            state,
            inode,
            uid,
            tx_queue,
            rx_queue,
            process: None,
            owner_state: None,
            unix_path: None,
        });
    }

    entries
}

pub fn parse_proc_net_unix(content: &str) -> Vec<SocketEntry> {
    let mut entries = Vec::new();

    for line in content.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Format: Num RefCount Protocol Flags Type St Inode [Path]
        if parts.len() < 7 {
            continue;
        }

        let inode = parts[6].parse::<u64>().unwrap_or(0);
        // Paths may contain whitespace — the kernel prints them verbatim.
        let path = if parts.len() >= 8 {
            Some(parts[7..].join(" "))
        } else {
            None
        };
        // Type column: 0001=stream, 0002=dgram, 0005=seqpacket.
        let kind = match u32::from_str_radix(parts[4], 16).unwrap_or(0) {
            0x0001 => SocketKind::UnixStream,
            0x0002 => SocketKind::UnixDgram,
            _ => SocketKind::Other,
        };
        // The St column's encoding differs from TCP states; leave Unknown
        // rather than mislabel unix sockets.
        let state = TcpState::Unknown;

        entries.push(SocketEntry {
            kind,
            local_address: "unix".to_string(),
            local_port: 0,
            remote_address: String::new(),
            remote_port: 0,
            state,
            inode,
            uid: 0,
            tx_queue: 0,
            rx_queue: 0,
            process: None,
            owner_state: None,
            unix_path: path,
        });
    }

    entries
}

/// inode→process map plus the set of process owner uids whose fd table
/// could not be read (permission denied). Sockets that fail correlation
/// but belong to an uninspectable uid are `owner_unknown`, not orphans.
pub struct ProcessSocketScan {
    pub inode_map: HashMap<u64, SocketProcess>,
    pub uninspectable_uids: HashSet<u32>,
    pub uninspectable_count: usize,
}

pub fn scan_process_socket_inodes(proc_dir: &Path) -> ProcessSocketScan {
    let mut scan = ProcessSocketScan {
        inode_map: HashMap::new(),
        uninspectable_uids: HashSet::new(),
        uninspectable_count: 0,
    };
    let inode_map = &mut scan.inode_map;

    let entries = match fs::read_dir(proc_dir) {
        Ok(e) => e,
        Err(_) => return scan,
    };

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        let pid = match name_str.parse::<u32>() {
            Ok(p) => p,
            Err(_) => continue,
        };

        let pid_path = entry.path();
        let fd_dir = pid_path.join("fd");

        // Read proc name and cmdline
        let comm = fs::read_to_string(pid_path.join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = fs::read_to_string(pid_path.join("cmdline"))
            .unwrap_or_default()
            .replace('\0', " ")
            .trim()
            .to_string();

        let fd_entries = match fs::read_dir(fd_dir) {
            Ok(e) => e,
            Err(_) => {
                // EACCES on /proc/<pid>/fd — the process exists but its
                // sockets cannot be attributed. Record the owner uid so
                // unowned sockets of that uid classify as owner_unknown.
                scan.uninspectable_count += 1;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = fs::metadata(&pid_path) {
                        scan.uninspectable_uids.insert(meta.uid());
                    }
                }
                continue;
            }
        };

        for fd_entry in fd_entries.flatten() {
            let fd_num = fd_entry
                .file_name()
                .to_string_lossy()
                .parse::<u32>()
                .unwrap_or(0);

            if let Ok(target) = fs::read_link(fd_entry.path()) {
                let target_str = target.to_string_lossy();
                if let Some(inode_str) = target_str
                    .strip_prefix("socket:[")
                    .and_then(|s| s.strip_suffix(']'))
                {
                    if let Ok(inode) = inode_str.parse::<u64>() {
                        inode_map.insert(
                            inode,
                            SocketProcess {
                                pid,
                                name: comm.clone(),
                                cmdline: cmdline.clone(),
                                fd: fd_num,
                            },
                        );
                    }
                }
            }
        }
    }

    scan
}

pub fn inspect_network(proc_path: Option<&Path>) -> Result<NetReport> {
    let base = proc_path.unwrap_or_else(|| Path::new("/proc"));

    let tcp_path = base.join("net/tcp");
    let udp_path = base.join("net/udp");
    let tcp6_path = base.join("net/tcp6");
    let udp6_path = base.join("net/udp6");
    let unix_path = base.join("net/unix");

    let mut sockets = Vec::new();

    // net/tcp is mandatory: a procfs that cannot provide it (bad --proc-dir,
    // /proc not mounted) must fail closed instead of reporting 0 sockets.
    let tcp_content = fs::read_to_string(&tcp_path).map_err(|e| LensError::Io {
        path: tcp_path.clone(),
        source: e,
    })?;
    sockets.extend(parse_proc_net_tcp(&tcp_content, SocketKind::Tcp));
    if let Ok(content) = fs::read_to_string(&udp_path) {
        sockets.extend(parse_proc_net_tcp(&content, SocketKind::Udp));
    }
    if let Ok(content) = fs::read_to_string(&tcp6_path) {
        sockets.extend(parse_proc_net_tcp(&content, SocketKind::Tcp6));
    }
    if let Ok(content) = fs::read_to_string(&udp6_path) {
        sockets.extend(parse_proc_net_tcp(&content, SocketKind::Udp6));
    }
    if let Ok(content) = fs::read_to_string(&unix_path) {
        sockets.extend(parse_proc_net_unix(&content));
    }

    // Correlate with process inodes
    let proc_scan = scan_process_socket_inodes(base);
    for s in &mut sockets {
        if let Some(proc) = proc_scan.inode_map.get(&s.inode) {
            s.process = Some(proc.clone());
        } else if s.inode != 0
            && matches!(
                s.kind,
                SocketKind::Tcp | SocketKind::Tcp6 | SocketKind::Udp | SocketKind::Udp6
            )
        {
            // Not attributable to any inspected process. If the socket's
            // uid matches a process whose fd table we could not read, the
            // owner is unknown rather than the socket being orphaned.
            s.owner_state = Some(if proc_scan.uninspectable_uids.contains(&s.uid) {
                OwnerState::OwnerUnknown
            } else {
                OwnerState::Orphan
            });
        }
    }

    let mut summary = NetSummary::default();
    let mut listening = Vec::new();

    for s in &sockets {
        summary.total_sockets += 1;
        // UDP has no LISTEN state: a bound UDP socket shows st=07 (CLOSE) with
        // a wildcard remote endpoint, so detect listeners by remote == *:0.
        let udp_listening = matches!(s.kind, SocketKind::Udp | SocketKind::Udp6)
            && s.remote_port == 0
            && s.local_port != 0
            && (s.remote_address.is_empty()
                || s.remote_address == "0.0.0.0"
                || s.remote_address == "::");
        match s.state {
            TcpState::Listen => {
                summary.listening_ports += 1;
                listening.push(s.clone());
            }
            _ if udp_listening => {
                summary.listening_ports += 1;
                listening.push(s.clone());
            }
            TcpState::Established => summary.established_connections += 1,
            TcpState::TimeWait => summary.time_wait_sockets += 1,
            _ => {}
        }
        if s.kind == SocketKind::UnixStream || s.kind == SocketKind::UnixDgram {
            summary.unix_domain_sockets += 1;
        }
        // inode 0 sockets are kernel-owned (e.g. TIME_WAIT) — not orphans.
        if s.process.is_none() {
            match s.owner_state {
                Some(OwnerState::Orphan) => summary.orphan_sockets += 1,
                Some(OwnerState::OwnerUnknown) => summary.owner_unknown_sockets += 1,
                None => {}
            }
        }
    }
    summary.uninspectable_processes = proc_scan.uninspectable_count;

    Ok(NetReport {
        schema_version: crate::model::REPORT_SCHEMA_V1.to_string(),
        summary,
        sockets,
        listening,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proc_net_tcp() {
        let sample = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 998877 1 0000000000000000 100 0 0 10 0
   1: 0100007F:1F90 0100007F:D001 01 00000000:00000000 00:00000000 00000000  1000        0 998878 1 0000000000000000 100 0 0 10 0
";
        let entries = parse_proc_net_tcp(sample, SocketKind::Tcp);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].local_address, "127.0.0.1");
        assert_eq!(entries[0].local_port, 8080);
        assert_eq!(entries[0].state, TcpState::Listen);
        assert_eq!(entries[0].inode, 998877);

        assert_eq!(entries[1].local_port, 8080);
        assert_eq!(entries[1].state, TcpState::Established);
        assert_eq!(entries[1].inode, 998878);
    }

    #[test]
    fn test_parse_proc_net_tcp6() {
        let sample = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000000000000:0016 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0  12345 1 0000000000000000 100 0 0 10 0
";
        let entries = parse_proc_net_tcp(sample, SocketKind::Tcp6);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, SocketKind::Tcp6);
        assert_eq!(entries[0].local_address, "::");
        assert_eq!(entries[0].local_port, 22);
        assert_eq!(entries[0].state, TcpState::Listen);
        assert_eq!(entries[0].inode, 12345);
    }

    #[test]
    fn test_udp_bound_socket_counted_as_listening() {
        // A bound UDP socket reports st=07 and a wildcard remote endpoint.
        let tmp = tempfile::Builder::new()
            .prefix("lensnet-")
            .tempdir()
            .unwrap();
        let tmp = tmp.path();
        let net = tmp.join("net");
        std::fs::create_dir_all(&net).unwrap();
        // net/tcp is mandatory input — provide a header-only table.
        std::fs::write(
            net.join("tcp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n",
        )
        .unwrap();
        std::fs::write(
            net.join("udp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000   101        0 55555 1 0000000000000000 100 0 0 10 0\n",
        )
        .unwrap();
        let report = inspect_network(Some(tmp)).unwrap();
        assert_eq!(report.summary.listening_ports, 1);
        assert_eq!(report.listening.len(), 1);
        assert_eq!(report.listening[0].local_port, 53);
    }

    #[test]
    fn test_parse_proc_net_unix() {
        let sample = "Num       RefCount Protocol Flags    Type St Inode Path
0000000000000000: 00000002 00000000 00010000 0001 01 123456 /run/test.sock
";
        let entries = parse_proc_net_unix(sample);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].inode, 123456);
        assert_eq!(entries[0].unix_path, Some("/run/test.sock".to_string()));
    }
}
