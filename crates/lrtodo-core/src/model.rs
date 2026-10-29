use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

pub type ListId = String;
pub type TaskId = String;

/// Una tarea. Es también el formato en disco (YAML), así que el orden y los
/// nombres de los campos están pensados para que sea cómodo editarlo a mano.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "de::string"
    )]
    pub id: TaskId,
    #[serde(default, deserialize_with = "de::string")]
    pub title: String,
    #[serde(
        default,
        skip_serializing_if = "is_false",
        deserialize_with = "de::boolean"
    )]
    pub done: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de::opt_string"
    )]
    pub due: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de::opt_string"
    )]
    pub notes: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de::tags"
    )]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de::opt_string"
    )]
    pub created: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de::opt_string"
    )]
    pub updated: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de::opt_string"
    )]
    pub completed: Option<String>,
    /// Aristas que salen de esta tarea (`subtask`, `blocks`, `related`).
    /// En el doc viven aparte (cada una con su HLC); acá se proyectan para
    /// leer/escribir el YAML — también se pueden editar a mano.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "de::links"
    )]
    pub links: Vec<Link>,
    /// Campos desconocidos (agregados a mano): se preservan tal cual.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Task {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            ..Default::default()
        }
    }

    /// Cambios que llevan esta tarea a `other` (sólo campos sincronizados).
    pub fn changes_to(&self, other: &Task) -> Vec<Change> {
        other
            .all_changes()
            .into_iter()
            .filter(|c| c.differs(self))
            .collect()
    }

    /// Todos los campos sincronizados de la tarea, como cambios.
    pub fn all_changes(&self) -> Vec<Change> {
        vec![
            Change::Title(self.title.clone()),
            Change::Done(self.done),
            Change::Due(self.due.clone()),
            Change::Notes(self.notes.clone()),
            Change::Tags(self.tags.clone()),
            Change::Priority(self.priority),
        ]
    }

    /// Limpia los campos que la lista no soporta.
    pub fn strip_unsupported(&mut self, caps: &Caps) {
        if !caps.notes {
            self.notes = None;
        }
        if !caps.due {
            self.due = None;
        }
        if !caps.tags {
            self.tags.clear();
        }
        if !caps.priority {
            self.priority = None;
        }
        if !caps.extra {
            self.extra.clear();
        }
    }
}

/// Tipo de relación entre tareas.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// `src` es subtarea de `dst` (un padre por tarea: la arista nueva
    /// reemplaza a la anterior).
    Subtask,
    /// `src` bloquea a `dst` (`dst` se muestra como bloqueada hasta que
    /// `src` esté hecha; no impide tildarla).
    Blocks,
    /// Relación suelta, sin semántica extra.
    #[default]
    Related,
}

/// Una arista vista desde la tarea origen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// `id`, o `otra-lista/id` para cruzar listas.
    pub to: String,
    #[serde(default)]
    pub kind: LinkKind,
}

/// Campos sincronizables con reloj propio (LWW por campo).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Done,
    Due,
    Notes,
    Tags,
    Priority,
}

/// Nuevo valor para un campo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "snake_case")]
pub enum Change {
    Title(String),
    Done(bool),
    Due(Option<String>),
    Notes(Option<String>),
    Tags(Vec<String>),
    Priority(Option<u8>),
}

impl Change {
    pub fn field(&self) -> Field {
        match self {
            Change::Title(_) => Field::Title,
            Change::Done(_) => Field::Done,
            Change::Due(_) => Field::Due,
            Change::Notes(_) => Field::Notes,
            Change::Tags(_) => Field::Tags,
            Change::Priority(_) => Field::Priority,
        }
    }

    pub fn apply(&self, t: &mut Task) {
        match self {
            Change::Title(v) => t.title = v.clone(),
            Change::Done(v) => t.done = *v,
            Change::Due(v) => t.due = v.clone(),
            Change::Notes(v) => t.notes = v.clone(),
            Change::Tags(v) => t.tags = v.clone(),
            Change::Priority(v) => t.priority = *v,
        }
    }

    pub fn differs(&self, t: &Task) -> bool {
        match self {
            Change::Title(v) => &t.title != v,
            Change::Done(v) => t.done != *v,
            Change::Due(v) => &t.due != v,
            Change::Notes(v) => &t.notes != v,
            Change::Tags(v) => &t.tags != v,
            Change::Priority(v) => t.priority != *v,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListKind {
    /// Lista en archivos YAML.
    #[default]
    Local,
    /// Lista `todo.*` de Home Assistant.
    HomeAssistant,
}

/// Qué campos soporta una lista (las de Home Assistant soportan menos).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    pub due: bool,
    pub notes: bool,
    pub tags: bool,
    pub priority: bool,
    /// Preserva campos desconocidos.
    pub extra: bool,
}

impl Caps {
    pub const ALL: Caps = Caps {
        due: true,
        notes: true,
        tags: true,
        priority: true,
        extra: true,
    };

    pub const HOME_ASSISTANT: Caps = Caps {
        due: true,
        notes: true,
        tags: false,
        priority: false,
        extra: false,
    };

    pub fn supports(&self, f: Field) -> bool {
        match f {
            Field::Title | Field::Done => true,
            Field::Due => self.due,
            Field::Notes => self.notes,
            Field::Tags => self.tags,
            Field::Priority => self.priority,
        }
    }
}

impl Default for Caps {
    fn default() -> Self {
        Caps::ALL
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ListMeta {
    pub id: ListId,
    pub name: String,
    #[serde(default)]
    pub kind: ListKind,
    #[serde(default)]
    pub caps: Caps,
}

/// Deserializadores tolerantes para archivos editados a mano
/// (`title: 123`, `done: yes`, `tags: super`, etc.).
mod de {
    use super::*;
    use serde_json::Value;

    fn scalar_to_string(v: Value) -> Option<String> {
        match v {
            Value::Null => None,
            Value::String(s) => Some(s),
            Value::Bool(b) => Some(b.to_string()),
            Value::Number(n) => Some(n.to_string()),
            other => Some(other.to_string()),
        }
    }

    pub fn string<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        Ok(scalar_to_string(Value::deserialize(d)?).unwrap_or_default())
    }

    pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Ok(scalar_to_string(Value::deserialize(d)?).filter(|s| !s.is_empty()))
    }

    pub fn boolean<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Bool(b) => b,
            Value::Number(n) => n.as_i64().unwrap_or(0) != 0,
            Value::String(s) => matches!(
                s.trim().to_lowercase().as_str(),
                "true" | "yes" | "y" | "si" | "sí" | "x" | "1" | "done"
            ),
            _ => false,
        })
    }

    pub fn tags<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Array(items) => items.into_iter().filter_map(scalar_to_string).collect(),
            Value::String(s) => s
                .split([',', ' '])
                .filter(|t| !t.is_empty())
                .map(str::to_string)
                .collect(),
            Value::Null => vec![],
            other => scalar_to_string(other).into_iter().collect(),
        })
    }

    /// `links:` tolerante: acepta `{to: x, kind: blocks}` o un string
    /// (`kind: related` implícito).
    pub fn links<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Link>, D::Error> {
        Ok(match Value::deserialize(d)? {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(to) => Some(Link {
                        to,
                        kind: LinkKind::Related,
                    }),
                    other => serde_json::from_value(other).ok(),
                })
                .collect(),
            _ => vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_json() {
        let t: Task = serde_json::from_str(
            r#"{"title": 123, "done": "yes", "tags": "a, b", "color": "red"}"#,
        )
        .unwrap();
        assert_eq!(t.title, "123");
        assert!(t.done);
        assert_eq!(t.tags, vec!["a", "b"]);
        assert_eq!(t.extra["color"], "red");
    }

    #[test]
    fn changes_to() {
        let a = Task::new("1", "leche");
        let mut b = a.clone();
        b.done = true;
        b.title = "leche descremada".into();
        assert_eq!(
            a.changes_to(&b),
            vec![Change::Title("leche descremada".into()), Change::Done(true)]
        );
    }
}
