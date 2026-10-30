//! `~/.config/lrtodo/config.yaml` + variables de entorno.

use std::path::{Path, PathBuf};

use anyhow::Context;
use lrtodo_server::{HaConfig, ServerConfig, default_bind};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Directorio con los YAML (modo local y `serve`).
    pub data_dir: Option<String>,
    /// Lista por defecto para `ls`/`add`.
    pub default_list: Option<String>,
    /// Si está, la CLI trabaja contra el servidor (con cache offline).
    pub server: Option<RemoteConfig>,
    #[serde(default)]
    pub serve: ServeConfig,
}

pub use lrtodo_client::RemoteConfig;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServeConfig {
    pub bind: Option<String>,
    pub token: Option<String>,
    pub state_dir: Option<String>,
    pub ha: Option<HaSection>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaSection {
    pub url: String,
    pub token: Option<String>,
    /// Leer el token de un archivo (más cómodo para secretos).
    pub token_file: Option<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub poll_secs: Option<u64>,
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

impl Config {
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

    pub fn server_config(
        &self,
        data_dir: PathBuf,
        bind: Option<String>,
    ) -> anyhow::Result<ServerConfig> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let ha = match &self.serve.ha {
            Some(h) => {
                let token = match (&h.token, &h.token_file) {
                    (Some(t), _) => t.clone(),
                    (None, Some(f)) => std::fs::read_to_string(expand(f))
                        .with_context(|| format!("token_file {f}"))?
                        .trim()
                        .to_string(),
                    (None, None) => env("LRTODO_HA_TOKEN")
                        .context("falta serve.ha.token (o LRTODO_HA_TOKEN)")?,
                };
                Some(HaConfig {
                    url: h.url.clone(),
                    token,
                    include: h.include.clone(),
                    exclude: h.exclude.clone(),
                    poll_secs: h.poll_secs.unwrap_or(30),
                })
            }
            None => match (
                env("LRTODO_HA_URL"),
                env("LRTODO_HA_TOKEN"),
                env("SUPERVISOR_TOKEN"),
            ) {
                (Some(url), Some(token), _) => Some(ha_default(url, token)),
                // Dentro de un add-on de Home Assistant.
                (None, None, Some(token)) => {
                    Some(ha_default("http://supervisor/core".into(), token))
                }
                _ => None,
            },
        };
        Ok(ServerConfig {
            data_dir,
            state_dir: env("LRTODO_STATE_DIR")
                .or_else(|| self.serve.state_dir.clone())
                .as_deref()
                .map(expand),
            trust_ingress: env("LRTODO_TRUST_INGRESS").is_some_and(|v| v == "1" || v == "true"),
            bind: bind
                .or_else(|| env("LRTODO_BIND"))
                .or_else(|| self.serve.bind.clone())
                .unwrap_or_else(default_bind),
            token: env("LRTODO_TOKEN").or_else(|| self.serve.token.clone()),
            ha,
        })
    }
}

fn ha_default(url: String, token: String) -> HaConfig {
    let list = |k: &str| {
        std::env::var(k)
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    HaConfig {
        url,
        token,
        include: list("LRTODO_HA_INCLUDE"),
        exclude: list("LRTODO_HA_EXCLUDE"),
        poll_secs: std::env::var("LRTODO_HA_POLL")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30),
    }
}
