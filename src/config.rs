// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Deserialize, Serialize)]
pub struct Config {
    pub build_path: PathBuf,
    pub cache_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let build_path = std::env::temp_dir().join("bouwer");
        let cache_path = if let Some(path) = home_dir() {
            #[cfg(target_os = "macos")]
            {
                path.join("Library").join("Caches").join("bouwer")
            }
            #[cfg(target_os = "windows")]
            {
                std::env::var_os("LOCALAPPDATA")
                    .map_or_else(|| build_path.clone(), PathBuf::from)
                    .join("bouwer")
                    .join("cache")
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                std::env::var_os("XDG_CACHE_HOME")
                    .map_or_else(|| path.join(".cache"), PathBuf::from)
                    .join("bouwer")
            }
        } else {
            build_path.join("cache")
        };

        Self {
            build_path,
            cache_path,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = Self::config_file_path();
        if let Ok(content) = fs::read_to_string(&path)
            && let Ok(config) = serde_json::from_str::<Self>(&content)
        {
            println!("Loaded config from {}", path.display());
            return config;
        }

        println!("Config not found in {}. Using default one.", path.display());

        let config = Self::default();
        let _ = config.save();
        config
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::config_file_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;
        Ok(())
    }

    fn config_file_path() -> PathBuf {
        let mut path = if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
            PathBuf::from(config_home)
        } else if let Some(home) = home_dir() {
            #[cfg(target_os = "macos")]
            {
                home.join("Library").join("Application Support")
            }
            #[cfg(target_os = "windows")]
            {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or(home)
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                home.join(".config")
            }
        } else {
            std::env::temp_dir()
        };

        path.push("bouwer");
        path.push("config.json");
        path
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}
