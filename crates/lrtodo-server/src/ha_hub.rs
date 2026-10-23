//! Listas de Home Assistant dentro del servidor: descubrimiento, workers de
//! reconciliación (uno por lista) y persistencia del estado deseado, para que
//! los clientes vean las listas aunque HA no responda.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lrtodo_core::{Hlc, ListId, ListInfo, Op, Rejection, Task};
use lrtodo_ha::{HaClient, HaList, TodoEntity};
use lrtodo_store::{now_ms, write_atomic};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, Notify, RwLock, broadcast};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HaConfig {
    /// URL de HA sin `/api` (en el add-on: `http://supervisor/core`).
    pub url: String,
    pub token: String,
    /// Entidades a incluir (vacío = todas).
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
}

fn default_poll() -> u64 {
    30
}

impl HaConfig {
    fn wants(&self, entity: &str) -> bool {
        (self.include.is_empty() || self.include.iter().any(|e| e == entity))
            && !self.exclude.iter().any(|e| e == entity)
    }
}

struct HubList {
    state: Mutex<HaList>,
    wake: Notify,
    alive: AtomicBool,
    path: PathBuf,
}

impl HubList {
    async fn persist(&self) {
        let bytes = serde_json::to_vec(&*self.state.lock().await).expect("serializable");
        if let Err(e) = write_atomic(&self.path, &bytes) {
            tracing::error!("no se pudo guardar {}: {e}", self.path.display());
        }
    }
}

pub struct HaHub {
    client: HaClient,
    cfg: HaConfig,
    dir: PathBuf,
    lists: RwLock<BTreeMap<ListId, Arc<HubList>>>,
    events: broadcast::Sender<String>,
}

impl HaHub {
    pub async fn start(
        cfg: HaConfig,
        dir: PathBuf,
        events: broadcast::Sender<String>,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&dir)?;
        let client = HaClient::new(&cfg.url, &cfg.token, Duration::from_secs(10))?;
        let hub = Arc::new(Self {
            client,
            cfg,
            dir,
            lists: RwLock::new(BTreeMap::new()),
            events,
        });
        // Estado persistido: las listas están disponibles aunque HA no responda.
        for entry in std::fs::read_dir(&hub.dir)?.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            match std::fs::read(&path).map(|b| serde_json::from_slice::<HaList>(&b)) {
                Ok(Ok(state)) if hub.cfg.wants(&state.entity) => hub.insert(state).await,
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("{}: {e}", path.display()),
                Err(e) => tracing::warn!("{}: {e}", path.display()),
            }
        }
        tokio::spawn(hub.clone().discovery_loop());
        Ok(hub)
    }

    async fn insert(self: &Arc<Self>, state: HaList) {
        let id = state.list_id();
        let path = self.dir.join(format!("{}.json", state.entity));
        let list = Arc::new(HubList {
            state: Mutex::new(state),
            wake: Notify::new(),
            alive: AtomicBool::new(true),
            path,
        });
        self.lists.write().await.insert(id.clone(), list.clone());
        tokio::spawn(self.clone().worker(id, list));
    }

    async fn discovery_loop(self: Arc<Self>) {
        let mut delay = Duration::from_secs(5);
        loop {
            match self.client.todo_entities().await {
                Ok(entities) => {
                    self.reconcile_entities(entities).await;
                    delay = Duration::from_secs(self.cfg.poll_secs.max(5) * 10);
                }
                Err(e) => {
                    tracing::warn!("Home Assistant: {e}");
                    delay = (delay * 2).min(Duration::from_secs(300));
                }
            }
            tokio::time::sleep(delay).await;
        }
    }

    async fn reconcile_entities(self: &Arc<Self>, entities: Vec<TodoEntity>) {
        let wanted: Vec<TodoEntity> = entities
            .into_iter()
            .filter(|e| self.cfg.wants(&e.entity_id))
            .collect();
        let current: Vec<(ListId, Arc<HubList>)> = self
            .lists
            .read()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (id, list) in &current {
            let entity = list.state.lock().await.entity.clone();
            match wanted.iter().find(|e| e.entity_id == entity) {
                Some(e) => {
                    let mut s = list.state.lock().await;
                    if s.name != e.name || s.features != e.features {
                        s.name = e.name.clone();
                        s.features = e.features;
                        drop(s);
                        list.persist().await;
                        let _ = self.events.send(id.clone());
                    }
                }
                None => {
                    tracing::info!("la lista {entity} ya no existe en Home Assistant");
                    list.alive.store(false, Ordering::SeqCst);
                    list.wake.notify_one();
                    self.lists.write().await.remove(id);
                    let _ = std::fs::remove_file(&list.path);
                    let _ = self.events.send(id.clone());
                }
            }
        }
        for e in wanted {
            let id = HaList::list_id_for(&e.entity_id);
            if !self.lists.read().await.contains_key(&id) {
                tracing::info!(
                    "nueva lista de Home Assistant: {} ({})",
                    e.name,
                    e.entity_id
                );
                self.insert(HaList::new(&e.entity_id, &e.name, e.features, now_ms()))
                    .await;
                let _ = self.events.send(id);
            }
        }
    }

    async fn worker(self: Arc<Self>, id: ListId, list: Arc<HubList>) {
        let poll = Duration::from_secs(self.cfg.poll_secs.max(1));
        let mut retry = Duration::from_secs(2);
        while list.alive.load(Ordering::SeqCst) {
            let result = HaList::reconcile(&list.state, &self.client, &now_ms).await;
            list.persist().await;
            let wait = match result {
                Ok(changed) => {
                    if changed {
                        let _ = self.events.send(id.clone());
                    }
                    retry = Duration::from_secs(2);
                    poll
                }
                Err(e) => {
                    tracing::warn!("{id}: {e}");
                    let _ = self.events.send(id.clone());
                    retry = (retry * 2).min(poll);
                    retry
                }
            };
            tokio::select! {
                _ = list.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// Aplica ops de clientes. `None` si la lista no existe.
    pub async fn apply(
        &self,
        id: &str,
        ops: &[Op],
        acked: &mut Vec<String>,
        rejected: &mut Vec<Rejection>,
    ) -> Option<bool> {
        let list = self.lists.read().await.get(id).cloned()?;
        let changed = list.state.lock().await.apply_ops(ops, acked, rejected);
        if changed {
            list.persist().await;
            list.wake.notify_one();
        }
        Some(changed)
    }

    /// Pide una lectura de HA si la última es vieja (no espera el resultado).
    pub async fn refresh_if_stale(&self, max_age: Duration) {
        for list in self.lists.read().await.values() {
            let last = list.state.lock().await.last_fetch_ms.unwrap_or(0);
            if now_ms().saturating_sub(last) > max_age.as_millis() as u64 {
                list.wake.notify_one();
            }
        }
    }

    pub async fn snapshots(&self) -> Vec<(ListInfo, Vec<Task>, Option<Hlc>)> {
        let mut out = vec![];
        for list in self.lists.read().await.values() {
            let s = list.state.lock().await;
            out.push((
                ListInfo {
                    meta: s.meta(),
                    rev: s.doc.rev(),
                    pending_upstream: s.pending_upstream(),
                    error: s.error.clone(),
                    upstream_ms: s.last_fetch_ms,
                },
                s.doc.tasks.clone(),
                s.doc.max_hlc().cloned(),
            ));
        }
        out
    }
}
