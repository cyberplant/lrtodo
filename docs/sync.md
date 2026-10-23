# Sincronización

## Modelo

- Un cambio es una **operación**: `add`, `set` (de un campo: `title`, `done`,
  `due`, `notes`, `tags` o `priority`) o `delete`. Cada una lleva un id y un
  timestamp **HLC** (hybrid logical clock: ms + contador + nodo).
- El servidor guarda, para cada campo de cada tarea, el HLC del último cambio.
  Una op se aplica sólo si su HLC es mayor (**last-writer-wins por campo**). Por
  eso dos personas pueden tildar cosas distintas, o cambiar campos distintos de
  la misma tarea, sin pisarse.
- Los borrados dejan una lápida, y las ediciones posteriores de esa tarea se
  ignoran (el borrado gana).
- Reaplicar una op no tiene efecto, así que reintentar es seguro.
- El resultado no depende del orden en que llegan los lotes de cada dispositivo
  (hay un test de propiedades: `server_order_independent`).

## Cliente (`Replica`)

```
vista = snapshot confirmado por el servidor + outbox aplicado encima
```

1. Cada acción del usuario genera una op, que va al outbox (persistido en
   IndexedDB o en `~/.cache/lrtodo`) y se ve al instante.
2. En segundo plano, `POST /api/sync {device, ops: outbox, known: {lista: rev}}`.
3. El servidor responde con `lists` (estado de todas), `snapshots` (sólo las que
   cambiaron), `acked`/`rejected` y su HLC.
4. El cliente saca del outbox lo confirmado, reemplaza los snapshots y sigue
   mostrando encima lo que todavía no se confirmó (rebase).

Si la red falla o tarda más de 10 s, se registra el error, la UI sigue igual y se
reintenta con backoff (2 s → 60 s). También se reintenta al volver la conexión,
al traer la app al frente y con cada evento SSE (`GET /api/events`) que avisa que
otra cosa cambió una lista.

En la PWA, las pestañas se coordinan con Web Locks + BroadcastChannel, y el
service worker cachea la app completa (incluido el `.wasm`).

## Home Assistant

El servidor tiene un `HaList` por entidad `todo.*`:

- `doc`: el estado deseado (lo que ven los clientes, con relojes).
- `seen`: lo último que sabemos que tiene HA.

La reconciliación (cada `poll_secs`, o en el momento si llegan ops):

1. Lee `todo.get_items`. Lo que difiere de `seen` cambió en HA (app, voz,
   automatizaciones) y se aplica a `doc` con un HLC nuevo.
2. Calcula `doc` vs `seen` y llama `add_item`, `update_item` o `remove_item`. Lo
   enviado actualiza `seen`, así no se confunde con un cambio hecho en HA.
3. Las tareas creadas desde lrtodo se mapean a su `uid` de HA por título
   (`add_item` no devuelve el uid). Si la respuesta se pierde, no se duplica.

No se retiene ningún lock mientras se espera a HA: si HA está lento, las syncs de
los clientes no se traban. `pending_upstream` y `error` de cada lista informan a
la UI.

Qué soporta cada lista depende de los `supported_features` de la entidad (fechas,
descripción, borrar). Las etiquetas y la prioridad no existen en HA.
