//! Home Assistant como backend remoto de listas.
//!
//! [`HaClient`] habla con la REST API (`todo.get_items`, `todo.add_item`,
//! `todo.update_item`, `todo.remove_item`). [`HaList`] mantiene el estado
//! deseado de una lista (un [`lrtodo_core::ListDoc`]) y lo reconcilia con HA:
//! incorpora lo que cambió en HA desde la última lectura y empuja lo que
//! cambió en lrtodo.

mod client;
mod list;

pub use client::{HaClient, HaError, HaItem, TodoEntity, features};
pub use list::HaList;
