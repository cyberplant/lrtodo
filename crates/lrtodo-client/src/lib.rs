//! El [`Backend`] compartido: modo [`Local`] (YAML directo, atómico) y modo
//! [`Remote`] (servidor + cache offline en `Replica`). Lo usan la CLI, la
//! TUI (`ChanBackend`) y el tray.

mod backend;
mod config;

pub use backend::{Backend, ListRow, Local, Remote, SyncState, hostname};
pub use config::{ClientConfig, RemoteConfig, default_path, expand};

/// "hace 3 min" en corto (para mostrar la última sync).
pub fn ago(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86400),
    }
}
