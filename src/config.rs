use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

fn config_path() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".mole");
    fs::create_dir_all(&dir)?;
    Ok(dir.join("config.toml"))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Shell for completions (bash, zsh, fish)
    pub shell: Option<String>,
    /// Editor for `mole edit` (overrides $VISUAL/$EDITOR)
    pub editor: Option<String>,
    /// SSH Config file path (defaults to ~/.ssh/config)
    pub ssh_config: Option<String>,
    /// Health check timeout in seconds
    pub health_timeout: u64,
    /// Max log file size in bytes before rotation
    pub max_log_size: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            editor: None,
            ssh_config: None,
            health_timeout: 5,
            max_log_size: 1_048_576,
        }
    }
}

impl Config {
    /// Load config from ~/.mole/config.toml, falling back to defaults.
    pub fn load() -> Self {
        let path = match config_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        if !path.exists() {
            return Self::default();
        }
        match fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Resolve which editor to use: config > $VISUAL > $EDITOR > vi
    pub fn resolve_editor(&self) -> String {
        if let Some(ref e) = self.editor {
            return e.clone();
        }
        std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_string())
    }

    /// Write a default config file if none exists. Returns the path.
    pub fn init() -> Result<PathBuf> {
        let path = config_path()?;
        if path.exists() {
            return Ok(path);
        }
        let default = Self::default();
        let content = toml::to_string_pretty(&default)
            .context("failed to serialize default config")?;
        fs::write(&path, content)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }

}
