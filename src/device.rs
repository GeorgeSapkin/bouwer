// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use anyhow::Context;
use std::io::Read;

use crate::domain::{Package, PackageList, ProfileId, Target, Version};
use crate::ssh::{Ssh, SshOptions};
use serde::Deserialize;

const APK_QUERY: &str = "apk query --installed --world --fields name,tags --format json";
const OPKG_QUERY: &str = "opkg list-installed --strip-abi";
const UBUS_CMD: &str = "ubus call system board";

#[derive(Deserialize)]
struct ApkPackage {
    name: String,
    tags: Option<Vec<String>>,
}

pub struct Device<'device> {
    ssh: Ssh<'device>,
}

impl<'device> Device<'device> {
    pub fn new(options: SshOptions<'device>) -> Self {
        Self {
            ssh: Ssh::new(options),
        }
    }

    pub fn fetch_packages(&self) -> anyhow::Result<(Version, Target, ProfileId, PackageList)> {
        let (version, target, profile_id) = self.get_system_info()?;

        let is_apk = version.major >= 25;

        let mut package_buffer = String::new();
        {
            let cmd = if is_apk { APK_QUERY } else { OPKG_QUERY };
            let mut channel = self.ssh.connect().context("Failed to connect to device")?;
            channel
                .exec(cmd)
                .context("Failed to execute package query")?;
            channel
                .read_to_string(&mut package_buffer)
                .context("Failed to read package output")?;
        }

        let packages: Vec<Package> = if is_apk {
            serde_json::from_str::<Vec<ApkPackage>>(&package_buffer)
                .context("Failed to parse APK output")?
                .into_iter()
                .map(|pkg| {
                    let abi = pkg.tags.as_ref().and_then(|tags| {
                        tags.iter()
                            .find_map(|t| t.strip_prefix("openwrt:abiversion="))
                    });
                    sanitize_apk_package_name(&pkg.name, abi).into()
                })
                .collect()
        } else {
            package_buffer
                .lines()
                .filter_map(|line| line.split_once(" - "))
                .map(|(name, _)| name.trim().to_string().into())
                .collect()
        };

        Ok((version, target, profile_id, packages.into()))
    }

    fn get_system_info(&self) -> anyhow::Result<(Version, Target, ProfileId)> {
        let mut channel = self
            .ssh
            .connect()
            .context("Failed to connect for system info")?;
        channel.exec(UBUS_CMD)?;

        let mut buffer = String::new();
        channel.read_to_string(&mut buffer)?;

        let board: serde_json::Value =
            serde_json::from_str(buffer.trim()).context("Invalid system board JSON")?;
        let Some(release) = board.get("release") else {
            anyhow::bail!("Missing 'release' object in board info");
        };

        let Some(board_name) = board.get("board_name").and_then(|v| v.as_str()) else {
            anyhow::bail!("Missing 'board_name' in board info");
        };
        let Some(version) = release.get("version").and_then(|v| v.as_str()) else {
            anyhow::bail!("Missing version in board info");
        };
        let Some(target) = release.get("target").and_then(|v| v.as_str()) else {
            anyhow::bail!("Missing target in board info");
        };

        Ok((version.into(), target.into(), board_name.into()))
    }
}

fn sanitize_apk_package_name(name: &str, abi: Option<&str>) -> String {
    abi.and_then(|a| name.strip_suffix(a))
        .unwrap_or(name)
        .trim_end_matches('-')
        .to_string()
}
