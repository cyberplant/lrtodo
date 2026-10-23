use std::collections::BTreeMap;

use lrtodo_core::{
    Caps, Clock, ListDoc, ListKind, ListMeta, Op, OpKind, Rejection, Rng, Task, TaskId,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::client::{HaClient, HaError, HaItem, features as f};

/// Si un `add_item` no aparece en HA en este tiempo, se reintenta.
const ADD_TIMEOUT_MS: u64 = 120_000;

/// Estado de una lista de Home Assistant dentro del servidor de lrtodo.
///
/// `doc` es el estado deseado (lo que ven los clientes). `seen` es lo último
/// que sabemos que tiene HA. La reconciliación:
/// 1. lee HA; lo que cambió respecto de `seen` es un cambio hecho en HA
///    (app de HA, voz, automatizaciones) y se aplica a `doc` con un HLC nuevo;
/// 2. calcula la diferencia `doc` vs `seen` y la empuja con los servicios `todo.*`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HaList {
    pub entity: String,
    pub name: String,
    pub features: u32,
    pub doc: ListDoc,
    #[serde(default)]
    seen: Vec<HaItem>,
    /// Tareas creadas desde lrtodo: id propio → uid de HA.
    #[serde(default)]
    uid_of: BTreeMap<TaskId, String>,
    /// `add_item` enviados cuyo uid todavía no vimos: id → (summary, ms).
    #[serde(default)]
    adding: BTreeMap<TaskId, (String, u64)>,
    clock: Clock,
    rng: Rng,
    #[serde(default)]
    pub last_fetch_ms: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// Llamada pendiente a HA.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Add {
        task_id: TaskId,
        summary: String,
        fields: Map<String, Value>,
    },
    Update {
        uid: String,
        fields: Map<String, Value>,
        pushed: HaItem,
    },
    Remove {
        uid: String,
    },
}

impl HaList {
    pub fn new(entity: &str, name: &str, features: u32, seed: u64) -> Self {
        Self {
            entity: entity.to_string(),
            name: name.to_string(),
            features,
            doc: ListDoc::default(),
            seen: vec![],
            uid_of: BTreeMap::new(),
            adding: BTreeMap::new(),
            clock: Clock::new(format!("ha-{}", entity.trim_start_matches("todo."))),
            rng: Rng::new(seed),
            last_fetch_ms: None,
            error: None,
        }
    }

    pub fn list_id_for(entity: &str) -> String {
        format!("ha:{}", entity.trim_start_matches("todo."))
    }

    pub fn list_id(&self) -> String {
        Self::list_id_for(&self.entity)
    }

    pub fn caps(&self) -> Caps {
        Caps {
            due: self.features & (f::SET_DUE_DATE | f::SET_DUE_DATETIME) != 0,
            notes: self.features & f::SET_DESCRIPTION != 0,
            ..Caps::HOME_ASSISTANT
        }
    }

    pub fn meta(&self) -> ListMeta {
        ListMeta {
            id: self.list_id(),
            name: self.name.clone(),
            kind: ListKind::HomeAssistant,
            caps: self.caps(),
        }
    }

    /// Aplica ops de clientes al estado deseado.
    pub fn apply_ops<'a>(
        &mut self,
        ops: impl IntoIterator<Item = &'a Op>,
        acked: &mut Vec<String>,
        rejected: &mut Vec<Rejection>,
    ) -> bool {
        let caps = self.caps();
        let mut changed = false;
        for op in ops {
            self.clock.observe(&op.hlc);
            let needed = match op.kind {
                OpKind::Add { .. } => f::CREATE,
                OpKind::Set { .. } => f::UPDATE,
                OpKind::Delete { .. } => f::DELETE,
            };
            if self.features & needed == 0 {
                rejected.push(Rejection {
                    op: op.id.clone(),
                    list: op.list.clone(),
                    reason: "esta lista de Home Assistant no permite esa operación".into(),
                });
                continue;
            }
            changed |= self.doc.apply_batch([op], &caps, acked, rejected);
        }
        changed
    }

    /// Cambios aceptados que todavía no llegaron a HA.
    pub fn pending_upstream(&self) -> usize {
        self.plan_push().len() + self.adding.len()
    }

    fn uid_for(&self, task_id: &str) -> Option<String> {
        self.uid_of.get(task_id).cloned().or_else(|| {
            self.seen
                .iter()
                .any(|i| i.uid == task_id)
                .then(|| task_id.to_string())
        })
    }

    fn task_for_uid(&self, uid: &str) -> Option<TaskId> {
        self.uid_of
            .iter()
            .find(|(_, u)| u.as_str() == uid)
            .map(|(t, _)| t.clone())
    }

    fn to_task(&self, item: &HaItem, id: &str) -> Task {
        let caps = self.caps();
        Task {
            id: id.to_string(),
            title: item.summary.clone(),
            done: item.done(),
            due: item.due.clone().filter(|_| caps.due),
            notes: item
                .description
                .clone()
                .filter(|d| caps.notes && !d.is_empty()),
            ..Default::default()
        }
    }

    fn op(&mut self, now_ms: u64, kind: OpKind) -> Op {
        Op {
            id: self.rng.ulid(now_ms),
            list: self.list_id(),
            hlc: self.clock.tick(now_ms),
            kind,
        }
    }

    fn apply_local(&mut self, now_ms: u64, kind: OpKind) -> bool {
        let op = self.op(now_ms, kind);
        let caps = self.caps();
        self.doc.apply(&op, &caps) == lrtodo_core::Outcome::Applied
    }

    /// Incorpora lo que hay en HA. Devuelve true si `doc` cambió.
    pub fn ingest_remote(&mut self, items: Vec<HaItem>, now_ms: u64) -> bool {
        let prev: BTreeMap<String, HaItem> =
            self.seen.drain(..).map(|i| (i.uid.clone(), i)).collect();
        self.adding
            .retain(|_, (_, ms)| now_ms.saturating_sub(*ms) < ADD_TIMEOUT_MS);
        let mut changed = false;

        // Uids nuevos que corresponden a tareas que agregamos nosotros.
        for item in &items {
            if prev.contains_key(&item.uid) || self.task_for_uid(&item.uid).is_some() {
                continue;
            }
            let pending = self
                .adding
                .iter()
                .find(|(_, (summary, _))| *summary == item.summary)
                .map(|(t, _)| t.clone());
            let orphan = || {
                self.doc
                    .tasks
                    .iter()
                    .find(|t| {
                        t.title == item.summary
                            && !self.uid_of.contains_key(&t.id)
                            && !prev.contains_key(&t.id)
                            && !items.iter().any(|i| i.uid == t.id)
                    })
                    .map(|t| t.id.clone())
            };
            if let Some(task_id) = pending.or_else(orphan) {
                self.adding.remove(&task_id);
                self.uid_of.insert(task_id, item.uid.clone());
            }
        }

        for item in &items {
            let id = self
                .task_for_uid(&item.uid)
                .unwrap_or_else(|| item.uid.clone());
            let remote = self.to_task(item, &id);
            match prev.get(&item.uid) {
                None => {
                    if self.doc.get(&id).is_none() && !self.doc.tombstones.contains_key(&id) {
                        changed |= self.apply_local(now_ms, OpKind::Add { task: remote });
                    }
                }
                Some(before) => {
                    let before = self.to_task(before, &id);
                    for change in before.changes_to(&remote) {
                        if self.doc.get(&id).is_some_and(|t| change.differs(t)) {
                            let kind = OpKind::Set {
                                id: id.clone(),
                                change,
                            };
                            changed |= self.apply_local(now_ms, kind);
                        }
                    }
                }
            }
        }

        for uid in prev.keys().filter(|u| !items.iter().any(|i| &i.uid == *u)) {
            let id = self.task_for_uid(uid).unwrap_or_else(|| uid.clone());
            if self.doc.get(&id).is_some() {
                changed |= self.apply_local(now_ms, OpKind::Delete { id });
            }
            self.uid_of.retain(|_, u| u != uid);
        }

        // Mismo orden que en HA; lo que todavía no llegó, al final.
        let position: BTreeMap<String, usize> = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.uid.clone(), i))
            .collect();
        let keys: BTreeMap<TaskId, usize> = self
            .doc
            .tasks
            .iter()
            .map(|t| {
                let pos = self
                    .uid_of
                    .get(&t.id)
                    .and_then(|u| position.get(u))
                    .or_else(|| position.get(&t.id))
                    .copied()
                    .unwrap_or(usize::MAX);
                (t.id.clone(), pos)
            })
            .collect();
        let before: Vec<TaskId> = self.doc.tasks.iter().map(|t| t.id.clone()).collect();
        self.seen = items;
        let mut tasks = std::mem::take(&mut self.doc.tasks);
        tasks.sort_by_key(|t| keys[&t.id]);
        self.doc.tasks = tasks;
        changed |= self.doc.tasks.iter().map(|t| &t.id).ne(before.iter());
        changed
    }

    fn desired(&self, t: &Task, uid: &str) -> HaItem {
        let caps = self.caps();
        HaItem {
            uid: uid.to_string(),
            summary: t.title.clone(),
            status: if t.done { "completed" } else { "needs_action" }.into(),
            due: t.due.clone().filter(|_| caps.due),
            description: t.notes.clone().filter(|_| caps.notes),
        }
    }

    fn due_field(&self, due: Option<&str>) -> (String, Value) {
        let date_ok = self.features & f::SET_DUE_DATE != 0;
        let datetime_ok = self.features & f::SET_DUE_DATETIME != 0;
        match due {
            None if date_ok => ("due_date".into(), Value::Null),
            None => ("due_datetime".into(), Value::Null),
            Some(d) => {
                let has_time = d.len() > 10;
                if (has_time && datetime_ok) || !date_ok {
                    let v = if has_time {
                        d.to_string()
                    } else {
                        format!("{d}T00:00:00")
                    };
                    ("due_datetime".into(), json!(v))
                } else {
                    ("due_date".into(), json!(&d[..d.len().min(10)]))
                }
            }
        }
    }

    /// Diferencias entre lo deseado y lo que tiene HA.
    pub fn plan_push(&self) -> Vec<Action> {
        let caps = self.caps();
        let mut actions = vec![];
        for t in &self.doc.tasks {
            match self.uid_for(&t.id) {
                None if self.adding.contains_key(&t.id) => {}
                None => {
                    let mut fields = Map::new();
                    if let Some(due) = t.due.as_deref().filter(|_| caps.due) {
                        let (k, v) = self.due_field(Some(due));
                        fields.insert(k, v);
                    }
                    if let Some(notes) = t.notes.as_deref().filter(|_| caps.notes) {
                        fields.insert("description".into(), json!(notes));
                    }
                    actions.push(Action::Add {
                        task_id: t.id.clone(),
                        summary: t.title.clone(),
                        fields,
                    });
                }
                Some(uid) => {
                    let Some(current) = self.seen.iter().find(|i| i.uid == uid) else {
                        continue;
                    };
                    let want = self.desired(t, &uid);
                    let mut fields = Map::new();
                    if want.summary != current.summary {
                        fields.insert("rename".into(), json!(want.summary));
                    }
                    if want.status != current.status {
                        fields.insert("status".into(), json!(want.status));
                    }
                    if caps.due && want.due != current.due {
                        let (k, v) = self.due_field(want.due.as_deref());
                        fields.insert(k, v);
                    }
                    if caps.notes
                        && want.description.as_deref().unwrap_or("")
                            != current.description.as_deref().unwrap_or("")
                    {
                        fields.insert("description".into(), json!(want.description));
                    }
                    if !fields.is_empty() {
                        actions.push(Action::Update {
                            uid,
                            fields,
                            pushed: want,
                        });
                    }
                }
            }
        }
        for id in self.doc.tombstones.keys() {
            if let Some(uid) = self.uid_for(id) {
                actions.push(Action::Remove { uid });
            }
        }
        actions
    }

    /// Registra una llamada exitosa, para no confundirla con un cambio hecho en HA.
    pub fn record_pushed(&mut self, action: Action, now_ms: u64) {
        match action {
            Action::Add {
                task_id, summary, ..
            } => {
                self.adding.insert(task_id, (summary, now_ms));
            }
            Action::Update { uid, pushed, .. } => {
                if let Some(item) = self.seen.iter_mut().find(|i| i.uid == uid) {
                    *item = pushed;
                }
            }
            Action::Remove { uid } => {
                self.seen.retain(|i| i.uid != uid);
                self.uid_of.retain(|_, u| *u != uid);
            }
        }
    }

    /// Tareas tal como las tiene HA ahora (para tests y diagnóstico).
    pub fn seen(&self) -> &[HaItem] {
        &self.seen
    }

    /// Reconciliación completa contra HA. No retiene el lock mientras espera a
    /// la red, así las syncs de los clientes no se traban si HA está lento.
    pub async fn reconcile(
        state: &tokio::sync::Mutex<HaList>,
        client: &HaClient,
        now: &(dyn Fn() -> u64 + Sync),
    ) -> Result<bool, HaError> {
        let entity = state.lock().await.entity.clone();
        let mut changed = false;
        let result: Result<(), HaError> = async {
            for _ in 0..3 {
                let items = client.get_items(&entity).await?;
                let actions = {
                    let mut s = state.lock().await;
                    changed |= s.ingest_remote(items, now());
                    s.last_fetch_ms = Some(now());
                    s.plan_push()
                };
                if actions.is_empty() {
                    break;
                }
                for action in actions {
                    execute(client, &entity, &action).await?;
                    state.lock().await.record_pushed(action, now());
                }
            }
            Ok(())
        }
        .await;
        let mut s = state.lock().await;
        s.error = result.as_ref().err().map(|e| e.to_string());
        result.map(|()| changed)
    }
}

async fn execute(client: &HaClient, entity: &str, action: &Action) -> Result<(), HaError> {
    match action {
        Action::Add {
            summary, fields, ..
        } => client.add_item(entity, summary, fields.clone()).await,
        Action::Update { uid, fields, .. } => client.update_item(entity, uid, fields.clone()).await,
        Action::Remove { uid } => client.remove_item(entity, uid).await,
    }
}
