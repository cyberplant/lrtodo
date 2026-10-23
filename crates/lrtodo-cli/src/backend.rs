//! Dos formas de trabajar con las mismas operaciones:
//! - [`Local`]: directo sobre los YAML (mismo camino atómico que el servidor).
//! - [`Remote`]: réplica offline-first contra `lrtodo serve`, con cache en disco.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use lrtodo_core::{
    Caps, Change, Clock, ListInfo, ListKind, Op, OpKind, Replica, Rng, SyncResponse, Task, TaskId,
    TaskView,
};
use lrtodo_store::{Layout, Store, now_ms, write_atomic};

use crate::config::RemoteConfig;

/// Una fila del listado de listas.
pub struct ListRow {
    pub info: ListInfo,
    pub open: usize,
    pub total: usize,
    pub pending: usize,
}

pub trait Backend {
    fn lists(&mut self) -> anyhow::Result<Vec<ListRow>>;
    fn tasks(&mut self, list: &str) -> anyhow::Result<Vec<TaskView>>;
    fn caps(&mut self, list: &str) -> anyhow::Result<Caps>;
    fn add(&mut self, list: &str, task: Task) -> anyhow::Result<TaskId>;
    fn set(&mut self, list: &str, id: &str, change: Change) -> anyhow::Result<()>;
    fn delete(&mut self, list: &str, id: &str) -> anyhow::Result<()>;
    fn move_task(&mut self, from: &str, id: &str, to: &str) -> anyhow::Result<TaskId> {
        let mut task = self
            .tasks(from)?
            .into_iter()
            .find(|t| t.task.id == id)
            .ok_or_else(|| anyhow!("no existe la tarea {id}"))?
            .task;
        task.id = String::new();
        task.updated = None;
        task.completed = None;
        let new_id = self.add(to, task)?;
        self.delete(from, id)?;
        Ok(new_id)
    }
    fn create_list(&mut self, id: &str, name: Option<&str>, layout: Layout) -> anyhow::Result<()>;
    /// Sincroniza (remoto). Devuelve un mensaje de estado para mostrar, si hay.
    fn sync(&mut self) -> anyhow::Result<Option<String>>;
}

// ---------------------------------------------------------------- local

pub struct Local {
    pub store: Store,
    clock: Clock,
    rng: Rng,
}

impl Local {
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        let node = format!("cli-{}", hostname());
        Ok(Self {
            store: Store::open(&dir, &node).with_context(|| format!("{}", dir.display()))?,
            clock: Clock::new(node),
            rng: Rng::new(now_ms() ^ (std::process::id() as u64) << 20),
        })
    }

    fn apply(&mut self, list: &str, kind: impl FnOnce(&mut Rng) -> OpKind) -> anyhow::Result<()> {
        let clock = &mut self.clock;
        let rng = &mut self.rng;
        let (_, outcome) = self.store.mutate(list, |l| {
            if let Some(h) = l.doc.max_hlc() {
                clock.observe(h);
            }
            let now = now_ms();
            let op = Op {
                id: rng.ulid(now),
                list: list.to_string(),
                hlc: clock.tick(now),
                kind: kind(rng),
            };
            let caps = l.meta.caps;
            l.doc.apply(&op, &caps)
        })?;
        match outcome {
            lrtodo_core::Outcome::Rejected(reason) => bail!(reason),
            _ => Ok(()),
        }
    }
}

impl Backend for Local {
    fn lists(&mut self) -> anyhow::Result<Vec<ListRow>> {
        let mut rows = vec![];
        for id in self.store.list_ids()? {
            let row = match self.store.read(&id) {
                Ok(l) => ListRow {
                    open: l.doc.tasks.iter().filter(|t| !t.done).count(),
                    total: l.doc.tasks.len(),
                    pending: 0,
                    info: ListInfo {
                        rev: l.doc.rev(),
                        meta: l.meta,
                        pending_upstream: 0,
                        error: None,
                        upstream_ms: None,
                    },
                },
                Err(e) => ListRow {
                    open: 0,
                    total: 0,
                    pending: 0,
                    info: ListInfo {
                        meta: lrtodo_core::ListMeta {
                            id: id.clone(),
                            name: id.clone(),
                            kind: ListKind::Local,
                            caps: Caps::ALL,
                        },
                        rev: String::new(),
                        pending_upstream: 0,
                        error: Some(e.to_string()),
                        upstream_ms: None,
                    },
                },
            };
            rows.push(row);
        }
        Ok(rows)
    }

    fn tasks(&mut self, list: &str) -> anyhow::Result<Vec<TaskView>> {
        ensure_local(list)?;
        Ok(self
            .store
            .read(list)?
            .doc
            .tasks
            .into_iter()
            .map(|task| TaskView {
                task,
                pending: false,
            })
            .collect())
    }

    fn caps(&mut self, list: &str) -> anyhow::Result<Caps> {
        ensure_local(list)?;
        Ok(self.store.read(list)?.meta.caps)
    }

    fn add(&mut self, list: &str, mut task: Task) -> anyhow::Result<TaskId> {
        ensure_local(list)?;
        task.title = task.title.trim().to_string();
        if task.title.is_empty() {
            bail!("el título no puede estar vacío");
        }
        if task.id.is_empty() {
            task.id = self.rng.ulid(now_ms());
        }
        let id = task.id.clone();
        self.apply(list, |_| OpKind::Add { task })?;
        Ok(id)
    }

    fn set(&mut self, list: &str, id: &str, change: Change) -> anyhow::Result<()> {
        ensure_local(list)?;
        self.apply(list, |_| OpKind::Set {
            id: id.into(),
            change,
        })
    }

    fn delete(&mut self, list: &str, id: &str) -> anyhow::Result<()> {
        ensure_local(list)?;
        self.apply(list, |_| OpKind::Delete { id: id.into() })
    }

    fn create_list(&mut self, id: &str, name: Option<&str>, layout: Layout) -> anyhow::Result<()> {
        self.store.create_list(id, name, layout)?;
        Ok(())
    }

    fn sync(&mut self) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

fn ensure_local(list: &str) -> anyhow::Result<()> {
    if list.starts_with("ha:") {
        bail!(
            "las listas de Home Assistant sólo están disponibles vía servidor (configurá `server:` o --server)"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- remoto

pub struct Remote {
    cfg: RemoteConfig,
    replica: Replica,
    path: PathBuf,
    _lock: File,
    http: reqwest::Client,
    rt: tokio::runtime::Runtime,
    synced: bool,
}

impl Remote {
    pub fn open(cfg: RemoteConfig) -> anyhow::Result<Self> {
        let dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("lrtodo");
        std::fs::create_dir_all(&dir)?;
        let key: String = cfg
            .url
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let path = dir.join(format!("{key}.json"));
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{key}.lock")))?;
        lock.lock()?;
        let seed = now_ms() ^ (std::process::id() as u64) << 20;
        let replica = match std::fs::read(&path) {
            Ok(bytes) => {
                let mut r: Replica = serde_json::from_slice(&bytes)
                    .with_context(|| format!("cache {}", path.display()))?;
                r.reseed(seed);
                r
            }
            Err(_) => {
                let mut rng = Rng::new(seed);
                Replica::new(format!("cli-{}-{}", hostname(), &rng.ulid(0)[22..]), seed)
            }
        };
        Ok(Self {
            cfg,
            replica,
            path,
            _lock: lock,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(4))
                .build()?,
            rt: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
            synced: false,
        })
    }

    fn save(&self) -> anyhow::Result<()> {
        write_atomic(&self.path, &serde_json::to_vec(&self.replica)?)?;
        Ok(())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.url.trim_end_matches('/'))
    }

    fn request(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.cfg.token {
            Some(t) => req.bearer_auth(t),
            None => req,
        }
    }

    fn try_sync(&mut self) -> Result<(), String> {
        let req = self.replica.sync_request();
        let http = self.request(self.http.post(self.url("/api/sync")).json(&req));
        let result: Result<SyncResponse, String> = self.rt.block_on(async {
            let resp = http.send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!(
                    "{}: {}",
                    resp.status(),
                    resp.text().await.unwrap_or_default()
                ));
            }
            resp.json().await.map_err(|e| e.to_string())
        });
        match result {
            Ok(resp) => {
                self.replica.apply_response(resp, now_ms());
                self.synced = true;
                Ok(())
            }
            Err(e) => {
                self.replica.sync_failed(e.clone());
                Err(e)
            }
        }
    }

    /// Sincroniza una vez por comando antes de leer (si se puede).
    fn ensure_fresh(&mut self) {
        if !self.synced && self.try_sync().is_err() {
            self.synced = true; // no reintentar en el mismo comando
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.save();
    }
}

impl Backend for Remote {
    fn lists(&mut self) -> anyhow::Result<Vec<ListRow>> {
        self.ensure_fresh();
        Ok(self
            .replica
            .lists()
            .into_iter()
            .map(|v| ListRow {
                open: v.open,
                total: v.total,
                pending: v.pending,
                info: v.info,
            })
            .collect())
    }

    fn tasks(&mut self, list: &str) -> anyhow::Result<Vec<TaskView>> {
        self.ensure_fresh();
        Ok(self.replica.view(list)?)
    }

    fn caps(&mut self, list: &str) -> anyhow::Result<Caps> {
        self.ensure_fresh();
        self.replica
            .lists
            .get(list)
            .map(|l| l.info.meta.caps)
            .ok_or_else(|| anyhow!("no existe la lista {list}"))
    }

    fn add(&mut self, list: &str, task: Task) -> anyhow::Result<TaskId> {
        self.ensure_fresh();
        let id = self.replica.add(list, task, now_ms())?;
        self.save()?;
        Ok(id)
    }

    fn set(&mut self, list: &str, id: &str, change: Change) -> anyhow::Result<()> {
        self.ensure_fresh();
        self.replica.set(list, id, change, now_ms())?;
        self.save()
    }

    fn delete(&mut self, list: &str, id: &str) -> anyhow::Result<()> {
        self.ensure_fresh();
        self.replica.delete(list, id, now_ms())?;
        self.save()
    }

    fn create_list(&mut self, id: &str, name: Option<&str>, layout: Layout) -> anyhow::Result<()> {
        let body = serde_json::json!({"id": id, "name": name, "layout": layout});
        let http = self.request(self.http.post(self.url("/api/lists")).json(&body));
        self.rt.block_on(async {
            let resp = http
                .send()
                .await
                .context("crear listas requiere conexión con el servidor")?;
            if !resp.status().is_success() {
                bail!("{}", resp.text().await.unwrap_or_default());
            }
            Ok(())
        })?;
        self.synced = false;
        self.ensure_fresh();
        Ok(())
    }

    fn sync(&mut self) -> anyhow::Result<Option<String>> {
        let result = match &self.replica.last_error {
            // Ya falló en este comando: no esperar otro timeout.
            Some(e) if self.synced => Err(e.clone()),
            _ if self.replica.pending() > 0 || !self.synced => self.try_sync(),
            _ => Ok(()),
        };
        self.save()?;
        let mut msgs: Vec<String> = self
            .replica
            .notices
            .drain(..)
            .map(|n| format!("{}: {}", n.list, n.message))
            .collect();
        if let Err(e) = result {
            msgs.push(format!(
                "sin conexión con {} ({e}); {} cambio(s) pendiente(s), se enviarán en la próxima sync",
                self.cfg.url,
                self.replica.pending()
            ));
        } else if self.replica.pending() > 0 {
            msgs.push(format!("{} cambio(s) pendiente(s)", self.replica.pending()));
        }
        self.save()?;
        Ok((!msgs.is_empty()).then(|| msgs.join("\n")))
    }
}

impl Remote {
    pub fn status(&self) -> String {
        let r = &self.replica;
        let last = r
            .last_sync_ms
            .map(|ms| format!("hace {}", crate::output::ago(now_ms().saturating_sub(ms))))
            .unwrap_or_else(|| "nunca".into());
        let mut s = format!(
            "servidor: {}\ndispositivo: {}\núltima sync: {last}\ncambios pendientes: {}",
            self.cfg.url,
            r.device,
            r.pending()
        );
        if let Some(e) = &r.last_error {
            s.push_str(&format!("\núltimo error: {e}"));
        }
        s
    }
}

pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
        })
        .map(|h| h.trim().split('.').next().unwrap_or("").to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "host".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
