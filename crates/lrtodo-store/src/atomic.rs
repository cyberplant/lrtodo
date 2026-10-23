use std::fs::File;
use std::io::Write;
use std::path::Path;

use crate::{Result, StoreError};

/// Escribe `bytes` en `path` de forma atómica: temporal en el mismo
/// directorio → fsync → rename → fsync del directorio. Un lector ve el
/// archivo viejo o el nuevo, nunca uno a medias.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let err = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(err)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".lrtodo-tmp")
        .tempfile_in(dir)
        .map_err(err)?;
    tmp.write_all(bytes).map_err(err)?;
    tmp.as_file().sync_all().map_err(err)?;
    tmp.persist(path).map_err(|e| err(e.error))?;
    #[cfg(unix)]
    File::open(dir).and_then(|d| d.sync_all()).map_err(err)?;
    Ok(())
}
