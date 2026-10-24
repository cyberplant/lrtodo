//! `lrtodo tui`: interfaz de terminal sobre el mismo `Backend` que la CLI.
//!
//! La UI corre en el hilo principal y nunca espera a la red ni al disco: el
//! backend vive en [`worker`] y los cambios remotos/archivos llegan por eventos.

mod app;
mod ui;
mod worker;

use std::io::stdout;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::backend::Backend;
use app::{App, Effect};
use worker::{ChanBackend, Msg};

/// Restaura la terminal aunque se salga por error.
struct Restore {
    active: bool,
}

impl Restore {
    fn leave() -> Self {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
        Self { active: false }
    }

    fn enter(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        execute!(stdout(), EnterAlternateScreen)?;
        self.active = true;
        Ok(())
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(stdout(), LeaveAlternateScreen);
        }
    }
}

pub fn run(backend: Box<dyn Backend + Send>, default_list: Option<String>) -> anyhow::Result<()> {
    let mut restore = Restore { active: false };
    restore.enter().context("¿es una terminal?")?;
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    term.clear()?;

    let (ui_tx, ui_rx) = channel::<Msg>();
    // Los datos del backend se toman antes de moverlo al hilo worker.
    let remote = backend.remote_info();
    let root = backend.local_root();
    let channels = worker::start(backend, ui_tx.clone());
    let chan = ChanBackend::new(channels.req.clone(), channels.out);

    if let Some((url, token)) = remote {
        worker::spawn_sse(url, token, ui_tx.clone());
    }
    // El watcher se queda vivo en `_watch` hasta que termina el programa.
    let _watch = root.and_then(|r| worker::spawn_fs_watch(r, ui_tx));

    let mut app = App::new(chan, default_list);
    loop {
        term.draw(|f| ui::draw(f, &mut app))?;
        while let Ok(msg) = ui_rx.try_recv() {
            app.msg(msg);
        }
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.key(key);
        }
        if let Some(Effect::Edit { list }) = app.effect.take() {
            edit(&mut restore, &mut term, &mut app, &list);
        }
        if app.quit {
            return Ok(());
        }
    }
}

/// `$EDITOR` con la TUI suspendida, como `lrtodo edit`.
fn edit(
    restore: &mut Restore,
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    list: &str,
) {
    *restore = Restore::leave();
    let result = crate::editor::edit(&mut app.chan, list);
    let _ = restore.enter();
    let _ = term.clear();
    match result {
        Ok(n) => app.msg(Msg::Flash(format!("{n} cambio(s) aplicado(s)"))),
        Err(e) => app.msg(Msg::Flash(format!("editor: {e:#}"))),
    }
    app.refresh_lists();
    app.refresh_tasks();
}
