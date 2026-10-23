# Formato de archivos

## Layout `file`: `<lista>.yaml`

```yaml
lrtodo: 1                 # versión del formato
name: Compras             # opcional (por defecto, el nombre del archivo)
items:
- id: 01M5EAVA6XN9T0CRSBJEX39GVT
  title: leche descremada
  done: true
  tags: [super]
  created: 2026-10-21T04:17:27Z
  updated: 2026-10-21T04:17:27Z
  completed: 2026-10-21T04:17:27Z
- title: pan               # sin id: se le asigna uno al leerlo
```

## Layout `dir`: `<lista>/`

- `_list.yaml`: `lrtodo`, `name` y otros campos propios de la lista.
- `<id>.yaml`: una tarea con los mismos campos que un item. Si falta `id`, se usa
  el nombre del archivo. Se ordenan por `created`.

## Campos de una tarea

| campo       | tipo                | notas                                          |
|-------------|---------------------|------------------------------------------------|
| `id`        | texto `[A-Za-z0-9_-]` | ULID generado por lrtodo                     |
| `title`     | texto               | obligatorio                                    |
| `done`      | bool                | también acepta `yes`, `si`, `x`, `1`           |
| `due`       | texto               | `2026-10-25` o `2026-10-25T18:00:00-03:00`     |
| `notes`     | texto               |                                                |
| `tags`      | lista o `"a, b"`    |                                                |
| `priority`  | número 0-255        |                                                |
| `created`, `updated`, `completed` | RFC 3339 | los mantiene lrtodo                   |
| *otros*     | cualquiera          | se preservan tal cual                          |

## Editar a mano

Se puede editar con cualquier editor, aunque el servidor esté corriendo: al
próximo acceso, lrtodo compara el archivo con lo último que escribió, y las
diferencias cuentan como cambios hechos en ese momento. Borrar un item lo
elimina, y un item sin `id` es una tarea nueva. Si el archivo queda con un error
de sintaxis o con marcadores de conflicto de git, la lista se marca con error,
los clientes conservan su cache y los cambios pendientes esperan hasta que se
arregle (`lrtodo check` lo diagnostica).

`lrtodo edit <lista>` es más seguro: abre una copia y aplica sólo lo que
cambiaste, como operaciones. Si mientras editás alguien tilda algo desde el
celular, ese cambio no se pierde.

Limitaciones: los comentarios YAML no se conservan al reescribir y el archivo se
normaliza (orden de campos, comillas).

## `.lrtodo/`

`state/<lista>.json` guarda el último contenido conocido, los relojes HLC por
campo y las lápidas de lo borrado (se conservan 60 días). `locks/` tiene los
archivos de lock. En el servidor, `ha/` guarda el estado de las listas de Home
Assistant. Todo esto se regenera solo y conviene agregarlo a `.gitignore`.
