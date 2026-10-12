pub mod diff;
pub mod model;
pub mod parser;

pub use diff::{diff_net_reports, NetDiffReport};
pub use model::{
    NetReport, NetSummary, OwnerState, SocketEntry, SocketKind, SocketProcess, TcpState,
    REPORT_SCHEMA_V1,
};
pub use parser::inspect_network;
