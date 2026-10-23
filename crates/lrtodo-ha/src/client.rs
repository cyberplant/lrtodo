use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Bits de `supported_features` de las entidades `todo.*`.
pub mod features {
    pub const CREATE: u32 = 1;
    pub const DELETE: u32 = 2;
    pub const UPDATE: u32 = 4;
    pub const MOVE: u32 = 8;
    pub const SET_DUE_DATE: u32 = 16;
    pub const SET_DUE_DATETIME: u32 = 32;
    pub const SET_DESCRIPTION: u32 = 64;
}

#[derive(Debug, thiserror::Error)]
pub enum HaError {
    #[error("no se pudo conectar con Home Assistant: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Home Assistant respondió {status}: {body}")]
    Status { status: u16, body: String },
    #[error("respuesta inesperada de Home Assistant: {0}")]
    Decode(String),
}

pub type Result<T> = std::result::Result<T, HaError>;

/// Item tal como lo devuelve `todo.get_items`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HaItem {
    pub uid: String,
    pub summary: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl HaItem {
    pub fn done(&self) -> bool {
        self.status == "completed"
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TodoEntity {
    pub entity_id: String,
    pub name: String,
    pub features: u32,
}

#[derive(Clone)]
pub struct HaClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl HaClient {
    /// `base` es la URL de HA sin `/api` (p.ej. `http://homeassistant.local:8123`
    /// o `http://supervisor/core` desde un add-on).
    pub fn new(base: &str, token: &str, timeout: Duration) -> Result<Self> {
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            token: token.to_string(),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(timeout.min(Duration::from_secs(5)))
                .build()?,
        })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value> {
        let resp = req.bearer_auth(&self.token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(HaError::Status {
                status: status.as_u16(),
                body: body.chars().take(300).collect(),
            });
        }
        Ok(resp.json().await?)
    }

    async fn call(&self, service: &str, body: Value, return_response: bool) -> Result<Value> {
        let mut url = format!("{}/api/services/todo/{service}", self.base);
        if return_response {
            url.push_str("?return_response");
        }
        self.send(self.http.post(url).json(&body)).await
    }

    /// Entidades `todo.*` disponibles.
    pub async fn todo_entities(&self) -> Result<Vec<TodoEntity>> {
        let states = self
            .send(self.http.get(format!("{}/api/states", self.base)))
            .await?;
        let states = states
            .as_array()
            .ok_or_else(|| HaError::Decode("/api/states no es una lista".into()))?;
        let mut out: Vec<TodoEntity> = states
            .iter()
            .filter_map(|s| {
                let entity_id = s.get("entity_id")?.as_str()?;
                entity_id.starts_with("todo.").then(|| {
                    let attrs = s.get("attributes");
                    TodoEntity {
                        entity_id: entity_id.to_string(),
                        name: attrs
                            .and_then(|a| a.get("friendly_name"))
                            .and_then(Value::as_str)
                            .unwrap_or(entity_id)
                            .to_string(),
                        features: attrs
                            .and_then(|a| a.get("supported_features"))
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32,
                    }
                })
            })
            .collect();
        out.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
        Ok(out)
    }

    pub async fn get_items(&self, entity: &str) -> Result<Vec<HaItem>> {
        let resp = self
            .call(
                "get_items",
                json!({"entity_id": entity, "status": ["needs_action", "completed"]}),
                true,
            )
            .await?;
        let items = resp
            .pointer("/service_response")
            .and_then(|r| r.get(entity))
            .and_then(|r| r.get("items"))
            .cloned()
            .ok_or_else(|| HaError::Decode(format!("get_items sin items para {entity}")))?;
        serde_json::from_value(items).map_err(|e| HaError::Decode(e.to_string()))
    }

    /// `todo.add_item`; `fields` son campos opcionales (`due_date`, `description`, ...).
    pub async fn add_item(
        &self,
        entity: &str,
        summary: &str,
        fields: Map<String, Value>,
    ) -> Result<()> {
        let mut body = json!({"entity_id": entity, "item": summary});
        body.as_object_mut().expect("objeto").extend(fields);
        self.call("add_item", body, false).await.map(drop)
    }

    /// `todo.update_item` por uid; `fields` puede tener `rename`, `status`,
    /// `due_date`/`due_datetime`/`description` (null limpia el campo).
    pub async fn update_item(
        &self,
        entity: &str,
        uid: &str,
        fields: Map<String, Value>,
    ) -> Result<()> {
        let mut body = json!({"entity_id": entity, "item": uid});
        body.as_object_mut().expect("objeto").extend(fields);
        self.call("update_item", body, false).await.map(drop)
    }

    pub async fn remove_item(&self, entity: &str, uid: &str) -> Result<()> {
        self.call(
            "remove_item",
            json!({"entity_id": entity, "item": [uid]}),
            false,
        )
        .await
        .map(drop)
    }
}
