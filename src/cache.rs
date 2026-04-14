// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use crate::domain::{Profile, ProfileId, Target, Version};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct PackageCacheRecord {
    packages: String,
}

#[derive(Clone)]
pub struct MetadataCache {
    cache_path: PathBuf,
}

impl MetadataCache {
    pub fn new(cache_path: &Path) -> Self {
        Self {
            cache_path: cache_path.to_path_buf(),
        }
    }

    pub async fn get_profiles(&self, version: &Version) -> Option<Vec<Profile>> {
        let cache_file = self.get_profile_path(version);
        let content = tokio::fs::read_to_string(&cache_file).await.ok()?;
        let profiles = serde_json::from_str::<Vec<Profile>>(&content).ok()?;

        println!("Using cached profiles from {}", cache_file.display());
        Some(profiles)
    }

    pub async fn store_profiles(&self, version: &Version, profiles: &[Profile]) {
        let _ = tokio::fs::create_dir_all(&self.cache_path).await;
        let cache_file = self.get_profile_path(version);

        if let Ok(content) = serde_json::to_string(&profiles) {
            println!("Caching profiles to {}", cache_file.display());
            let _ = tokio::fs::write(&cache_file, content).await;
        }
    }

    pub async fn get_packages(
        &self,
        version: &Version,
        target: &Target,
        profile_id: &ProfileId,
    ) -> Option<String> {
        let cache_file = self.get_package_path(version, target, profile_id);
        let content = tokio::fs::read_to_string(&cache_file).await.ok()?;
        let cached = serde_json::from_str::<PackageCacheRecord>(&content).ok()?;

        println!("Using cached packages from {}", cache_file.display());
        Some(cached.packages)
    }

    pub async fn store_packages(
        &self,
        version: &Version,
        target: &Target,
        profile_id: &ProfileId,
        packages: &str,
    ) {
        if packages.is_empty() {
            return;
        }

        let _ = tokio::fs::create_dir_all(&self.cache_path).await;
        let cache_file = self.get_package_path(version, target, profile_id);
        let cache_data = PackageCacheRecord {
            packages: packages.to_string(),
        };
        if let Ok(content) = serde_json::to_string(&cache_data) {
            println!("Caching packages to {}", cache_file.display());
            let _ = tokio::fs::write(&cache_file, content).await;
        }
    }

    fn get_profile_path(&self, version: &Version) -> PathBuf {
        self.cache_path.join(format!("profiles-{version}.json"))
    }

    fn get_package_path(
        &self,
        version: &Version,
        target: &Target,
        profile_id: &ProfileId,
    ) -> PathBuf {
        self.cache_path.join(format!(
            "packages-{version}-{}-{profile_id}.json",
            target.to_slug()
        ))
    }
}
