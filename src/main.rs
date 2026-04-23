// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context;
use bollard::container::LogOutput;
use chrono::Local;
use futures_util::StreamExt;
use human_bytes::human_bytes;
use slint::platform::Key;
use slint::{Model, SharedString, VecModel};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::task::JoinHandle;

mod builder;
mod cache;
mod client;
#[macro_use]
mod clone;
mod config;
mod containers;
mod device;
mod domain;
mod ssh;
mod state;

use builder::{BuildArgs, ImageBuilder};
use cache::MetadataCache;
use client::OpenWrtClient;
use config::Config;
use containers::Containers;
use device::Device;
use domain::{ImageTag, PackageList, Preset, Profile, ProfileId, ProfileSliceExt, Target, Version};
use ssh::{Ssh, SshOptions};
use state::{AppWindowExt, AppWindowWeakExt, Notification, UIState};

slint::include_modules!();

const BASE_URL: &str = "https://downloads.openwrt.org";
const EXTRA_PACKAGES: &str = "luci luci-app-attendedsysupgrade";
const IMAGE_NAME: &str = "openwrt/imagebuilder";
const MIN_SEARCH_CHARS: usize = 3;
const MIN_SERIES: u8 = 21;
const SIZE_MB: f64 = 1_000_000.0;

const BUILD_MILESTONES: &[(&str, f32, &str)] = &[
    ("Generate local signing", 0.1, "Generating signing keys"),
    ("Building images for", 0.2, "Preparing build"),
    ("Building package index", 0.2, "Building package index"),
    ("Installing packages", 0.4, "Installing packages"),
    ("Finalizing root filesystem", 0.7, "Finalizing filesystem"),
    ("Building images...", 0.8, "Building images"),
    ("Calculating checksums", 0.9, "Calculating checksums"),
];

#[derive(Default)]
pub struct AppCore {
    pub config: Config,
    /// Selected profile packages
    pub packages: PackageList,
    pub profiles: Vec<Profile>,
    pub versions: Vec<Version>,
}

pub type SharedCore = Arc<RwLock<AppCore>>;

type GetImageBuilderFn =
    Arc<dyn Fn(&Version, &Target) -> anyhow::Result<ImageBuilder> + Send + Sync>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|arg| arg == "--version" || arg == "-v") {
        println!("bouwer v{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let backend = i_slint_backend_winit::Backend::new()?;
    slint::platform::set_platform(Box::new(backend)).context("Failed to set Slint platform")?;

    let ui = AppWindow::new()?;
    let config = Config::load();

    let core = Arc::new(RwLock::new(AppCore {
        config: config.clone(),
        ..Default::default()
    }));

    let get_image_builder: GetImageBuilderFn = {
        Arc::new(clone!((core), move |version, target| {
            let containers = Containers::new().context("Failed to initialize container engine")?;
            let build_path = core
                .read()
                .expect("Core lock poisoned")
                .config
                .build_path
                .clone();
            Ok(ImageBuilder::new(
                containers,
                IMAGE_NAME,
                &build_path,
                version,
                target,
            ))
        }))
    };

    let cache = MetadataCache::new(&config.cache_path);
    let client = OpenWrtClient::new(BASE_URL, cache.clone());

    setup_callbacks(&ui, &core, &client, &cache, &get_image_builder);

    init(&ui, &core, client, async || match Containers::new() {
        Ok(containers) => containers.is_available().await,
        Err(_) => false,
    });

    ui.update_state(|s| {
        s.build_path_text = config.build_path.to_string_lossy().as_ref().into();
        s.version = env!("CARGO_PKG_VERSION").into();
        s.switch_to(UIState::LoadingVersions);
    });

    ui.run()?;

    core.read()
        .expect("Core lock poisoned")
        .config
        .save()
        .context("Failed to save configuration")?;

    Ok(())
}

fn setup_callbacks(
    ui: &AppWindow,
    core: &SharedCore,
    client: &OpenWrtClient,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let ui_weak = ui.as_weak();
    let state_bridge = ui.global::<StateBridge>();

    state_bridge.on_build_path_edited(clone!((core), move |path| {
        if let Ok(mut c) = core.write().map_err(|_| eprintln!("Core lock poisoned")) {
            c.config.build_path = path.as_str().into();
        }
    }));

    state_bridge.on_build_requested(clone!((ui_weak, core, get_image_builder), move |data| {
        on_build(&ui_weak, &core, &get_image_builder, &data);
    }));

    state_bridge.on_download_builder_requested(clone!(
        (ui_weak, core, cache, get_image_builder),
        move |data| {
            on_download_builder(&ui_weak, &core, &cache, &get_image_builder, &data);
        }
    ));

    state_bridge.on_delete_builder_requested(clone!((ui_weak, get_image_builder), move |data| {
        on_delete_builder(&ui_weak, &get_image_builder, &data);
    }));

    state_bridge.on_fetch_from_device_requested(clone!(
        (ui_weak, cache, get_image_builder),
        move |data| {
            on_fetch_from_device(&ui_weak, &cache, &get_image_builder, &data);
        }
    ));

    state_bridge.on_fetch_ssh_agent_identities_requested(clone!((ui_weak), move || {
        on_fetch_ssh_agent_identities(&ui_weak);
    }));

    state_bridge.on_load_preset_requested(clone!(
        (ui_weak, core, cache, get_image_builder),
        move |version| {
            on_load_preset(&ui_weak, &core, &cache, &get_image_builder, &version);
        }
    ));

    state_bridge.on_save_preset_requested(clone!((ui_weak), move |data| on_save_preset(
        &ui_weak, data
    )));

    state_bridge.on_open_build_folder_requested(clone!((ui_weak, core), move |target| {
        on_open_build_folder(&ui_weak, &core, &target);
    }));

    state_bridge.on_select_build_folder_requested(clone!((ui_weak, core), move || {
        tokio::spawn(select_folder(
            ui_weak.clone(),
            "Select Build Folder",
            UIState::SelectBuildFolder,
            clone!((core), move |s, path| {
                if let Ok(mut core) = core.write() {
                    core.config.build_path = path.to_path_buf();
                }

                s.build_path_text = path.to_string_lossy().as_ref().into();
            }),
        ));
    }));

    state_bridge.on_select_overlay_folder_requested(clone!((ui_weak), move || {
        tokio::spawn(select_folder(
            ui_weak.clone(),
            "Select Overlay Folder",
            UIState::SelectOverlayFolder,
            move |s, path| {
                s.overlay_path_text = path.to_string_lossy().as_ref().into();
            },
        ));
    }));

    state_bridge.on_packages_edited({
        // Store the handle to the pending debounce task so we can cancel it if
        // the user types again
        let debounce_task = Arc::new(RwLock::new(None::<JoinHandle<()>>));
        clone!((ui_weak, core, debounce_task), move |text| {
            on_packages_edited(&ui_weak, &core, &debounce_task, &text);
        })
    });

    state_bridge.on_profile_search_edited(clone!((ui_weak, core), move |query| {
        on_profile_search(&ui_weak, &core, &query);
    }));

    state_bridge.on_profile_search_key_pressed(clone!((ui_weak), move |data| {
        on_profile_search_key_pressed(&ui_weak, data)
    }));

    state_bridge.on_profile_selected(clone!(
        (ui_weak, core, cache, get_image_builder),
        move |data| {
            on_profile_selected(&ui_weak, &core, &cache, &get_image_builder, &data);
        }
    ));

    state_bridge.on_refresh_builders_requested(clone!((ui_weak), move || {
        tokio::spawn(refresh_downloaded_builders(ui_weak.clone()));
    }));

    state_bridge.on_show_rcs_toggled(clone!((ui_weak, core), move |data| {
        on_show_rcs_toggled(&ui_weak, &core, data);
    }));

    state_bridge.on_version_changed(clone!((ui_weak, core, client), move |version| {
        on_version_changed(&ui_weak, &core, &client, &version);
    }));

    state_bridge.on_update_build_preview(clone!((ui_weak, core), move |data| {
        on_update_build_preview(&ui_weak, &core, &data);
    }));

    ui.window().on_close_requested(clone!((ui_weak), move || {
        let Some(ui) = ui_weak.upgrade() else {
            return slint::CloseRequestResponse::HideWindow;
        };
        if ui.get_state().busy {
            ui.set_notification(
                Notification::Warning,
                Some("An operation is in progress. Please wait for it to finish before closing."),
            );
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    }));
}

fn on_build(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    get_image_builder: &GetImageBuilderFn,
    data: &BuildData,
) {
    let version = Version::from(data.version.as_str());
    let profile_id = ProfileId::from(data.profile_id.as_str());
    let target = Target::from(data.target.as_str());

    let packages = {
        let mut packages = PackageList::from(data.packages.as_str());
        packages.extend(&core.read().expect("Core lock poisoned").packages, false);
        packages
    };

    let extra_image_name: String = data
        .extra_image_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();
    let rootfs_size = data.rootfs_size.cast_unsigned();
    let disabled_services = data.disabled_services.clone();
    let overlay_path = data.overlay_path.clone();

    tokio::spawn(clone!((ui_weak, core, get_image_builder), async move {
        println!("Initializing...");
        ui_weak.switch_state_to(UIState::Building {
            progress: None,
            status: Some("Initializing".into()),
        });

        let build_folder_path = core
            .read()
            .expect("Core lock poisoned")
            .config
            .build_path
            .join(target.to_path());

        let now = Local::now().format("%Y-%m-%d-%H-%M-%S");
        let filename = format!("build-{version}-{profile_id}-{now}.log");
        let log_file_path = build_folder_path.join(filename);

        let _ = tokio::fs::create_dir_all(&build_folder_path).await;

        let mut log_file = tokio::fs::File::create(&log_file_path)
            .await
            .expect("Failed to create build.log");

        println!("Version: {version}\nTarget: {target}\nProfile: {profile_id}");
        println!("Extra image name: {extra_image_name}\nRootFS size: {rootfs_size}");
        println!("Disabled services: {disabled_services}\nRequested packages: {packages}");
        println!("Overlay path: {overlay_path}\n");

        let image_builder = match get_image_builder(&version, &target) {
            Ok(ib) => ib,
            Err(e) => {
                eprintln!("Build failed: {e}");
                ui_weak.update_state(move |s| {
                    s.set_notification(Notification::Error, Some("Failed to build firmware"));
                    s.switch_to(UIState::Error("Build failed".into()));
                });
                return;
            }
        };

        let stream = image_builder
            .build_firmware(BuildArgs {
                profile_id,
                packages,
                extra_image_name: (!extra_image_name.is_empty())
                    .then_some(extra_image_name.as_str()),
                rootfs_size: (rootfs_size > 0).then_some(rootfs_size),
                disabled_services: (!disabled_services.is_empty())
                    .then_some(disabled_services.as_str()),
                overlay_path: (!overlay_path.is_empty()).then_some(overlay_path.as_str()),
            })
            .await;

        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Build failed: {e}");
                ui_weak.update_state(move |s| {
                    s.set_notification(Notification::Error, Some("Failed to build firmware"));
                    s.switch_to(UIState::Error("Build failed".into()));
                });
                return;
            }
        };

        let mut needs_update = false;
        let mut current_progress = 0.0f32;
        let mut current_status = String::new();
        let mut success = true;

        while let Some(log_result) = stream.next().await {
            let line = match log_result {
                Ok(LogOutput::StdOut { message } | LogOutput::StdErr { message }) => {
                    String::from_utf8_lossy(&message).to_string()
                }
                Err(e) => {
                    let msg = format!("Error receiving build logs: {e}");
                    eprintln!("\n{msg}");
                    ui_weak.set_notification(Notification::Error, Some(&msg));
                    success = false;
                    break;
                }
                _ => continue,
            };

            for l in line.lines() {
                println!("{l}");
                if let Err(e) = AsyncWriteExt::write_all(&mut log_file, l.as_bytes()).await {
                    eprintln!("Failed to write to build log file: {e}");
                } else {
                    let _ = AsyncWriteExt::write_all(&mut log_file, b"\n").await;
                }

                if let Some((new_progress, new_status)) = get_build_status(l) {
                    current_progress = current_progress.max(new_progress);
                    current_status = new_status;
                }

                current_progress += 0.0005;
                needs_update = true;
            }

            if needs_update {
                ui_weak.switch_state_to(UIState::Building {
                    progress: Some(current_progress),
                    status: if current_status.is_empty() {
                        None
                    } else {
                        Some(current_status.clone())
                    },
                });
                needs_update = false;
            }
        }

        drop(stream);

        if success {
            println!("Build completed");
            let _ = AsyncWriteExt::write_all(&mut log_file, current_status.as_bytes()).await;

            ui_weak.switch_state_to(UIState::Idle(Some("Build completed".into())));
        } else {
            eprintln!("Build failed");
            ui_weak.switch_state_to(UIState::Error("Build failed".into()));
        }
    }));
}

fn on_delete_builder(
    ui_weak: &slint::Weak<AppWindow>,
    get_image_builder: &GetImageBuilderFn,
    data: &DeleteBuilderData,
) {
    println!("Deleting image builder {}", data.tag);
    ui_weak.switch_state_to(UIState::Idle(Some("Deleting image builder".into())));
    tokio::spawn(clone!((ui_weak, data, get_image_builder), async move {
        let containers = match Containers::new() {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("Failed to initialize container engine: {e}");
                ui_weak.update_state(move |s| {
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(None));
                });
                return;
            }
        };

        match containers.remove_image(data.tag.as_str()).await {
            Ok(()) => {
                let image_tag: ImageTag = data.tag.as_str().into();
                let msg = match (Target::try_from(&image_tag), Version::try_from(&image_tag)) {
                    (Ok(target), Ok(version)) => {
                        format!("Image builder for {version} {target} deleted.")
                    }
                    _ => format!("Image builder {} deleted.", data.tag),
                };
                ui_weak.set_notification(Notification::Info, Some(msg.as_str()));

                refresh_downloaded_builders(ui_weak.clone()).await;

                if !data.profile_id.is_empty() {
                    let version = data.version.as_str().into();
                    let target = data.target.as_str().into();
                    set_image_exists(&ui_weak, &get_image_builder, &version, &target, false).await;
                }
            }
            Err(e) => {
                let msg = format!("Failed to delete image: {e}");
                ui_weak.set_notification(Notification::Error, Some(&msg));
            }
        }

        ui_weak.switch_state_to(UIState::Idle(None));
    }));
}

#[allow(clippy::cast_precision_loss)]
fn on_download_builder(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    data: &DownloadBuilderData,
) {
    let version = data.version.as_str().into();
    let target = data.target.as_str().into();
    let profile_id = data.profile_id.as_str().into();

    ui_weak.update_state(|s| {
        s.image_exists = false;
        s.set_notification(Notification::Info, None);
        s.switch_to(UIState::DownloadingBuilder {
            status: None,
            progress: Some(0.0),
        });
    });

    tokio::spawn(clone!(
        (ui_weak, core, cache, get_image_builder),
        async move {
            let image_builder = match get_image_builder(&version, &target) {
                Ok(ib) => ib,
                Err(e) => {
                    let msg = format!("{e}");
                    ui_weak.update_state(move |s| {
                        s.set_notification(Notification::Error, Some(&msg));
                        s.switch_to(UIState::Idle(None));
                    });
                    return;
                }
            };

            let mut stream = image_builder.download();
            let mut current_progress = 0.0f32;
            let mut layers = HashMap::<String, (i64, i64)>::new();

            while let Some(pull_result) = stream.next().await {
                match pull_result {
                    Ok(info) => {
                        if let (Some(id), Some(pd)) = (info.id, info.progress_detail.as_ref())
                            && let (Some(current), Some(total)) = (pd.current, pd.total)
                            && total > 0
                        {
                            layers.insert(id, (current, total));
                        }

                        let total_current: i64 = layers.values().map(|&(c, _)| c).sum();
                        let total_sum: i64 = layers.values().map(|&(_, t)| t).sum();

                        if total_sum > 0 {
                            current_progress =
                                current_progress.max(total_current as f32 / total_sum as f32);
                        } else if info.progress_detail.is_none() {
                            current_progress = (current_progress + 0.001).min(0.99);
                        }

                        let status_text: String = if total_sum > 0 {
                            let current_mib = total_current as f64 / SIZE_MB;
                            let total_mib = total_sum as f64 / SIZE_MB;
                            format!(
                                "Downloading image builder: {current_mib:.2} / {total_mib:.2} MB"
                            )
                        } else {
                            "Downloading image builder".to_string()
                        };

                        if !status_text.is_empty() {
                            use std::io::{self, Write};
                            print!("\r\x1b[K{status_text}");
                            let _ = io::stdout().flush();
                        }

                        ui_weak.switch_state_to(UIState::DownloadingBuilder {
                            progress: Some(current_progress),
                            status: Some(status_text),
                        });
                    }
                    Err(e) => {
                        let msg = format!("Pull error: {e}");
                        eprintln!("{msg}");
                        ui_weak.set_notification(Notification::Error, Some(&msg));
                        break;
                    }
                }
            }

            println!();
            drop(stream);

            let exists =
                set_image_exists(&ui_weak, &get_image_builder, &version, &target, true).await;
            if exists {
                fetch_and_update_packages(
                    &ui_weak,
                    core,
                    cache,
                    &get_image_builder,
                    &version,
                    &target,
                    &profile_id,
                )
                .await;

                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.global::<StateBridge>()
                        .invoke_refresh_builders_requested();
                });
            } else {
                ui_weak.switch_state_to(UIState::Idle(None));
            }
        }
    ));
}

fn on_fetch_from_device(
    ui_weak: &slint::Weak<AppWindow>,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    data: &FetchFromDeviceData,
) {
    let host = data.host.to_string();
    let user = data.user.to_string();
    let identity = data.identity.to_string();
    let password = data.password.to_string();

    let version: Version = data.version.as_str().into();
    let profile_id: ProfileId = data.profile_id.as_str().into();
    let target: Target = data.target.as_str().into();

    println!("Fetching package list from {host}");
    ui_weak.switch_state_to(UIState::FetchingFromDevice {
        progress: None,
        status: Some(format!("Connecting to {host}")),
    });

    tokio::spawn(clone!(
        (
            ui_weak,
            cache,
            get_image_builder,
            version,
            target,
            profile_id
        ),
        async move {
            let (device_version, device_target, device_profile_id, device_packages) = {
                let result = tokio::task::spawn_blocking(clone!(
                    (host, user, password, identity),
                    move || {
                        let identity = (!identity.is_empty()).then_some(identity.as_str());
                        let password = (!password.is_empty()).then_some(password.as_str());
                        let device = Device::new(SshOptions {
                            host: &host,
                            user: &user,
                            identity,
                            password,
                        });
                        device.fetch_packages()
                    }
                ))
                .await;

                match result {
                    Ok(Ok(res)) => res,
                    Ok(Err(e)) => {
                        let msg = format!("SSH Error: {e}");
                        ui_weak.update_state(move |s| {
                            s.set_notification(Notification::Error, Some(&msg));
                            s.switch_to(UIState::Idle(None));
                        });
                        return;
                    }
                    Err(e) => {
                        let msg = format!("Task Error: {e}");
                        ui_weak.update_state(move |s| {
                            s.set_notification(Notification::Error, Some(&msg));
                            s.switch_to(UIState::Idle(None));
                        });
                        return;
                    }
                }
            };

            if target != device_target
                || (profile_id != device_profile_id && profile_id.as_ref() != "generic")
            {
                ui_weak.update_state(move |s| {
                    let msg = format!("Wrong profile: expected {target} {profile_id}, got {device_target} {device_profile_id}");
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(Some("Failed to fetch package list".into())));
                });
                return;
            }

            // Load original packages for comparison
            let mut original_packages = fetch_packages_for_profile(
                &cache,
                &get_image_builder,
                &version,
                &target,
                &profile_id,
            )
            .await
            .unwrap_or_else(|e| {
                eprintln!("Error fetching package list: {e}");
                PackageList::default()
            });

            original_packages.extend(&PackageList::from(EXTRA_PACKAGES), true);
            let removed_packages = original_packages.diff(&device_packages).to_string();

            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                let mut s = ui.get_state();
                s.packages_text = device_packages.to_string().into();
                s.removed_packages_text = removed_packages.into();

                s.switch_to(UIState::Idle(None));

                if !device_version.same_release_series(&version.to_release_series()) {
                    let device_series = device_version.to_release_series();
                    let msg = format!(
                        "Package list from {device_series} might be incompatible with {}.",
                        version.to_release_series()
                    );
                    s.set_notification(Notification::Warning, Some(&msg));
                }

                ui.set_state(s);

                ui.invoke_recalculate_preview();
            });
        }
    ));
}

fn on_fetch_ssh_agent_identities(ui_weak: &slint::Weak<AppWindow>) {
    tokio::spawn(clone!((ui_weak), async move {
        let identities = match Ssh::list_identities() {
            Ok(identities) => identities,
            Err(e) => {
                eprintln!("Failed to fetch SSH identities: {e}");
                ui_weak
                    .set_notification(Notification::Warning, Some("SSH agent is not available."));
                return;
            }
        };

        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            let mut identities: Vec<SharedString> =
                identities.into_iter().map(SharedString::from).collect();

            // Prepend an empty string to allow a "no identity" selection by
            // default in the UI
            identities.insert(0, SharedString::new());

            let model = Rc::new(VecModel::from(identities));
            ui.update_state(|s| s.ssh_agent_identities = model.into());
        });
    }));
}

fn on_load_preset(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &SharedString,
) {
    ui_weak.switch_state_to(UIState::Idle(None));
    let version = version.as_str().into();

    tokio::spawn(clone!(
        (ui_weak, core, cache, get_image_builder),
        async move {
            let picker = rfd::AsyncFileDialog::new()
                .add_filter("JSON files", &["json"])
                .set_title("Load Preset")
                .pick_file();

            let Some(path) = picker.await else {
                ui_weak.switch_state_to(UIState::Idle(None));
                return;
            };

            ui_weak.switch_state_to(UIState::LoadingPreset);

            let path = PathBuf::from(path);
            if let Err(e) =
                load_preset_from_path(&ui_weak, &core, &path, cache, &get_image_builder, &version)
                    .await
            {
                eprintln!("Error loading preset: {e}");
                let msg = format!("{e}");
                ui_weak.update_state(move |s| {
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(None));
                });
            }
        }
    ));
}

fn on_save_preset(ui_weak: &slint::Weak<AppWindow>, data: BuildData) {
    let preset = Preset::from(data);
    let filename = if preset.extra_image_name.is_empty() {
        format!("preset-{}.json", preset.profile_id)
    } else {
        format!(
            "preset-{}-{}.json",
            preset.profile_id, preset.extra_image_name
        )
    };

    ui_weak.switch_state_to(UIState::SavingPreset);

    tokio::spawn(clone!((ui_weak), async move {
        let picker = rfd::AsyncFileDialog::new()
            .add_filter("JSON files", &["json"])
            .set_title("Save Preset")
            .set_file_name(filename)
            .save_file();

        let Some(path) = picker.await else {
            ui_weak.switch_state_to(UIState::Idle(None));
            return;
        };

        let path = PathBuf::from(path);
        match serde_json::to_string_pretty(&preset) {
            Ok(text) if let Err(e) = std::fs::write(&path, &text) => {
                let msg = format!("Error saving preset {}: {e}", path.display());
                ui_weak.set_notification(Notification::Error, Some(&msg));
            }
            Err(e) => {
                let msg = format!("Failed to serialize preset: {e}");
                ui_weak.set_notification(Notification::Error, Some(&msg));
            }
            _ => {}
        }

        ui_weak.switch_state_to(UIState::Idle(None));
    }));
}

fn on_open_build_folder(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    target: &SharedString,
) {
    let build_folder_path = core
        .read()
        .expect("Core lock poisoned")
        .config
        .build_path
        .join(Target::from(target.as_str()).to_path());

    tokio::spawn(clone!((ui_weak, build_folder_path), async move {
        open_dir(&ui_weak, &build_folder_path).await;
    }));
}

fn on_packages_edited(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    debounce_task: &Arc<RwLock<Option<JoinHandle<()>>>>,
    text: &SharedString,
) {
    ui_weak.update_state(clone!((text), move |s| {
        s.packages_text = text;
    }));

    let mut handle_lock = debounce_task.write().expect("Debounce lock poisoned");
    if let Some(h) = handle_lock.take() {
        h.abort();
    }

    *handle_lock = Some(tokio::spawn(clone!((ui_weak, core, text), async move {
        // Wait for 500ms of inactivity
        tokio::time::sleep(Duration::from_millis(500)).await;

        let removed_packages = core
            .read()
            .expect("Core lock poisoned")
            .packages
            .diff(&text.as_str().into())
            .to_string();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.update_state(|s| s.removed_packages_text = removed_packages.into());
            ui.invoke_recalculate_preview();
        });
    })));
}

fn on_profile_search_key_pressed(
    ui_weak: &slint::Weak<AppWindow>,
    data: ProfileSearchKeyData,
) -> bool {
    let Some(ui) = ui_weak.upgrade() else {
        return false;
    };
    let count = data.profiles.row_count();
    if count == 0 {
        return false;
    }

    let mut current = data.index;
    let count: i32 = count.try_into().expect("Profile count exceeds i32");

    if data.text == <Key as Into<SharedString>>::into(Key::DownArrow) {
        current = (current + 1) % count;
        ui.update_state(move |s| {
            s.current_highlighted_profile_index = current;
        });
        true
    } else if data.text == <Key as Into<SharedString>>::into(Key::UpArrow) {
        current = if current <= 0 { count - 1 } else { current - 1 };
        ui.update_state(move |s| {
            s.current_highlighted_profile_index = current;
        });
        true
    } else if data.text == <Key as Into<SharedString>>::into(Key::Return) {
        if let Some(name) = (current >= 0 && current < count)
            .then(|| {
                data.profiles
                    .row_data(current.try_into().expect("Invalid index"))
            })
            .flatten()
        {
            ui.global::<StateBridge>()
                .invoke_profile_selected(ProfileData {
                    version: data.version,
                    name,
                });
            return true;
        }
        false
    } else {
        false
    }
}

fn on_profile_search(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore, query: &SharedString) {
    let Some(ui) = ui_weak.upgrade() else { return };
    let mut s = ui.get_state();
    s.reset_profile();
    s.current_highlighted_profile_index = -1;

    let filtered = if query.chars().count() >= MIN_SEARCH_CHARS {
        core.read()
            .expect("Core lock poisoned")
            .profiles
            .filter(query)
            .into_iter()
            .map(SharedString::from)
            .collect()
    } else {
        vec![]
    };
    s.profiles = Rc::new(VecModel::from(filtered)).into();
    ui.set_state(s);
}

fn on_profile_selected(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    data: &ProfileData,
) {
    let profile = core
        .read()
        .expect("Core lock poisoned")
        .profiles
        .find_by_display_name(&data.name);

    let Some(profile) = profile else {
        return;
    };

    ui_weak.update_state(clone!((data, profile), move |s| {
        s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
        s.search_text = data.name;
        s.selected_id = profile.id.as_ref().into();
        s.selected_target = profile.target.to_string().into();
        s.selected_model = profile.format_all_models().as_str().into();

        s.switch_to(UIState::FetchingPackages);
    }));

    let profile_id = profile.id.clone();
    let target = profile.target.clone();
    let version = Version::from(data.version.as_str());

    tokio::spawn(clone!(
        (
            ui_weak,
            core,
            cache,
            get_image_builder,
            version,
            target,
            profile_id
        ),
        async move {
            let exists =
                set_image_exists(&ui_weak, &get_image_builder, &version, &target, false).await;
            if exists {
                fetch_and_update_packages(
                    &ui_weak,
                    core,
                    cache,
                    &get_image_builder,
                    &version,
                    &target,
                    &profile_id,
                )
                .await;
            } else {
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.switch_state_to(UIState::Idle(None));

                    let ui_weak = ui.as_weak();
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.invoke_request_profile_search_focus();
                    });
                });
            }
        }
    ));
}

fn on_show_rcs_toggled(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore, data: ShowRcsData) {
    let versions = &core.read().expect("Core lock poisoned").versions;
    let filtered = filter_versions(versions, data.show_rcs);

    ui_weak.update_state(move |s| {
        s.versions = Rc::new(VecModel::from(filtered)).into();

        if !data.show_rcs && data.version.contains("-rc") {
            s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
            s.search_text = "".into();
            s.selected_version = SharedString::new();
            s.reset_profile();
        }
    });
}

fn on_update_build_preview(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore, data: &BuildData) {
    let preview = get_build_command_preview(core, data);
    ui_weak.update_state(move |s| {
        s.build_command_preview = preview.into();
    });
}

fn on_version_changed(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    client: &OpenWrtClient,
    version: &SharedString,
) {
    let version = Version::from(version.as_str());
    ui_weak.switch_state_to(UIState::LoadingProfiles(version.clone()));
    tokio::spawn(clone!((ui_weak, client, core, version), async move {
        if let Ok(profiles) = client.fetch_profiles(&version).await {
            if let Ok(mut c) = core.write() {
                c.profiles = profiles;
            }
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.update_state(|s| {
                    s.reset_profile();
                    s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
                    s.switch_to(UIState::Idle(None));
                });

                let ui_weak = ui.as_weak();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.invoke_request_profile_search_focus();
                });
            });
        } else {
            ui_weak.update_state(move |s| {
                s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
                s.profiles_fetch_failed = true;
                let msg = format!("Failed to fetch profiles for version {version}");
                s.set_notification(Notification::Error, Some(&msg));
                s.switch_to(UIState::Idle(None));
            });
        }
    }));
}

fn init<F, Fut>(
    ui: &AppWindow,
    core: &SharedCore,
    client: OpenWrtClient,
    is_containers_available: F,
) where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    let ui_weak = ui.as_weak();
    let show_rcs = ui.get_state().show_rcs;
    tokio::spawn(clone!((core), async move {
        if !is_containers_available().await {
            ui_weak.update_state(|s| {
                s.switch_to(UIState::Idle(Some("No container engines found".into())));
                s.set_notification(
                    Notification::Error,
                    Some("Podman or Docker not found. Please ensure either of them is running."),
                );
            });
            return;
        }

        let versions = match client.fetch_versions().await {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("Initial load error: {e}");
                ui_weak.set_notification(Notification::Error, Some(&msg));
                return;
            }
        };

        if let Ok(mut c) = core.write() {
            c.versions.clone_from(&versions);
        }

        let filtered = filter_versions(&versions, show_rcs);
        let Some(first_version) = filtered.first().map(|s| Version::from(s.as_str())) else {
            ui_weak.set_notification(Notification::Error, Some("No OpenWrt versions found."));
            return; // No versions, nothing to load
        };

        ui_weak.update_state(clone!((first_version), move |s| {
            s.versions = Rc::new(VecModel::from(filtered)).into();
            s.switch_to(UIState::LoadingProfiles(first_version));
        }));

        let profiles = client.fetch_profiles(&first_version).await;
        match profiles {
            Ok(profiles) if let Ok(mut c) = core.write() => c.profiles = profiles,
            Err(e) => {
                let msg = format!("Initial load error: {e}");
                ui_weak.update_state(move |s| {
                    s.profiles_fetch_failed = true;
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(None));
                });
                return;
            }
            _ => {}
        }

        refresh_downloaded_builders(ui_weak.clone()).await;

        // Always set busy to false and clear profiles at the end of the initial
        // load task
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.update_state(move |s| {
                s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
                s.switch_to(UIState::Idle(None));
            });

            let ui_weak = ui.as_weak();
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.invoke_request_profile_search_focus();
            });
        });
    }));
}

async fn fetch_and_update_packages(
    ui_weak: &slint::Weak<AppWindow>,
    core: SharedCore,
    cache: MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &Version,
    target: &Target,
    profile_id: &ProfileId,
) {
    ui_weak.switch_state_to(UIState::FetchingPackages);

    let packages =
        fetch_packages_for_profile(&cache, get_image_builder, version, target, profile_id).await;

    let packages = match packages {
        Ok(p) => {
            let mut original_packages = p;
            original_packages.extend(&PackageList::from(EXTRA_PACKAGES), true);

            if let Ok(mut c) = core.write() {
                c.packages.clone_from(&original_packages);
            }
            original_packages
        }
        Err(e) => {
            let msg = format!("Error fetching package list for profile: {e}");
            ui_weak.set_notification(Notification::Error, Some(&msg));
            PackageList::default()
        }
    };

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.update_state(|s| {
            s.packages_text = packages.to_string().into();
            s.removed_packages_text = SharedString::new();
            s.switch_to(UIState::Idle(None));
        });

        ui.invoke_recalculate_preview();

        let ui_weak = ui.as_weak();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.invoke_request_profile_search_focus();
        });
    });
}

/// Get default and device-specific packages from the image builder
async fn fetch_packages_for_profile(
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &Version,
    target: &Target,
    profile_id: &ProfileId,
) -> anyhow::Result<PackageList> {
    if let Some(packages) = cache.get_packages(version, target, profile_id).await {
        return Ok(packages);
    }

    println!("Fetching package info for {profile_id} from {version} and {target}");

    let image_builder = get_image_builder(version, target)?;
    let packages = image_builder.fetch_package_list(profile_id).await?;

    cache
        .store_packages(version, target, profile_id, &packages)
        .await;
    Ok(packages)
}

fn filter_versions(versions: &[Version], show_rcs: bool) -> Vec<SharedString> {
    versions
        .iter()
        .filter(|v| (show_rcs || v.rc.is_none()) && v.major >= MIN_SERIES)
        .map(|v| v.to_string().into())
        .collect()
}

fn get_build_command_preview(core: &SharedCore, data: &BuildData) -> String {
    let profile_id = ProfileId::from(data.profile_id.as_str());
    if profile_id.is_empty() {
        return String::new();
    }

    let packages = {
        let mut packages = PackageList::from(data.packages.as_str());
        packages.extend(&core.read().expect("Core lock poisoned").packages, false);
        packages
    };

    let extra_image_name: String = data
        .extra_image_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();
    let rootfs_size = data.rootfs_size.cast_unsigned();

    let args: Vec<String> = BuildArgs {
        profile_id,
        packages,
        extra_image_name: (!extra_image_name.is_empty()).then_some(extra_image_name.as_str()),
        rootfs_size: (rootfs_size > 0).then_some(rootfs_size),
        disabled_services: (!data.disabled_services.is_empty())
            .then_some(data.disabled_services.as_str()),
        overlay_path: (!data.overlay_path.is_empty()).then_some(data.overlay_path.as_str()),
    }
    .into();

    if args.len() <= 2 {
        return args.join(" ");
    }

    format!(
        "{} {}\n\n    {}",
        args[0],
        args[1],
        args[2..].join("\n\n    ")
    )
}

fn get_build_status(line: &str) -> Option<(f32, String)> {
    BUILD_MILESTONES
        .iter()
        .find(|&&(prefix, _, _)| line.starts_with(prefix))
        .map(|&(_, progress, status)| (progress, status.to_owned()))
}

#[cfg(not(target_os = "windows"))]
fn handle_open_result(
    ui_weak: &slint::Weak<AppWindow>,
    path: &Path,
    result: &Result<std::process::ExitStatus, std::io::Error>,
) {
    if let Ok(status) = result
        && status.success()
    {
        return;
    }

    let msg = format!("Failed to open folder: {}", path.display());
    ui_weak.set_notification(Notification::Error, Some(&msg));
}

async fn load_preset_from_path(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    path: &Path,
    cache: MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &Version,
) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(&path).await?;
    let preset: Preset = serde_json::from_str(&content)?;

    let found = {
        let core = core.read().expect("Core lock poisoned");
        core.profiles
            .iter()
            .find(|p| p.id == preset.profile_id && p.target == preset.target)
            .map(|p| {
                let name = p
                    .format()
                    .first()
                    .cloned()
                    .unwrap_or_else(|| p.id.to_string());
                (name, p.format_all_models(), p.target.clone())
            })
    };

    let (name, model, target) = found.ok_or_else(|| {
        anyhow::anyhow!(
            "Profile '{}' for target '{}' not found in current version",
            preset.profile_id,
            preset.target
        )
    })?;

    let profile_id = preset.profile_id.clone();
    let overlay_path = preset.overlay_path.clone();
    ui_weak.update_state(clone!(
        (profile_id, name, model, target, overlay_path),
        move |s| {
            s.overlay_path_text = overlay_path.to_string_lossy().as_ref().into();
            s.packages_text = SharedString::new();
            s.removed_packages_text = SharedString::new();
            s.search_text = name.into();
            s.selected_id = profile_id.as_ref().into();
            s.selected_model = model.into();
            s.selected_target = target.to_string().into();
        }
    ));

    let exists = set_image_exists(ui_weak, get_image_builder, version, &target, false).await;
    if !exists {
        ui_weak.switch_state_to(UIState::Idle(None));
        return Ok(());
    }

    // Load original packages for comparison
    let mut original_packages =
        fetch_packages_for_profile(&cache, get_image_builder, version, &target, &profile_id)
            .await
            .unwrap_or_else(|e| {
                eprintln!("Error fetching packages for preset: {e}");
                PackageList::default()
            });

    // TODO: Disable build when there are no packages
    original_packages.extend(&PackageList::from(EXTRA_PACKAGES), true);
    let removed_packages = original_packages.diff(&preset.packages).to_string();

    if let Ok(mut c) = core.write() {
        c.packages.clone_from(&original_packages);
    }

    ui_weak.upgrade_in_event_loop(clone!((version), move |ui| {
        let mut s = ui.get_state();
        s.disabled_services_text = preset.disabled_services.into();
        s.extra_image_name_text = preset.extra_image_name.into();
        s.overlay_path_text = preset.overlay_path.to_string_lossy().as_ref().into();
        s.packages_text = preset.packages.to_string().into();
        s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
        s.removed_packages_text = removed_packages.into();
        s.rootfs_size_text = if preset.rootfs_size == 0 {
            SharedString::new()
        } else {
            preset.rootfs_size.to_string().into()
        };
        s.switch_to(UIState::Idle(None));

        if !version.same_release_series(&preset.release_series) {
            let msg = format!(
                "Package list from {} might be incompatible with {}.",
                preset.release_series,
                version.to_release_series()
            );
            s.set_notification(Notification::Warning, Some(&msg));
        }

        ui.set_state(s);

        let ui_weak = ui.as_weak();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.invoke_request_profile_search_focus();
            ui.invoke_recalculate_preview();
        });
    }))?;

    Ok(())
}

async fn open_dir(ui_weak: &slint::Weak<AppWindow>, path: &Path) {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        let msg = format!("Folder does not exist: {}", path.display());
        ui_weak.set_notification(Notification::Error, Some(&msg));
        return;
    }

    cfg_select! {
        target_os = "linux" => {
            eprintln!("Attempting to open folder: {}", path.display());
            let status = Command::new("xdg-open")
                .arg(path.as_os_str())
                .status()
                .await;
            handle_open_result(ui_weak, path, &status);
        }
        target_os = "macos" => {
            eprintln!("Attempting to open folder: {}", path.display());
            let status = Command::new("open").arg(path.as_os_str()).status().await;
            handle_open_result(ui_weak, path, &status);
        }
        target_os = "windows" => {
            eprintln!("Attempting to open folder: {}", path.display());
            let _ = Command::new("explorer")
                .arg(path.as_os_str())
                .status()
                .await;
            // Explorer seems to return failure sometimes even when it opens a
            // folder successfully
            // handle_open_result(ui_weak, Path::new(&path_str), &status);
        }
        _ => {
            let msg = "Opening file explorer is not supported on this OS.";
            ui_weak.set_notification(Notification::Error, Some(&msg));
        }
    }
}

#[allow(clippy::cast_precision_loss)]
async fn refresh_downloaded_builders(ui_weak: slint::Weak<AppWindow>) {
    let containers = match Containers::new() {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("Failed to initialize container engine: {e}");
            ui_weak.set_notification(Notification::Error, Some(&msg));
            return;
        }
    };

    let Ok(tags) = containers.list_images(IMAGE_NAME).await else {
        return;
    };

    let mut items_with_keys: Vec<(Option<(Version, Target)>, ImageBuilderItem)> = tags
        .into_iter()
        .map(|(tag_str, size)| {
            let size_str = human_bytes(size as f64);
            let image_tag: ImageTag = tag_str.into();

            if let (Ok(target), Ok(version)) =
                (Target::try_from(&image_tag), Version::try_from(&image_tag))
            {
                return (
                    Some((version.clone(), target.clone())),
                    ImageBuilderItem {
                        tag: image_tag.as_ref().into(),
                        version: version.to_string().into(),
                        target: target.to_string().into(),
                        size: size_str.into(),
                    },
                );
            }

            let shared_tag: SharedString = image_tag.as_ref().into();
            (
                None,
                ImageBuilderItem {
                    tag: shared_tag.clone(),
                    version: shared_tag,
                    target: SharedString::default(),
                    size: size_str.into(),
                },
            )
        })
        .collect();

    items_with_keys.sort_by(|(ka, ia), (kb, ib)| match (ka, kb) {
        (Some((v_a, t_a)), Some((v_b, t_b))) => v_b.cmp(v_a).then_with(|| t_a.cmp(t_b)),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => ia.tag.cmp(&ib.tag),
    });

    let items: Vec<_> = items_with_keys.into_iter().map(|(_, item)| item).collect();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        let model = Rc::new(VecModel::from(items));
        ui.update_state(|s| {
            s.downloaded_builders = model.into();
        });
    });
}

async fn set_image_exists(
    ui_weak: &slint::Weak<AppWindow>,
    get_image_builder: &GetImageBuilderFn,
    version: &Version,
    target: &Target,
    wait: bool,
) -> bool {
    println!("Checking if image exists: {version} {target}");

    let image_builder = match get_image_builder(version, target) {
        Ok(ib) => ib,
        Err(e) => {
            let msg = format!("{e}");
            ui_weak.update_state(move |s| {
                s.set_notification(Notification::Error, Some(&msg));
                s.image_exists = false;
            });
            return false;
        }
    };

    let exists = if wait {
        image_builder.wait_until_ready().await
    } else {
        image_builder.exists().await
    };

    ui_weak.update_state(move |s| {
        s.image_exists = exists;
        s.set_notification(
            Notification::Info,
            if exists {
                None
            } else {
                Some("Image builder not found locally. Please download it first.")
            },
        );
    });

    exists
}

async fn select_folder<F>(
    ui_weak: slint::Weak<AppWindow>,
    dialog_title: &str,
    initial_state: UIState,
    update_state: F,
) where
    F: FnOnce(&mut AppState, &Path) + Send + 'static,
{
    ui_weak.switch_state_to(initial_state);

    let picker = rfd::AsyncFileDialog::new()
        .set_title(dialog_title)
        .pick_folder();

    let Some(path_handle) = picker.await else {
        ui_weak.switch_state_to(UIState::Idle(None));
        return;
    };

    let path = path_handle.path().to_path_buf();
    println!("Selected folder: {}", path.display());
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.update_state(move |s| {
            update_state(s, &path);
            s.switch_to(UIState::Idle(None));
        });

        ui.invoke_recalculate_preview();
    });
}
