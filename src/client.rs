// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use crate::cache::MetadataCache;
use crate::data::{OpenWrtOverview, OpenWrtVersions, Profile};

pub const USER_AGENT: &str = "BouwerOpenWrtFetcher/1.0";

#[derive(Clone)]
pub struct OpenWrtClient {
    base_url: String,
    cache: MetadataCache,
}

impl OpenWrtClient {
    pub fn new(base_url: &str, cache: MetadataCache) -> Self {
        Self {
            base_url: base_url.to_string(),
            cache,
        }
    }

    pub async fn fetch_profiles(
        &self,
        version: &str,
    ) -> Result<Vec<Profile>, Box<dyn std::error::Error + Send + Sync>> {
        if let Some(profiles) = self.cache.get_profiles(version).await {
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

        self.cache.store_profiles(version, &profiles).await;

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
