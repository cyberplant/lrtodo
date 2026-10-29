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

use lrtodo_core::{Caps, Clock, ListDoc, ListKind, ListMeta, Rng, Task};
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
        })
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
        Ok(meta(id, &header))
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
        write_atomic(&self.state_path(id), &bytes)
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
                task.id = stem;
            }
            tasks.push(task);
        }
        // Orden estable: por fecha de creación y luego id (ULID ~ creación).
        tasks.sort_by(|a, b| (&a.created, &a.id).cmp(&(&b.created, &b.id)));
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
                for task in tasks {
                    wanted.insert(format!("{}.yaml", task.id), task);
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
