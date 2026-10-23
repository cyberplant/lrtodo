use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::format_ms;
use crate::hlc::{Clock, Hlc};
use crate::model::{Caps, Field, Task, TaskId};
use crate::op::{Op, OpKind};
use crate::protocol::Rejection;

/// Estado autoritativo de una lista: tareas + relojes por campo + lápidas.
///
/// Reglas de merge (independientes del orden de llegada):
/// - cada campo es last-writer-wins según su HLC;
/// - un borrado es definitivo: las ediciones posteriores de esa tarea se ignoran;
/// - agregar una tarea que ya existe equivale a setear todos sus campos.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListDoc {
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub clocks: BTreeMap<TaskId, BTreeMap<Field, Hlc>>,
    #[serde(default)]
    pub tombstones: BTreeMap<TaskId, Hlc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// La op perdió contra un valor más nuevo, ya estaba aplicada o la tarea fue borrada.
    Ignored,
    Rejected(String),
}

impl ListDoc {
    pub fn from_tasks(tasks: Vec<Task>) -> Self {
        Self {
            tasks,
            ..Default::default()
        }
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|t| t.id == id)
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    /// Mayor HLC presente (para inicializar relojes tras cargar de disco).
    pub fn max_hlc(&self) -> Option<&Hlc> {
        self.clocks
            .values()
            .flat_map(|m| m.values())
            .chain(self.tombstones.values())
            .max()
    }

    /// Revisión opaca del contenido visible (FNV-1a 64 sobre JSON canónico).
    pub fn rev(&self) -> String {
        rev_of(&self.tasks)
    }

    pub fn apply(&mut self, op: &Op, caps: &Caps) -> Outcome {
        match &op.kind {
            OpKind::Add { task } => self.apply_add(task, &op.hlc, caps),
            OpKind::Set { id, change } => {
                if !caps.supports(change.field()) {
                    return Outcome::Rejected(format!(
                        "la lista no soporta el campo {:?}",
                        change.field()
                    ));
                }
                if self.tombstones.contains_key(id) {
                    return Outcome::Ignored;
                }
                let Some(i) = self.index(id) else {
                    return Outcome::Rejected("la tarea no existe".into());
                };
                let field = change.field();
                let clocks = self.clocks.entry(id.clone()).or_default();
                if clocks.get(&field).is_some_and(|c| c >= &op.hlc) {
                    return Outcome::Ignored;
                }
                clocks.insert(field, op.hlc.clone());
                change.apply(&mut self.tasks[i]);
                self.refresh_derived(i);
                Outcome::Applied
            }
            OpKind::Delete { id } => {
                let tomb = self
                    .tombstones
                    .entry(id.clone())
                    .or_insert_with(|| op.hlc.clone());
                if *tomb < op.hlc {
                    *tomb = op.hlc.clone();
                }
                self.clocks.remove(id);
                match self.index(id) {
                    Some(i) => {
                        self.tasks.remove(i);
                        Outcome::Applied
                    }
                    None => Outcome::Ignored,
                }
            }
        }
    }

    fn apply_add(&mut self, task: &Task, hlc: &Hlc, caps: &Caps) -> Outcome {
        if !valid_task_id(&task.id) {
            return Outcome::Rejected(format!("id de tarea inválido: {:?}", task.id));
        }
        if self.tombstones.contains_key(&task.id) {
            return Outcome::Ignored;
        }
        let mut task = task.clone();
        task.strip_unsupported(caps);
        let i = match self.index(&task.id) {
            Some(i) => i,
            None => {
                if task.created.is_none() {
                    task.created = Some(format_ms(hlc.ms));
                }
                self.tasks.push(Task {
                    id: task.id.clone(),
                    created: task.created.clone(),
                    extra: task.extra.clone(),
                    ..Default::default()
                });
                self.tasks.len() - 1
            }
        };
        let clocks = self.clocks.entry(task.id.clone()).or_default();
        let mut applied = false;
        for change in task.all_changes() {
            let field = change.field();
            if clocks.get(&field).is_some_and(|c| c >= hlc) {
                continue;
            }
            clocks.insert(field, hlc.clone());
            change.apply(&mut self.tasks[i]);
            applied = true;
        }
        self.refresh_derived(i);
        if applied {
            Outcome::Applied
        } else {
            Outcome::Ignored
        }
    }

    /// Aplica un lote de ops (de una misma lista) y separa las procesadas de
    /// las rechazadas. Devuelve true si el contenido cambió.
    pub fn apply_batch<'a>(
        &mut self,
        ops: impl IntoIterator<Item = &'a Op>,
        caps: &Caps,
        acked: &mut Vec<String>,
        rejected: &mut Vec<Rejection>,
    ) -> bool {
        let mut changed = false;
        for op in ops {
            match self.apply(op, caps) {
                Outcome::Applied => {
                    changed = true;
                    acked.push(op.id.clone());
                }
                Outcome::Ignored => acked.push(op.id.clone()),
                Outcome::Rejected(reason) => rejected.push(Rejection {
                    op: op.id.clone(),
                    list: op.list.clone(),
                    reason,
                }),
            }
        }
        changed
    }

    /// `updated` y `completed` se derivan de los relojes, así no dependen del
    /// orden en que llegaron las ops.
    fn refresh_derived(&mut self, i: usize) {
        let task = &mut self.tasks[i];
        let Some(clocks) = self.clocks.get(&task.id) else {
            return;
        };
        if let Some(max) = clocks.values().max() {
            task.updated = Some(format_ms(max.ms));
        }
        task.completed = match (task.done, clocks.get(&Field::Done)) {
            (true, Some(c)) => Some(format_ms(c.ms)),
            (true, None) => task.completed.take(),
            (false, _) => None,
        };
    }

    /// Incorpora un estado externo (archivo editado a mano, git pull, Home
    /// Assistant): lo que difiera se considera un cambio hecho "ahora".
    /// `new_id` genera ids para tareas que no tienen.
    /// Devuelve true si algo cambió.
    pub fn ingest(
        &mut self,
        incoming: Vec<Task>,
        clock: &mut Clock,
        now_ms: u64,
        new_id: &mut dyn FnMut() -> String,
    ) -> bool {
        let mut changed = false;
        let mut seen = BTreeSet::new();
        let mut order = Vec::with_capacity(incoming.len());
        for mut task in incoming {
            if !valid_task_id(&task.id) || seen.contains(&task.id) {
                task.id = new_id();
                changed = true;
            }
            seen.insert(task.id.clone());
            order.push(task.id.clone());
            match self.index(&task.id) {
                Some(i) => {
                    let current = &self.tasks[i];
                    let changes = current.changes_to(&task);
                    if current.extra != task.extra {
                        self.tasks[i].extra = task.extra.clone();
                        changed = true;
                    }
                    if !changes.is_empty() {
                        let hlc = clock.tick(now_ms);
                        let clocks = self.clocks.entry(task.id.clone()).or_default();
                        for change in &changes {
                            clocks.insert(change.field(), hlc.clone());
                            change.apply(&mut self.tasks[i]);
                        }
                        self.refresh_derived(i);
                        changed = true;
                    }
                }
                None => {
                    // Presente en el archivo: gana aunque estuviera borrada.
                    self.tombstones.remove(&task.id);
                    let hlc = clock.tick(now_ms);
                    if task.created.is_none() {
                        task.created = Some(format_ms(now_ms));
                    }
                    let clocks: BTreeMap<Field, Hlc> = task
                        .all_changes()
                        .iter()
                        .map(|c| (c.field(), hlc.clone()))
                        .collect();
                    self.clocks.insert(task.id.clone(), clocks);
                    self.tasks.push(task);
                    let i = self.tasks.len() - 1;
                    self.refresh_derived(i);
                    changed = true;
                }
            }
        }
        let before: Vec<TaskId> = self.tasks.iter().map(|t| t.id.clone()).collect();
        changed |= before != order;
        let mut by_id: BTreeMap<TaskId, Task> =
            self.tasks.drain(..).map(|t| (t.id.clone(), t)).collect();
        // Adopta el orden del estado externo; lo que falta se considera borrado.
        self.tasks = order.iter().filter_map(|id| by_id.remove(id)).collect();
        for id in by_id.into_keys() {
            let hlc = clock.tick(now_ms);
            self.clocks.remove(&id);
            self.tombstones.insert(id, hlc);
        }
        changed
    }

    /// Borra lápidas anteriores a `before_ms`.
    pub fn prune_tombstones(&mut self, before_ms: u64) {
        self.tombstones.retain(|_, h| h.ms >= before_ms);
    }
}

/// Ids de tarea: se usan como nombre de archivo, así que se restringen.
pub fn valid_task_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with(['.', '_', '-'])
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn rev_of(tasks: &[Task]) -> String {
    let bytes = serde_json::to_vec(tasks).unwrap_or_default();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}
