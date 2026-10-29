//! Binding WASM de [`lrtodo_core::Replica`] para la PWA. Todo entra y sale
//! como JSON: la lógica (vista offline, outbox, rebase, conflictos) es la
//! misma que usa la CLI.

use lrtodo_core::{Change, Replica, SyncResponse, Task};
use serde::Serialize;
use wasm_bindgen::prelude::*;

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

fn link_kind(s: &str) -> Result<lrtodo_core::LinkKind, JsError> {
    match s {
        "subtask" => Ok(lrtodo_core::LinkKind::Subtask),
        "blocks" => Ok(lrtodo_core::LinkKind::Blocks),
        "related" => Ok(lrtodo_core::LinkKind::Related),
        other => Err(JsError::new(&format!("kind inválido: {other}"))),
    }
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("serializable")
}

#[wasm_bindgen]
pub struct WasmReplica {
    inner: Replica,
}

#[derive(Serialize)]
struct Status<'a> {
    device: &'a str,
    pending: usize,
    last_sync_ms: Option<u64>,
    last_error: Option<&'a str>,
    notices: &'a [lrtodo_core::Notice],
}

#[wasm_bindgen]
impl WasmReplica {
    #[wasm_bindgen(constructor)]
    pub fn new(device: String, seed: f64) -> WasmReplica {
        WasmReplica {
            inner: Replica::new(device, seed as u64),
        }
    }

    /// Restaura desde JSON (IndexedDB), mezclando entropía nueva.
    pub fn load(data: &str, seed: f64) -> Result<WasmReplica, JsError> {
        let mut inner: Replica = serde_json::from_str(data).map_err(err)?;
        inner.reseed(seed as u64);
        Ok(WasmReplica { inner })
    }

    #[wasm_bindgen(js_name = toJson)]
    pub fn to_json(&self) -> String {
        json(&self.inner)
    }

    pub fn lists(&self) -> String {
        json(&self.inner.lists())
    }

    pub fn view(&self, list: &str) -> Result<String, JsError> {
        self.inner.view(list).map(|v| json(&v)).map_err(err)
    }

    pub fn status(&self) -> String {
        json(&Status {
            device: &self.inner.device,
            pending: self.inner.pending(),
            last_sync_ms: self.inner.last_sync_ms,
            last_error: self.inner.last_error.as_deref(),
            notices: &self.inner.notices,
        })
    }

    /// `task` es un JSON parcial de [`Task`] (al menos `title`). Devuelve el id.
    pub fn add(&mut self, list: &str, task: &str, now: f64) -> Result<String, JsError> {
        let task: Task = serde_json::from_str(task).map_err(err)?;
        self.inner.add(list, task, now as u64).map_err(err)
    }

    /// `change` es `{"field": "done", "value": true}` (ver [`Change`]).
    pub fn set(&mut self, list: &str, id: &str, change: &str, now: f64) -> Result<(), JsError> {
        let change: Change = serde_json::from_str(change).map_err(err)?;
        self.inner.set(list, id, change, now as u64).map_err(err)
    }

    pub fn remove(&mut self, list: &str, id: &str, now: f64) -> Result<(), JsError> {
        self.inner.delete(list, id, now as u64).map_err(err)
    }

    #[wasm_bindgen(js_name = moveTask)]
    pub fn move_task(
        &mut self,
        from: &str,
        id: &str,
        to: &str,
        now: f64,
    ) -> Result<String, JsError> {
        self.inner.move_task(from, id, to, now as u64).map_err(err)
    }

    /// Arista `src → dst` (`dst` puede ser `otra-lista/id`).
    /// `kind`: "subtask" | "blocks" | "related".
    pub fn link(
        &mut self,
        list: &str,
        src: &str,
        dst: &str,
        kind: &str,
        now: f64,
    ) -> Result<(), JsError> {
        self.inner
            .link(list, src, dst, link_kind(kind)?, now as u64)
            .map_err(err)
    }

    pub fn unlink(
        &mut self,
        list: &str,
        src: &str,
        dst: &str,
        kind: &str,
        now: f64,
    ) -> Result<(), JsError> {
        self.inner
            .unlink(list, src, dst, link_kind(kind)?, now as u64)
            .map_err(err)
    }

    #[wasm_bindgen(js_name = syncRequest)]
    pub fn sync_request(&self) -> String {
        json(&self.inner.sync_request())
    }

    #[wasm_bindgen(js_name = applyResponse)]
    pub fn apply_response(&mut self, response: &str, now: f64) -> Result<(), JsError> {
        let resp: SyncResponse = serde_json::from_str(response).map_err(err)?;
        self.inner.apply_response(resp, now as u64);
        Ok(())
    }

    #[wasm_bindgen(js_name = syncFailed)]
    pub fn sync_failed(&mut self, error: &str) {
        self.inner.sync_failed(error);
    }

    #[wasm_bindgen(js_name = clearNotices)]
    pub fn clear_notices(&mut self) {
        self.inner.clear_notices();
    }

    pub fn pending(&self) -> usize {
        self.inner.pending()
    }
}
