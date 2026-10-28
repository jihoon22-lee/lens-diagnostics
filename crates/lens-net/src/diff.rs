use serde::{Deserialize, Serialize};

use crate::model::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetDiffReport {
    pub schema: String,
    pub new_listeners: Vec<SocketEntry>,
    pub closed_listeners: Vec<SocketEntry>,
    pub new_connections: Vec<SocketEntry>,
    pub closed_connections: Vec<SocketEntry>,
    pub time_wait_delta: i64,
}

pub fn diff_net_reports(left: &NetReport, right: &NetReport) -> NetDiffReport {
    let mut new_listeners = Vec::new();
    let mut closed_listeners = Vec::new();

    // Track listeners by (kind, address, port): a bind-address change such as
    // 127.0.0.1 -> 0.0.0.0 is surfaced as closed + new, which is safer than
    // silently treating the widened listener as unchanged.
    let listener_key = |s: &SocketEntry| (s.kind, s.local_address.clone(), s.local_port);
    let right_keys: std::collections::HashSet<_> =
        right.listening.iter().map(listener_key).collect();
    let left_keys: std::collections::HashSet<_> = left.listening.iter().map(listener_key).collect();

    for r in &right.listening {
        if !left_keys.contains(&listener_key(r)) {
            new_listeners.push(r.clone());
        }
    }

    for l in &left.listening {
        if !right_keys.contains(&listener_key(l)) {
            closed_listeners.push(l.clone());
        }
    }

    let mut new_connections = Vec::new();
    let mut closed_connections = Vec::new();

    // Match established connections by 5-tuple, not inode: socket inodes are
    // not stable across two snapshots taken at different times.
    let conn_key = |s: &SocketEntry| {
        (
            s.kind,
            s.local_address.clone(),
            s.local_port,
            s.remote_address.clone(),
            s.remote_port,
        )
    };
    let right_conns: std::collections::HashSet<_> = right
        .sockets
        .iter()
        .filter(|s| s.state == TcpState::Established)
        .map(&conn_key)
        .collect();
    let left_conns: std::collections::HashSet<_> = left
        .sockets
        .iter()
        .filter(|s| s.state == TcpState::Established)
        .map(&conn_key)
        .collect();

    for r in &right.sockets {
        if r.state == TcpState::Established && !left_conns.contains(&conn_key(r)) {
            new_connections.push(r.clone());
        }
    }

    for l in &left.sockets {
        if l.state == TcpState::Established && !right_conns.contains(&conn_key(l)) {
            closed_connections.push(l.clone());
        }
    }

    let time_wait_delta =
        right.summary.time_wait_sockets as i64 - left.summary.time_wait_sockets as i64;

    NetDiffReport {
        schema: "lens.net.diff/v1".to_string(),
        new_listeners,
        closed_listeners,
        new_connections,
        closed_connections,
        time_wait_delta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_net_reports() {
        let left = NetReport {
            scope: None,
            schema_version: "lens.net/v1".to_string(),
            summary: NetSummary {
                total_sockets: 1,
                listening_ports: 1,
                time_wait_sockets: 2,
                ..Default::default()
            },
            sockets: vec![],
            listening: vec![SocketEntry {
                kind: SocketKind::Tcp,
                local_address: "127.0.0.1".to_string(),
                local_port: 80,
                remote_address: String::new(),
                remote_port: 0,
                state: TcpState::Listen,
                inode: 1,
                uid: 0,
                tx_queue: 0,
                rx_queue: 0,
                process: None,
                unix_path: None,
                owner_state: None,
            }],
        };

        let right = NetReport {
            scope: None,
            schema_version: "lens.net/v1".to_string(),
            summary: NetSummary {
                total_sockets: 1,
                listening_ports: 1,
                time_wait_sockets: 5,
                ..Default::default()
            },
            sockets: vec![],
            listening: vec![SocketEntry {
                kind: SocketKind::Tcp,
                local_address: "127.0.0.1".to_string(),
                local_port: 443,
                remote_address: String::new(),
                remote_port: 0,
                state: TcpState::Listen,
                inode: 2,
                uid: 0,
                tx_queue: 0,
                rx_queue: 0,
                process: None,
                unix_path: None,
                owner_state: None,
            }],
        };

        let diff = diff_net_reports(&left, &right);
        assert_eq!(diff.new_listeners.len(), 1);
        assert_eq!(diff.new_listeners[0].local_port, 443);
        assert_eq!(diff.closed_listeners.len(), 1);
        assert_eq!(diff.closed_listeners[0].local_port, 80);
        assert_eq!(diff.time_wait_delta, 3);
    }

    fn listener(addr: &str, port: u16) -> SocketEntry {
        SocketEntry {
            kind: SocketKind::Tcp,
            local_address: addr.to_string(),
            local_port: port,
            remote_address: String::new(),
            remote_port: 0,
            state: TcpState::Listen,
            inode: 1,
            uid: 0,
            tx_queue: 0,
            rx_queue: 0,
            process: None,
            unix_path: None,
            owner_state: None,
        }
    }

    fn report(listening: Vec<SocketEntry>, sockets: Vec<SocketEntry>) -> NetReport {
        NetReport {
            scope: None,
            schema_version: "lens.net/v1".to_string(),
            summary: NetSummary::default(),
            sockets,
            listening,
        }
    }

    #[test]
    fn test_bind_address_expansion_detected() {
        // 127.0.0.1:80 -> 0.0.0.0:80 must surface, not be treated as unchanged.
        let left = report(vec![listener("127.0.0.1", 80)], vec![]);
        let right = report(vec![listener("0.0.0.0", 80)], vec![]);
        let diff = diff_net_reports(&left, &right);
        assert_eq!(diff.new_listeners.len(), 1);
        assert_eq!(diff.new_listeners[0].local_address, "0.0.0.0");
        assert_eq!(diff.closed_listeners.len(), 1);
        assert_eq!(diff.closed_listeners[0].local_address, "127.0.0.1");
    }

    #[test]
    fn test_same_listener_no_diff() {
        let entry = listener("127.0.0.1", 80);
        let left = report(vec![entry.clone()], vec![]);
        let right = report(vec![entry], vec![]);
        let diff = diff_net_reports(&left, &right);
        assert!(diff.new_listeners.is_empty());
        assert!(diff.closed_listeners.is_empty());
    }
}
