//! Estado de la TUI y manejo de teclas. No dibuja (eso es `ui.rs`) ni habla
//! con la red directamente: todo pasa por el [`ChanBackend`].

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use lrtodo_core::{Change, Task, TaskView};

use super::worker::{ChanBackend, Msg};
use crate::backend::{Backend, ListRow, SyncState};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Lists,
    Tasks,
}

pub enum Mode {
    Normal,
    /// Prompt de una línea (agregar tarea, nueva lista, filtro).
    Input {
        kind: InputKind,
        buf: Line,
    },
    /// Formulario de edición de una tarea.
    Form {
        id: String,
        fields: Vec<Field>,
        sel: usize,
    },
    /// Confirmación de borrado.
    Confirm {
        text: String,
        id: String,
    },
    /// Elegir lista destino para mover la tarea.
    PickList {
        id: String,
        sel: usize,
    },
    Help,
}

#[derive(Clone)]
pub enum InputKind {
    AddTask,
    NewList,
    Filter,
    /// Borrar una lista: hay que tipear el id para confirmar.
    DeleteList(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Title,
    Due,
    Notes,
    Tags,
    Prio,
}

pub struct Field {
    pub kind: FieldKind,
    pub label: &'static str,
    pub buf: Line,
}

/// Edición de una línea de texto.
pub struct Line {
    pub chars: Vec<char>,
    pub pos: usize,
}

impl Line {
    pub fn new(s: impl Into<String>) -> Self {
        let chars: Vec<char> = s.into().chars().collect();
        Self {
            pos: chars.len(),
            chars,
        }
    }

    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.chars.clear();
                self.pos = 0;
            }
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                while self.pos > 0 && self.chars[self.pos - 1] == ' ' {
                    self.chars.remove(self.pos - 1);
                    self.pos -= 1;
                }
                while self.pos > 0 && self.chars[self.pos - 1] != ' ' {
                    self.chars.remove(self.pos - 1);
                    self.pos -= 1;
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.chars.insert(self.pos, c);
                self.pos += 1;
            }
            KeyCode::Backspace if self.pos > 0 => {
                self.chars.remove(self.pos - 1);
                self.pos -= 1;
            }
            KeyCode::Delete if self.pos < self.chars.len() => {
                self.chars.remove(self.pos);
            }
            KeyCode::Left => self.pos = self.pos.saturating_sub(1),
            KeyCode::Right => self.pos = (self.pos + 1).min(self.chars.len()),
            KeyCode::Home => self.pos = 0,
            KeyCode::End => self.pos = self.chars.len(),
            _ => return false,
        }
        true
    }
}

/// Efectos que `run` ejecuta (necesitan la terminal).
pub enum Effect {
    /// Abrir `$EDITOR` sobre la lista (suspende la TUI).
    Edit { list: String },
}

pub struct App {
    pub chan: ChanBackend,
    pub lists: Vec<ListRow>,
    pub sel_list: usize,
    pub tasks: Vec<TaskView>,
    pub sel_task: usize,
    pub focus: Focus,
    pub mode: Mode,
    pub show_done: bool,
    pub filter: String,
    pub state: SyncState,
    pub syncing: bool,
    pub flash: Option<(String, Instant)>,
    pub quit: bool,
    pub effect: Option<Effect>,
    last_sync_seen: Option<u64>,
    poll_in_flight: bool,
}

impl App {
    pub fn new(chan: ChanBackend, default_list: Option<String>) -> Self {
        let mut app = Self {
            chan,
            lists: vec![],
            sel_list: 0,
            tasks: vec![],
            sel_task: 0,
            focus: Focus::Lists,
            mode: Mode::Normal,
            show_done: true,
            filter: String::new(),
            state: SyncState::Local,
            syncing: true, // el worker arranca sincronizando
            flash: None,
            quit: false,
            effect: None,
            last_sync_seen: None,
            poll_in_flight: false,
        };
        app.refresh_lists();
        if let Some(want) = default_list
            && let Some(i) = app.lists.iter().position(|r| r.info.meta.id == want)
        {
            app.sel_list = i;
        }
        app.refresh_tasks();
        app
    }

    // ------------------------------------------------------------ datos

    fn flash_err(&mut self, prefix: &str, e: anyhow::Error) {
        self.flash = Some((format!("{prefix}: {e:#}"), Instant::now()));
    }

    fn try_call<T>(&mut self, prefix: &str, r: anyhow::Result<T>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.flash_err(prefix, e);
                None
            }
        }
    }

    pub fn refresh_lists(&mut self) {
        let current = self.current_list_id();
        match self.chan.lists() {
            Ok(lists) => {
                self.lists = lists;
                self.sel_list = current
                    .and_then(|id| self.lists.iter().position(|r| r.info.meta.id == id))
                    .unwrap_or(0)
                    .min(self.lists.len().saturating_sub(1));
            }
            Err(e) => self.flash_err("listas", e),
        }
    }

    pub fn refresh_tasks(&mut self) {
        self.tasks = match self.current_list_id() {
            Some(id) => match self.chan.tasks(&id) {
                Ok(t) => t,
                Err(e) => {
                    self.flash_err("tareas", e);
                    vec![]
                }
            },
            None => vec![],
        };
        self.clamp_sel();
    }

    fn refresh_all(&mut self) {
        self.refresh_lists();
        self.refresh_tasks();
    }

    pub fn current_list_id(&self) -> Option<String> {
        self.lists
            .get(self.sel_list)
            .map(|r| r.info.meta.id.clone())
    }

    /// Tareas visibles con el filtro y el toggle de completadas.
    pub fn visible(&self) -> Vec<&TaskView> {
        let f = self.filter.to_lowercase();
        self.tasks
            .iter()
            .filter(|t| self.show_done || !t.task.done)
            .filter(|t| {
                f.is_empty()
                    || t.task.title.to_lowercase().contains(&f)
                    || t.task
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&f))
            })
            .collect()
    }

    fn selected_task(&self) -> Option<(String, bool)> {
        let v = self.visible();
        v.get(self.sel_task)
            .map(|t| (t.task.id.clone(), t.task.done))
    }

    fn clamp_sel(&mut self) {
        self.sel_list = self.sel_list.min(self.lists.len().saturating_sub(1));
        self.sel_task = self.sel_task.min(self.visible().len().saturating_sub(1));
    }

    // ------------------------------------------------------------ mensajes

    pub fn msg(&mut self, msg: Msg) {
        match msg {
            Msg::State(s) => {
                self.syncing = false;
                self.poll_in_flight = false;
                let new_sync = match &s {
                    SyncState::Remote { last_sync_ms, .. } => *last_sync_ms,
                    SyncState::Local => None,
                };
                let pulled = new_sync.is_some() && new_sync != self.last_sync_seen;
                if new_sync.is_some() {
                    self.last_sync_seen = new_sync;
                }
                self.state = s;
                if pulled {
                    self.refresh_all();
                }
            }
            Msg::Flash(m) => self.flash = Some((m, Instant::now())),
            Msg::Poll => {
                if !self.poll_in_flight {
                    self.poll_in_flight = true;
                    self.syncing = true;
                    self.chan.poll();
                }
            }
            Msg::Refresh => self.refresh_all(),
        }
    }

    // ------------------------------------------------------------ teclas

    pub fn key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Normal => self.key_normal(key),
            Mode::Input { .. } => self.key_input(key),
            Mode::Form { .. } => self.key_form(key),
            Mode::Confirm { .. } => self.key_confirm(key),
            Mode::PickList { .. } => self.key_pick(key),
            Mode::Help => self.mode = Mode::Normal,
        }
    }

    fn key_normal(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                self.filter.clear();
                self.clamp_sel();
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Tab
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Left
            | KeyCode::Right => {
                self.focus = match self.focus {
                    Focus::Lists => Focus::Tasks,
                    Focus::Tasks => Focus::Lists,
                };
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('G') | KeyCode::End => self.move_sel(isize::MAX),
            KeyCode::Home => self.move_sel(isize::MIN),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Enter | KeyCode::Char(' ') => match self.focus {
                Focus::Lists => self.focus = Focus::Tasks,
                Focus::Tasks => self.toggle(),
            },
            KeyCode::Char('a') => {
                self.mode = Mode::Input {
                    kind: InputKind::AddTask,
                    buf: Line::new(""),
                };
            }
            KeyCode::Char('n') => {
                self.mode = Mode::Input {
                    kind: InputKind::NewList,
                    buf: Line::new(""),
                };
            }
            KeyCode::Char('D') if self.focus == Focus::Lists => {
                if let Some(row) = self.lists.get(self.sel_list) {
                    let id = row.info.meta.id.clone();
                    if row.info.meta.kind == lrtodo_core::ListKind::HomeAssistant {
                        self.flash = Some((
                            "las listas de Home Assistant se quitan con `exclude` en la config"
                                .into(),
                            Instant::now(),
                        ));
                    } else {
                        self.mode = Mode::Input {
                            kind: InputKind::DeleteList(id),
                            buf: Line::new(""),
                        };
                    }
                }
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Input {
                    kind: InputKind::Filter,
                    buf: Line::new(self.filter.clone()),
                };
            }
            // Mutan la tarea seleccionada: sólo con el panel de tareas activo.
            KeyCode::Char('e') if self.focus == Focus::Tasks => self.open_form(),
            KeyCode::Char('d') if self.focus == Focus::Tasks => self.ask_delete(),
            KeyCode::Char('m') if self.focus == Focus::Tasks => self.open_picker(),
            KeyCode::Char('E') => {
                if let Some(list) = self.current_list_id() {
                    self.effect = Some(Effect::Edit { list });
                }
            }
            KeyCode::Char('v') => {
                self.show_done = !self.show_done;
                self.clamp_sel();
            }
            KeyCode::Char('r') => {
                self.poll_in_flight = true;
                self.syncing = true;
                self.chan.poll();
            }
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: isize) {
        match self.focus {
            Focus::Lists if !self.lists.is_empty() => {
                self.sel_list = self
                    .sel_list
                    .saturating_add_signed(delta)
                    .min(self.lists.len() - 1);
                self.refresh_tasks();
            }
            Focus::Tasks => {
                let len = self.visible().len();
                if len > 0 {
                    self.sel_task = self.sel_task.saturating_add_signed(delta).min(len - 1);
                }
            }
            _ => {}
        }
    }

    fn key_input(&mut self, key: KeyEvent) {
        let Mode::Input { kind, buf } = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                let kind = kind.clone();
                let text = buf.text();
                self.mode = Mode::Normal;
                self.submit_input(kind, text);
            }
            _ => {
                if buf.key(&key) && matches!(kind, InputKind::Filter) {
                    // El filtro se aplica en vivo.
                    self.filter = buf.text();
                    self.clamp_sel();
                }
            }
        }
    }

    fn submit_input(&mut self, kind: InputKind, text: String) {
        match kind {
            InputKind::AddTask => {
                let title = text.trim().to_string();
                if title.is_empty() {
                    return;
                }
                if let Some(list) = self.current_list_id() {
                    let task = Task {
                        title,
                        ..Default::default()
                    };
                    let r = self.chan.add(&list, task);
                    if self.try_call("agregar", r).is_some() {
                        self.refresh_all();
                        self.sel_task = self.visible().len().saturating_sub(1);
                    }
                }
            }
            InputKind::NewList => {
                let (id, name) = match text.split_once('|') {
                    Some((i, n)) => (i.trim(), Some(n.trim())),
                    None => (text.trim(), None),
                };
                if id.is_empty() {
                    return;
                }
                match self.chan.create_list(id, name, lrtodo_store::Layout::File) {
                    Ok(()) => {
                        self.refresh_lists();
                        if let Some(i) = self.lists.iter().position(|r| r.info.meta.id == id) {
                            self.sel_list = i;
                            self.refresh_tasks();
                        }
                    }
                    Err(e) => self.flash_err("crear lista", e),
                }
            }
            InputKind::Filter => self.filter = text.trim().to_string(),
            InputKind::DeleteList(id) => {
                if text.trim() != id {
                    self.flash = Some((
                        format!("no coincide con «{id}»; no se borró nada"),
                        Instant::now(),
                    ));
                    return;
                }
                match self.chan.delete_list(&id) {
                    Ok(()) => {
                        self.flash = Some((format!("lista «{id}» borrada"), Instant::now()));
                        self.refresh_all();
                    }
                    Err(e) => self.flash_err("borrar lista", e),
                }
            }
        }
    }

    fn key_form(&mut self, key: KeyEvent) {
        let Mode::Form { fields, sel, id } = &mut self.mode else {
            return;
        };
        let id = id.clone();
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => {
                self.apply_form(id);
            }
            KeyCode::Tab | KeyCode::Down => *sel = (*sel + 1) % fields.len(),
            KeyCode::BackTab | KeyCode::Up => {
                *sel = (*sel + fields.len() - 1) % fields.len();
            }
            _ => {
                if let Some(f) = fields.get_mut(*sel) {
                    f.buf.key(&key);
                }
            }
        }
    }

    fn open_form(&mut self) {
        let Some((id, _)) = self.selected_task() else {
            return;
        };
        let Some(list) = self.current_list_id() else {
            return;
        };
        let r = self.chan.caps(&list);
        let Some(caps) = self.try_call("capacidades", r) else {
            return;
        };
        let v = self.visible();
        let Some(tv) = v.get(self.sel_task) else {
            return;
        };
        let t = &tv.task;
        let mut fields = vec![Field {
            kind: FieldKind::Title,
            label: "Título",
            buf: Line::new(t.title.clone()),
        }];
        if caps.due {
            fields.push(Field {
                kind: FieldKind::Due,
                label: "Vence",
                buf: Line::new(t.due.clone().unwrap_or_default()),
            });
        }
        if caps.notes {
            fields.push(Field {
                kind: FieldKind::Notes,
                label: "Notas",
                buf: Line::new(t.notes.clone().unwrap_or_default()),
            });
        }
        if caps.tags {
            fields.push(Field {
                kind: FieldKind::Tags,
                label: "Tags",
                buf: Line::new(t.tags.join(", ")),
            });
        }
        if caps.priority {
            fields.push(Field {
                kind: FieldKind::Prio,
                label: "Prio 1-9",
                buf: Line::new(t.priority.map(|p| p.to_string()).unwrap_or_default()),
            });
        }
        self.mode = Mode::Form { id, fields, sel: 0 };
    }

    fn apply_form(&mut self, id: String) {
        let Mode::Form { fields, .. } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let Some(list) = self.current_list_id() else {
            return;
        };
        let v = self.visible();
        let Some(orig) = v.iter().find(|t| t.task.id == id).map(|t| t.task.clone()) else {
            self.flash = Some(("la tarea ya no existe".into(), Instant::now()));
            self.refresh_tasks();
            return;
        };
        let mut after = orig.clone();
        for f in &fields {
            let text = f.buf.text().trim().to_string();
            match f.kind {
                FieldKind::Title => after.title = text,
                FieldKind::Due => after.due = (!text.is_empty()).then_some(text),
                FieldKind::Notes => after.notes = (!text.is_empty()).then_some(text),
                FieldKind::Tags => {
                    after.tags = text
                        .split([',', ' '])
                        .filter(|t| !t.is_empty())
                        .map(str::to_string)
                        .collect();
                }
                FieldKind::Prio => {
                    after.priority = text.parse::<u8>().ok().filter(|p| *p > 0);
                }
            }
        }
        let changes: Vec<Change> = orig.changes_to(&after);
        if changes.is_empty() {
            return;
        }
        let r = self.chan.set_many(&list, &id, changes);
        if self.try_call("editar", r).is_some() {
            self.refresh_all();
        }
    }

    fn ask_delete(&mut self) {
        let Some((id, _)) = self.selected_task() else {
            return;
        };
        let v = self.visible();
        let Some(t) = v.get(self.sel_task) else {
            return;
        };
        self.mode = Mode::Confirm {
            text: format!("¿Borrar «{}»? [s/N]", t.task.title),
            id,
        };
    }

    fn key_confirm(&mut self, key: KeyEvent) {
        let Mode::Confirm { id, .. } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        if !matches!(key.code, KeyCode::Char('s' | 'S' | 'y' | 'Y')) {
            return;
        }
        if let Some(list) = self.current_list_id() {
            let r = self.chan.delete(&list, &id);
            if self.try_call("borrar", r).is_some() {
                self.refresh_all();
            }
        }
    }

    fn open_picker(&mut self) {
        let Some((id, _)) = self.selected_task() else {
            return;
        };
        if self.lists.len() < 2 {
            self.flash = Some(("no hay otra lista".into(), Instant::now()));
            return;
        }
        self.mode = Mode::PickList {
            id,
            sel: self.sel_list,
        };
    }

    fn key_pick(&mut self, key: KeyEvent) {
        let Mode::PickList { id, sel } = &mut self.mode else {
            return;
        };
        let id = id.clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                *sel = (*sel + 1) % self.lists.len();
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *sel = (*sel + self.lists.len() - 1) % self.lists.len();
            }
            KeyCode::Enter => {
                let to = self.lists.get(*sel).map(|r| r.info.meta.id.clone());
                self.mode = Mode::Normal;
                let (Some(from), Some(to)) = (self.current_list_id(), to) else {
                    return;
                };
                if from == to {
                    return;
                }
                let r = self.chan.move_task(&from, &id, &to);
                if self.try_call("mover", r).is_some() {
                    self.refresh_all();
                }
            }
            _ => {}
        }
    }

    fn toggle(&mut self) {
        let Some(list) = self.current_list_id() else {
            return;
        };
        let Some((id, done)) = self.selected_task() else {
            return;
        };
        let r = self.chan.set(&list, &id, Change::Done(!done));
        if self.try_call("cambiar", r).is_some() {
            self.refresh_all();
        }
    }
}
