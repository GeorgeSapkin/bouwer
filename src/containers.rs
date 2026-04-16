// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::env;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::domain::ImageTag;
use bollard::Docker;
use bollard::container::LogOutput;
use bollard::errors::Error as BollardError;
use bollard::models::{ContainerCreateBody, CreateImageInfo};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, ListImagesOptions, LogsOptions,
    RemoveContainerOptions, RemoveImageOptions, StartContainerOptions, WaitContainerOptions,
};
use bollard::service::HostConfig;
use futures_util::{Stream, StreamExt};
use tokio::fs;

pub struct Volume {
    pub src: PathBuf,
    pub dest: PathBuf,
}

#[derive(Clone)]
pub struct Containers {
    docker: Docker,
    volume_suffix: String,
}

pub struct ContainerGuard<S> {
    containers: Containers,
    container_id: String,
    stream: S,
}

impl<S> ContainerGuard<S> {
    fn new(containers: Containers, container_id: String, stream: S) -> Self {
        Self {
            containers,
            container_id,
            stream,
        }
    }
}

impl<S> Drop for ContainerGuard<S> {
    fn drop(&mut self) {
        let containers = self.containers.clone();
        let id = self.container_id.clone();
        tokio::spawn(async move {
            containers.wait_and_remove(&id).await;
        });
    }
}

impl<S> Stream for ContainerGuard<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.stream).poll_next(cx)
    }
}

/// Extension trait for streams of bollard `LogOutput` results to easily read
/// them into a `String`.
pub trait LogStreamExt: Stream<Item = Result<LogOutput, BollardError>> {
    async fn read_to_string(mut self) -> Result<String, BollardError>
    where
        Self: Sized + Unpin,
    {
        let mut output = String::new();
        while let Some(log) = self.next().await {
            match log? {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    output.push_str(&String::from_utf8_lossy(&message));
                }
                _ => {}
            }
        }
        Ok(output)
    }
}

// Blanket implementation for ContainerGuard to gain LogStreamExt functionality
impl<S> LogStreamExt for ContainerGuard<S> where
    S: Stream<Item = Result<LogOutput, BollardError>> + Unpin
{
}

impl Containers {
    pub fn new() -> anyhow::Result<Self> {
        let docker = if let Ok(host) =
            env::var("DOCKER_HOST").or_else(|_| env::var("CONTAINER_HOST"))
        {
            Docker::connect_with_host(&host)?
        } else {
            cfg_select! {
                unix => {
                    // Check for rootless Podman socket
                    let socket = env::var("XDG_RUNTIME_DIR").map_or_else(
                        |_| "/run/podman/podman.sock".to_string(),
                        |rt| format!("{rt}/podman/podman.sock"),
                    );

                    if Path::new(&socket).exists() {
                        Docker::connect_with_socket(&socket, 120, bollard::API_DEFAULT_VERSION)?
                    } else {
                        // Fallback to defaults
                        Docker::connect_with_local_defaults()?
                    }
                }
                windows => {
                    // Check for Podman default named pipe
                    let podman_pipe = r"\\.\pipe\podman-machine-default";
                    if Path::new(podman_pipe).exists() {
                        Docker::connect_with_named_pipe(podman_pipe, 120, bollard::API_DEFAULT_VERSION)?
                    } else {
                        // Fallback to defaults
                        Docker::connect_with_local_defaults()?
                    }
                }
                _ => Docker::connect_with_local_defaults()?
            }
        };

        Ok(Self {
            docker,
            volume_suffix: Self::get_vol_suffix().to_string(),
        })
    }

    /// Checks if a specific container image exists locally
    pub async fn image_exists(&self, tag: &ImageTag) -> bool {
        self.docker.inspect_image(&tag.0).await.is_ok()
    }

    /// Checks if the container daemon is available
    pub async fn is_available(&self) -> bool {
        self.docker.ping().await.is_ok()
    }

    pub async fn list_images(&self, prefix: &str) -> anyhow::Result<Vec<(String, i64)>> {
        let images = self.docker.list_images(None::<ListImagesOptions>).await?;
        let mut tags = Vec::new();
        for img in images {
            for tag in img.repo_tags {
                if tag.contains(prefix) {
                    tags.push((tag, img.size));
                }
            }
        }
        Ok(tags)
    }

    pub async fn remove_image(&self, tag: &str) -> anyhow::Result<()> {
        self.docker
            .remove_image(tag, None::<RemoveImageOptions>, None)
            .await?;
        Ok(())
    }

    /// Pulls the specified image
    pub fn pull_image(
        &self,
        image_tag: &ImageTag,
    ) -> impl Stream<Item = Result<CreateImageInfo, BollardError>> {
        self.docker.create_image(
            Some(CreateImageOptions {
                from_image: image_tag.into(),
                ..Default::default()
            }),
            None,
            None,
        )
    }

    pub async fn run(
        &self,
        image_tag: &ImageTag,
        cmd: impl Into<Vec<String>>,
        volumes: Vec<Volume>,
    ) -> anyhow::Result<ContainerGuard<impl Stream<Item = Result<LogOutput, BollardError>>>> {
        let binds = if volumes.is_empty() {
            None
        } else {
            let mut binds = Vec::new();
            for volume in volumes {
                let _ = fs::create_dir_all(&volume.src).await;
                binds.push(format!(
                    "{}:{}{}",
                    volume.src.display(),
                    volume.dest.display(),
                    self.volume_suffix
                ));
            }
            Some(binds)
        };

        let host_config = HostConfig {
            binds,
            // Podman-specific rootless build support
            userns_mode: Some("keep-id".to_string()),
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: image_tag.into(),
            cmd: Some(cmd.into()),
            host_config: Some(host_config),
            ..Default::default()
        };

        let id = self
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await?
            .id;

        self.docker
            .start_container(&id, None::<StartContainerOptions>)
            .await?;

        let logs_options = LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            ..Default::default()
        };

        let stream = self.docker.logs(&id, Some(logs_options));
        let guard = ContainerGuard::new(self.clone(), id, stream);
        Ok(guard)
    }

    /// Waits for an image to be ready for use. Useful on Windows where images
    /// might not be immediately available for running after a pull.
    pub async fn wait_for_image(&self, tag: &ImageTag, retries: usize) -> bool {
        for i in 0..retries {
            if self.image_exists(tag).await {
                return true;
            }
            if i < retries - 1 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        false
    }

    fn get_vol_suffix() -> &'static str {
        #[cfg(target_os = "linux")]
        if Path::new("/sys/fs/selinux").exists() {
            return ":z";
        }
        ""
    }

    async fn wait_and_remove(&self, id: &str) {
        let mut wait_stream = self.docker.wait_container(id, None::<WaitContainerOptions>);
        while let Some(res) = wait_stream.next().await {
            if let Err(e) = res {
                eprintln!("Error waiting for container {id}: {e}");
            }
        }

        println!("Closing container connection {id}");

        let _ = self
            .docker
            .remove_container(
                id,
                Some(RemoveContainerOptions {
                    force: true,
                    // Clean up volumes
                    v: true,
                    ..Default::default()
                }),
            )
            .await;
    }
}
