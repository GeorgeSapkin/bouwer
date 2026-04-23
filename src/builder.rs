// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::models::CreateImageInfo;
use futures_util::Stream;
use std::path::{Path, PathBuf};

use crate::containers::{ContainerGuard, Containers, LogStreamExt, Volume};
use crate::domain::{ImageTag, PackageList, ProfileId, Target, Version};

pub struct BuildArgs<'args> {
    pub profile_id: ProfileId,
    pub packages: PackageList,
    pub extra_image_name: Option<&'args str>,
    pub rootfs_size: Option<u32>,
    pub disabled_services: Option<&'args str>,
    pub overlay_path: Option<&'args str>,
}

impl From<BuildArgs<'_>> for Vec<String> {
    fn from(args: BuildArgs<'_>) -> Self {
        let mut cmd = vec![
            "make".to_string(),
            "image".to_string(),
            format!("PROFILE={}", args.profile_id),
            format!("PACKAGES={}", args.packages),
        ];

        let optional_vars = [
            args.extra_image_name
                .map(|v| format!("EXTRA_IMAGE_NAME={v}")),
            args.rootfs_size.map(|v| format!("ROOTFS_PARTSIZE={v}")),
            args.disabled_services
                .map(|v| format!("DISABLED_SERVICES={v}")),
            args.overlay_path.map(|_| "FILES=/overlay".to_string()),
        ];

        cmd.extend(optional_vars.into_iter().flatten());
        cmd
    }
}

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
        args: BuildArgs<'_>,
    ) -> anyhow::Result<ContainerGuard<impl Stream<Item = Result<LogOutput, BollardError>>>> {
        let volumes = self.get_build_volumes(args.overlay_path.map(Path::new));
        self.containers.run(&self.image_tag, args, volumes).await
    }

    pub fn download(&self) -> impl Stream<Item = Result<CreateImageInfo, BollardError>> {
        self.containers.pull_image(&self.image_tag)
    }

    pub async fn exists(&self) -> bool {
        self.containers.image_exists(&self.image_tag).await
    }

    pub async fn fetch_package_list(&self, profile_id: &ProfileId) -> anyhow::Result<PackageList> {
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
        Ok(result.as_str().into())
    }

    pub async fn wait_until_ready(&self) -> bool {
        self.containers.wait_for_image(&self.image_tag, 10).await
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_args_minimal() {
        let original_packages: PackageList = "pkg1 pkg2 pkg3".into();
        let mut packages: PackageList = "pkg1 pkg3".into();
        packages.extend(&original_packages, false);

        let args = BuildArgs {
            profile_id: "test-profile".into(),
            packages,
            extra_image_name: None,
            rootfs_size: None,
            disabled_services: None,
            overlay_path: None,
        };

        let cmd: Vec<String> = args.into();
        assert_eq!(
            cmd,
            vec![
                "make".to_string(),
                "image".to_string(),
                "PROFILE=test-profile".to_string(),
                "PACKAGES=pkg1 pkg3 -pkg2".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_args_full() {
        let original_packages: PackageList = "pkg1 pkg2 pkg3".into();
        let mut packages: PackageList = "pkg1 pkg3".into();
        packages.extend(&original_packages, false);

        let args = BuildArgs {
            profile_id: "test-profile".into(),
            packages,
            extra_image_name: Some("custom-name"),
            rootfs_size: Some(256),
            disabled_services: Some("service1 service2"),
            overlay_path: Some("/path/to/overlay"),
        };

        let cmd: Vec<String> = args.into();
        assert_eq!(
            cmd,
            vec![
                "make".to_string(),
                "image".to_string(),
                "PROFILE=test-profile".to_string(),
                "PACKAGES=pkg1 pkg3 -pkg2".to_string(),
                "EXTRA_IMAGE_NAME=custom-name".to_string(),
                "ROOTFS_PARTSIZE=256".to_string(),
                "DISABLED_SERVICES=service1 service2".to_string(),
                "FILES=/overlay".to_string(),
            ]
        );
    }
}
