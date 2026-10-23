use serde::{Deserialize, Serialize};

use crate::hlc::Hlc;
use crate::model::{Change, ListId, Task, TaskId};

/// Una operación: la unidad de cambio y de sincronización.
/// Es idempotente: reaplicarla no tiene efecto (LWW por HLC).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub id: String,
    pub list: ListId,
    pub hlc: Hlc,
    pub kind: OpKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpKind {
    Add { task: Task },
    Set { id: TaskId, change: Change },
    Delete { id: TaskId },
}

impl OpKind {
    pub fn task_id(&self) -> &str {
        match self {
            OpKind::Add { task } => &task.id,
            OpKind::Set { id, .. } | OpKind::Delete { id } => id,
        }
    }
}
