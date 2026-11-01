//! Estado de la TUI y manejo de teclas. No dibuja (eso es `ui.rs`) ni habla
//! con la red directamente: todo pasa por el [`ChanBackend`].

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lrtodo_core::{Change, LinkKind, Task, TaskView};
use ratatui::layout::Rect;

use super::worker::{ChanBackend, Msg};
use lrtodo_client::{Backend, ListRow, SyncState};

fn kind_label(k: LinkKind) -> &'static str {
    match k {
        LinkKind::Subtask => "subtarea",
        LinkKind::Blocks => "bloquea",
        LinkKind::Related => "relacionada",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Lists,
    Tasks,
}

pub enum Mode {
    Normal,
    /// Prompt de una línea (agregar tarea, filtro, confirmación tipeada).
    Input {
        kind: InputKind,
        buf: Line,
    },
    /// Nueva lista: nombre + id que se autocompleta hasta tocarlo a mano.
    NewList {
        name: Line,
        id: Line,
        /// 0 = nombre, 1 = id.
        focus: usize,
        /// true cuando el usuario editó el id: se apaga el autocompletado.
        id_dirty: bool,
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
    /// Elegir entre opciones (mover a lista, kind/destino de link, unlink).
    Pick {
        title: String,
        options: Vec<String>,
        sel: usize,
        next: PickNext,
    },
    /// Visor de notas markdown de una tarea (scroll con j/k, e edita).
    Notes {
        id: String,
        offset: usize,
    },
    Help,
}

/// Qué hacer al elegir una opción del picker.
pub enum PickNext {
    /// Mover la tarea `id` a la lista elegida (options = ids).
    MoveTask { id: String },
    /// Elegir el kind del link de `src` (options = descripciones).
    LinkKind { src: String },
    /// Elegir destino (options = títulos; `dsts` son los ids paralelos).
    LinkDst {
        src: String,
        kind: LinkKind,
        dsts: Vec<String>,
    },
    /// Elegir qué link quitar (options = descripciones; `dsts` paralelo).
    Unlink {
        src: String,
        dsts: Vec<(String, LinkKind)>,
    },
}

#[derive(Clone)]
pub enum InputKind {
    AddTask,
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
    /// Editar sólo la nota de una tarea en `$EDITOR`.
    EditNotes { list: String, id: String },
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
    /// Geometría del último dibujado (para hit-testing del mouse).
    pub lists_area: Rect,
    pub tasks_area: Rect,
    pub lists_offset: usize,
    pub tasks_offset: usize,
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
            lists_area: Rect::default(),
            tasks_area: Rect::default(),
            lists_offset: 0,
            tasks_offset: 0,
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

    /// Mapa padre → hijos (ids, en orden), sólo links `subtask` resolubles
    /// en esta lista.
    pub fn children_map(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let ids: std::collections::BTreeSet<&str> =
            self.tasks.iter().map(|t| t.task.id.as_str()).collect();
        let mut map: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for t in &self.tasks {
            if let Some(l) = t
                .task
                .links
                .iter()
                .find(|l| l.kind == LinkKind::Subtask && ids.contains(l.to.as_str()))
            {
                map.entry(l.to.clone()).or_default().push(t.task.id.clone());
            }
        }
        map
    }

    /// Guías del árbol para dibujar `├─`/`└─`/`│`:
    /// `[propio es-último, padre es-último, abuelo es-último…]`. Vacío = raíz.
    pub fn tree_guides(
        &self,
        children: &std::collections::BTreeMap<String, Vec<String>>,
        id: &str,
    ) -> Vec<bool> {
        let mut guides = vec![];
        let mut cur = id.to_string();
        let mut seen = std::collections::BTreeSet::new();
        while guides.len() < 4 {
            let parent = self
                .tasks
                .iter()
                .find(|t| t.task.id == cur)
                .and_then(|t| {
                    t.task
                        .links
                        .iter()
                        .find(|l| l.kind == LinkKind::Subtask && !l.to.contains('/'))
                })
                .map(|l| l.to.clone());
            let Some(p) = parent else { break };
            if !children.contains_key(&p) || !seen.insert(p.clone()) {
                break;
            }
            guides.push(children[&p].last() == Some(&cur));
            cur = p;
        }
        guides
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
            Mode::NewList { .. } => self.key_newlist(key),
            Mode::Form { .. } => self.key_form(key),
            Mode::Confirm { .. } => self.key_confirm(key),
            Mode::Pick { .. } => self.key_pick(key),
            Mode::Notes { .. } => self.key_notes(key),
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
            // `a` agrega al final / `i` inserta: agregar tarea o lista según
            // el panel enfocado (la inserción en posición espera orden manual).
            KeyCode::Char('a' | 'i') => match self.focus {
                Focus::Tasks => {
                    self.mode = Mode::Input {
                        kind: InputKind::AddTask,
                        buf: Line::new(""),
                    };
                }
                Focus::Lists => {
                    self.mode = Mode::NewList {
                        name: Line::new(""),
                        id: Line::new(""),
                        focus: 0,
                        id_dirty: false,
                    }
                }
            },
            KeyCode::Char('n') => {
                self.mode = Mode::NewList {
                    name: Line::new(""),
                    id: Line::new(""),
                    focus: 0,
                    id_dirty: false,
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
            KeyCode::Char('L') if self.focus == Focus::Tasks => self.link_start(),
            KeyCode::Char('U') if self.focus == Focus::Tasks => self.unlink_start(),
            KeyCode::Char('o') if self.focus == Focus::Tasks => {
                if let Some((id, _)) = self.selected_task() {
                    self.mode = Mode::Notes { id, offset: 0 };
                }
            }
            KeyCode::Char('p') if self.focus == Focus::Tasks => self.paste_asset(),
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

    /// Nueva lista: nombre + id. El id se autocompleta del nombre
    /// (minúsculas, espacios→`_`) hasta que se edita a mano.
    fn key_newlist(&mut self, key: KeyEvent) {
        let Mode::NewList {
            name,
            id,
            focus,
            id_dirty,
        } = &mut self.mode
        else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                *focus = 1 - *focus;
            }
            KeyCode::Enter => {
                let id_text = id.text().trim().to_string();
                let name_text = name.text().trim().to_string();
                self.mode = Mode::Normal;
                self.submit_newlist(name_text, id_text);
            }
            _ => {
                let buf = if *focus == 0 { &mut *name } else { &mut *id };
                if buf.key(&key) {
                    if *focus == 0 && !*id_dirty {
                        // Autocompletar el id mientras no se haya tocado a mano.
                        let slug = id_slug(&buf.text());
                        *id = Line::new(slug);
                    } else if *focus == 1 {
                        *id_dirty = true;
                    }
                }
            }
        }
    }

    fn submit_newlist(&mut self, name: String, id: String) {
        if id.is_empty() {
            return;
        }
        let name = (!name.is_empty()).then_some(name);
        match self
            .chan
            .create_list(&id, name.as_deref(), lrtodo_store::Layout::File)
        {
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
        let title = self
            .visible()
            .get(self.sel_task)
            .map(|t| t.task.title.clone())
            .unwrap_or_default();
        self.mode = Mode::Pick {
            title: format!("mover «{title}» a:"),
            options: self
                .lists
                .iter()
                .map(|r| r.info.meta.name.clone())
                .collect(),
            sel: self.sel_list,
            next: PickNext::MoveTask { id },
        };
    }

    /// `L`: linkear la tarea seleccionada — primero el kind, luego el destino.
    fn link_start(&mut self) {
        let v = self.visible();
        let Some(t) = v.get(self.sel_task) else {
            return;
        };
        if v.len() < 2 {
            self.flash = Some(("no hay otra tarea".into(), Instant::now()));
            return;
        }
        self.mode = Mode::Pick {
            title: format!("«{}» es…", t.task.title),
            options: vec![
                "subtarea de…".into(),
                "bloquea a…".into(),
                "relacionada con…".into(),
            ],
            sel: 0,
            next: PickNext::LinkKind {
                src: t.task.id.clone(),
            },
        };
    }

    /// `U`: quitar un link de la tarea seleccionada.
    fn unlink_start(&mut self) {
        let v = self.visible();
        let Some(t) = v.get(self.sel_task) else {
            return;
        };
        if t.task.links.is_empty() {
            self.flash = Some((
                "«{}» no tiene links".replace("{}", &t.task.title),
                Instant::now(),
            ));
            return;
        }
        let options: Vec<String> = t
            .task
            .links
            .iter()
            .map(|l| format!("{} {}", kind_label(l.kind), self.task_label(&l.to)))
            .collect();
        let dsts: Vec<(String, LinkKind)> = t
            .task
            .links
            .iter()
            .map(|l| (l.to.clone(), l.kind))
            .collect();
        self.mode = Mode::Pick {
            title: format!("quitar link de «{}»:", t.task.title),
            options,
            sel: 0,
            next: PickNext::Unlink {
                src: t.task.id.clone(),
                dsts,
            },
        };
    }

    /// Cómo mostrar el destino de un link: título si es de esta lista.
    fn task_label(&self, dst: &str) -> String {
        match dst.split_once('/') {
            Some((list, id)) => format!("{list}/{id}"),
            None => self
                .tasks
                .iter()
                .find(|t| t.task.id == dst)
                .map(|t| t.task.title.clone())
                .unwrap_or_else(|| dst.to_string()),
        }
    }

    fn key_pick(&mut self, key: KeyEvent) {
        let Mode::Pick { options, sel, .. } = &mut self.mode else {
            return;
        };
        let n = options.len();
        if n == 0 {
            self.mode = Mode::Normal;
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => *sel = (*sel + 1) % n,
            KeyCode::Char('k') | KeyCode::Up => *sel = (*sel + n - 1) % n,
            KeyCode::Enter => {
                let Mode::Pick { next, sel, .. } = std::mem::replace(&mut self.mode, Mode::Normal)
                else {
                    return;
                };
                self.pick_enter(next, sel);
            }
            _ => {}
        }
    }

    fn pick_enter(&mut self, next: PickNext, sel: usize) {
        match next {
            PickNext::MoveTask { id } => {
                let to = self.lists.get(sel).map(|r| r.info.meta.id.clone());
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
            PickNext::LinkKind { src } => {
                let kind = [LinkKind::Subtask, LinkKind::Blocks, LinkKind::Related][sel];
                let dsts: Vec<String> = self
                    .tasks
                    .iter()
                    .filter(|t| t.task.id != src)
                    .map(|t| t.task.id.clone())
                    .collect();
                let options: Vec<String> = self
                    .tasks
                    .iter()
                    .filter(|t| t.task.id != src)
                    .map(|t| t.task.title.clone())
                    .collect();
                self.mode = Mode::Pick {
                    title: format!("{} de:", kind_label(kind)),
                    options,
                    sel: 0,
                    next: PickNext::LinkDst { src, kind, dsts },
                };
            }
            PickNext::LinkDst { src, kind, dsts } => {
                let (Some(list), Some(dst)) = (self.current_list_id(), dsts.get(sel)) else {
                    return;
                };
                let r = self.chan.link(&list, &src, dst, kind);
                if self.try_call("linkear", r).is_some() {
                    self.refresh_all();
                }
            }
            PickNext::Unlink { src, dsts } => {
                let (Some(list), Some((dst, kind))) = (self.current_list_id(), dsts.get(sel))
                else {
                    return;
                };
                let r = self.chan.unlink(&list, &src, dst, *kind);
                if self.try_call("deslinkear", r).is_some() {
                    self.refresh_all();
                }
            }
        }
    }

    /// Mouse: click en un panel lo enfoca y selecciona la fila; click sobre
    /// el `[ ]` tilda; la rueda scrollea el panel bajo el cursor.
    pub fn mouse(&mut self, ev: MouseEvent) {
        if !matches!(self.mode, Mode::Normal) {
            return;
        }
        let (x, y) = (ev.column, ev.row);
        let in_area = |a: Rect| x >= a.x && x < a.x + a.width && y >= a.y && y < a.y + a.height;
        match ev.kind {
            MouseEventKind::ScrollUp if in_area(self.tasks_area) => self.move_sel(-3),
            MouseEventKind::ScrollDown if in_area(self.tasks_area) => self.move_sel(3),
            MouseEventKind::ScrollUp if in_area(self.lists_area) => {
                self.focus = Focus::Lists;
                self.move_sel(-1);
            }
            MouseEventKind::ScrollDown if in_area(self.lists_area) => {
                self.focus = Focus::Lists;
                self.move_sel(1);
            }
            MouseEventKind::Down(MouseButton::Left) if in_area(self.lists_area) => {
                self.focus = Focus::Lists;
                let row = self
                    .lists_offset
                    .saturating_add((y - self.lists_area.y - 1) as usize);
                if row < self.lists.len() {
                    self.sel_list = row;
                    self.refresh_tasks();
                }
            }
            MouseEventKind::Down(MouseButton::Left) if in_area(self.tasks_area) => {
                self.focus = Focus::Tasks;
                let row = self
                    .tasks_offset
                    .saturating_add((y - self.tasks_area.y - 1) as usize);
                let id = {
                    let v = self.visible();
                    match v.get(row) {
                        Some(tv) => tv.task.id.clone(),
                        None => return,
                    }
                };
                self.sel_task = row;
                // ¿Sobre el casillero? Borde (1) + árbol (3 por guía) + "[ ]".
                let children = self.children_map();
                let guides = self.tree_guides(&children, &id);
                let check_x = self.tasks_area.x + 1 + guides.len() as u16 * 3;
                if x >= check_x && x < check_x + 4 {
                    self.toggle();
                }
            }
            _ => {}
        }
    }

    /// j/k scrollea, `e` edita la nota en $EDITOR, esc/q cierra.
    fn key_notes(&mut self, key: KeyEvent) {
        let Mode::Notes { id, .. } = &self.mode else {
            return;
        };
        let id = id.clone();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                if let Mode::Notes { offset, .. } = &mut self.mode {
                    *offset = offset.saturating_add(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Mode::Notes { offset, .. } = &mut self.mode {
                    *offset = offset.saturating_sub(1);
                }
            }
            KeyCode::Char('e' | 'E') => {
                if let Some(list) = self.current_list_id() {
                    self.effect = Some(Effect::EditNotes { list, id });
                    self.mode = Mode::Normal;
                }
            }
            _ => {}
        }
    }

    /// `p`: pega la imagen del clipboard como adjunto de la tarea y
    /// appendea `![img](ruta)` a la nota. Usa `pngpaste` (brew install pngpaste).
    fn paste_asset(&mut self) {
        let Some(list) = self.current_list_id() else {
            return;
        };
        let Some((id, _)) = self.selected_task() else {
            return;
        };
        let tmp = std::env::temp_dir().join(format!("lrtodo-{}.png", std::process::id()));
        let ok = std::process::Command::new("pngpaste")
            .arg(&tmp)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            self.flash = Some((
                "no hay imagen en el clipboard (¿falta `brew install pngpaste`?)".into(),
                Instant::now(),
            ));
            return;
        }
        let r = std::fs::read(&tmp).map_err(|e| anyhow::anyhow!("{e}"));
        let _ = std::fs::remove_file(&tmp);
        let result = r.and_then(|data| self.chan.add_asset(&list, &id, "img.png", &data));
        match result {
            Ok(rel) => {
                // Append `![img](rel)` a la nota actual.
                let notes = self
                    .tasks
                    .iter()
                    .find(|t| t.task.id == id)
                    .and_then(|t| t.task.notes.clone())
                    .unwrap_or_default();
                let notes = format!("{notes}\n\n![img]({rel})\n");
                let r = self.chan.set(&list, &id, Change::Notes(Some(notes)));
                if self.try_call("adjuntar", r).is_some() {
                    self.refresh_tasks();
                    self.flash = Some((format!("adjunto en {rel}"), Instant::now()));
                }
            }
            Err(e) => self.flash = Some((format!("adjunto: {e:#}"), Instant::now())),
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

/// Id sugerido para una lista: minúsculas, espacios/no-alfanumérico → `_`.
fn id_slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_us = true; // evita `_` inicial
    for c in name.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_alphanumeric() || c == '-' {
            slug.push(c);
            last_us = false;
        } else if !last_us {
            slug.push('_');
            last_us = true;
        }
    }
    slug.trim_end_matches('_').to_string()
}
