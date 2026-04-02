// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct OpenWrtVersions {
    pub versions_list: Vec<String>,
}

#[derive(Deserialize)]
pub struct OpenWrtOverview {
    pub profiles: Vec<Profile>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Preset {
    pub release_series: String,
    pub target: String,
    pub profile_id: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub extra_image_name: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub rootfs_size: String,
    pub packages: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub disabled_services: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub overlay_path: String,
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
