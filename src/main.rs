// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Context;
use bollard::container::LogOutput;
use chrono::Local;
use futures_util::StreamExt;
use slint::platform::Key;
use slint::{Model, SharedString, VecModel};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
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
mod domain;
mod state;

use builder::ImageBuilder;
use cache::MetadataCache;
use client::OpenWrtClient;
use config::Config;
use containers::Containers;
use domain::{Preset, Profile, ProfileId, ProfileSliceExt, Target, Version};
use state::{AppWindowExt, AppWindowWeakExt, Notification, UIState};

slint::include_modules!();

const ABOUT_URL: &str = "https://github.com/georgesapkin/bouwer";
const BASE_URL: &str = "https://downloads.openwrt.org";
const EXTRA_PACKAGES: &str = "luci luci-app-attendedsysupgrade";
const IMAGE_NAME: &str = "openwrt/imagebuilder";
const MIN_SEARCH_CHARS: usize = 3;
const MIN_SERIES: u8 = 21;
const SIZE_MIB: f32 = 1024.0 * 1024.0;

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
    pub packages: Vec<String>,
    pub profiles: Vec<Profile>,
    pub versions: Vec<Version>,
}

pub type SharedCore = Arc<RwLock<AppCore>>;

type GetImageBuilderFn = Arc<dyn Fn(&Version, &Target) -> ImageBuilder + Send + Sync>;

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
            let containers = Containers::new().unwrap();
            let build_path = core.read().unwrap().config.build_path.clone();
            ImageBuilder::new(containers.clone(), IMAGE_NAME, &build_path, version, target)
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
        .unwrap()
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
        if let Ok(mut c) = core.write() {
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

    state_bridge.on_show_rcs_toggled(clone!((ui_weak, core), move |data| {
        on_show_rcs_toggled(&ui_weak, &core, data);
    }));

    state_bridge.on_version_changed(clone!((ui_weak, core, client), move |version| {
        on_version_changed(&ui_weak, &core, &client, &version);
    }));

    state_bridge.on_open_github_link(|| {
        if let Err(e) = webbrowser::open(ABOUT_URL) {
            eprintln!("Failed to open GitHub link: {e}");
        }
    });
}

fn on_build(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    get_image_builder: &GetImageBuilderFn,
    data: &BuildData,
) {
    let version = Version::from(data.version.as_str());
    let target = Target::from(data.target.as_str());
    let profile_id = ProfileId::from(data.profile_id.as_str());

    let packages = {
        let current_set: HashSet<_> = data.packages.split_whitespace().collect();
        let core = core.read().unwrap();
        let selected = data.packages.as_str();
        let removed = core
            .packages
            .iter()
            .filter(|p| !current_set.contains(p.as_str()))
            .enumerate()
            .fold(String::new(), |mut acc, (i, p)| {
                if i > 0 {
                    acc.push(' ');
                }
                write!(acc, "-{p}").unwrap();
                acc
            });
        format!("{selected} {removed}")
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

        let build_path = core.read().unwrap().config.build_path.clone();
        let build_folder_path = build_path.join(target.to_path());

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

        let image_builder = get_image_builder(&version, &target);
        let stream = image_builder
            .build_firmware(
                &profile_id,
                &packages,
                &extra_image_name,
                rootfs_size,
                &disabled_services,
                (!overlay_path.is_empty()).then(|| Path::new(overlay_path.as_str())),
            )
            .await;

        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                let msg = "Failed to build firmware";
                eprintln!("{msg}: {e}");
                ui_weak.update_state(move |s| {
                    s.set_notification(Notification::Error, Some(msg));
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
                AsyncWriteExt::write_all(&mut log_file, l.as_bytes())
                    .await
                    .unwrap();
                AsyncWriteExt::write_all(&mut log_file, b"\n")
                    .await
                    .unwrap();

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
            AsyncWriteExt::write_all(&mut log_file, current_status.as_bytes())
                .await
                .unwrap();

            ui_weak.switch_state_to(UIState::Idle(Some("Build completed".into())));
        } else {
            ui_weak.switch_state_to(UIState::Error("Build failed".into()));
        }
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
    let version = Version::from(data.version.as_str());
    let target = Target::from(data.target.as_str());
    let profile_id = ProfileId::from(data.profile_id.as_str());

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
            let image_builder = get_image_builder(&version, &target);
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
                            let current_mib = total_current as f32 / SIZE_MIB;
                            let total_mib = total_sum as f32 / SIZE_MIB;
                            format!(
                                "Downloading image builder: {current_mib:.2} / {total_mib:.2} MiB"
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
            } else {
                ui_weak.switch_state_to(UIState::Idle(None));
            }
        }
    ));
}

fn on_load_preset(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &SharedString,
) {
    ui_weak.switch_state_to(UIState::Idle(None));
    let version = Version::from(version.as_str());

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
        let text = serde_json::to_string_pretty(&preset).unwrap();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("Error saving preset {}: {e}", path.display());
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
        .unwrap()
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

    let mut handle_lock = debounce_task.write().unwrap();
    if let Some(h) = handle_lock.take() {
        h.abort();
    }

    *handle_lock = Some(tokio::spawn(clone!((ui_weak, core, text), async move {
        // Wait for 500ms of inactivity
        tokio::time::sleep(Duration::from_millis(500)).await;

        let current_set: HashSet<_> = text.split_whitespace().collect();
        let core = core.read().unwrap();
        let removed_str = core
            .packages
            .iter()
            .filter(|p| !current_set.contains(p.as_str()))
            .enumerate()
            .fold(String::new(), |mut acc, (i, p)| {
                if i > 0 {
                    acc.push(' ');
                }
                write!(acc, "{p}").unwrap();
                acc
            });
        ui_weak.update_state(|s| {
            s.removed_packages_text = removed_str.into();
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
    let count_i32: i32 = count.try_into().unwrap();

    if data.text == <Key as Into<SharedString>>::into(Key::DownArrow) {
        current = (current + 1) % count_i32;
        ui.update_state(move |s| {
            s.current_highlighted_profile_index = current;
        });
        true
    } else if data.text == <Key as Into<SharedString>>::into(Key::UpArrow) {
        current = if current <= 0 {
            count_i32 - 1
        } else {
            current - 1
        };
        ui.update_state(move |s| {
            s.current_highlighted_profile_index = current;
        });
        true
    } else if data.text == <Key as Into<SharedString>>::into(Key::Return) {
        if let Some(val) = (current >= 0 && current < count_i32)
            .then(|| data.profiles.row_data(current.try_into().unwrap()))
            .flatten()
        {
            ui.global::<StateBridge>()
                .invoke_profile_selected(ProfileData {
                    version: data.version,
                    name: val,
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

    let core = core.read().unwrap();
    let filtered = if query.chars().count() >= MIN_SEARCH_CHARS {
        core.profiles
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
    let profile = {
        let core = core.read().unwrap();
        core.profiles.find_by_display_name(&data.name)
    };

    let Some(profile) = profile else {
        return;
    };

    ui_weak.update_state(clone!((data, profile), move |s| {
        s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
        s.search_text = data.name;
        s.selected_id = profile.id.0.as_str().into();
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
    let core = core.read().unwrap();
    let filtered = filter_versions(&core.versions, data.show_rcs);

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

        let versions_res = client.fetch_versions().await;
        if let Err(e) = versions_res {
            let msg = format!("Initial load error: {e}");
            ui_weak.set_notification(Notification::Error, Some(&msg));
            return;
        }

        let versions = versions_res.unwrap();
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

        let profiles_res = client.fetch_profiles(&first_version).await;
        match profiles_res {
            Ok(profiles) => {
                if let Ok(mut c) = core.write() {
                    c.profiles = profiles;
                }
            }
            Err(e) => {
                let msg = format!("Initial load error: {e}");
                ui_weak.update_state(move |s| {
                    s.profiles_fetch_failed = true;
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(None));
                });
                return;
            }
        }

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
            let packages = p + " " + EXTRA_PACKAGES;

            let mut pkgs_vec = packages
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>();
            pkgs_vec.sort_unstable();
            pkgs_vec.dedup();

            if let Ok(mut c) = core.write() {
                c.packages.clone_from(&pkgs_vec);
            }
            pkgs_vec.join(" ")
        }
        Err(e) => {
            let msg = format!("Error fetching package list for profile: {e}");
            ui_weak.set_notification(Notification::Error, Some(&msg));
            String::new()
        }
    };

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.update_state(|s| {
            s.packages_text = packages.into();
            s.removed_packages_text = SharedString::new();
            s.switch_to(UIState::Idle(None));
        });

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
) -> anyhow::Result<String> {
    if let Some(packages) = cache.get_packages(version, target, profile_id).await {
        return Ok(packages);
    }

    println!("Fetching package info for {profile_id} from {version} and {target}");

    let image_builder = get_image_builder(version, target);
    let result = image_builder.fetch_package_list(profile_id).await?;

    cache
        .store_packages(version, target, profile_id, &result)
        .await;
    Ok(result)
}

fn filter_versions(versions: &[Version], show_rcs: bool) -> Vec<SharedString> {
    versions
        .iter()
        .filter(|v| (show_rcs || v.rc.is_none()) && v.major >= MIN_SERIES)
        .map(|v| SharedString::from(v.to_string()))
        .collect()
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
        let core = core.read().unwrap();
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
            s.selected_id = profile_id.0.as_str().into();
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
    let fetched_pkgs =
        fetch_packages_for_profile(&cache, get_image_builder, version, &target, &profile_id)
            .await
            .unwrap_or_else(|e| {
                eprintln!("Error fetching packages for preset: {e}");
                EXTRA_PACKAGES.to_string()
            });

    // TODO: Disable build when there are no packages
    let packages_source = if fetched_pkgs.is_empty() {
        EXTRA_PACKAGES.to_string()
    } else {
        fetched_pkgs + " " + EXTRA_PACKAGES
    };

    let mut original_pkgs = packages_source
        .split_whitespace()
        .map(String::from)
        .collect::<Vec<_>>();
    original_pkgs.sort_unstable();
    original_pkgs.dedup();

    let current_set: HashSet<_> = preset.packages.split_whitespace().collect();
    let removed: Vec<String> = original_pkgs
        .iter()
        .filter(|p| !current_set.contains(p.as_str()))
        .cloned()
        .collect();
    let removed_str = removed.join(" ");

    ui_weak.upgrade_in_event_loop(clone!((original_pkgs, core, version), move |ui| {
        let mut s = ui.get_state();
        s.disabled_services_text = preset.disabled_services.into();
        s.extra_image_name_text = preset.extra_image_name.into();
        s.overlay_path_text = preset.overlay_path.to_string_lossy().as_ref().into();
        s.packages_text = preset.packages.into();
        s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
        s.removed_packages_text = removed_str.into();
        s.rootfs_size_text = if preset.rootfs_size == 0 {
            SharedString::new()
        } else {
            preset.rootfs_size.to_string().into()
        };
        s.switch_to(UIState::Idle(None));

        if !version.same_release_series(&preset.release_series) {
            let info_msg = format!(
                "Package list from preset series {} might be incompatible with {}.",
                preset.release_series,
                version.to_release_series()
            );
            s.set_notification(Notification::Warning, Some(&info_msg));
        }

        ui.set_state(s);

        if let Ok(mut c) = core.write() {
            c.packages = original_pkgs;
        }

        let ui_weak = ui.as_weak();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.invoke_request_profile_search_focus();
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

    #[cfg(target_os = "linux")]
    {
        eprintln!("Attempting to open folder: {}", path.display());
        let status = Command::new("xdg-open")
            .arg(path.as_os_str())
            .status()
            .await;
        handle_open_result(ui_weak, path, &status);
    }
    #[cfg(target_os = "macos")]
    {
        eprintln!("Attempting to open folder: {}", path.display());
        let status = Command::new("open").arg(path.as_os_str()).status().await;
        handle_open_result(ui_weak, path, &status);
    }
    #[cfg(target_os = "windows")]
    {
        eprintln!("Attempting to open folder: {}", path.display());
        let _ = Command::new("explorer")
            .arg(path.as_os_str())
            .status()
            .await;
        // Explorer seems to return failure sometimes even when it opens a
        // folder successfully
        // handle_open_result(ui_weak, Path::new(&path_str), &status);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let msg = "Opening file explorer is not supported on this OS.";
        ui_weak.set_notification(Notification::Error, Some(&msg));
    }
}

async fn set_image_exists(
    ui_weak: &slint::Weak<AppWindow>,
    get_image_builder: &GetImageBuilderFn,
    version: &Version,
    target: &Target,
    wait: bool,
) -> bool {
    println!("Checking if image exists: {version} {target}");

    let image_builder = get_image_builder(version, target);
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
    ui_weak.update_state(move |s| {
        update_state(s, &path);
        s.switch_to(UIState::Idle(None));
    });
}
