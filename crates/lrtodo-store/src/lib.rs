//! Almacenamiento de listas en YAML editable a mano.
//!
//! Layouts (por lista, se detecta solo):
//! - `file`: `<root>/<lista>.yaml` con todas las tareas.
//! - `dir`:  `<root>/<lista>/_list.yaml` + `<root>/<lista>/<id>.yaml` por tarea.
//!
//! Metadatos de sincronización (relojes, lápidas) en `<root>/.lrtodo/state/`,
//! que no hace falta versionar. Toda mutación pasa por [`Store::mutate`]:
//! lock exclusivo por lista → releer (incorporando ediciones a mano) →
//! aplicar → escribir a temporal + fsync + rename. Así CLI, servidor y
//! editores no se pisan en la misma máquina.

pub mod atomic;

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use lrtodo_core::{Caps, Change, Clock, LinkKind, ListDoc, ListKind, ListMeta, OpKind, Rng, Task};
use serde::{Deserialize, Serialize};

pub use atomic::write_atomic;

const STATE_DIR: &str = ".lrtodo";
const DIR_HEADER: &str = "_list.yaml";
const FORMAT_VERSION: u32 = 1;
/// Las lápidas se guardan 60 días (para que dispositivos offline no "revivan" tareas).
const TOMBSTONE_TTL_MS: u64 = 60 * 24 * 3600 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no existe la lista {0}")]
    NotFound(String),
    #[error("ya existe la lista {0}")]
    Exists(String),
    #[error("id de lista inválido: {0:?} (usar letras, números, '-' o '_')")]
    InvalidId(String),
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("{0}: existe como archivo y como directorio")]
    Ambiguous(String),
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

pub type Result<T> = std::result::Result<T, StoreError>;

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Un archivo por lista.
    File,
    /// Un archivo por tarea.
    Dir,
}

impl std::str::FromStr for Layout {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "file" => Ok(Layout::File),
            "dir" => Ok(Layout::Dir),
            _ => Err(format!("layout inválido: {s} (file|dir)")),
        }
    }
}

/// Cabecera de una lista (`<lista>.yaml` o `_list.yaml`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Header {
    #[serde(default = "format_version")]
    lrtodo: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

fn format_version() -> u32 {
    FORMAT_VERSION
}

#[derive(Serialize, Deserialize)]
struct ListFile {
    #[serde(flatten)]
    header: Header,
    #[serde(default)]
    items: Vec<Task>,
}

/// Contenido leído de disco.
struct Source {
    layout: Layout,
    header: Header,
    tasks: Vec<Task>,
    hash: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SideState {
    source_hash: String,
    doc: ListDoc,
}

/// Una lista cargada, lista para leer o modificar dentro de [`Store::mutate`].
pub struct Loaded {
    pub meta: ListMeta,
    pub layout: Layout,
    pub doc: ListDoc,
    header: Header,
}

pub struct Store {
    root: PathBuf,
    clock: Mutex<Clock>,
    rng: Mutex<Rng>,
    /// Versionado git opcional: autocommit tras cada `save`.
    versioned: bool,
    /// Descripción de la próxima mutación para el mensaje de commit
    /// (la setea el caller antes de `mutate`/`save`; se consume al commitear).
    pending_desc: Mutex<Option<String>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub fn validate_id(id: &str) -> Result<()> {
    let ok = !id.is_empty()
        && !id.starts_with(['.', '_', '-'])
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(StoreError::InvalidId(id.into()))
    }
}

impl Store {
    /// Abre (y crea si hace falta) un directorio de datos. `node` identifica
    /// a este proceso en los relojes HLC (p.ej. "server" o "cli-laptop").
    pub fn open(root: impl Into<PathBuf>, node: &str) -> Result<Self> {
        let root = root.into();
        let state = root.join(STATE_DIR).join("state");
        fs::create_dir_all(&state).map_err(io_err(&state))?;
        let locks = root.join(STATE_DIR).join("locks");
        fs::create_dir_all(&locks).map_err(io_err(&locks))?;
        let seed = now_ms() ^ (std::process::id() as u64) << 32;
        Ok(Self {
            root,
            clock: Mutex::new(Clock::new(node)),
            rng: Mutex::new(Rng::new(seed)),
            versioned: false,
            pending_desc: Mutex::new(None),
        })
    }

    /// Descripción para el próximo commit git (consumida por `git_commit`).
    pub fn set_commit_desc(&self, desc: impl Into<String>) {
        *self.pending_desc.lock().expect("pending_desc") = Some(desc.into());
    }

    /// Activa el versionado git: inicializa el repo si no existe
    /// (`.gitignore` de `.lrtodo/`, README, AGENTS.md) y commitea cada `save`.
    pub fn set_versioned(&mut self, versioned: bool) {
        if versioned {
            let _ = ensure_git(&self.root);
        }
        self.versioned = versioned;
    }

    fn git_commit(&self, list: &str) {
        if !self.versioned {
            return;
        }
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&self.root)
                .args(args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        git(&["add", "-A"]);
        let msg = self
            .pending_desc
            .lock()
            .expect("pending_desc")
            .take()
            .unwrap_or_else(|| format!("lrtodo: {list}"));
        git(&["commit", "-qm", &msg]);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{id}.yaml"))
    }

    fn dir_path(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    fn state_path(&self, id: &str) -> PathBuf {
        self.root
            .join(STATE_DIR)
            .join("state")
            .join(format!("{id}.json"))
    }

    fn lock(&self, id: &str) -> Result<File> {
        let path = self
            .root
            .join(STATE_DIR)
            .join("locks")
            .join(format!("{id}.lock"));
        let f = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_err(&path))?;
        f.lock().map_err(io_err(&path))?;
        Ok(f)
    }

    /// Layout de una lista existente.
    pub fn layout(&self, id: &str) -> Result<Layout> {
        let file = self.file_path(id).is_file();
        let dir = self.dir_path(id).join(DIR_HEADER).is_file();
        match (file, dir) {
            (true, true) => Err(StoreError::Ambiguous(id.into())),
            (true, false) => Ok(Layout::File),
            (false, true) => Ok(Layout::Dir),
            (false, false) => Err(StoreError::NotFound(id.into())),
        }
    }

    /// Ids de todas las listas, ordenados.
    pub fn list_ids(&self) -> Result<Vec<String>> {
        let mut ids = vec![];
        for entry in fs::read_dir(&self.root).map_err(io_err(&self.root))? {
            let entry = entry.map_err(io_err(&self.root))?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            let id = if path.is_dir() && path.join(DIR_HEADER).is_file() {
                name
            } else if path.is_file() && name.ends_with(".yaml") {
                name.trim_end_matches(".yaml").to_string()
            } else {
                continue;
            };
            if validate_id(&id).is_ok() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Metadatos de todas las listas (sin tomar locks).
    pub fn lists(&self) -> Result<Vec<ListMeta>> {
        self.list_ids()?
            .into_iter()
            .map(|id| {
                let src = self.read_source(&id)?;
                Ok(meta(&id, &src.header))
            })
            .collect()
    }

    pub fn create_list(&self, id: &str, name: Option<&str>, layout: Layout) -> Result<ListMeta> {
        validate_id(id)?;
        let _lock = self.lock(id)?;
        match self.layout(id) {
            Err(StoreError::NotFound(_)) => {}
            Ok(_) | Err(StoreError::Ambiguous(_)) => return Err(StoreError::Exists(id.into())),
            Err(e) => return Err(e),
        }
        let header = Header {
            lrtodo: FORMAT_VERSION,
            name: name.map(str::to_string),
            extra: BTreeMap::new(),
        };
        self.write_source(id, layout, &header, &[])?;
        self.set_commit_desc(format!("{id}: creamos la lista"));
        self.git_commit(id);
        Ok(meta(id, &header))
    }

    /// Directorio de adjuntos de la lista: `<lista>.assets/` al lado del
    /// archivo o del directorio de la lista.
    fn assets_root(&self, list: &str) -> PathBuf {
        self.root.join(format!("{list}.assets"))
    }

    /// Guarda un adjunto de una tarea (`{task_id}`) y devuelve la ruta
    /// relativa al data_dir para usarla en markdown:
    /// `<lista>.assets/NNNN-slug/<archivo>` en file layout,
    /// `<lista>/NNNN-slug.assets/<archivo>` en dir layout.
    pub fn add_asset(&self, list: &str, task_id: &str, name: &str, data: &[u8]) -> Result<String> {
        validate_id(list)?;
        let loaded = self.read(list)?;
        let tasks = loaded.doc.tasks_with_links();
        let (i, task) = tasks
            .iter()
            .enumerate()
            .find(|(_, t)| t.id == task_id)
            .ok_or_else(|| StoreError::NotFound(task_id.into()))?;
        let stem = task_file_name(i, task).replace(".yaml", "");
        // Si es imagen, la bajamos a un tamaño manejable (≤1600px) y
        // re-encodeamos (jpeg q80; png si tiene alpha). Si no decodifica,
        // se guarda tal cual (adjuntos no-imagen siguen sirviendo).
        let (data, file) = match shrink_image(data, name) {
            Some((bytes, ext)) => (
                std::borrow::Cow::Owned(bytes),
                rename_ext(&sanitize_asset_name(name), ext),
            ),
            None => (std::borrow::Cow::Borrowed(data), sanitize_asset_name(name)),
        };
        let (dir, rel) = match loaded.layout {
            Layout::Dir => (
                self.dir_path(list).join(format!("{stem}.assets")),
                format!("{list}/{stem}.assets/{file}"),
            ),
            Layout::File => (
                self.assets_root(list).join(&stem),
                format!("{list}.assets/{stem}/{file}"),
            ),
        };
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        let path = dir.join(&file);
        write_atomic(&path, &data)?;
        Ok(rel)
    }

    /// Lee una lista (incorporando ediciones a mano, que se persisten).
    pub fn read(&self, id: &str) -> Result<Loaded> {
        self.mutate(id, |_| ()).map(|(loaded, ())| loaded)
    }

    /// Lock → cargar → `f` → guardar si cambió → unlock.
    pub fn mutate<R>(&self, id: &str, f: impl FnOnce(&mut Loaded) -> R) -> Result<(Loaded, R)> {
        validate_id(id)?;
        let _lock = self.lock(id)?;
        let (mut loaded, mut dirty) = self.load(id)?;
        let before = loaded.doc.clone();
        let out = f(&mut loaded);
        dirty |= loaded.doc != before;
        if dirty {
            loaded
                .doc
                .prune_tombstones(now_ms().saturating_sub(TOMBSTONE_TTL_MS));
            self.save(id, &loaded)?;
        }
        Ok((loaded, out))
    }

    /// Borra una lista entera: los datos y el estado de sync (relojes y
    /// lápidas). Si se vuelve a crear con el mismo id, empieza de cero.
    /// Irreversible — el archivo de lock queda (es inofensivo y borrarlo
    /// abriría una carrera con otro proceso que lo esté tomando).
    pub fn remove_list(&self, id: &str) -> Result<()> {
        validate_id(id)?;
        let _lock = self.lock(id)?;
        match self.layout(id)? {
            Layout::File => {
                let p = self.file_path(id);
                fs::remove_file(&p).map_err(io_err(&p))?;
            }
            Layout::Dir => {
                let p = self.dir_path(id);
                fs::remove_dir_all(&p).map_err(io_err(&p))?;
            }
        }
        let state = self.state_path(id);
        match fs::remove_file(&state) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&state)(e)),
        }
        self.set_commit_desc(format!("{id}: eliminamos la lista"));
        self.git_commit(id);
        Ok(())
    }

    /// Cambia el layout de una lista conservando su estado.
    pub fn convert(&self, id: &str, layout: Layout) -> Result<()> {
        validate_id(id)?;
        let _lock = self.lock(id)?;
        let (mut loaded, _) = self.load(id)?;
        if loaded.layout == layout {
            return Ok(());
        }
        let old = loaded.layout;
        loaded.layout = layout;
        self.set_commit_desc(format!("{id}: convertimos el layout"));
        self.save(id, &loaded)?;
        match old {
            Layout::File => {
                fs::remove_file(self.file_path(id)).map_err(io_err(&self.file_path(id)))
            }
            Layout::Dir => {
                fs::remove_dir_all(self.dir_path(id)).map_err(io_err(&self.dir_path(id)))
            }
        }
    }

    fn load(&self, id: &str) -> Result<(Loaded, bool)> {
        let src = self.read_source(id)?;
        let state_path = self.state_path(id);
        let state: Option<SideState> = fs::read(&state_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let mut clock = self.clock.lock().expect("clock");
        let (doc, dirty) = match state {
            Some(state) if state.source_hash == src.hash => {
                if let Some(h) = state.doc.max_hlc() {
                    clock.observe(h);
                }
                (state.doc, false)
            }
            state => {
                let mut doc = state.map(|s| s.doc).unwrap_or_default();
                if let Some(h) = doc.max_hlc() {
                    clock.observe(h);
                }
                let now = now_ms();
                let mut rng = self.rng.lock().expect("rng");
                doc.ingest(src.tasks, &mut clock, now, &mut || rng.ulid(now));
                (doc, true)
            }
        };
        Ok((
            Loaded {
                meta: meta(id, &src.header),
                layout: src.layout,
                doc,
                header: src.header,
            },
            dirty,
        ))
    }

    fn save(&self, id: &str, loaded: &Loaded) -> Result<()> {
        let hash = self.write_source(
            id,
            loaded.layout,
            &loaded.header,
            &loaded.doc.tasks_with_links(),
        )?;
        let state = SideState {
            source_hash: hash,
            doc: loaded.doc.clone(),
        };
        let bytes = serde_json::to_vec(&state).expect("serializable");
        write_atomic(&self.state_path(id), &bytes)?;
        self.git_commit(id);
        Ok(())
    }

    fn read_source(&self, id: &str) -> Result<Source> {
        match self.layout(id)? {
            Layout::File => {
                let path = self.file_path(id);
                let bytes = fs::read(&path).map_err(io_err(&path))?;
                let file: ListFile = parse_yaml(&path, &bytes)?.unwrap_or(ListFile {
                    header: Header::default(),
                    items: vec![],
                });
                Ok(Source {
                    layout: Layout::File,
                    header: file.header,
                    tasks: file.items,
                    hash: blake3::hash(&bytes).to_hex().to_string(),
                })
            }
            Layout::Dir => self.read_dir_source(id),
        }
    }

    fn read_dir_source(&self, id: &str) -> Result<Source> {
        let dir = self.dir_path(id);
        let header_path = dir.join(DIR_HEADER);
        let header_bytes = fs::read(&header_path).map_err(io_err(&header_path))?;
        let header: Header = parse_yaml(&header_path, &header_bytes)?.unwrap_or_default();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&header_bytes);
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(io_err(&dir))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension().is_some_and(|e| e == "yaml")
                    && p.file_name().is_some_and(|n| n != DIR_HEADER)
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .collect();
        files.sort();
        let mut tasks = Vec::with_capacity(files.len());
        for path in files {
            let bytes = fs::read(&path).map_err(io_err(&path))?;
            let stem = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            hasher.update(stem.as_bytes());
            hasher.update(&[0]);
            hasher.update(&bytes);
            hasher.update(&[0]);
            let Some(mut task) = parse_yaml::<Task>(&path, &bytes)? else {
                continue;
            };
            if task.id.is_empty() {
                // Sin id en el archivo: el slug del nombre (sin el prefijo
                // `NNNN-`) hace de id, así no queda `0001-x` como id.
                task.id = stem
                    .split_once('-')
                    .filter(|(pre, _)| pre.chars().all(|c| c.is_ascii_digit()))
                    .map(|(_, rest)| rest.to_string())
                    .unwrap_or(stem);
            }
            tasks.push(task);
        }
        // El orden lo da el nombre del archivo (`files.sort()` arriba):
        // `NNNN-slug.yaml` — renombrar el prefijo reordena la lista.
        Ok(Source {
            layout: Layout::Dir,
            header,
            tasks,
            hash: hasher.finalize().to_hex().to_string(),
        })
    }

    /// Escribe la lista en disco (sólo los archivos que cambian) y devuelve el
    /// hash del contenido resultante.
    fn write_source(
        &self,
        id: &str,
        layout: Layout,
        header: &Header,
        tasks: &[Task],
    ) -> Result<String> {
        match layout {
            Layout::File => {
                let path = self.file_path(id);
                let file = ListFile {
                    header: header.clone(),
                    items: tasks.to_vec(),
                };
                let bytes = to_yaml(&file);
                if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
                    write_atomic(&path, &bytes)?;
                }
                Ok(blake3::hash(&bytes).to_hex().to_string())
            }
            Layout::Dir => {
                let dir = self.dir_path(id);
                fs::create_dir_all(&dir).map_err(io_err(&dir))?;
                let header_path = dir.join(DIR_HEADER);
                let header_bytes = to_yaml(header);
                if fs::read(&header_path).ok().as_deref() != Some(header_bytes.as_slice()) {
                    write_atomic(&header_path, &header_bytes)?;
                }
                let mut wanted = BTreeMap::new();
                for (i, task) in tasks.iter().enumerate() {
                    wanted.insert(task_file_name(i, task), task);
                }
                for entry in fs::read_dir(&dir).map_err(io_err(&dir))? {
                    let entry = entry.map_err(io_err(&dir))?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.ends_with(".yaml")
                        && name != DIR_HEADER
                        && !name.starts_with('.')
                        && !wanted.contains_key(&name)
                    {
                        fs::remove_file(entry.path()).map_err(io_err(&entry.path()))?;
                    }
                    // Adjuntos huérfanos: `NNNN-slug.assets/` sin su `.yaml`.
                    if name.ends_with(".assets")
                        && !wanted
                            .contains_key(&format!("{}.yaml", name.trim_end_matches(".assets")))
                    {
                        let _ = fs::remove_dir_all(entry.path());
                    }
                }
                for (name, task) in &wanted {
                    let path = dir.join(name);
                    let bytes = to_yaml(task);
                    if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
                        write_atomic(&path, &bytes)?;
                    }
                }
                // Recalcula el hash igual que read_source.
                Ok(self.read_dir_source(id)?.hash)
            }
        }
    }

    /// Valida todas las listas sin modificarlas.
    pub fn check(&self) -> Vec<String> {
        let mut problems = vec![];
        let ids = match self.list_ids() {
            Ok(ids) => ids,
            Err(e) => return vec![e.to_string()],
        };
        for id in ids {
            match self.read_source(&id) {
                Err(e) => problems.push(e.to_string()),
                Ok(src) => {
                    let mut seen = std::collections::BTreeSet::new();
                    for t in &src.tasks {
                        if t.title.trim().is_empty() {
                            problems.push(format!("{id}: tarea {} sin título", t.id));
                        }
                        if !t.id.is_empty() && !seen.insert(t.id.clone()) {
                            problems.push(format!("{id}: id duplicado {}", t.id));
                        }
                    }
                }
            }
        }
        problems
    }
}

fn meta(id: &str, header: &Header) -> ListMeta {
    ListMeta {
        id: id.to_string(),
        name: header.name.clone().unwrap_or_else(|| id.to_string()),
        kind: ListKind::Local,
        caps: Caps::ALL,
    }
}

fn parse_yaml<T: for<'de> Deserialize<'de>>(path: &Path, bytes: &[u8]) -> Result<Option<T>> {
    let parse_err = |message: String| StoreError::Parse {
        path: path.to_path_buf(),
        message,
    };
    let text = std::str::from_utf8(bytes).map_err(|e| parse_err(e.to_string()))?;
    if text
        .lines()
        .any(|l| l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>"))
    {
        return Err(parse_err("tiene marcadores de conflicto de git".into()));
    }
    if text.trim().is_empty() {
        return Ok(None);
    }
    serde_yaml_ng::from_str(text)
        .map(Some)
        .map_err(|e| parse_err(e.to_string()))
}

fn to_yaml<T: Serialize>(value: &T) -> Vec<u8> {
    serde_yaml_ng::to_string(value)
        .expect("serializable")
        .into_bytes()
}

#[cfg(test)]
mod tests;

/// `NNNN-slug.yaml`: la posición en el doc da el prefijo, el título el slug.
fn task_file_name(i: usize, task: &Task) -> String {
    format!("{:04}-{}.yaml", i + 1, slugify(&task.title))
}

/// Slug del título: alfanumérico (unicode) y `-`/`_`; el resto a `_`.
/// Es estable sólo si el título no cambia (renombrar título = rename en git).
fn slugify(title: &str) -> String {
    let mut slug = String::with_capacity(title.len());
    let mut last_us = false;
    for c in title.chars().take(40) {
        if c.is_alphanumeric() || c == '-' {
            slug.push(c);
            last_us = false;
        } else if !last_us && !slug.is_empty() {
            slug.push('_');
            last_us = true;
        }
    }
    let slug = slug.trim_end_matches('_').to_string();
    if slug.is_empty() {
        "tarea".into()
    } else {
        slug
    }
}

/// `git init` en `root` si no hay `.git`, con `.gitignore`, README y AGENTS.
/// Errores se ignoran: el versionado nunca rompe un save.
fn ensure_git(root: &Path) -> std::io::Result<()> {
    // Adjuntos por git-lfs — corre también sobre repos ya inicializados,
    // porque el .gitattributes pudo no existir.
    if root.join(".git").exists() {
        ensure_lfs(root);
        return Ok(());
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["init", "-q"])
        .status()?;
    fs::write(root.join(".gitignore"), ".lrtodo/\n")?;
    fs::write(
        root.join("README.md"),
        "# lrtodo\n\nTracking de tareas con [lrtodo](https://github.com/cyberplant/lrtodo).\n\
         Cada directorio es una lista y cada archivo `NNNN-slug.yaml` una tarea;\n\
         el prefijo numérico es su posición (renombrarlo reordena). Cada cambio\n\
         hace un commit automático — `git log` es la historia, `git revert` deshace.\n",
    )?;
    fs::write(
        root.join("AGENTS.md"),
        "# lrtodo — guía para agentes\n\n\
         Podés leer y editar las tareas directamente: los `.yaml` se ingestan\n\
         automáticamente (los cambios se convierten en ops y se sincronizan).\n\n\
         ## Estructura\n\n\
         - `<lista>/` = una lista; `<lista>/_list.yaml` su nombre/layout.\n\
         - `<lista>/NNNN-slug.yaml` = una tarea; `NNNN` es la posición\n\
           (para reordenar, renombrá el prefijo).\n\n\
         ## Campos de una tarea\n\n\
         ```yaml\n\
         id: 01J...\n\
         title: \"vender la Mac mini\"\n\
         done: false\n\
         created: 1700000000000   # ms epoch\n\
         completed: null          # ms epoch si está hecha\n\
         due: null                # YYYY-MM-DD\n\
         priority: null           # u8\n\
         notes: |                 # markdown libre\n\
         tags: [tag1]\n\
         links:\n\
           - to: 01JXXX           # id de otra tarea, o lista/id\n\
             kind: subtask        # subtask | blocks | related\n\
         ```\n\n\
         Para crear una tarea agregá un `.yaml` con `title` (id opcional: lo\n\
         toma del nombre del archivo). Para borrarla, borrá el archivo.\n\n\
         Adjuntos: `<lista>.assets/` o `NNNN-slug.assets/` (imágenes etc.,\n\
         bajadas a 1600px) — van por git-lfs.\n",
    )?;
    ensure_lfs(root);
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["add", "-A"])
        .status();
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["commit", "-qm", "lrtodo: init"])
        .status();
    Ok(())
}

/// Describe una op para el mensaje de commit git ("lista: agregamos tarea «x»").
pub fn describe_op(list: &str, kind: &OpKind, doc: &ListDoc) -> String {
    let title_of = |id: &str| -> String {
        doc.tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| format!("«{}»", t.title))
            .unwrap_or_else(|| id.to_string())
    };
    match kind {
        OpKind::Add { task } => format!("{list}: agregamos tarea «{}»", task.title),
        OpKind::Delete { id } => format!("{list}: eliminamos tarea {}", title_of(id)),
        OpKind::Set { id, change } => {
            let t = title_of(id);
            let accion = match change {
                Change::Done(true) => "completamos",
                Change::Done(false) => "reabrimos",
                Change::Title(_) => "renombramos",
                Change::Due(_) => "cambiamos el vencimiento de",
                Change::Notes(_) => "editamos las notas de",
                Change::Tags(_) => "cambiamos los tags de",
                Change::Priority(_) => "cambiamos la prioridad de",
            };
            format!("{list}: {accion} tarea {t}")
        }
        OpKind::Link { src, dst, kind } => {
            let rel = match kind {
                LinkKind::Subtask => "es subtarea de",
                LinkKind::Blocks => "bloquea a",
                LinkKind::Related => "se relaciona con",
            };
            format!("{list}: {} {rel} «{dst}»", title_of(src))
        }
        OpKind::Unlink { src, dst, kind } => {
            let rel = match kind {
                LinkKind::Subtask => "deja de ser subtarea de",
                LinkKind::Blocks => "deja de bloquear a",
                LinkKind::Related => "deja de relacionarse con",
            };
            format!("{list}: {} {rel} «{dst}»", title_of(src))
        }
    }
}

/// Nombre de archivo seguro para un adjunto (sin `..` ni separadores).
fn sanitize_asset_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let clean: String = base
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' '))
        .collect();
    if clean.is_empty() {
        "adjunto".into()
    } else {
        clean.trim().to_string()
    }
}

/// Tamaño máximo del lado mayor de las imágenes adjuntadas (px).
const MAX_ASSET_DIM: u32 = 1600;

/// Si `data` decodifica como imagen: baja la resolución a MAX_ASSET_DIM
/// conservando aspecto y re-encodea (jpeg q80; png si tiene alpha).
/// Devuelve `(bytes, ext)` o `None` si no era imagen.
fn shrink_image(data: &[u8], _name: &str) -> Option<(Vec<u8>, &'static str)> {
    use image::ImageEncoder;
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    let mut img = image::load_from_memory(data).ok()?;
    // Orientación EXIF (fotos del celular vienen rotadas como flag, no
    // como píxeles — sin esto quedan de lado o boca abajo).
    img = apply_exif_orientation(data, img);
    let img = if img.width().max(img.height()) > MAX_ASSET_DIM {
        img.resize(
            MAX_ASSET_DIM,
            MAX_ASSET_DIM,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        img
    };
    let mut out = Vec::new();
    if img.color().has_alpha() {
        PngEncoder::new(&mut out)
            .write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )
            .ok()?;
        Some((out, "png"))
    } else {
        JpegEncoder::new_with_quality(&mut out, 80)
            .write_image(
                img.as_bytes(),
                img.width(),
                img.height(),
                img.color().into(),
            )
            .ok()?;
        Some((out, "jpg"))
    }
}

/// Cambia la extensión de un nombre de archivo (`foto.heic` → `foto.jpg`).
fn rename_ext(name: &str, ext: &str) -> String {
    match name.rsplit_once('.') {
        Some((base, _)) if !base.is_empty() => format!("{base}.{ext}"),
        _ => format!("{name}.{ext}"),
    }
}

/// `.gitattributes` con `*.assets/**` por LFS + `git lfs install --local`
/// si git-lfs está instalado. Si falta, los binarios commitean normal.
fn ensure_lfs(root: &Path) {
    let attrs = root.join(".gitattributes");
    if !attrs.exists() {
        let _ = fs::write(&attrs, "*.assets/** filter=lfs diff=lfs merge=lfs -text\n");
    }
    let has_lfs = std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_lfs {
        let _ = std::process::Command::new("git")
            .args(["-C", &*root.to_string_lossy(), "lfs", "install", "--local"])
            .status();
    }
}

/// Aplica la orientación EXIF (fotos del celular): la flag EXIF dice cómo
/// rotar/mirrorar los píxeles para ver la foto "de pie".
fn apply_exif_orientation(data: &[u8], img: image::DynamicImage) -> image::DynamicImage {
    let mut cur = std::io::Cursor::new(data);
    let o = exif::Reader::new()
        .read_from_container(&mut cur)
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        })
        .unwrap_or(1);
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.fliph().rotate270(),
        6 => img.rotate270(),
        7 => img.fliph().rotate90(),
        8 => img.rotate90(),
        _ => img,
    }
}
