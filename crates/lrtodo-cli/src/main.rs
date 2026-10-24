mod backend;
mod config;
mod editor;
mod output;
mod tui;

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, anyhow, bail};
use backend::{Backend, Local, Remote};
use clap::{Parser, Subcommand};
use config::Config;
use lrtodo_core::{Change, Task, TaskView};
use lrtodo_store::Layout;

/// Tareas en YAML editable a mano, offline-first, con PWA y Home Assistant.
#[derive(Parser)]
#[command(name = "lrtodo", version)]
struct Cli {
    /// Archivo de configuración (por defecto ~/.config/lrtodo/config.yaml).
    #[arg(long, global = true, env = "LRTODO_CONFIG")]
    config: Option<PathBuf>,
    /// Directorio de datos (modo local y serve).
    #[arg(long, global = true)]
    data_dir: Option<String>,
    /// URL de un servidor lrtodo (modo remoto, con cache offline); "local" fuerza modo local.
    #[arg(long, global = true)]
    server: Option<String>,
    /// Salida en JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Muestra las listas.
    Lists,
    /// Muestra las tareas de una lista.
    Ls {
        list: Option<String>,
        /// Sólo las abiertas.
        #[arg(short, long)]
        open: bool,
    },
    /// Agrega una tarea.
    Add {
        list: String,
        #[arg(required = true, num_args = 1..)]
        title: Vec<String>,
        #[arg(long)]
        due: Option<String>,
        #[arg(short, long = "tag")]
        tags: Vec<String>,
        #[arg(short, long)]
        notes: Option<String>,
        #[arg(short, long)]
        prio: Option<u8>,
    },
    /// Marca tareas como hechas (por número, prefijo de id o título).
    Done {
        list: String,
        #[arg(required = true)]
        refs: Vec<String>,
    },
    /// Vuelve a abrir tareas.
    Undo {
        list: String,
        #[arg(required = true)]
        refs: Vec<String>,
    },
    /// Borra tareas.
    Rm {
        list: String,
        #[arg(required = true)]
        refs: Vec<String>,
    },
    /// Modifica una tarea.
    Set {
        list: String,
        task: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        due: Option<String>,
        #[arg(long)]
        no_due: bool,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        no_notes: bool,
        /// Reemplaza las etiquetas.
        #[arg(short, long = "tag")]
        tags: Option<Vec<String>>,
        #[arg(long)]
        prio: Option<u8>,
    },
    /// Mueve una tarea a otra lista.
    Mv {
        list: String,
        task: String,
        to: String,
    },
    /// Edita una lista en $EDITOR.
    Edit { list: String },
    /// Crea una lista.
    New {
        id: String,
        #[arg(long)]
        name: Option<String>,
        /// file = un archivo por lista, dir = un archivo por tarea.
        #[arg(long, default_value = "file")]
        layout: Layout,
    },
    /// Borra una lista entera, con sus tareas (irreversible).
    RmList { id: String },
    /// Cambia el layout de una lista (sólo modo local).
    Convert {
        id: String,
        #[arg(long)]
        layout: Layout,
    },
    /// Valida los archivos (sólo modo local).
    Check,
    /// Sincroniza con el servidor.
    Sync,
    /// Estado de la sincronización.
    Status,
    /// Interfaz de terminal interactiva (teclas tipo vim).
    Tui,
    /// Levanta el servidor (API + PWA).
    Serve {
        #[arg(long)]
        bind: Option<String>,
    },
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LRTODO_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let cfg = Config::load(cli.config.as_deref())?;
    let data_dir = cfg.data_dir(cli.data_dir.as_deref());
    // Sin subcomando: en una terminal abre la TUI; en un pipe, lista como antes.
    let cmd = cli.cmd.unwrap_or_else(|| {
        if std::io::stdout().is_terminal() {
            Cmd::Tui
        } else {
            Cmd::Lists
        }
    });

    if let Cmd::Serve { bind } = cmd {
        let server = cfg.server_config(data_dir, bind)?;
        return tokio::runtime::Runtime::new()?.block_on(lrtodo_server::serve(server));
    }

    let remote = cfg.remote(cli.server.as_deref());
    if let Cmd::Tui = cmd {
        let backend: Box<dyn Backend + Send> = match remote {
            Some(r) => Box::new(Remote::open(r)?),
            None => Box::new(Local::open(data_dir)?),
        };
        return tui::run(backend, cfg.default_list.clone());
    }

    let mut local: Option<Local> = None;
    let mut remote_backend: Option<Remote> = None;
    let backend: &mut dyn Backend = match remote {
        Some(r) => remote_backend.insert(Remote::open(r)?),
        None => local.insert(Local::open(data_dir)?),
    };
    let default_list = cfg.default_list.clone();

    match cmd {
        Cmd::Serve { .. } | Cmd::Tui => unreachable!(),
        Cmd::Lists => {
            let rows = backend.lists()?;
            if cli.json {
                let v: Vec<_> = rows
                    .iter()
                    .map(|r| serde_json::json!({"info": r.info, "open": r.open, "total": r.total, "pending": r.pending}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                output::lists(&rows);
            }
        }
        Cmd::Ls { list, open } => {
            let list = list.or(default_list);
            match list {
                None => output::lists(&backend.lists()?),
                Some(list) => {
                    let tasks = backend.tasks(&list)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&tasks)?);
                    } else {
                        let name = backend
                            .lists()?
                            .into_iter()
                            .find(|r| r.info.meta.id == list)
                            .map(|r| r.info.meta.name)
                            .unwrap_or(list);
                        output::tasks(&name, &tasks, open);
                    }
                }
            }
        }
        Cmd::Add {
            list,
            title,
            due,
            tags,
            notes,
            prio,
        } => {
            let task = Task {
                title: title.join(" "),
                due,
                tags,
                notes,
                priority: prio,
                ..Default::default()
            };
            let id = backend.add(&list, task)?;
            if cli.json {
                println!("{}", serde_json::json!({"id": id}));
            }
        }
        Cmd::Done { list, refs } => for_each(backend, &list, &refs, |b, id| {
            b.set(&list, id, Change::Done(true))
        })?,
        Cmd::Undo { list, refs } => for_each(backend, &list, &refs, |b, id| {
            b.set(&list, id, Change::Done(false))
        })?,
        Cmd::Rm { list, refs } => for_each(backend, &list, &refs, |b, id| b.delete(&list, id))?,
        Cmd::Set {
            list,
            task,
            title,
            due,
            no_due,
            notes,
            no_notes,
            tags,
            prio,
        } => {
            let id = resolve(&backend.tasks(&list)?, &task)?;
            let mut changes = vec![];
            if let Some(t) = title {
                changes.push(Change::Title(t));
            }
            if due.is_some() || no_due {
                changes.push(Change::Due(due));
            }
            if notes.is_some() || no_notes {
                changes.push(Change::Notes(notes));
            }
            if let Some(t) = tags {
                changes.push(Change::Tags(t));
            }
            if prio.is_some() {
                changes.push(Change::Priority(prio.filter(|p| *p > 0)));
            }
            if changes.is_empty() {
                bail!("nada que cambiar (ver lrtodo set --help)");
            }
            for c in changes {
                backend.set(&list, &id, c)?;
            }
        }
        Cmd::Mv { list, task, to } => {
            let id = resolve(&backend.tasks(&list)?, &task)?;
            backend.move_task(&list, &id, &to)?;
        }
        Cmd::Edit { list } => {
            let n = editor::edit(backend, &list)?;
            eprintln!("{n} cambio(s) aplicado(s)");
        }
        Cmd::New { id, name, layout } => backend.create_list(&id, name.as_deref(), layout)?,
        Cmd::RmList { id } => backend.delete_list(&id)?,
        Cmd::Convert { id, layout } => {
            let local = local
                .as_ref()
                .context("convert sólo funciona en modo local (--server local)")?;
            local.store.convert(&id, layout)?;
        }
        Cmd::Check => {
            let local = local
                .as_ref()
                .context("check sólo funciona en modo local (--server local)")?;
            let problems = local.store.check();
            if problems.is_empty() {
                println!("ok");
            } else {
                for p in &problems {
                    println!("{p}");
                }
                bail!("{} problema(s)", problems.len());
            }
        }
        Cmd::Sync => {}
        Cmd::Status => match &remote_backend {
            Some(r) => println!("{}", r.status()),
            None => println!(
                "modo local: {}",
                local.as_ref().expect("local").store.root().display()
            ),
        },
    }
    let backend: &mut dyn Backend = match (&mut remote_backend, &mut local) {
        (Some(r), _) => r,
        (None, Some(l)) => l,
        _ => unreachable!(),
    };
    if let Some(msg) = backend.sync()? {
        eprintln!("{msg}");
    }
    Ok(())
}

fn for_each(
    backend: &mut dyn Backend,
    list: &str,
    refs: &[String],
    mut f: impl FnMut(&mut dyn Backend, &str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    // Se resuelven todas antes de modificar, así los números no se corren.
    let tasks = backend.tasks(list)?;
    let ids = refs
        .iter()
        .map(|r| resolve(&tasks, r))
        .collect::<anyhow::Result<Vec<_>>>()?;
    for id in ids {
        f(backend, &id)?;
    }
    Ok(())
}

/// Número (1-based, como en `ls`), prefijo de id o título exacto.
fn resolve(tasks: &[TaskView], r: &str) -> anyhow::Result<String> {
    if let Ok(n) = r.parse::<usize>()
        && (1..=tasks.len()).contains(&n)
    {
        return Ok(tasks[n - 1].task.id.clone());
    }
    let lower = r.to_lowercase();
    let by_id: Vec<_> = tasks
        .iter()
        .filter(|t| t.task.id.to_lowercase().starts_with(&lower))
        .collect();
    if by_id.len() == 1 {
        return Ok(by_id[0].task.id.clone());
    }
    let by_title: Vec<_> = tasks
        .iter()
        .filter(|t| t.task.title.to_lowercase() == lower)
        .collect();
    match by_title.len() {
        1 => Ok(by_title[0].task.id.clone()),
        0 if by_id.is_empty() => Err(anyhow!("no encontré la tarea {r:?}")),
        _ => Err(anyhow!(
            "{r:?} es ambiguo; usá el número o más caracteres del id"
        )),
    }
}
