// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::fs;
use std::process::Stdio;
use tokio::process::Command;

const SEL_SUFFIX: &str = ":z";

pub struct Volume {
    pub src: String,
    pub dest: String,
}

/// Checks if the podman command is available in the system PATH.
pub async fn podman_available() -> bool {
    Command::new("podman")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Checks if a specific container image exists locally.
pub async fn image_exists(image_tag: &str) -> bool {
    Command::new("podman")
        .args(["image", "exists", image_tag])
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Returns a Command configured to pull the specified image.
pub fn pull_image(image_tag: &str) -> Command {
    let mut cmd = Command::new("podman");
    cmd.args(["pull", image_tag]);
    cmd
}

/// Create a podman run command with optional volume mounting for the build environment.
pub fn run_container(image_tag: &str, container_args: &[&str], volumes: &[Volume]) -> Command {
    let mut cmd = Command::new("podman");
    cmd.args(["run", "--rm"]);

    if !volumes.is_empty() {
        cmd.args(["--userns=keep-id"]);
        let vol_suffix = get_vol_suffix();
        for volume in volumes {
            let _ = fs::create_dir_all(&volume.src);
            cmd.args(["-v", &format!("{}:{}{vol_suffix}", volume.src, volume.dest)]);
        }
    }

    cmd.arg(image_tag);
    cmd.args(container_args);
    cmd
}

fn get_vol_suffix() -> String {
    #[cfg(target_os = "linux")]
    {
        if fs::metadata("/sys/fs/selinux").is_ok() {
            return SEL_SUFFIX.to_string();
        }
    }
    String::new()
}
