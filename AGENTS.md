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
- `crates/lrtodo-client`: el `Backend` compartido (`Local`/`Remote`, `SyncState`,
  `ClientConfig` tolerante). Lo usan cli, tui y tray.
- `crates/lrtodo-tray`: `lrtodo-tray`, ícono en la barra de menús
  (tray-icon/muda/tao). Worker con el backend + poll 10 s; menú con las tareas
  (click tilda). `.app` con `cargo bundle` (LSUIElement en `Info.plist`).
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

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:46cd31e7 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   bd dolt push
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Beads Issue Tracker

Use Beads (`bd`) for durable task tracking in repositories that include it. Use the `beads` skill at `.agents/skills/beads/SKILL.md` (project install) or `~/.agents/skills/beads/SKILL.md` (global install) for Beads workflow guidance, then use the `bd` CLI for issue operations.

### Quick Reference

```bash
bd ready                # Find available work
bd show <id>            # View issue details
bd update <id> --claim  # Claim work
bd close <id>           # Complete work
bd prime                # Refresh Beads context
```

### Rules

- Use `bd` for all task tracking; do not create markdown TODO lists.
- Run `bd prime` when Beads context is missing or stale. Codex 0.129.0+ can load Beads context automatically through native hooks; use `/hooks` to inspect or toggle them.
- Keep persistent project memory in Beads via `bd remember`; do not create ad hoc memory files.

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.
<!-- END BEADS CODEX SETUP -->
