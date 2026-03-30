// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
pub struct OpenWrtVersions {
    pub versions_list: Vec<String>,
}

#[derive(Deserialize)]
pub struct OpenWrtOverview {
    pub profiles: Vec<Profile>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ProfileTitle {
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub variant: Option<String>,
    pub title: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Profile {
    pub id: String,
    pub titles: Vec<ProfileTitle>,
    pub target: String,
}

pub const USER_AGENT: &str = "BouwerOpenWrtFetcher/1.0";

#[derive(Clone)]
pub struct OpenWrtClient {
    base_url: String,
    cache_path: PathBuf,
}

impl OpenWrtClient {
    pub fn new(base_url: &str, cache_path: &Path) -> Self {
        Self {
            base_url: base_url.to_string(),
            cache_path: cache_path.to_path_buf(),
        }
    }

    pub async fn fetch_profiles(
        &self,
        version: &str,
    ) -> Result<Vec<Profile>, Box<dyn std::error::Error + Send + Sync>> {
        let cache_file = self.cache_path.join(format!("profiles-{version}.json"));

        if let Ok(content) = tokio::fs::read_to_string(&cache_file).await
            && let Ok(profiles) = serde_json::from_str::<Vec<Profile>>(&content)
        {
            println!("Using cached profiles from {}", cache_file.display());
            return Ok(profiles);
        }

        let url = format!("{}/releases/{version}/.overview.json", self.base_url);
        println!("Fetching profiles from {url}");
        let profiles = tokio::task::spawn_blocking(move || {
            let mut data: OpenWrtOverview = ureq::get(&url)
                .header("User-Agent", USER_AGENT)
                .call()?
                .body_mut()
                .read_json()?;
            data.profiles.sort_by(|a, b| a.id.cmp(&b.id));
            Ok::<Vec<Profile>, Box<dyn std::error::Error + Send + Sync>>(data.profiles)
        })
        .await??;

        let _ = tokio::fs::create_dir_all(&self.cache_path).await;
        if let Ok(content) = serde_json::to_string(&profiles) {
            println!("Caching profiles to {}", cache_file.display());
            let _ = tokio::fs::write(&cache_file, content).await;
        }

        Ok(profiles)
    }

    pub async fn fetch_versions(
        &self,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        let url = format!("{}/.versions.json", self.base_url);
        println!("Fetching versions from {url}");
        tokio::task::spawn_blocking(move || {
            Ok(ureq::get(&url)
                .header("User-Agent", USER_AGENT)
                .call()?
                .body_mut()
                .read_json::<OpenWrtVersions>()?
                .versions_list)
        })
        .await?
    }
}
