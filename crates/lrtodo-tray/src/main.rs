//! `lrtodo-tray`: las tareas en la barra de menús.
//!
//! Un hilo worker es dueño del [`Backend`] (local o remoto con cache, igual
//! que la CLI/TUI) y el hilo principal corre el event loop de `tao`: el
//! worker manda [`MenuModel`]s y recibe acciones del menú por canales.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use lrtodo_client::{Backend, ClientConfig, Local, Remote, SyncState, ago};
use lrtodo_core::{Change, LinkKind};
use lrtodo_store::now_ms;
use muda::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIconBuilder};

const POLL: Duration = Duration::from_secs(10);
const TASKS_PER_LIST: usize = 20;

#[derive(Parser)]
#[command(about = "lrtodo en la barra de menús")]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    data_dir: Option<String>,
    /// URL del servidor (o "local").
    #[arg(long)]
    server: Option<String>,
}

/// Acciones que la UI le manda al worker.
enum Work {
    Toggle { list: String, task: String },
    Sync,
    Quit,
}

/// Un modelo inmutable del estado, para reconstruir el menú.
struct MenuModel {
    title: String,
    lists: Vec<MenuList>,
}

struct MenuList {
    id: String,
    name: String,
    tasks: Vec<MenuTask>,
}

struct MenuTask {
    id: String,
    title: String,
    done: bool,
    depth: usize,
}

enum UserEvent {
    Model(MenuModel),
}

fn main() -> Result<()> {
    let args = Args::parse();
    let cfg = ClientConfig::load(args.config.as_deref())?;
    let backend: Box<dyn Backend> = match cfg.remote(args.server.as_deref()) {
        Some(rc) => Box::new(Remote::open(rc)?),
        None => Box::new(Local::open(cfg.data_dir(args.data_dir.as_deref()))?),
    };

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (tx, rx) = channel::<Work>();

    // Icono: casillero con tilde dibujado a pixels (22x22, template).
    let tray = TrayIconBuilder::new()
        .with_icon(icon_checkbox())
        .with_title("")
        .with_tooltip("lrtodo")
        .build()?;

    std::thread::spawn(move || worker(backend, rx, proxy));

    // Primer poll inmediato.
    let _ = tx.send(Work::Sync);

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::NewEvents(StartCause::Init) => {}
            Event::UserEvent(UserEvent::Model(m)) => {
                tray.set_menu(Some(Box::new(build_menu(&m))));
                tray.set_title(Some(&m.title));
            }
            _ => {}
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = ev.id.0.as_str();
            if id == "sync" {
                let _ = tx.send(Work::Sync);
            } else if id == "quit" {
                let _ = tx.send(Work::Quit);
                *flow = ControlFlow::Exit;
            } else if let Some((list, task)) = id.strip_prefix("t:").and_then(|s| s.split_once('/'))
            {
                let _ = tx.send(Work::Toggle {
                    list: list.to_string(),
                    task: task.to_string(),
                });
            }
        }
    });
}

/// Poll + acciones del usuario. Recomputa el modelo tras cada cambio.
fn worker(
    mut backend: Box<dyn Backend>,
    rx: Receiver<Work>,
    proxy: tao::event_loop::EventLoopProxy<UserEvent>,
) {
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Work::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(Work::Toggle { list, task }) => {
                // El estado destino lo decide el worker (muda no reporta `checked`).
                if let Ok(tasks) = backend.tasks(&list)
                    && let Some(t) = tasks.iter().find(|t| t.task.id == task)
                {
                    let _ = backend.set(&list, &task, Change::Done(!t.task.done));
                }
            }
            Ok(Work::Sync) | Err(RecvTimeoutError::Timeout) => {
                let _ = backend.poll();
            }
        }
        match model(&mut *backend) {
            Ok(m) => {
                if proxy.send_event(UserEvent::Model(m)).is_err() {
                    break;
                }
            }
            Err(e) => eprintln!("modelo: {e}"),
        }
    }
}

/// Lee el backend y arma el modelo del menú.
fn model(backend: &mut dyn Backend) -> Result<MenuModel> {
    let mut lists = vec![];
    for row in backend.lists()? {
        let tasks: Vec<MenuTask> = backend
            .tasks(&row.info.meta.id)?
            .iter()
            .take(TASKS_PER_LIST)
            .map(|t| MenuTask {
                id: t.task.id.clone(),
                title: t.task.title.clone(),
                done: t.task.done,
                depth: depth_of(&t.task.links, &row.info.meta.id),
            })
            .collect();
        lists.push(MenuList {
            id: row.info.meta.id,
            name: row.info.meta.name,
            tasks,
        });
    }
    let title = match backend.sync_state() {
        SyncState::Local => String::new(),
        SyncState::Remote {
            pending,
            last_sync_ms,
            last_error,
            ..
        } => {
            if last_error.is_some() {
                "⛔".into()
            } else if pending > 0 {
                format!(" ●{pending}")
            } else {
                last_sync_ms
                    .map(|ms| format!(" {}", ago(now_ms().saturating_sub(ms))))
                    .unwrap_or_default()
            }
        }
    };
    Ok(MenuModel { title, lists })
}

/// Indentación para subtareas (sólo mismo listado).
fn depth_of(links: &[lrtodo_core::Link], _list: &str) -> usize {
    usize::from(
        links
            .iter()
            .any(|l| l.kind == LinkKind::Subtask && !l.to.contains('/')),
    )
}

fn build_menu(m: &MenuModel) -> Menu {
    let menu = Menu::new();
    if m.lists.is_empty() {
        let _ = menu.append(&MenuItem::new("sin listas", false, None));
    }
    for l in &m.lists {
        let open = l.tasks.iter().filter(|t| !t.done).count();
        let sub = Submenu::new(format!("{} ({open})", l.name), true);
        for t in &l.tasks {
            let label = format!("{}{}", "  ".repeat(t.depth), t.title);
            let item = CheckMenuItem::with_id(
                MenuId::from(format!("t:{}/{}", l.id, t.id)),
                label,
                true,
                t.done,
                None,
            );
            let _ = sub.append(&item);
        }
        if l.tasks.is_empty() {
            let _ = sub.append(&MenuItem::new("(vacía)", false, None));
        }
        let _ = menu.append(&sub);
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        MenuId::from("sync"),
        "↻ Sincronizar",
        true,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        MenuId::from("quit"),
        "Salir",
        true,
        None,
    ));
    menu
}

/// Casillero con tilde: 22x22 RGBA (blanco opaco = ícono template).
fn icon_checkbox() -> Icon {
    const S: usize = 22;
    let mut px = vec![0u8; S * S * 4];
    let mut set = |x: usize, y: usize, a: u8| {
        if x < S && y < S {
            let i = (y * S + x) * 4;
            px[i..i + 4].copy_from_slice(&[255, 255, 255, a]);
        }
    };
    // Borde del casillero (3,3)-(18,18), grosor 2.
    for i in 3..=18usize {
        for t in 0..2usize {
            set(i, 3 + t, 255);
            set(i, 17 - t, 255);
            set(3 + t, i, 255);
            set(17 - t, i, 255);
        }
    }
    // Tilde: (6,11)→(9,14) y (9,14)→(15,7), grosor 2.
    for (x, y) in [
        (6, 11),
        (7, 12),
        (8, 13),
        (9, 14),
        (10, 13),
        (11, 12),
        (12, 11),
        (13, 10),
        (14, 9),
        (15, 8),
    ] {
        set(x, y, 255);
        set(x, y + 1, 255);
    }
    Icon::from_rgba(px, S as u32, S as u32).expect("icono 22x22")
}
