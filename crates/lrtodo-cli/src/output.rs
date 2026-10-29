use std::io::IsTerminal;

use lrtodo_core::{ListKind, TaskView};

use crate::backend::ListRow;

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

pub fn ago(ms: u64) -> String {
    let s = ms / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86400),
    }
}

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

/// Profundidad de subtarea: cuántos padres tiene encima (máx 4, seguro ante ciclos).
fn depth_of(tasks: &[TaskView], id: &str) -> usize {
    let mut depth = 0;
    let mut cur = id.to_string();
    let ids: std::collections::BTreeSet<&str> = tasks.iter().map(|t| t.task.id.as_str()).collect();
    let mut seen = std::collections::BTreeSet::new();
    while depth < 4 {
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
        match parent {
            Some(p) if ids.contains(p.as_str()) && seen.insert(p.clone()) => {
                depth += 1;
                cur = p;
            }
            _ => break,
        }
    }
    depth
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
    for (i, tv) in tasks.iter().enumerate() {
        let t = &tv.task;
        if only_open && t.done {
            continue;
        }
        let check = if t.done { "[x]" } else { "[ ]" };
        let indent = "  ".repeat(depth_of(tasks, &t.id));
        let mut line = format!("{:>width$} {check} {indent}", i + 1);
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
