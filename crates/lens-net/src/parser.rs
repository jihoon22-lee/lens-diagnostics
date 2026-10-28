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
        // Format: Num RefCount Protocol Flags Type St Inode [Path]
        let mut rest = line;
        let mut fields = Vec::with_capacity(7);
        for _ in 0..7 {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
            fields.push(&rest[..end]);
            rest = &rest[end..];
        }
        if fields.len() < 7 {
            continue;
        }

        let inode = fields[6].parse::<u64>().unwrap_or(0);
        // Paths may contain multiple consecutive spaces — preserve them verbatim.
        let path_str = rest
            .trim_start_matches([' ', '\t'])
            .trim_end_matches(['\r', '\n']);
        let path = if path_str.is_empty() {
            None
        } else {
            Some(path_str.to_string())
        };
        // Type column: 0001=stream, 0002=dgram, 0005=seqpacket.
        let kind = match u32::from_str_radix(fields[4], 16).unwrap_or(0) {
            0x0001 | 0x0005 => SocketKind::UnixStream,
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
    pub enumeration_failed: bool,
    pub capability: lens_core::collection::Capability,
}

pub fn scan_process_socket_inodes(proc_dir: &Path) -> ProcessSocketScan {
    let mut scan = ProcessSocketScan {
        inode_map: HashMap::new(),
        uninspectable_uids: HashSet::new(),
        uninspectable_count: 0,
        enumeration_failed: false,
        capability: lens_core::collection::Capability::collected(),
    };
    let inode_map = &mut scan.inode_map;

    let entries = match lens_core::read_policy::read_dir(proc_dir) {
        Ok(e) => e,
        Err(error) => {
            scan.enumeration_failed = true;
            scan.capability = lens_core::collection::Capability::from_error(&error);
            if scan.capability.state == lens_core::collection::CapabilityState::Unavailable {
                scan.capability.state = lens_core::collection::CapabilityState::Failed;
            }
            return scan;
        }
    };

    let mut pids = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                if let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() {
                    pids.push((pid, entry.path()));
                }
            }
            Err(error) => {
                scan.enumeration_failed = true;
                scan.capability = lens_core::collection::Capability::from_error(&error);
                if scan.capability.state == lens_core::collection::CapabilityState::Unavailable {
                    scan.capability.state = lens_core::collection::CapabilityState::Failed;
                }
            }
        }
    }
    // Sort PIDs in ascending order so shared sockets (e.g. pre-fork workers)
    // deterministically attribute to the lowest PID (master process).
    pids.sort_by_key(|(pid, _)| *pid);

    for (pid, pid_path) in pids {
        let fd_dir = pid_path.join("fd");

        let fd_entries = match lens_core::read_policy::read_dir(&fd_dir) {
            Ok(e) => e,
            Err(error) => {
                // A process that exited during enumeration is not a permission failure.
                if error.kind() == std::io::ErrorKind::NotFound && !pid_path.exists() {
                    continue;
                }
                if scan.capability.state != lens_core::collection::CapabilityState::Failed {
                    scan.capability = lens_core::collection::Capability::from_error(&error);
                    if scan.capability.state == lens_core::collection::CapabilityState::Unavailable
                    {
                        scan.capability.state = lens_core::collection::CapabilityState::Failed;
                    }
                }
                // EACCES on /proc/<pid>/fd — the process exists but its
                // sockets cannot be attributed. Record the owner uid so
                // unowned sockets of that uid classify as owner_unknown.
                scan.uninspectable_count += 1;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if let Ok(meta) = lens_core::read_policy::metadata(&pid_path) {
                        scan.uninspectable_uids.insert(meta.uid());
                    } else {
                        scan.enumeration_failed = true;
                    }
                }
                continue;
            }
        };

        let mut socket_fds: Vec<(u32, u64)> = Vec::new();
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
                        socket_fds.push((fd_num, inode));
                    }
                }
            }
        }

        if socket_fds.is_empty() {
            continue;
        }
        socket_fds.sort_by_key(|(fd_num, _)| *fd_num);

        // Read proc name and cmdline lazily only after at least one socket fd is found.
        let comm = lens_core::read_policy::read_to_string(pid_path.join("comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = lens_core::read_policy::read_to_string(pid_path.join("cmdline"))
            .unwrap_or_default()
            .replace('\0', " ")
            .trim()
            .to_string();

        for (fd_num, inode) in socket_fds {
            inode_map.entry(inode).or_insert_with(|| SocketProcess {
                pid,
                name: comm.clone(),
                cmdline: cmdline.clone(),
                fd: fd_num,
            });
        }
    }

    scan
}

fn socket_kind_rank(kind: SocketKind) -> u8 {
    match kind {
        SocketKind::Tcp => 0,
        SocketKind::Tcp6 => 1,
        SocketKind::Udp => 2,
        SocketKind::Udp6 => 3,
        SocketKind::UnixStream => 4,
        SocketKind::UnixDgram => 5,
        SocketKind::Other => 6,
    }
}

fn sort_sockets(sockets: &mut [SocketEntry]) {
    sockets.sort_by(|a, b| {
        socket_kind_rank(a.kind)
            .cmp(&socket_kind_rank(b.kind))
            .then_with(|| a.local_address.cmp(&b.local_address))
            .then_with(|| a.local_port.cmp(&b.local_port))
            .then_with(|| a.remote_address.cmp(&b.remote_address))
            .then_with(|| a.remote_port.cmp(&b.remote_port))
            .then_with(|| a.unix_path.cmp(&b.unix_path))
            .then_with(|| a.inode.cmp(&b.inode))
    });
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
    let tcp_content =
        lens_core::read_policy::read_to_string(&tcp_path).map_err(|e| LensError::Io {
            path: tcp_path.clone(),
            source: e,
        })?;
    sockets.extend(parse_proc_net_tcp(&tcp_content, SocketKind::Tcp));
    let mut capabilities = std::collections::BTreeMap::new();
    capabilities.insert("tcp".into(), lens_core::collection::Capability::collected());
    for (name, path, kind) in [
        ("udp", udp_path, SocketKind::Udp),
        ("tcp6", tcp6_path, SocketKind::Tcp6),
        ("udp6", udp6_path, SocketKind::Udp6),
        ("unix", unix_path, SocketKind::Other),
    ] {
        match lens_core::read_policy::read_to_string(&path) {
            Ok(content) => {
                if name == "unix" {
                    sockets.extend(parse_proc_net_unix(&content));
                } else {
                    sockets.extend(parse_proc_net_tcp(&content, kind));
                }
                capabilities.insert(name.into(), lens_core::collection::Capability::collected());
            }
            Err(error) => {
                capabilities.insert(
                    name.into(),
                    lens_core::collection::Capability::from_error(&error),
                );
            }
        }
    }

    // Correlate with process inodes
    let proc_scan = scan_process_socket_inodes(base);
    for s in &mut sockets {
        if let Some(proc) = proc_scan.inode_map.get(&s.inode) {
            s.process = Some(proc.clone());
        } else if s.inode != 0
            && matches!(
                s.kind,
                SocketKind::Tcp
                    | SocketKind::Tcp6
                    | SocketKind::Udp
                    | SocketKind::Udp6
                    | SocketKind::UnixStream
                    | SocketKind::UnixDgram
            )
        {
            // Not attributable to any inspected process. If the socket's
            // uid matches a process whose fd table we could not read, the
            // owner is unknown rather than the socket being orphaned.
            s.owner_state = Some(
                if proc_scan.enumeration_failed || proc_scan.uninspectable_uids.contains(&s.uid) {
                    OwnerState::OwnerUnknown
                } else {
                    OwnerState::Orphan
                },
            );
        }
    }

    sort_sockets(&mut sockets);

    capabilities.insert("process_owners".into(), proc_scan.capability);
    let mut report = NetReport {
        scope: Some(lens_core::collection::CollectionScope {
            source: base.to_string_lossy().into_owned(),
            capabilities,
        }),
        schema_version: crate::model::REPORT_SCHEMA_V1.to_string(),
        summary: NetSummary {
            uninspectable_processes: proc_scan.uninspectable_count,
            ..Default::default()
        },
        sockets,
        listening: Vec::new(),
    };
    report.refresh_summary();
    Ok(report)
}

impl NetReport {
    /// Recompute every socket-derived count and listener from the current set.
    /// Process inspection coverage is independent of the socket filter.
    pub fn refresh_summary(&mut self) {
        let mut summary = NetSummary {
            uninspectable_processes: self.summary.uninspectable_processes,
            ..Default::default()
        };
        let mut listening = Vec::new();
        for s in &self.sockets {
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
        self.summary = summary;
        self.listening = listening;
    }
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
0000000000000000: 00000002 00000000 00010000 0005 01 654321 /tmp/path  with   multiple spaces.sock
";
        let entries = parse_proc_net_unix(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].inode, 123456);
        assert_eq!(entries[0].kind, SocketKind::UnixStream);
        assert_eq!(entries[0].unix_path, Some("/run/test.sock".to_string()));

        assert_eq!(entries[1].inode, 654321);
        assert_eq!(entries[1].kind, SocketKind::UnixStream);
        assert_eq!(
            entries[1].unix_path,
            Some("/tmp/path  with   multiple spaces.sock".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_shared_socket_attributes_to_lowest_pid_and_sorts_deterministically() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::Builder::new()
            .prefix("lensnet-n01-")
            .tempdir()
            .unwrap();
        let root = tmp.path();
        let net = root.join("net");
        std::fs::create_dir_all(&net).unwrap();

        // Put port 9090 before port 8080 in /proc/net/tcp to verify deterministic sorting.
        std::fs::write(
            net.join("tcp"),
            "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 0100007F:2382 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 77777 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 88888 1 0000000000000000 100 0 0 10 0\n",
        )
        .unwrap();
        std::fs::write(
            net.join("unix"),
            "Num       RefCount Protocol Flags    Type St Inode Path\n0000000000000000: 00000002 00000000 00010000 0005 01 99999 /run/orphan  seq.sock\n",
        )
        .unwrap();

        // Both PID 200 (worker) and PID 100 (master) hold socket:[88888].
        for (pid, name) in [(200u32, "nginx-worker"), (100u32, "nginx-master")] {
            let pdir = root.join(pid.to_string());
            let fddir = pdir.join("fd");
            std::fs::create_dir_all(&fddir).unwrap();
            std::fs::write(pdir.join("comm"), format!("{name}\n")).unwrap();
            std::fs::write(pdir.join("cmdline"), format!("{name}\0--daemon\0")).unwrap();
            symlink("socket:[88888]", fddir.join("3")).unwrap();
        }

        let report = inspect_network(Some(root)).unwrap();
        // Listening sockets must be sorted deterministically: 8080 then 9090.
        assert_eq!(report.listening.len(), 2);
        assert_eq!(report.listening[0].local_port, 8080);
        assert_eq!(report.listening[1].local_port, 9090);

        // Shared socket 88888 must attribute to lowest PID (100, nginx-master).
        let proc = report.listening[0].process.as_ref().unwrap();
        assert_eq!(proc.pid, 100);
        assert_eq!(proc.name, "nginx-master");

        // Unowned unix seqpacket socket with non-zero inode has owner_state = Orphan.
        let unix_sock = report
            .sockets
            .iter()
            .find(|s| s.inode == 99999)
            .expect("unix socket");
        assert_eq!(unix_sock.kind, SocketKind::UnixStream);
        assert_eq!(unix_sock.owner_state, Some(OwnerState::Orphan));
        assert_eq!(report.summary.unix_domain_sockets, 1);
    }
}
