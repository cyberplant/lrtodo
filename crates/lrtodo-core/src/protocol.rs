//! Mensajes de `POST /api/sync`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;
use crate::model::{ListId, ListMeta, Task};
use crate::op::Op;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    pub device: String,
    #[serde(default)]
    pub ops: Vec<Op>,
    /// Revisión que el cliente tiene de cada lista; el servidor sólo manda
    /// snapshots de las que cambiaron.
    #[serde(default)]
    pub known: BTreeMap<ListId, String>,
    /// Último HLC del cliente (para que el reloj del servidor no quede atrás).
    #[serde(default)]
    pub hlc: Option<Hlc>,
}

/// Estado resumido de una lista; se manda siempre (es chico).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListInfo {
    pub meta: ListMeta,
    pub rev: String,
    /// Cambios aceptados por el servidor que aún no llegaron al backend
    /// remoto (p.ej. Home Assistant no responde).
    #[serde(default)]
    pub pending_upstream: usize,
    /// Último error del backend remoto, si lo hay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Último momento (ms) en que el servidor leyó el backend remoto.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListSnapshot {
    pub list: ListId,
    pub rev: String,
    pub tasks: Vec<Task>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rejection {
    pub op: String,
    pub list: ListId,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SyncResponse {
    pub lists: Vec<ListInfo>,
    #[serde(default)]
    pub snapshots: Vec<ListSnapshot>,
    /// Ops procesadas (aplicadas o ignoradas por LWW): el cliente las saca del outbox.
    #[serde(default)]
    pub acked: Vec<String>,
    /// Ops rechazadas: también salen del outbox, y se avisa al usuario.
    #[serde(default)]
    pub rejected: Vec<Rejection>,
    pub hlc: Hlc,
}
