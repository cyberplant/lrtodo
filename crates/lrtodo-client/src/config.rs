//! Configuración del lado del cliente (CLI, TUI, tray): campos tolerantes —
//! ignora claves que no conoce (`serve:`, etc.).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfig {
    pub url: String,
    pub token: Option<String>,
}

/// Subconjunto de la config que necesita un cliente (el `serve:` del YAML
/// se ignora acá, así el mismo archivo sirve para todos).
#[derive(Debug, Default, Deserialize)]
pub struct ClientConfig {
    /// Directorio con los YAML (modo local).
    pub data_dir: Option<String>,
    /// Lista por defecto.
    pub default_list: Option<String>,
    /// Si está, se trabaja contra el servidor (con cache offline).
    pub server: Option<RemoteConfig>,
}

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("lrtodo")
        .join("config.yaml")
}

impl ClientConfig {
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let explicit = path.is_some();
        let path = path.map(Path::to_path_buf).unwrap_or_else(default_path);
        match std::fs::read_to_string(&path) {
            Ok(text) if text.trim().is_empty() => Ok(Self::default()),
            Ok(text) => {
                serde_yaml_ng::from_str(&text).with_context(|| format!("{}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("{}", path.display())),
        }
    }

    pub fn data_dir(&self, flag: Option<&str>) -> PathBuf {
        flag.map(str::to_string)
            .or_else(|| std::env::var("LRTODO_DATA_DIR").ok())
            .or_else(|| self.data_dir.clone())
            .map(|p| expand(&p))
            .unwrap_or_else(|| {
                dirs::data_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("lrtodo")
            })
    }

    pub fn remote(&self, flag: Option<&str>) -> Option<RemoteConfig> {
        let token = std::env::var("LRTODO_TOKEN").ok();
        match flag
            .map(str::to_string)
            .or_else(|| std::env::var("LRTODO_SERVER").ok())
        {
            Some(url) if url == "local" || url.is_empty() => None,
            Some(url) => Some(RemoteConfig {
                url,
                token: token.or_else(|| self.server.as_ref().and_then(|s| s.token.clone())),
            }),
            None => self.server.clone().map(|mut s| {
                s.token = token.or(s.token);
                s
            }),
        }
    }
}
