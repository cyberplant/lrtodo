use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::format_ms;
use crate::hlc::{Clock, Hlc};
use crate::model::{Caps, Field, Link, LinkKind, Task, TaskId};
use crate::op::{Op, OpKind};
use crate::protocol::Rejection;

/// Clave de una arista: (origen, destino, tipo). El destino puede ser
/// `otra-lista/id`.
pub type EdgeKey = (TaskId, String, LinkKind);

/// Estado autoritativo de una lista: tareas + relojes por campo + lápidas +
/// aristas (links entre tareas, cada una con su HLC y lápida propias).
///
/// Reglas de merge (independientes del orden de llegada):
/// - cada campo es last-writer-wins según su HLC;
/// - un borrado es definitivo: las ediciones posteriores de esa tarea se ignoran;
/// - agregar una tarea que ya existe equivale a setear todos sus campos;
/// - una arista `subtask` reemplaza cualquier padre anterior del origen.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ListDoc {
    /// Sin `links` proyectados: el estado canónico de las aristas es `edges`.
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub clocks: BTreeMap<TaskId, BTreeMap<Field, Hlc>>,
    #[serde(default)]
    pub tombstones: BTreeMap<TaskId, Hlc>,
    #[serde(default, with = "edge_serde")]
    pub edges: BTreeMap<EdgeKey, Hlc>,
    #[serde(default, with = "edge_serde")]
    pub edge_tombstones: BTreeMap<EdgeKey, Hlc>,
}

/// Las claves de arista son tuplas (JSON/YAML exigen strings): se
/// serializan como lista ordenada de pares.
mod edge_serde {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(m: &BTreeMap<EdgeKey, Hlc>, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(m.iter())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<EdgeKey, Hlc>, D::Error> {
        Ok(Vec::<(EdgeKey, Hlc)>::deserialize(d)?.into_iter().collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    /// La op perdió contra un valor más nuevo, ya estaba aplicada o la tarea fue borrada.
    Ignored,
    Rejected(String),
}

impl ListDoc {
    /// Construye un doc a partir de tareas que pueden traer `links` (p.ej.
    /// snapshots de un servidor o archivos YAML): las aristas se reconstruyen
    /// con un HLC mínimo — para vistas, donde el tiempo no importa.
    pub fn from_tasks(tasks: Vec<Task>) -> Self {
        let epoch = Hlc::new(0, 0, "");
        let mut edges = BTreeMap::new();
        let tasks: Vec<Task> = tasks
            .into_iter()
            .map(|mut t| {
                for l in std::mem::take(&mut t.links) {
                    edges.insert((t.id.clone(), l.to, l.kind), epoch.clone());
                }
                t
            })
            .collect();
        Self {
            tasks,
            edges,
            ..Default::default()
        }
    }

    /// Las tareas con `links` proyectados desde `edges` (para vistas y YAML),
    /// en orden de árbol: cada hijo va inmediatamente después de su padre.
    /// El orden entre raíces y entre hermanos es el del doc.
    pub fn tasks_with_links(&self) -> Vec<Task> {
        self.tree_order()
            .into_iter()
            .cloned()
            .map(|mut t| {
                t.links = self.links_of(&t.id);
                t
            })
            .collect()
    }

    /// Orden de árbol (DFS): raíces en orden del doc, cada una seguida de sus
    /// subtareas. Seguro ante ciclos (`seen`) y links a otras listas.
    fn tree_order(&self) -> Vec<&Task> {
        let ids: BTreeSet<&str> = self.tasks.iter().map(|t| t.id.as_str()).collect();
        let mut children: BTreeMap<&str, Vec<&Task>> = BTreeMap::new();
        let mut roots: Vec<&Task> = Vec::new();
        for t in &self.tasks {
            match self
                .parent_of(&t.id)
                .filter(|p| ids.contains(p.as_str()) && p != &t.id)
            {
                Some(p) => {
                    let p: &str = self
                        .tasks
                        .iter()
                        .find(|x| x.id == p)
                        .map(|x| x.id.as_str())
                        .unwrap();
                    children.entry(p).or_default().push(t);
                }
                None => roots.push(t),
            }
        }
        let mut out = Vec::with_capacity(self.tasks.len());
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        fn emit<'a>(
            t: &'a Task,
            children: &BTreeMap<&'a str, Vec<&'a Task>>,
            seen: &mut BTreeSet<&'a str>,
            out: &mut Vec<&'a Task>,
        ) {
            if !seen.insert(t.id.as_str()) {
                return;
            }
            out.push(t);
            if let Some(kids) = children.get(t.id.as_str()) {
                for k in kids {
                    emit(k, children, seen, out);
                }
            }
        }
        for t in roots {
            emit(t, &children, &mut seen, &mut out);
        }
        // Lo que quedó fuera (ciclo desconectado del árbol), al final.
        for t in &self.tasks {
            if seen.insert(t.id.as_str()) {
                out.push(t);
            }
        }
        out
    }

    /// Aristas que salen de `src`, ordenadas por (kind, destino).
    pub fn links_of(&self, src: &str) -> Vec<Link> {
        let mut links: Vec<Link> = self
            .edges
            .keys()
            .filter(|(s, _, _)| s == src)
            .map(|(_, dst, kind)| Link {
                to: dst.clone(),
                kind: *kind,
            })
            .collect();
        links.sort_by(|a, b| (a.kind, &a.to).cmp(&(b.kind, &b.to)));
        links
    }

    /// Padre de `id` si es subtarea (dst sin `/` se resuelve en esta lista).
    pub fn parent_of(&self, id: &str) -> Option<String> {
        self.edges
            .keys()
            .find(|(s, _, k)| s == id && *k == LinkKind::Subtask)
            .map(|(_, dst, _)| dst.clone())
    }

    /// `id` está bloqueada: algún `blocks` desde una tarea abierta de esta lista.
    pub fn blocked(&self, id: &str) -> bool {
        self.edges.iter().any(|((s, dst, k), _)| {
            *k == LinkKind::Blocks && dst == id && self.get(s).is_some_and(|t| !t.done)
        })
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
            .chain(self.edges.values())
            .chain(self.edge_tombstones.values())
            .max()
    }

    /// Revisión opaca del contenido visible (FNV-1a 64 sobre JSON canónico).
    /// Incluye las aristas (vía `links` proyectados) para que un cambio de
    /// link también dispare sync.
    pub fn rev(&self) -> String {
        rev_of(&self.tasks_with_links())
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
                self.kill_edges(id, &op.hlc);
                match self.index(id) {
                    Some(i) => {
                        self.tasks.remove(i);
                        Outcome::Applied
                    }
                    None => Outcome::Ignored,
                }
            }
            OpKind::Link { src, dst, kind } => {
                if self.tombstones.contains_key(src) {
                    return Outcome::Ignored;
                }
                if self.get(src).is_none() {
                    return Outcome::Rejected("la tarea no existe".into());
                }
                if src == dst {
                    return Outcome::Rejected("una tarea no se puede linkear a sí misma".into());
                }
                // Destino en esta misma lista (sin '/'): tiene que existir.
                if !dst.contains('/') && self.get(dst).is_none() {
                    return Outcome::Rejected("la tarea destino no existe".into());
                }
                if *kind == LinkKind::Subtask && self.reaches(src, dst) {
                    return Outcome::Rejected("crearía un ciclo de subtareas".into());
                }
                self.add_edge(src, dst, *kind, &op.hlc)
            }
            OpKind::Unlink { src, dst, kind } => self.remove_edge(src, dst, *kind, &op.hlc),
        }
    }

    /// ¿`dst` es ancestro de `src` por aristas `subtask` de esta lista?
    /// (link `src --subtask→ dst` crearía un ciclo).
    fn reaches(&self, src: &str, dst: &str) -> bool {
        let mut seen = BTreeSet::new();
        let mut cur = src.to_string();
        while let Some(parent) = self.parent_of(&cur) {
            if parent == dst {
                return true;
            }
            if !seen.insert(parent.clone()) {
                break;
            }
            cur = parent;
        }
        false
    }

    /// Inserta una arista respetando HLC, lápidas y la regla de un padre
    /// único para `subtask`.
    fn add_edge(&mut self, src: &str, dst: &str, kind: LinkKind, hlc: &Hlc) -> Outcome {
        let key: EdgeKey = (src.to_string(), dst.to_string(), kind);
        if self.edge_tombstones.get(&key).is_some_and(|t| t >= hlc)
            || self.edges.get(&key).is_some_and(|e| e >= hlc)
        {
            return Outcome::Ignored;
        }
        if kind == LinkKind::Subtask {
            // Un solo padre: reemplaza la arista subtask previa de `src`.
            let old: Vec<EdgeKey> = self
                .edges
                .keys()
                .filter(|(s, _, k)| s == src && *k == LinkKind::Subtask && *dst != key.1)
                .cloned()
                .collect();
            for old_key in old {
                self.edges.remove(&old_key);
                self.edge_tombstones.insert(old_key, hlc.clone());
            }
        }
        self.edges.insert(key, hlc.clone());
        Outcome::Applied
    }

    fn remove_edge(&mut self, src: &str, dst: &str, kind: LinkKind, hlc: &Hlc) -> Outcome {
        let key: EdgeKey = (src.to_string(), dst.to_string(), kind);
        let tomb = self
            .edge_tombstones
            .entry(key.clone())
            .or_insert_with(|| hlc.clone());
        if *tomb < *hlc {
            *tomb = hlc.clone();
        }
        if self.edges.remove(&key).is_some() {
            Outcome::Applied
        } else {
            Outcome::Ignored
        }
    }

    /// Mata las aristas que tocan a `id` (al borrar la tarea).
    fn kill_edges(&mut self, id: &str, hlc: &Hlc) {
        let dead: Vec<EdgeKey> = self
            .edges
            .keys()
            .filter(|(s, d, _)| s == id || d == id)
            .cloned()
            .collect();
        for key in dead {
            self.edges.remove(&key);
            self.edge_tombstones.insert(key, hlc.clone());
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
        let src = task.id.clone();
        for link in &task.links {
            applied |= self.add_edge(&src, &link.to, link.kind, hlc) == Outcome::Applied;
        }
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
            // Difá los links como cualquier otro campo: los que faltan se
            // agregan, los que sobran se borran (lápida).
            {
                let hlc = clock.tick(now_ms);
                let desired: BTreeSet<EdgeKey> = task
                    .links
                    .iter()
                    .map(|l| (task.id.clone(), l.to.clone(), l.kind))
                    .collect();
                let current: BTreeSet<EdgeKey> = self
                    .edges
                    .keys()
                    .filter(|(s, _, _)| s == &task.id)
                    .cloned()
                    .collect();
                for key in current.difference(&desired) {
                    changed |= self.remove_edge(&key.0, &key.1, key.2, &hlc) == Outcome::Applied;
                }
                for key in desired.difference(&current) {
                    changed |= self.add_edge(&key.0, &key.1, key.2, &hlc) == Outcome::Applied;
                }
            }
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
            self.kill_edges(&id, &hlc);
            self.tombstones.insert(id, hlc);
        }
        changed
    }

    /// Borra lápidas anteriores a `before_ms`.
    pub fn prune_tombstones(&mut self, before_ms: u64) {
        self.tombstones.retain(|_, h| h.ms >= before_ms);
        self.edge_tombstones.retain(|_, h| h.ms >= before_ms);
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
