//! Dibujado con ratatui: paneles de listas y tareas, barra de estado y popups.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use super::app::{App, Focus, InputKind, Mode};
use crate::output::ago;
use lrtodo_client::SyncState;
use lrtodo_core::{LinkKind, ListKind, TaskView};
use lrtodo_store::now_ms;

const ACCENT: Color = Color::Cyan;
const WARN: Color = Color::Yellow;
const ERR: Color = Color::Red;
const OK: Color = Color::Green;
/// Fondos por panel: el enfocado un gris apenas más claro, el otro más tenue.
const FOCUS_BG: Color = Color::Indexed(236);
const BLUR_BG: Color = Color::Indexed(233);

fn bg(focused: bool) -> Color {
    if focused { FOCUS_BG } else { BLUR_BG }
}

fn sel_style() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

fn border(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    // Línea superior: nombre + versión derivada de git.
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                " lrtodo",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", env!("LRTODO_VERSION")),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        chunks[0],
    );
    let main = Layout::horizontal([Constraint::Length(28), Constraint::Min(20)]).split(chunks[1]);
    draw_lists(f, app, main[0]);
    draw_tasks(f, app, main[1]);
    draw_status(f, app, chunks[2]);
    draw_keys(f, app, chunks[3]);
    match &app.mode {
        Mode::Input { .. } => draw_input(f, app, area),
        Mode::NewList { .. } => draw_newlist(f, app, area),
        Mode::Form { .. } => draw_form(f, app, area),
        Mode::Confirm { .. } => draw_confirm(f, app, area),
        Mode::Pick { .. } => draw_pick(f, app, area),
        Mode::Notes { .. } => draw_notes(f, app, area),
        Mode::Help => draw_help(f, area),
        Mode::Normal => {}
    }
}

// ---------------------------------------------------------------- paneles

fn draw_lists(f: &mut Frame, app: &mut App, area: Rect) {
    app.lists_area = area;
    let items: Vec<ListItem> = app
        .lists
        .iter()
        .map(|r| {
            let meta = &r.info.meta;
            let mut spans = vec![
                Span::raw(format!("{}/{}", r.open, r.total)),
                Span::raw(" "),
                Span::styled(&meta.name, Style::default().add_modifier(Modifier::BOLD)),
            ];
            if r.pending > 0 {
                spans.push(Span::styled(
                    format!(" *{}", r.pending),
                    Style::default().fg(WARN),
                ));
            }
            if meta.kind == ListKind::HomeAssistant {
                spans.push(Span::styled(" HA", Style::default().fg(Color::Magenta)));
            }
            if r.info.error.is_some() {
                spans.push(Span::styled(" !", Style::default().fg(ERR)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();
    let title = format!(" Listas ({}) ", app.lists.len());
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(app.focus == Focus::Lists))
                .style(Style::default().bg(bg(app.focus == Focus::Lists)))
                .title(title),
        )
        .highlight_style(sel_style());
    let mut state = ListState::default();
    if !app.lists.is_empty() {
        state.select(Some(app.sel_list));
    }
    f.render_stateful_widget(list, area, &mut state);
    app.lists_offset = state.offset();
}

fn draw_tasks(f: &mut Frame, app: &mut App, area: Rect) {
    app.tasks_area = area;
    let visible = app.visible();
    let open = visible.iter().filter(|t| !t.task.done).count();
    let name = app
        .lists
        .get(app.sel_list)
        .map(|r| r.info.meta.name.clone())
        .unwrap_or_else(|| "—".into());
    let mut title = format!(" {name} ({open} abiertas) ");
    if !app.filter.is_empty() {
        title.push_str(&format!("· /{} ", app.filter));
    }
    if !app.show_done {
        title.push_str("· hechas ocultas ");
    }
    let children = app.children_map();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|tv| ListItem::new(task_line(tv, &app.tree_guides(&children, &tv.task.id))))
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border(app.focus == Focus::Tasks))
                .style(Style::default().bg(bg(app.focus == Focus::Tasks)))
                .title(title),
        )
        .highlight_style(sel_style());
    let mut state = ListState::default();
    if !visible.is_empty() {
        state.select(Some(app.sel_task.min(visible.len() - 1)));
    }
    f.render_stateful_widget(list, area, &mut state);
    app.tasks_offset = state.offset();
}

fn task_line(tv: &TaskView, guides: &[bool]) -> Line<'static> {
    let t = &tv.task;
    let check = if t.done { "[x] " } else { "[ ] " };
    let title_style = if t.done {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::CROSSED_OUT)
    } else if tv.blocked {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default()
    };
    // Árbol: `│  ` por ancestros no-últimos y `├─`/`└─` en el propio nivel.
    let mut tree = String::new();
    for i in (1..guides.len()).rev() {
        tree.push_str(if guides[i] { "   " } else { "│  " });
    }
    if !guides.is_empty() {
        tree.push_str(if guides[0] { "└─ " } else { "├─ " });
    }
    let mut spans = vec![
        Span::styled(tree, Style::default().fg(Color::DarkGray)),
        Span::styled(check, Style::default().fg(if t.done { OK } else { ACCENT })),
    ];
    if tv.blocked && !t.done {
        spans.push(Span::styled("⊘ ", Style::default().fg(ERR)));
    }
    spans.push(Span::styled(t.title.clone(), title_style));
    if let Some(p) = t.priority {
        spans.push(Span::styled(format!(" !{p}"), Style::default().fg(WARN)));
    }
    for tag in &t.tags {
        spans.push(Span::styled(
            format!(" #{tag}"),
            Style::default().fg(ACCENT),
        ));
    }
    if let Some(due) = &t.due {
        spans.push(Span::styled(format!(" ({due})"), Style::default().fg(WARN)));
    }
    if let Some(notes) = &t.notes
        && let Some(first) = notes.lines().next()
        && !first.is_empty()
    {
        spans.push(Span::styled(
            format!(" · {first}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    for l in &t.links {
        if l.kind != LinkKind::Subtask {
            let kind = if l.kind == LinkKind::Blocks {
                "bloquea"
            } else {
                "~"
            };
            spans.push(Span::styled(
                format!(" {kind}:{}", l.to),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }
    if tv.pending {
        spans.push(Span::styled(" ●", Style::default().fg(WARN)));
    }
    Line::from(spans)
}

// ---------------------------------------------------------------- barras

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![];
    match &app.state {
        SyncState::Local => {
            spans.push(Span::styled(" local", Style::default().fg(Color::DarkGray)));
        }
        SyncState::Remote {
            pending,
            last_sync_ms,
            last_error,
            ..
        } => {
            if app.syncing {
                spans.push(Span::styled(" sincronizando…", Style::default().fg(ACCENT)));
            } else if last_error.is_some() {
                spans.push(Span::styled(" sin conexión", Style::default().fg(ERR)));
            } else {
                spans.push(Span::styled(" al día", Style::default().fg(OK)));
            }
            if *pending > 0 {
                spans.push(Span::styled(
                    format!(" · {pending} pendiente(s)"),
                    Style::default().fg(WARN),
                ));
            }
            if let Some(ms) = last_sync_ms {
                spans.push(Span::styled(
                    format!(" · hace {}", ago(now_ms().saturating_sub(*ms))),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }
    }
    // Error de la lista actual o aviso del servidor.
    let note = app
        .lists
        .get(app.sel_list)
        .and_then(|r| r.info.error.clone())
        .or_else(|| {
            app.flash
                .as_ref()
                .filter(|(_, at)| at.elapsed().as_secs() < 10)
                .map(|(m, _)| m.clone())
        });
    if let Some(note) = note {
        let text = format!("{note} ");
        let width = area.width as usize;
        let left: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        if left + 2 < width {
            let pad = width.saturating_sub(left + text.chars().count());
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(text, Style::default().fg(WARN)));
        }
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_keys(f: &mut Frame, app: &App, area: Rect) {
    let hint = match &app.mode {
        Mode::Normal => match app.focus {
            Focus::Lists => {
                " j/k lista · enter ir a tareas · a/i nueva · D borrar · ? ayuda · q salir"
            }
            Focus::Tasks => {
                " espacio tildar · a/i agregar · e editar · d borrar · m mover · L link · U unlink · / filtrar · v hechas · E editor · r sync · ? ayuda · q salir"
            }
        },
        Mode::Input { .. } => " enter aceptar · esc cancelar",
        Mode::NewList { .. } => " tab/↑/↓ campo · enter crear · esc cancelar",
        Mode::Form { .. } => " tab/↑/↓ campo · enter guardar · esc cancelar",
        Mode::Confirm { .. } => " s confirmar · cualquier otra tecla cancela",
        Mode::Pick { .. } => " j/k elegir · enter aceptar · esc cancelar",
        Mode::Notes { .. } => " j/k scrollear · e editar en $EDITOR · esc/q cerrar",
        Mode::Help => " cualquier tecla para volver",
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(Color::DarkGray),
        ))),
        area,
    );
}

// ---------------------------------------------------------------- popups

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn popup<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(format!(" {title} "))
}

/// Nueva lista: dos campos (nombre arriba, id autocompletado abajo).
fn draw_newlist(f: &mut Frame, app: &mut App, area: Rect) {
    let Mode::NewList {
        name, id, focus, ..
    } = &app.mode
    else {
        return;
    };
    let rect = centered(area, 56, 4);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("nombre: ", Style::default().fg(ACCENT)),
                Span::raw(name.text()),
            ]),
            Line::from(vec![
                Span::styled(
                    "id: ",
                    Style::default().fg(if *focus == 1 { ACCENT } else { Color::DarkGray }),
                ),
                Span::raw(format!("    {}", id.text())),
            ]),
        ])
        .block(popup("Nueva lista")),
        rect,
    );
    // Cursor en el campo activo (borde 1 + label).
    let (y, label_w, buf) = if *focus == 0 {
        (rect.y + 1, 8u16, name)
    } else {
        (rect.y + 2, 8u16, id)
    };
    f.set_cursor_position((rect.x + 1 + label_w + buf.pos as u16, y));
}

fn draw_input(f: &mut Frame, app: &mut App, area: Rect) {
    let Mode::Input { kind, buf } = &app.mode else {
        return;
    };
    let (title, prompt) = match kind {
        InputKind::AddTask => ("Agregar tarea", "tarea".to_string()),
        InputKind::Filter => ("Filtrar", "texto".to_string()),
        InputKind::DeleteList(id) => ("Borrar lista", format!("escribí «{id}»")),
    };
    let danger = matches!(kind, InputKind::DeleteList(_));
    // Ancho según el contenido (hasta ~80% de la terminal) y scroll
    // horizontal si el texto es más largo que el popup.
    let prompt_w = prompt.chars().count() + 2; // "prompt: "
    let text_w = prompt_w + buf.chars.len();
    let w = (text_w as u16 + 3).clamp(40, area.width.saturating_sub(4).max(40));
    let rect = centered(area, w, 3);
    let inner_w = rect.width.saturating_sub(3) as usize; // borde + margen
    let offset = (prompt_w + buf.pos).saturating_sub(inner_w);
    f.render_widget(Clear, rect);
    let mut block = popup(title);
    if danger {
        block = block.border_style(Style::default().fg(ERR));
    }
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("{prompt}: "),
                Style::default().fg(if danger { ERR } else { ACCENT }),
            ),
            Span::raw(buf.text()),
        ]))
        .block(block)
        .scroll((0, offset as u16)),
        rect,
    );
    // Borde (1) + "prompt: " + posición dentro del texto, menos el scroll.
    let x = rect.x + 3 + (prompt_w + buf.pos) as u16 - offset as u16;
    f.set_cursor_position((x.min(rect.x + rect.width - 2), rect.y + 1));
}

fn draw_form(f: &mut Frame, app: &mut App, area: Rect) {
    let Mode::Form { fields, sel, .. } = &app.mode else {
        return;
    };
    let label_w = fields.iter().map(|f| f.label.len()).max().unwrap_or(0) as u16;
    let rect = centered(area, 60, fields.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let lines: Vec<Line> = fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let style = if i == *sel {
                Style::default().fg(ACCENT)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(
                    format!("{:>label_w$}: ", field.label, label_w = label_w as usize),
                    style,
                ),
                Span::styled(field.buf.text(), style),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(popup("Editar tarea")), rect);
    let cur = &fields[*sel];
    // Borde (1) + "label: " (label_w + 2) + posición dentro del texto.
    let x = rect.x + 3 + label_w + cur.buf.pos as u16;
    f.set_cursor_position((x.min(rect.x + rect.width - 2), rect.y + 1 + *sel as u16));
}

fn draw_confirm(f: &mut Frame, app: &mut App, area: Rect) {
    let Mode::Confirm { text, .. } = &app.mode else {
        return;
    };
    let rect = centered(area, 50, 3);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(text.clone())).block(popup("Confirmar")),
        rect,
    );
}

fn draw_pick(f: &mut Frame, app: &mut App, area: Rect) {
    let Mode::Pick {
        title,
        options,
        sel,
        ..
    } = &app.mode
    else {
        return;
    };
    let w = (options
        .iter()
        .map(|o| o.chars().count())
        .max()
        .unwrap_or(10) as u16
        + 6)
    .clamp(4, area.width.saturating_sub(2).max(4));
    let rect = centered(area, w, (options.len() as u16 + 2).min(14));
    f.render_widget(Clear, rect);
    let items: Vec<ListItem> = options
        .iter()
        .map(|o| ListItem::new(Line::from(Span::raw(o.clone()))))
        .collect();
    let list = List::new(items)
        .block(popup(title))
        .highlight_style(sel_style());
    let mut state = ListState::default();
    state.select(Some(*sel));
    f.render_stateful_widget(list, rect, &mut state);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rows: &[(&str, &str)] = &[
        ("j / k / ↑ / ↓", "moverse"),
        ("tab · h · l", "cambiar de panel"),
        ("enter", "en listas: ir a tareas · en tareas: tildar"),
        ("espacio", "tildar la tarea"),
        ("a / i", "agregar (tarea o lista, según el panel)"),
        ("e", "editar tarea (formulario)"),
        ("d", "borrar tarea (pide confirmación)"),
        ("m", "mover a otra lista"),
        ("n", "crear lista (id|nombre)"),
        ("/", "filtrar por título o tag"),
        ("v", "mostrar/ocultar completadas"),
        ("E", "abrir la lista en $EDITOR"),
        ("r", "sincronizar ahora"),
        ("esc", "limpiar filtro / cerrar popup"),
        ("q", "salir"),
    ];
    let rect = centered(area, 62, rows.len() as u16 + 2);
    f.render_widget(Clear, rect);
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| {
            Line::from(vec![
                Span::styled(format!("{k:>16}  "), Style::default().fg(ACCENT)),
                Span::raw(*d),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(popup("Teclas")), rect);
}

// ---------------------------------------------------------- notas (markdown)

fn draw_notes(f: &mut Frame, app: &App, area: Rect) {
    let Mode::Notes { id, offset } = &app.mode else {
        return;
    };
    let task = app.tasks.iter().find(|t| t.task.id == *id);
    let title = task
        .map(|t| t.task.title.clone())
        .unwrap_or_else(|| "nota".into());
    let text = task
        .and_then(|t| t.task.notes.clone())
        .unwrap_or_else(|| "(sin nota — `e` para crearla)".into());
    let lines = md_lines(&text);
    let rect = centered(
        area,
        70,
        (lines.len() as u16 + 2).clamp(6, area.height.saturating_sub(2)),
    );
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines)
            .block(popup(&format!("{title} — notas")))
            .scroll((*offset as u16, 0)),
        rect,
    );
}

/// Markdown → líneas con estilo (pulldown-cmark → spans de ratatui).
fn md_lines(text: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};

    let mut lines: Vec<Line<'static>> = vec![];
    let mut spans: Vec<Span<'static>> = vec![];
    let mut stack: Vec<Style> = vec![];
    let mut style = Style::default();
    let flush = flush_line;
    let mut list_depth = 0usize;
    let mut ordered_next = 0usize;

    for ev in Parser::new(text) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut lines, &mut spans);
                let color = match level {
                    HeadingLevel::H1 => ACCENT,
                    HeadingLevel::H2 => WARN,
                    _ => Color::White,
                };
                stack.push(style);
                style = style.add_modifier(Modifier::BOLD).fg(color);
                spans.push(Span::styled(
                    format!("{} ", "#".repeat(level as usize)),
                    style,
                ));
            }
            Event::End(TagEnd::Heading(_)) => {
                flush(&mut lines, &mut spans);
                style = stack.pop().unwrap_or_default();
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => flush(&mut lines, &mut spans),
            Event::Start(Tag::Emphasis) => {
                stack.push(style);
                style = style.add_modifier(Modifier::ITALIC);
            }
            Event::End(TagEnd::Emphasis)
            | Event::End(TagEnd::Strong)
            | Event::End(TagEnd::Link)
            | Event::End(TagEnd::Image) => {
                style = stack.pop().unwrap_or_default();
            }
            Event::Start(Tag::Strong) => {
                stack.push(style);
                style = style.add_modifier(Modifier::BOLD);
            }
            Event::Start(Tag::Link { .. }) | Event::Start(Tag::Image { .. }) => {
                stack.push(style);
                style = style.fg(ACCENT).add_modifier(Modifier::UNDERLINED);
            }
            Event::Start(Tag::CodeBlock(_)) => {
                stack.push(style);
                style = style.fg(Color::Green);
            }
            Event::End(TagEnd::CodeBlock) => {
                style = stack.pop().unwrap_or_default();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                stack.push(style);
                style = style.fg(Color::DarkGray);
                spans.push(Span::styled("│ ", style));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                style = stack.pop().unwrap_or_default();
            }
            Event::Start(Tag::List(first)) => {
                list_depth += 1;
                ordered_next = first.map(|f| f as usize).unwrap_or(0);
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                let bullet = if ordered_next > 0 {
                    let b = format!("{ordered_next}. ");
                    ordered_next += 1;
                    b
                } else {
                    "- ".to_string()
                };
                spans.push(Span::raw(format!("{indent}{bullet}")));
            }
            Event::End(TagEnd::Item) => flush(&mut lines, &mut spans),
            Event::Text(t) => spans.push(Span::styled(t.to_string(), style)),
            Event::Code(t) => spans.push(Span::styled(
                format!(" {t} "),
                Style::default().fg(Color::Yellow).bg(Color::Indexed(235)),
            )),
            Event::SoftBreak => spans.push(Span::raw(" ")),
            Event::HardBreak => flush(&mut lines, &mut spans),
            Event::Rule => {
                flush(&mut lines, &mut spans);
                lines.push(Line::from(Span::styled(
                    "─".repeat(30),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            _ => {}
        }
    }
    if !spans.is_empty() {
        lines.push(spans.into());
    }
    if lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines
}

fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    lines.push(std::mem::take(spans).into());
}
