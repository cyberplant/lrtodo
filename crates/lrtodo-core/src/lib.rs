//! Core de lrtodo: modelo de datos, operaciones, relojes HLC, resolución de
//! conflictos (last-writer-wins por campo) y motor de sincronización
//! offline-first. No hace IO: el llamador aporta la hora, la persistencia y
//! el transporte, así el mismo código corre en la CLI, el servidor y el
//! navegador (WASM).

pub mod doc;
pub mod hlc;
pub mod id;
pub mod model;
pub mod op;
pub mod protocol;
pub mod replica;

pub use doc::{ListDoc, Outcome};
pub use hlc::{Clock, Hlc};
pub use id::Rng;
pub use model::{Caps, Change, Field, Link, LinkKind, ListId, ListKind, ListMeta, Task, TaskId};
pub use op::{Op, OpKind};
pub use protocol::{ListInfo, ListSnapshot, Rejection, SyncRequest, SyncResponse};
pub use replica::{CachedList, ListView, Notice, Replica, TaskView};

/// Formatea milisegundos Unix como RFC 3339 (UTC, segundos).
pub fn format_ms(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .unwrap_or_default()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}
