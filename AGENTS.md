# lrtodo

Gestor de tareas offline-first: YAML + CLI + PWA + Home Assistant. Rust (edition
2024) para el core, el servidor y la CLI; Svelte 5 + Vite para la PWA, que usa el
core compilado a WASM.

## Estructura

- `crates/lrtodo-core`: modelo, ops, HLC, LWW (`ListDoc`), réplica offline
  (`Replica`) y protocolo. **Sans-IO**: no lee la hora ni accede a disco o red.
  Toda la lógica de negocio va acá.
- `crates/lrtodo-store`: YAML (layouts `file`/`dir`), escritura atómica, locks,
  `.lrtodo/state`.
- `crates/lrtodo-ha`: cliente REST de HA y reconciliación (`HaList`).
- `crates/lrtodo-server`: axum (`/api/sync`, `/api/lists`, `/api/events` SSE,
  `/api/health`), hub de HA, watcher y PWA embebida (`web/dist` vía rust-embed).
- `crates/lrtodo-cli`: binario `lrtodo` (local, remoto con cache, `serve`, `tui`).
  La TUI (`src/tui/`): la UI corre en el hilo principal y el backend en un hilo
  worker (`worker.rs`); se comunican con `ChanBackend`, que implementa `Backend`
  vía canales. Refresco en vivo: SSE `/api/events` en remoto, `notify` en local.
- `crates/lrtodo-wasm`: binding wasm-bindgen de `Replica` (JSON in/out).
- `web/`: PWA. `src/lib/app.svelte.ts` tiene el estado y el motor de sync.
- `ha-addon/lrtodo/`, `repository.yaml` y `Dockerfile`: add-on de HA / imagen.

## Comandos

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

cd web
npm ci
npm run build          # wasm-pack + vite build -> web/dist
npm run check          # svelte-check
cd .. && cargo build -p lrtodo-cli   # re-embebe web/dist en el binario
cd web && npm run test:e2e           # Playwright (offline, red lenta, pestañas)

docker build --platform linux/arm64 -t lrtodo:dev .
```

## Convenciones

- Textos de UI, errores y comentarios en español.
- Versiones de dependencias publicadas hace más de 7 días.
- `wasm-bindgen` fijado a `=0.2.128` (tiene que coincidir con el CLI instalado).
- Los e2e levantan su propio servidor en `127.0.0.1:8798` con datos en
  `/tmp/lrtodo-e2e`. Hay que compilar la PWA y el binario antes.
- Usuario de GitHub/ghcr: `cyberplant`.
