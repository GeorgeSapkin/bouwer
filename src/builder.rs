// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::models::CreateImageInfo;
use futures_util::Stream;
use std::path::{Path, PathBuf};

use crate::containers::{ContainerGuard, Containers, LogStreamExt, Volume};
use crate::domain::{ImageTag, ProfileId, Target, Version};

pub struct ImageBuilder {
    build_path: PathBuf,
    containers: Containers,
    image_tag: ImageTag,
}

impl ImageBuilder {
    pub fn new(
        containers: Containers,
        image_base: &str,
        build_path: &Path,
        version: &Version,
        target: &Target,
    ) -> Self {
        let image_tag = ImageTag::new(target, version, image_base);

        Self {
            build_path: build_path.to_path_buf(),
            containers,
            image_tag,
        }
    }

    pub async fn build_firmware(
        &self,
        profile_id: &ProfileId,
        packages: &str,
        extra_image_name: &str,
        rootfs_size: u32,
        disabled_services: &str,
        overlay_path: Option<&Path>,
    ) -> anyhow::Result<ContainerGuard<impl Stream<Item = Result<LogOutput, BollardError>>>> {
        let cmd = Self::create_build_args(
            profile_id,
            packages,
            extra_image_name,
            rootfs_size,
            disabled_services,
            overlay_path.is_some(),
        );

        let volumes = self.get_build_volumes(overlay_path);
        self.containers.run(&self.image_tag, cmd, volumes).await
    }

    pub fn download(&self) -> impl Stream<Item = Result<CreateImageInfo, BollardError>> {
        self.containers.pull_image(&self.image_tag)
    }

    pub async fn exists(&self) -> bool {
        self.containers.image_exists(&self.image_tag).await
    }

    pub async fn fetch_package_list(&self, profile_id: &ProfileId) -> anyhow::Result<String> {
        let stdout = {
            let stream = self
                .containers
                .run(
                    &self.image_tag,
                    vec!["make".to_string(), "info".to_string()],
                    vec![],
                )
                .await?;
            stream.read_to_string().await?
        };

        let mut default_pkgs = String::new();
        let mut device_pkgs = String::new();
        let mut in_profile_block = false;
        let profile_prefix = format!("{profile_id}:");

        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(pkgs) = trimmed.strip_prefix("Default Packages:") {
                default_pkgs = pkgs.trim().to_string();
            } else if trimmed.starts_with(&profile_prefix) {
                in_profile_block = true;
            } else if in_profile_block && let Some(pkgs) = trimmed.strip_prefix("Packages:") {
                device_pkgs = pkgs.trim().to_string();
                break; // Found both, exit
            }
        }

        let result = format!("{default_pkgs} {device_pkgs}");
        Ok(result)
    }

    pub async fn wait_until_ready(&self) -> bool {
        self.containers.wait_for_image(&self.image_tag, 10).await
    }

    pub fn create_build_args(
        profile_id: &ProfileId,
        packages: &str,
        extra_image_name: &str,
        rootfs_size: u32,
        disabled_services: &str,
        has_overlay: bool,
    ) -> Vec<String> {
        let mut args = vec![
            "make".to_string(),
            "image".to_string(),
            format!("PROFILE={profile_id}"),
            format!("PACKAGES={packages}"),
        ];

        let optional_vars = [
            (!extra_image_name.is_empty()).then(|| format!("EXTRA_IMAGE_NAME={extra_image_name}")),
            (rootfs_size > 0).then(|| format!("ROOTFS_PARTSIZE={rootfs_size}")),
            (!disabled_services.is_empty())
                .then(|| format!("DISABLED_SERVICES={disabled_services}")),
            has_overlay.then(|| "FILES=/overlay".to_string()),
        ];

        args.extend(optional_vars.into_iter().flatten());
        args
    }

    fn get_build_volumes(&self, overlay_path: Option<&Path>) -> Vec<Volume> {
        let dl_path = self.build_path.join("dl");
        let mut volumes = vec![
            Volume {
                src: self.build_path.clone(),
                dest: PathBuf::from("/builder/bin/targets"),
            },
            Volume {
                src: dl_path,
                dest: PathBuf::from("/builder/dl"),
            },
        ];

        if let Some(path) = overlay_path {
            volumes.push(Volume {
                src: path.to_path_buf(),
                dest: PathBuf::from("/overlay"),
            });
        }

        volumes
    }
}
