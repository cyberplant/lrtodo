//! El backend vive en un hilo propio: la UI nunca se bloquea esperando la red
//! o el disco. Las lecturas/escrituras van por [`ChanBackend`], que implementa
//! [`Backend`] traduciendo cada llamada a un mensaje (así se puede reutilizar
//! `editor::edit`). La sync es asíncrona: la UI la pide con [`Req::Poll`] y el
//! estado llega por [`Msg::State`].

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::anyhow;
use lrtodo_core::{Caps, Change, Task, TaskId, TaskView};
use lrtodo_store::Layout;

use crate::backend::{Backend, ListRow, SyncState};

/// Cada cuánto se sincroniza en segundo plano.
const POLL_OK: Duration = Duration::from_secs(15);
/// Con error de conexión se espera más (backoff).
const POLL_ERR: Duration = Duration::from_secs(60);
/// Tras una mutación local se espera este poquito por si vienen más (debounce).
const POLL_AFTER_EDIT: Duration = Duration::from_millis(800);

pub enum Call {
    Lists,
    Tasks(String),
    Caps(String),
    Add {
        list: String,
        task: Task,
    },
    Set {
        list: String,
        id: String,
        changes: Vec<Change>,
    },
    Delete {
        list: String,
        id: String,
    },
    Move {
        from: String,
        id: String,
        to: String,
    },
    CreateList {
        id: String,
        name: Option<String>,
    },
    DeleteList {
        id: String,
    },
}

pub enum CallOut {
    Lists(Vec<ListRow>),
    Tasks(Vec<TaskView>),
    Caps(Caps),
    Id(TaskId),
    Unit,
}

pub enum Req {
    Call(Box<Call>),
    /// Sincronizar ahora (evento SSE, `r` manual o pull periódico adelantado).
    Poll,
}

/// Mensajes del worker y de los watchers hacia la UI.
pub enum Msg {
    /// Estado de sync nuevo (tras un poll o una mutación).
    State(SyncState),
    /// Aviso puntual (rechazos del servidor, errores).
    Flash(String),
    /// El SSE avisó que algo cambió en el servidor.
    Poll,
    /// Un archivo cambió en disco (modo local): releer.
    Refresh,
}

pub struct Channels {
    pub req: Sender<Req>,
    pub out: Receiver<anyhow::Result<CallOut>>,
}

/// Arranca el hilo dueño del backend. Devuelve los canales para hablarle.
pub fn start(mut backend: Box<dyn Backend + Send>, ui: Sender<Msg>) -> Channels {
    let (req_tx, req_rx) = channel();
    let (out_tx, out_rx) = channel();
    thread::spawn(move || run(&mut *backend, req_rx, out_tx, ui));
    Channels {
        req: req_tx,
        out: out_rx,
    }
}

fn run(
    backend: &mut dyn Backend,
    req: Receiver<Req>,
    out: Sender<anyhow::Result<CallOut>>,
    ui: Sender<Msg>,
) {
    let mut poll_at = Instant::now();
    loop {
        let timeout = poll_at
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(50));
        match req.recv_timeout(timeout) {
            Ok(Req::Call(call)) => {
                let mutating = matches!(
                    *call,
                    Call::Add { .. }
                        | Call::Set { .. }
                        | Call::Delete { .. }
                        | Call::Move { .. }
                        | Call::CreateList { .. }
                        | Call::DeleteList { .. }
                );
                if out.send(dispatch(backend, *call)).is_err() {
                    return;
                }
                if mutating {
                    // Que los cambios salgan pronto, pero agrupando ráfagas.
                    poll_at = Instant::now() + POLL_AFTER_EDIT;
                    let _ = ui.send(Msg::State(backend.sync_state()));
                }
            }
            Ok(Req::Poll) => poll_at = poll(backend, &ui),
            Err(RecvTimeoutError::Timeout) if Instant::now() >= poll_at => {
                poll_at = poll(backend, &ui);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn dispatch(b: &mut dyn Backend, call: Call) -> anyhow::Result<CallOut> {
    Ok(match call {
        Call::Lists => CallOut::Lists(b.lists()?),
        Call::Tasks(list) => CallOut::Tasks(b.tasks(&list)?),
        Call::Caps(list) => CallOut::Caps(b.caps(&list)?),
        Call::Add { list, task } => CallOut::Id(b.add(&list, task)?),
        Call::Set { list, id, changes } => {
            for change in changes {
                b.set(&list, &id, change)?;
            }
            CallOut::Unit
        }
        Call::Delete { list, id } => {
            b.delete(&list, &id)?;
            CallOut::Unit
        }
        Call::Move { from, id, to } => CallOut::Id(b.move_task(&from, &id, &to)?),
        Call::CreateList { id, name } => {
            b.create_list(&id, name.as_deref(), Layout::File)?;
            CallOut::Unit
        }
        Call::DeleteList { id } => {
            b.delete_list(&id)?;
            CallOut::Unit
        }
    })
}

fn poll(backend: &mut dyn Backend, ui: &Sender<Msg>) -> Instant {
    match backend.poll() {
        Ok(Some(msg)) => {
            let _ = ui.send(Msg::Flash(msg));
        }
        Err(e) => {
            let _ = ui.send(Msg::Flash(format!("{e:#}")));
        }
        _ => {}
    }
    let state = backend.sync_state();
    let interval = match &state {
        SyncState::Remote {
            last_error: Some(_),
            ..
        } => POLL_ERR,
        _ => POLL_OK,
    };
    let _ = ui.send(Msg::State(state));
    Instant::now() + interval
}

// ---------------------------------------------------------------- ChanBackend

/// Un `Backend` que vive del otro lado del canal: cada llamada espera su
/// respuesta (operaciones locales, rápidas), mientras `sync`/`poll` sólo
/// encolan el pedido y no esperan a la red.
pub struct ChanBackend {
    req: Sender<Req>,
    out: Receiver<anyhow::Result<CallOut>>,
}

impl ChanBackend {
    pub fn new(req: Sender<Req>, out: Receiver<anyhow::Result<CallOut>>) -> Self {
        Self { req, out }
    }

    fn call(&mut self, call: Call) -> anyhow::Result<CallOut> {
        self.req
            .send(Req::Call(Box::new(call)))
            .map_err(|_| anyhow!("el hilo de datos se cerró"))?;
        self.out
            .recv()
            .map_err(|_| anyhow!("el hilo de datos se cerró"))?
    }

    /// Encola una sincronización sin esperar.
    pub fn poll(&mut self) {
        let _ = self.req.send(Req::Poll);
    }
}

impl Backend for ChanBackend {
    fn lists(&mut self) -> anyhow::Result<Vec<ListRow>> {
        match self.call(Call::Lists)? {
            CallOut::Lists(v) => Ok(v),
            _ => unreachable!(),
        }
    }

    fn tasks(&mut self, list: &str) -> anyhow::Result<Vec<TaskView>> {
        match self.call(Call::Tasks(list.to_string()))? {
            CallOut::Tasks(v) => Ok(v),
            _ => unreachable!(),
        }
    }

    fn caps(&mut self, list: &str) -> anyhow::Result<Caps> {
        match self.call(Call::Caps(list.to_string()))? {
            CallOut::Caps(v) => Ok(v),
            _ => unreachable!(),
        }
    }

    fn add(&mut self, list: &str, task: Task) -> anyhow::Result<TaskId> {
        match self.call(Call::Add {
            list: list.to_string(),
            task,
        })? {
            CallOut::Id(id) => Ok(id),
            _ => unreachable!(),
        }
    }

    fn set(&mut self, list: &str, id: &str, change: Change) -> anyhow::Result<()> {
        self.call(Call::Set {
            list: list.to_string(),
            id: id.to_string(),
            changes: vec![change],
        })?;
        Ok(())
    }

    /// Todos los cambios en un solo mensaje (el formulario de edición).
    fn set_many(&mut self, list: &str, id: &str, changes: Vec<Change>) -> anyhow::Result<()> {
        self.call(Call::Set {
            list: list.to_string(),
            id: id.to_string(),
            changes,
        })?;
        Ok(())
    }

    fn delete(&mut self, list: &str, id: &str) -> anyhow::Result<()> {
        self.call(Call::Delete {
            list: list.to_string(),
            id: id.to_string(),
        })?;
        Ok(())
    }

    fn move_task(&mut self, from: &str, id: &str, to: &str) -> anyhow::Result<TaskId> {
        match self.call(Call::Move {
            from: from.to_string(),
            id: id.to_string(),
            to: to.to_string(),
        })? {
            CallOut::Id(id) => Ok(id),
            _ => unreachable!(),
        }
    }

    fn create_list(&mut self, id: &str, name: Option<&str>, _layout: Layout) -> anyhow::Result<()> {
        self.call(Call::CreateList {
            id: id.to_string(),
            name: name.map(str::to_string),
        })?;
        Ok(())
    }

    fn delete_list(&mut self, id: &str) -> anyhow::Result<()> {
        self.call(Call::DeleteList { id: id.to_string() })?;
        Ok(())
    }

    fn sync(&mut self) -> anyhow::Result<Option<String>> {
        self.poll();
        Ok(None)
    }
}

// ---------------------------------------------------------------- watchers

/// Hilo que escucha `/api/events` (SSE) y avisa a la UI cuando el servidor
/// cambia. Reconecta solo; si la UI se fue, termina.
pub fn spawn_sse(url: String, token: Option<String>, ui: Sender<Msg>) {
    thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(4))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let url = format!("{}/api/events", url.trim_end_matches('/'));
        loop {
            if listen(&client, &url, &token, &ui) {
                return; // la UI cerró el canal
            }
            thread::sleep(Duration::from_secs(5));
        }
    });
}

/// Lee el stream hasta que se corta. Devuelve `true` si hay que terminar.
fn listen(
    client: &reqwest::blocking::Client,
    url: &str,
    token: &Option<String>,
    ui: &Sender<Msg>,
) -> bool {
    let mut req = client.get(url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let Ok(resp) = req.send() else {
        return false;
    };
    if !resp.status().is_success() {
        return false;
    }
    // El servidor está alcanzable (otra vez): actualizar estado y, si había
    // cambios pendientes mientras estaba caído, mandarlos ya.
    if ui.send(Msg::Poll).is_err() {
        return true;
    }
    let reader = std::io::BufReader::new(resp);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.starts_with("data:") && ui.send(Msg::Poll).is_err() {
            return true;
        }
    }
    false
}

/// Vigila el directorio de datos en modo local (ediciones a mano, `git pull`,
/// otro `lrtodo`). El watcher hay que mantenerlo vivo guardándolo.
pub fn spawn_fs_watch(root: PathBuf, ui: Sender<Msg>) -> Option<notify::RecommendedWatcher> {
    use notify::Watcher;
    // notify reporta rutas canonicalizadas (p.ej. /tmp -> /private/tmp en macOS).
    let root = root.canonicalize().unwrap_or(root);
    let root2 = root.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        let touched = ev.paths.iter().any(|p| {
            p.strip_prefix(&root2)
                .ok()
                .and_then(|rel| rel.components().next())
                .is_some_and(|c| !c.as_os_str().to_string_lossy().starts_with('.'))
        });
        if touched {
            let _ = ui.send(Msg::Refresh);
        }
    })
    .ok()?;
    watcher
        .watch(&root, notify::RecursiveMode::Recursive)
        .ok()?;
    Some(watcher)
}
