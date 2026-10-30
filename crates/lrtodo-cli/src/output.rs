use std::io::IsTerminal;

use lrtodo_core::{ListKind, TaskView};

use lrtodo_client::ListRow;

struct Style {
    on: bool,
}

impl Style {
    fn new() -> Self {
        Self {
            on: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }
    fn paint(&self, code: &str, s: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
    fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }
    fn bold(&self, s: &str) -> String {
        self.paint("1", s)
    }
    fn cyan(&self, s: &str) -> String {
        self.paint("36", s)
    }
    fn yellow(&self, s: &str) -> String {
        self.paint("33", s)
    }
    fn red(&self, s: &str) -> String {
        self.paint("31", s)
    }
}

pub use lrtodo_client::ago;

pub fn lists(rows: &[ListRow]) {
    let st = Style::new();
    if rows.is_empty() {
        println!("No hay listas. Creá una con: lrtodo new compras --name Compras");
        return;
    }
    let width = rows
        .iter()
        .map(|r| r.info.meta.id.chars().count())
        .max()
        .unwrap_or(0);
    for r in rows {
        let id = &r.info.meta.id;
        let pad = " ".repeat(width - id.chars().count());
        let mut line = format!(
            "{}{pad}  {:>3}/{:<3} {}",
            st.cyan(id),
            r.open,
            r.total,
            st.bold(&r.info.meta.name)
        );
        if r.info.meta.kind == ListKind::HomeAssistant {
            line.push_str(&st.dim("  [Home Assistant]"));
        }
        if r.pending > 0 {
            line.push_str(&st.yellow(&format!("  {} sin sincronizar", r.pending)));
        }
        if r.info.pending_upstream > 0 {
            line.push_str(&st.yellow(&format!("  {} pendiente(s) en HA", r.info.pending_upstream)));
        }
        if let Some(e) = &r.info.error {
            line.push_str(&st.red(&format!("  error: {e}")));
        }
        println!("{line}");
    }
}

/// Mapa padre → hijos (ids, en orden), sólo links `subtask` resolubles acá.
fn children_map(tasks: &[TaskView]) -> std::collections::BTreeMap<String, Vec<String>> {
    let ids: std::collections::BTreeSet<&str> = tasks.iter().map(|t| t.task.id.as_str()).collect();
    let mut map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for t in tasks {
        if let Some(l) = t
            .task
            .links
            .iter()
            .find(|l| l.kind == lrtodo_core::LinkKind::Subtask && ids.contains(l.to.as_str()))
        {
            map.entry(l.to.clone()).or_default().push(t.task.id.clone());
        }
    }
    map
}

/// Guías del árbol: `[propio es-último, padre es-último, abuelo es-último…]`
/// para dibujar `├─`/`└─` y las verticales `│`. Vacío = raíz.
fn tree_guides(
    tasks: &[TaskView],
    children: &std::collections::BTreeMap<String, Vec<String>>,
    id: &str,
) -> Vec<bool> {
    let mut guides = vec![];
    let mut cur = id.to_string();
    let mut seen = std::collections::BTreeSet::new();
    while guides.len() < 4 {
        let parent = tasks
            .iter()
            .find(|t| t.task.id == cur)
            .and_then(|t| {
                t.task
                    .links
                    .iter()
                    .find(|l| l.kind == lrtodo_core::LinkKind::Subtask && !l.to.contains('/'))
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

/// Prefijo visual del árbol: `│   ` por ancestros no-últimos y `├─`/`└─`.
fn tree_prefix(guides: &[bool]) -> String {
    let mut s = String::new();
    for i in (1..guides.len()).rev() {
        s.push_str(if guides[i] { "    " } else { "│   " });
    }
    if !guides.is_empty() {
        s.push_str(if guides[0] {
            "└── "
        } else {
            "├── "
        });
    }
    s
}

pub fn tasks(name: &str, tasks: &[TaskView], only_open: bool) {
    let st = Style::new();
    let open = tasks.iter().filter(|t| !t.task.done).count();
    println!(
        "{} {}",
        st.bold(name),
        st.dim(&format!("({open} abiertas de {})", tasks.len()))
    );
    let width = tasks.len().to_string().len();
    let children = children_map(tasks);
    for (i, tv) in tasks.iter().enumerate() {
        let t = &tv.task;
        if only_open && t.done {
            continue;
        }
        let check = if t.done { "[x]" } else { "[ ]" };
        let prefix = tree_prefix(&tree_guides(tasks, &children, &t.id));
        let mut line = format!("{:>width$} {check} {prefix}", i + 1);
        if tv.blocked && !t.done {
            line.push_str(&st.red("⊘ "));
        }
        line.push_str(&if t.done || tv.blocked {
            st.dim(&t.title)
        } else {
            t.title.clone()
        });
        if let Some(p) = t.priority {
            line.push_str(&st.yellow(&format!(" !{p}")));
        }
        for tag in &t.tags {
            line.push_str(&st.cyan(&format!(" #{tag}")));
        }
        if let Some(due) = &t.due {
            line.push_str(&st.yellow(&format!(" (vence {due})")));
        }
        if let Some(notes) = &t.notes {
            let first = notes.lines().next().unwrap_or("");
            line.push_str(&st.dim(&format!(" · {first}")));
        }
        for l in &t.links {
            if l.kind != lrtodo_core::LinkKind::Subtask {
                let kind = match l.kind {
                    lrtodo_core::LinkKind::Blocks => "bloquea",
                    _ => "~",
                };
                line.push_str(&st.dim(&format!(" {kind}:{}", l.to)));
            }
        }
        if tv.pending {
            line.push_str(&st.yellow(" *"));
        }
        println!("{line}");
    }
}
