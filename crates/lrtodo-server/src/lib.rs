//! Servidor de lrtodo: API de sincronización (`POST /api/sync`), eventos en
//! vivo (SSE) y la PWA embebida.

mod ha_hub;
mod web;

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::Stream;
use lrtodo_core::{Clock, ListInfo, ListSnapshot, Op, Rejection, SyncRequest, SyncResponse, Task};
use lrtodo_store::{Layout, Store, StoreError, now_ms};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

pub use ha_hub::{HaConfig, HaHub};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServerConfig {
    pub data_dir: PathBuf,
    /// Dónde guardar el estado de las listas de HA (por defecto `<data_dir>/.lrtodo/ha`).
    #[serde(default)]
    pub state_dir: Option<PathBuf>,
    #[serde(default = "default_bind")]
    pub bind: String,
    /// Token requerido por la API (Bearer o `?token=`). Sin token, la API es abierta.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub ha: Option<HaConfig>,
    /// Add-on de HA: las requests que llegan por el ingress (ya autenticadas
    /// por Home Assistant) no necesitan token.
    #[serde(default)]
    pub trust_ingress: bool,
}

/// IP desde la que el Supervisor de HA hace de proxy del ingress.
const INGRESS_PROXY: std::net::IpAddr =
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(172, 30, 32, 2));

pub fn default_bind() -> String {
    "0.0.0.0:8765".into()
}

pub struct App {
    store: Arc<Store>,
    ha: Option<Arc<HaHub>>,
    events: broadcast::Sender<String>,
    token: Option<String>,
    trust_ingress: bool,
    clock: Mutex<Clock>,
}

impl App {
    pub async fn new(cfg: &ServerConfig) -> anyhow::Result<Arc<Self>> {
        let store = Arc::new(Store::open(&cfg.data_dir, "server")?);
        let (events, _) = broadcast::channel(256);
        let ha = match &cfg.ha {
            Some(ha) => {
                let dir = cfg
                    .state_dir
                    .clone()
                    .unwrap_or_else(|| cfg.data_dir.join(".lrtodo").join("ha"));
                Some(HaHub::start(ha.clone(), dir, events.clone()).await?)
            }
            None => None,
        };
        Ok(Arc::new(Self {
            store,
            ha,
            events,
            token: cfg.token.clone().filter(|t| !t.is_empty()),
            trust_ingress: cfg.trust_ingress,
            clock: Mutex::new(Clock::new("server")),
        }))
    }

    fn observe(&self, h: &lrtodo_core::Hlc) {
        self.clock.lock().expect("clock").observe(h);
    }
}

pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/sync", post(sync))
        .route("/lists", get(lists).post(create_list))
        .route("/lists/{id}", delete(delete_list))
        .route("/events", get(events))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        .route("/health", get(health));
    Router::new()
        .nest("/api", api)
        .fallback(web::static_handler)
        .with_state(app)
}

pub async fn serve(cfg: ServerConfig) -> anyhow::Result<()> {
    let app = App::new(&cfg).await?;
    let _watcher = watch(&app)?;
    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!("lrtodo escuchando en http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        router(app).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// Vigila el directorio de datos: ediciones a mano o `git pull` notifican a los clientes.
fn watch(app: &Arc<App>) -> anyhow::Result<notify::RecommendedWatcher> {
    use notify::Watcher;
    let root = app.store.root().canonicalize()?;
    let events = app.events.clone();
    let root2 = root.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        for path in ev.paths {
            let Ok(rel) = path.strip_prefix(&root2) else {
                continue;
            };
            let Some(first) = rel.components().next() else {
                continue;
            };
            let name = first.as_os_str().to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let id = name.trim_end_matches(".yaml").to_string();
            let _ = events.send(id);
        }
    })?;
    watcher.watch(&root, notify::RecursiveMode::Recursive)?;
    Ok(watcher)
}

async fn auth(
    State(app): State<Arc<App>>,
    Query(q): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    let via_ingress = app.trust_ingress
        && headers.contains_key("x-ingress-path")
        && req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .is_some_and(|c| c.0.ip() == INGRESS_PROXY);
    if let Some(token) = app.token.as_ref().filter(|_| !via_ingress) {
        let bearer = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let given = bearer.or(q.get("token").map(String::as_str));
        if !given.is_some_and(|g| constant_time_eq(g.as_bytes(), token.as_bytes())) {
            return (StatusCode::UNAUTHORIZED, "token inválido").into_response();
        }
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (StatusCode::INTERNAL_SERVER_ERROR, format!("{:#}", self.0)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        Self(e.into())
    }
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

async fn sync(
    State(app): State<Arc<App>>,
    Json(req): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    Ok(Json(handle_sync(&app, req).await?))
}

pub async fn handle_sync(app: &Arc<App>, req: SyncRequest) -> anyhow::Result<SyncResponse> {
    if let Some(h) = &req.hlc {
        app.observe(h);
    }
    let mut groups: BTreeMap<String, Vec<Op>> = BTreeMap::new();
    for op in req.ops {
        app.observe(&op.hlc);
        groups.entry(op.list.clone()).or_default().push(op);
    }
    let mut acked = vec![];
    let mut rejected = vec![];
    for (list, ops) in groups {
        let reject_all = |rejected: &mut Vec<Rejection>, reason: &str| {
            rejected.extend(ops.iter().map(|o| Rejection {
                op: o.id.clone(),
                list: list.clone(),
                reason: reason.into(),
            }))
        };
        if list.starts_with("ha:") {
            let applied = match &app.ha {
                Some(ha) => ha.apply(&list, &ops, &mut acked, &mut rejected).await,
                None => None,
            };
            match applied {
                Some(true) => {
                    let _ = app.events.send(list.clone());
                }
                Some(false) => {}
                None => reject_all(&mut rejected, "la lista de Home Assistant no existe"),
            }
            continue;
        }
        let store = app.store.clone();
        let id = list.clone();
        let ops2 = ops.clone();
        let result = tokio::task::spawn_blocking(move || {
            let (mut a, mut r) = (vec![], vec![]);
            store
                .mutate(&id, |l| {
                    let caps = l.meta.caps;
                    l.doc.apply_batch(&ops2, &caps, &mut a, &mut r)
                })
                .map(|(_, changed)| (changed, a, r))
        })
        .await?;
        match result {
            Ok((changed, a, r)) => {
                acked.extend(a);
                rejected.extend(r);
                if changed {
                    let _ = app.events.send(list.clone());
                }
            }
            Err(e @ (StoreError::NotFound(_) | StoreError::InvalidId(_))) => {
                reject_all(&mut rejected, &e.to_string())
            }
            // Error transitorio (archivo con error de sintaxis, disco): no se
            // confirman, el cliente reintenta.
            Err(e) => tracing::warn!("{list}: {e}"),
        }
    }

    let (lists, snapshots) = collect(app, &req.known).await?;
    if let Some(ha) = &app.ha {
        ha.refresh_if_stale(Duration::from_secs(15)).await;
    }
    let hlc = app.clock.lock().expect("clock").tick(now_ms());
    Ok(SyncResponse {
        lists,
        snapshots,
        acked,
        rejected,
        hlc,
    })
}

/// Info de una lista, sus tareas (si se pudieron leer) y su mayor HLC.
type Collected = (ListInfo, Option<Vec<Task>>, Option<lrtodo_core::Hlc>);

/// Estado de todas las listas + snapshots de las que el cliente no tiene al día.
async fn collect(
    app: &Arc<App>,
    known: &BTreeMap<String, String>,
) -> anyhow::Result<(Vec<ListInfo>, Vec<ListSnapshot>)> {
    let store = app.store.clone();
    let known2 = known.clone();
    let local = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Collected>> {
        let mut out = vec![];
        for id in store.list_ids()? {
            match store.read(&id) {
                Ok(loaded) => out.push((
                    ListInfo {
                        meta: loaded.meta.clone(),
                        rev: loaded.doc.rev(),
                        pending_upstream: 0,
                        error: None,
                        upstream_ms: None,
                    },
                    Some(loaded.doc.tasks.clone()),
                    loaded.doc.max_hlc().cloned(),
                )),
                // Archivo roto (p.ej. editado a mano con un error): se informa,
                // y el cliente conserva lo que tenía.
                Err(e) => out.push((
                    ListInfo {
                        meta: lrtodo_core::ListMeta {
                            id: id.clone(),
                            name: id.clone(),
                            kind: lrtodo_core::ListKind::Local,
                            caps: lrtodo_core::Caps::ALL,
                        },
                        rev: known2.get(&id).cloned().unwrap_or_default(),
                        pending_upstream: 0,
                        error: Some(e.to_string()),
                        upstream_ms: None,
                    },
                    None,
                    None,
                )),
            }
        }
        Ok(out)
    })
    .await??;
    let mut all = local;
    if let Some(ha) = &app.ha {
        all.extend(
            ha.snapshots()
                .await
                .into_iter()
                .map(|(i, t, h)| (i, Some(t), h)),
        );
    }
    let mut lists = vec![];
    let mut snapshots = vec![];
    for (info, tasks, max) in all {
        if let Some(h) = &max {
            app.observe(h);
        }
        if let Some(tasks) = tasks
            && known.get(&info.meta.id) != Some(&info.rev)
        {
            snapshots.push(ListSnapshot {
                list: info.meta.id.clone(),
                rev: info.rev.clone(),
                tasks,
            });
        }
        lists.push(info);
    }
    Ok((lists, snapshots))
}

async fn lists(State(app): State<Arc<App>>) -> Result<Json<Vec<ListInfo>>, AppError> {
    Ok(Json(collect(&app, &BTreeMap::new()).await?.0))
}

#[derive(Deserialize)]
struct CreateList {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    layout: Option<Layout>,
}

async fn create_list(State(app): State<Arc<App>>, Json(body): Json<CreateList>) -> Response {
    let store = app.store.clone();
    let result = tokio::task::spawn_blocking(move || {
        store.create_list(
            &body.id,
            body.name.as_deref(),
            body.layout.unwrap_or(Layout::File),
        )
    })
    .await;
    match result {
        Ok(Ok(meta)) => {
            let _ = app.events.send(meta.id.clone());
            (StatusCode::CREATED, Json(meta)).into_response()
        }
        Ok(Err(e @ (StoreError::Exists(_) | StoreError::InvalidId(_)))) => {
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Ok(Err(e)) => AppError(e.into()).into_response(),
        Err(e) => AppError(e.into()).into_response(),
    }
}

async fn delete_list(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    if id.starts_with("ha:") {
        return (
            StatusCode::BAD_REQUEST,
            "las listas de Home Assistant no se pueden borrar desde acá (usá `exclude` en la config)",
        )
            .into_response();
    }
    let store = app.store.clone();
    let id2 = id.clone();
    let result = tokio::task::spawn_blocking(move || store.remove_list(&id2)).await;
    match result {
        Ok(Ok(())) => {
            let _ = app.events.send(id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(e @ StoreError::NotFound(_))) => {
            (StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
        Ok(Err(e @ StoreError::InvalidId(_))) => {
            (StatusCode::BAD_REQUEST, e.to_string()).into_response()
        }
        Ok(Err(e)) => AppError(e.into()).into_response(),
        Err(e) => AppError(e.into()).into_response(),
    }
}

async fn events(State(app): State<Arc<App>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(app.events.subscribe()).map(|msg| {
        Ok(match msg {
            Ok(list) => Event::default().event("changed").data(list),
            // Se perdieron eventos: que el cliente sincronice todo.
            Err(_) => Event::default().event("changed").data("*"),
        })
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))
}

#[cfg(test)]
mod tests;
