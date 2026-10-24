# lrtodo

Gestor de tareas **offline-first**, con los datos en **YAML editable a mano**,
interfaz **CLI**, **TUI** (terminal interactiva), **web instalable en el celular
(PWA)** y acceso a las listas `todo` de **Home Assistant**.

- La app del celular siempre muestra y deja editar lo que tiene en cache, aunque
  no haya señal o la red esté lenta. Los cambios se envían en segundo plano, y
  una barra arriba indica el estado ("Al día", "Sin conexión · datos de hace
  5 min", "2 cambios sin enviar").
- Los cambios son atómicos (lock + archivo temporal + `rename`) y se resuelven por
  campo con relojes híbridos (HLC). Así no se pisan la CLI, la web, el celular y
  las ediciones hechas a mano.
- Hay un solo core en Rust (`lrtodo-core`). La CLI y el servidor lo usan nativo,
  y la PWA lo usa compilado a WebAssembly.

## Uso rápido

```sh
cargo build --release
lrtodo new compras --name Compras        # crea ~/.local/share/lrtodo/compras.yaml (o --data-dir)
lrtodo add compras leche -t super
lrtodo add compras huevos --due 2026-10-25 -n "docena"
lrtodo ls compras
lrtodo done compras 1                    # número, prefijo de id o título
lrtodo edit compras                      # abre $EDITOR; se aplica como merge
lrtodo                                   # en una terminal abre la TUI
lrtodo serve                             # API + PWA en http://0.0.0.0:8765
```

Comandos: `lists`, `ls`, `add`, `done`, `undo`, `rm`, `set`, `mv`, `edit`, `new`,
`convert`, `check`, `sync`, `status`, `tui` y `serve`. Con `--json` la salida es
JSON. `lrtodo` a secas abre la TUI si está en una terminal (en un pipe, lista).

### TUI

`lrtodo tui` usa el mismo backend que la CLI (local o `--server`): en modo local
vigila los archivos y en modo remoto escucha los eventos del servidor, así que
los cambios de otros clientes aparecen solos. Funciona offline igual que la PWA:
la barra de abajo muestra "al día", "sincronizando…", "sin conexión" y los
cambios pendientes.

Teclas: `j`/`k` moverse, `tab`/`h`/`l` cambiar de panel, `espacio`/`enter`
tildar, `a` agregar, `e` editar (formulario), `d` borrar, `m` mover, `n` crear
lista (`id|nombre`), `/` filtrar, `v` mostrar/ocultar hechas, `E` abrir la lista
en `$EDITOR`, `r` sincronizar, `?` ayuda, `q` salir.

## Configuración (`~/.config/lrtodo/config.yaml`)

```yaml
data_dir: ~/todo            # dónde viven los YAML (modo local y serve)
default_list: compras       # para `lrtodo ls` sin argumentos

# Opcional: usar la CLI contra un servidor (con cache offline) en vez de los archivos.
server:
  url: http://homeassistant.local:8765
  token: mi-token

serve:
  bind: 0.0.0.0:8765
  token: mi-token           # opcional; la PWA lo pide una vez
  ha:
    url: http://homeassistant.local:8123
    token_file: ~/.config/lrtodo/ha-token   # long-lived access token
    include: []             # entidades todo.* (vacío = todas)
    exclude: []
    poll_secs: 30
```

Variables de entorno: `LRTODO_DATA_DIR`, `LRTODO_SERVER` (`local` fuerza modo
local), `LRTODO_TOKEN`, `LRTODO_HA_URL`, `LRTODO_HA_TOKEN`, `LRTODO_BIND` y
`LRTODO_LOG`.

## Archivos

Hay dos layouts, que se pueden elegir por lista (`lrtodo new x --layout dir`,
`lrtodo convert x --layout file`):

- `compras.yaml`: un archivo por lista.
- `compras/_list.yaml` + `compras/<id>.yaml`: un archivo por tarea. Da menos
  conflictos si sincronizás con git o Syncthing.

Ver [docs/format.md](docs/format.md). Los relojes y lápidas de sincronización van
en `.lrtodo/` y no hace falta versionarlos.

## Home Assistant

Las listas `todo.*` aparecen como `ha:<nombre>`. Viven en HA (lrtodo no guarda
una copia en YAML). El servidor las reconcilia en los dos sentidos: lo que se
agrega por voz o desde la app de HA aparece en lrtodo, y lo que se tilda en el
súper llega a HA. Si HA no responde, los cambios quedan en cola en el servidor y
se envían cuando vuelve.

### Add-on

1. En HA, ir a *Configuración → Add-ons → Tienda → ⋮ → Repositorios* y agregar
   `https://github.com/cyberplant/lrtodo`.
2. Instalar **lrtodo** y, si querés, configurar un `token`.
3. Los datos quedan en `/share/lrtodo`, accesibles por Samba/SSH.
4. En el celular, abrir `http://<ip-de-ha>:8765` (o por Tailscale/reverse proxy)
   y elegir "Agregar a pantalla de inicio". El panel "Tareas" de la barra lateral
   usa el ingress de HA.

La imagen (`ghcr.io/cyberplant/lrtodo`, amd64/arm64) se publica con el workflow
`release.yml` al crear un tag `v*`. El `Dockerfile` también sirve para correrla
fuera de HA:

```sh
docker run -p 8765:8765 -v $PWD/todo:/data/lrtodo \
  -e LRTODO_HA_URL=http://ha:8123 -e LRTODO_HA_TOKEN=... ghcr.io/cyberplant/lrtodo
```

## Desarrollo

Ver [AGENTS.md](AGENTS.md) y [docs/sync.md](docs/sync.md).
