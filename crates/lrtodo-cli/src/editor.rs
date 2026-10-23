//! `lrtodo edit <lista>`: abre la lista en `$EDITOR` y aplica lo editado como
//! operaciones (merge a 3 vías: lo que no tocaste no pisa cambios ajenos).

use std::collections::BTreeMap;
use std::io::{BufRead, Write};

use anyhow::{Context, bail};
use lrtodo_core::{Caps, Change, Task};
use serde::{Deserialize, Serialize};

use crate::backend::Backend;

#[derive(Serialize, Deserialize)]
struct Doc {
    #[serde(default)]
    items: Vec<Task>,
}

fn editable(mut t: Task) -> Task {
    t.created = None;
    t.updated = None;
    t.completed = None;
    t
}

pub fn edit(backend: &mut dyn Backend, list: &str) -> anyhow::Result<usize> {
    let caps = backend.caps(list)?;
    let before: Vec<Task> = backend
        .tasks(list)?
        .into_iter()
        .map(|t| editable(t.task))
        .collect();
    let header = format!(
        "# lrtodo: lista {list}. Guardá y cerrá para aplicar; borrá un item para eliminarlo,\n\
         # agregá items sin id para crearlos. Los cambios ajenos hechos mientras editás se respetan.\n"
    );
    let body = serde_yaml_ng::to_string(&Doc {
        items: before.clone(),
    })?;
    let mut tmp = tempfile::Builder::new()
        .prefix("lrtodo-")
        .suffix(".yaml")
        .tempfile()?;
    tmp.write_all(header.as_bytes())?;
    tmp.write_all(body.as_bytes())?;
    tmp.flush()?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".into());
    let after = loop {
        let mut parts = editor.split_whitespace();
        let program = parts.next().unwrap_or("vi");
        let status = std::process::Command::new(program)
            .args(parts)
            .arg(tmp.path())
            .status()
            .with_context(|| format!("no se pudo ejecutar {editor}"))?;
        if !status.success() {
            bail!("el editor terminó con error; no se aplicó nada");
        }
        let text = std::fs::read_to_string(tmp.path())?;
        match serde_yaml_ng::from_str::<Doc>(&text) {
            Ok(doc) => break doc.items,
            Err(e) => {
                eprintln!("YAML inválido: {e}");
                eprint!("¿Volver a editar? [S/n] ");
                let mut answer = String::new();
                std::io::stdin().lock().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    bail!(
                        "cancelado; tu edición quedó en {}",
                        tmp.into_temp_path().keep()?.display()
                    );
                }
            }
        }
    };
    apply_diff(backend, list, &caps, &before, after)
}

pub fn apply_diff(
    backend: &mut dyn Backend,
    list: &str,
    caps: &Caps,
    before: &[Task],
    after: Vec<Task>,
) -> anyhow::Result<usize> {
    let old: BTreeMap<&str, &Task> = before.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut kept = std::collections::BTreeSet::new();
    let mut n = 0;
    for task in after {
        match old.get(task.id.as_str()) {
            Some(prev) if kept.insert(task.id.clone()) => {
                for change in prev.changes_to(&task) {
                    if !caps.supports(change.field()) {
                        continue;
                    }
                    if let Change::Title(t) = &change
                        && t.trim().is_empty()
                    {
                        continue;
                    }
                    backend.set(list, &task.id, change)?;
                    n += 1;
                }
            }
            _ => {
                if task.title.trim().is_empty() {
                    continue;
                }
                let mut t = task;
                t.id = String::new();
                backend.add(list, t)?;
                n += 1;
            }
        }
    }
    for t in before {
        if !kept.contains(&t.id) {
            backend.delete(list, &t.id)?;
            n += 1;
        }
    }
    Ok(n)
}
