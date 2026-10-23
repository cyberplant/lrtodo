//! Réplica offline-first de un cliente (PWA, CLI remota).
//!
//! Estado = snapshot confirmado por el servidor + outbox de ops pendientes.
//! La vista que ve el usuario es el snapshot con el outbox aplicado encima,
//! así que nunca depende de la red. La sync manda el outbox y recibe los
//! snapshots nuevos; las ops confirmadas salen del outbox y las demás se
//! siguen aplicando sobre el snapshot nuevo (rebase).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::doc::ListDoc;
use crate::hlc::Clock;
use crate::id::Rng;
use crate::model::{Caps, Change, ListId, Task, TaskId};
use crate::op::{Op, OpKind};
use crate::protocol::{ListInfo, SyncRequest, SyncResponse};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CachedList {
    pub info: ListInfo,
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub synced_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub ms: u64,
    pub list: ListId,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListView {
    #[serde(flatten)]
    pub info: ListInfo,
    pub open: usize,
    pub total: usize,
    /// Ops locales que todavía no confirmó el servidor.
    pub pending: usize,
    pub synced_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    /// Tiene cambios locales sin confirmar.
    pub pending: bool,
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ReplicaError {
    #[error("no existe la lista {0}")]
    UnknownList(String),
    #[error("no existe la tarea {0}")]
    UnknownTask(String),
    #[error("la lista no soporta ese campo")]
    Unsupported,
    #[error("el título no puede estar vacío")]
    EmptyTitle,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Replica {
    pub device: String,
    clock: Clock,
    rng: Rng,
    #[serde(default)]
    pub lists: BTreeMap<ListId, CachedList>,
    #[serde(default)]
    pub outbox: Vec<Op>,
    #[serde(default)]
    pub last_sync_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub notices: Vec<Notice>,
}

impl Replica {
    pub fn new(device: impl Into<String>, seed: u64) -> Self {
        let device = device.into();
        Self {
            clock: Clock::new(device.clone()),
            rng: Rng::new(seed),
            device,
            lists: BTreeMap::new(),
            outbox: Vec::new(),
            last_sync_ms: None,
            last_error: None,
            notices: Vec::new(),
        }
    }

    /// Mezcla entropía nueva (al cargar desde disco) para no repetir ids.
    pub fn reseed(&mut self, seed: u64) {
        self.rng = Rng::new(self.rng.next_u64() ^ seed);
    }

    pub fn new_id(&mut self, now_ms: u64) -> String {
        self.rng.ulid(now_ms)
    }

    fn caps(&self, list: &str) -> Result<Caps, ReplicaError> {
        self.lists
            .get(list)
            .map(|l| l.info.meta.caps)
            .ok_or_else(|| ReplicaError::UnknownList(list.into()))
    }

    fn doc(&self, list: &str) -> Result<ListDoc, ReplicaError> {
        let cached = self
            .lists
            .get(list)
            .ok_or_else(|| ReplicaError::UnknownList(list.into()))?;
        let caps = cached.info.meta.caps;
        let mut doc = ListDoc::from_tasks(cached.tasks.clone());
        for op in self.outbox.iter().filter(|o| o.list == list) {
            doc.apply(op, &caps);
        }
        Ok(doc)
    }

    fn push(&mut self, list: &str, kind: OpKind, now_ms: u64) -> &Op {
        let op = Op {
            id: self.rng.ulid(now_ms),
            list: list.to_string(),
            hlc: self.clock.tick(now_ms),
            kind,
        };
        self.outbox.push(op);
        self.outbox.last().expect("recién agregado")
    }

    /// Agrega una tarea (sólo se usan los campos sincronizados y `extra`).
    pub fn add(&mut self, list: &str, mut task: Task, now_ms: u64) -> Result<TaskId, ReplicaError> {
        let caps = self.caps(list)?;
        task.title = task.title.trim().to_string();
        if task.title.is_empty() {
            return Err(ReplicaError::EmptyTitle);
        }
        task.strip_unsupported(&caps);
        if task.id.is_empty() {
            task.id = self.rng.ulid(now_ms);
        }
        let id = task.id.clone();
        self.push(list, OpKind::Add { task }, now_ms);
        Ok(id)
    }

    pub fn set(
        &mut self,
        list: &str,
        id: &str,
        change: Change,
        now_ms: u64,
    ) -> Result<(), ReplicaError> {
        if !self.caps(list)?.supports(change.field()) {
            return Err(ReplicaError::Unsupported);
        }
        if let Change::Title(t) = &change
            && t.trim().is_empty()
        {
            return Err(ReplicaError::EmptyTitle);
        }
        let doc = self.doc(list)?;
        let task = doc
            .get(id)
            .ok_or_else(|| ReplicaError::UnknownTask(id.into()))?;
        if change.differs(task) {
            let kind = OpKind::Set {
                id: id.into(),
                change,
            };
            self.push(list, kind, now_ms);
        }
        Ok(())
    }

    pub fn delete(&mut self, list: &str, id: &str, now_ms: u64) -> Result<(), ReplicaError> {
        if self.doc(list)?.get(id).is_none() {
            return Err(ReplicaError::UnknownTask(id.into()));
        }
        self.push(list, OpKind::Delete { id: id.into() }, now_ms);
        Ok(())
    }

    /// Mueve una tarea a otra lista (agregar con id nuevo + borrar el original).
    pub fn move_task(
        &mut self,
        from: &str,
        id: &str,
        to: &str,
        now_ms: u64,
    ) -> Result<TaskId, ReplicaError> {
        let mut task = self
            .doc(from)?
            .get(id)
            .cloned()
            .ok_or_else(|| ReplicaError::UnknownTask(id.into()))?;
        task.id = String::new();
        task.updated = None;
        task.completed = None;
        let new_id = self.add(to, task, now_ms)?;
        self.delete(from, id, now_ms)?;
        Ok(new_id)
    }

    /// Tareas de la lista tal como las ve el usuario (incluye cambios pendientes).
    pub fn view(&self, list: &str) -> Result<Vec<TaskView>, ReplicaError> {
        let touched: BTreeSet<&str> = self
            .outbox
            .iter()
            .filter(|o| o.list == list)
            .map(|o| o.kind.task_id())
            .collect();
        Ok(self
            .doc(list)?
            .tasks
            .into_iter()
            .map(|task| TaskView {
                pending: touched.contains(task.id.as_str()),
                task,
            })
            .collect())
    }

    pub fn lists(&self) -> Vec<ListView> {
        self.lists
            .iter()
            .map(|(id, cached)| {
                let tasks = self.doc(id).map(|d| d.tasks).unwrap_or_default();
                ListView {
                    info: cached.info.clone(),
                    open: tasks.iter().filter(|t| !t.done).count(),
                    total: tasks.len(),
                    pending: self.outbox.iter().filter(|o| &o.list == id).count(),
                    synced_ms: cached.synced_ms,
                }
            })
            .collect()
    }

    pub fn pending(&self) -> usize {
        self.outbox.len()
    }

    pub fn sync_request(&self) -> SyncRequest {
        SyncRequest {
            device: self.device.clone(),
            ops: self.outbox.clone(),
            known: self
                .lists
                .iter()
                .map(|(id, l)| (id.clone(), l.info.rev.clone()))
                .collect(),
            hlc: Some(self.clock.peek()),
        }
    }

    pub fn apply_response(&mut self, resp: SyncResponse, now_ms: u64) {
        self.clock.observe(&resp.hlc);
        let done: BTreeSet<&str> = resp
            .acked
            .iter()
            .map(String::as_str)
            .chain(resp.rejected.iter().map(|r| r.op.as_str()))
            .collect();
        self.outbox.retain(|o| !done.contains(o.id.as_str()));
        for r in &resp.rejected {
            self.notices.push(Notice {
                ms: now_ms,
                list: r.list.clone(),
                message: r.reason.clone(),
            });
        }

        let mut snapshots: BTreeMap<ListId, _> = resp
            .snapshots
            .into_iter()
            .map(|s| (s.list.clone(), s))
            .collect();
        let mut lists = BTreeMap::new();
        for info in resp.lists {
            let id = info.meta.id.clone();
            let previous = self.lists.remove(&id);
            let entry = match (snapshots.remove(&id), previous) {
                (Some(snap), _) => CachedList {
                    info: ListInfo {
                        rev: snap.rev,
                        ..info
                    },
                    tasks: snap.tasks,
                    synced_ms: Some(now_ms),
                },
                (None, Some(prev)) if prev.info.rev == info.rev => CachedList {
                    info,
                    tasks: prev.tasks,
                    synced_ms: Some(now_ms),
                },
                // Cambió pero no vino el snapshot: queda con la revisión vieja
                // para pedirlo en la próxima sync.
                (None, Some(prev)) => CachedList {
                    info: ListInfo {
                        rev: prev.info.rev.clone(),
                        ..info
                    },
                    ..prev
                },
                (None, None) => CachedList {
                    info: ListInfo {
                        rev: String::new(),
                        ..info
                    },
                    tasks: vec![],
                    synced_ms: None,
                },
            };
            lists.insert(id, entry);
        }
        // Listas que ya no existen en el servidor: descartar sus ops.
        for gone in self.lists.keys() {
            let dropped = self.outbox.iter().filter(|o| &o.list == gone).count();
            if dropped > 0 {
                self.notices.push(Notice {
                    ms: now_ms,
                    list: gone.clone(),
                    message: format!("la lista ya no existe; se descartaron {dropped} cambios"),
                });
            }
        }
        self.outbox.retain(|o| lists.contains_key(&o.list));
        self.lists = lists;
        self.last_sync_ms = Some(now_ms);
        self.last_error = None;
    }

    pub fn sync_failed(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    pub fn clear_notices(&mut self) {
        self.notices.clear();
    }
}
