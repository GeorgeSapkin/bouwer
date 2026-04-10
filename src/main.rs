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
mod data;
mod profiles;
mod state;

use builder::ImageBuilder;
use cache::MetadataCache;
use client::OpenWrtClient;
use config::Config;
use containers::Containers;
use data::{Preset, Profile};
use profiles::{
    filter_profiles, find_profile_by_display_name, format_profile, get_all_models_string,
};
use state::{AppWindowExt, Notification, UIState};

slint::include_modules!();

const ABOUT_URL: &str = "https://github.com/georgesapkin/bouwer";
const BASE_URL: &str = "https://downloads.openwrt.org";
const EXTRA_PACKAGES: &str = "luci luci-app-attendedsysupgrade";
const IMAGE_NAME: &str = "openwrt/imagebuilder";
const MIN_SEARCH_CHARS: usize = 3;
const MIN_SERIES: usize = 21;
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
    pub versions: Vec<String>,
}

pub type SharedCore = Arc<RwLock<AppCore>>;

type GetImageBuilderFn = Arc<dyn Fn(&str, &str) -> ImageBuilder + Send + Sync>;

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
            let build_path = &core.read().unwrap().config.build_path;
            ImageBuilder::new(containers.clone(), IMAGE_NAME, build_path, version, target)
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

    state_bridge.on_build_requested(clone!((ui_weak, core, get_image_builder), move |_| {
        on_build(&ui_weak, &core, &get_image_builder);
    }));

    state_bridge.on_download_requested(clone!(
        (ui_weak, core, cache, get_image_builder),
        move || {
            on_download(&ui_weak, &core, &cache, &get_image_builder);
        }
    ));

    state_bridge.on_load_preset_requested(clone!(
        (ui_weak, core, cache, get_image_builder),
        move || {
            on_load_preset(&ui_weak, &core, &cache, &get_image_builder);
        }
    ));

    state_bridge.on_save_preset_requested(clone!(
        (ui_weak),
        move |target,
              profile_id,
              extra_image_name,
              rootfs_size,
              packages,
              disabled_services,
              overlay_path| {
            on_save_preset(
                &ui_weak,
                Preset {
                    release_series: String::new(),
                    target: target.into(),
                    profile_id: profile_id.into(),
                    extra_image_name: extra_image_name.into(),
                    rootfs_size: rootfs_size.into(),
                    packages: packages.into(),
                    disabled_services: disabled_services.into(),
                    overlay_path: overlay_path.into(),
                },
            );
        }
    ));

    state_bridge.on_open_build_folder_requested(clone!((ui_weak, core), move || {
        on_open_build_folder(&ui_weak, &core);
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

    state_bridge.on_profile_search_edited(clone!((ui_weak, core), move |search| {
        on_profile_search(&ui_weak, &core, &search);
    }));

    state_bridge.on_profile_search_key_pressed(clone!((ui_weak), move |event| {
        on_profile_search_key_pressed(&ui_weak, &event.text)
    }));

    state_bridge.on_profile_selected(clone!(
        (ui_weak, core, cache, get_image_builder),
        move |name| {
            on_profile_selected(&ui_weak, &core, &cache, &get_image_builder, &name);
        }
    ));

    state_bridge.on_show_rcs_toggled(clone!((ui_weak, core), move |show_rcs| {
        on_show_rcs_toggled(&ui_weak, &core, show_rcs);
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
) {
    let Some(ui) = ui_weak.upgrade() else {
        eprintln!("Error: UI handle lost during build request.");
        return;
    };
    let s = ui.get_state();

    let version = s.selected_version.to_string();
    let target = s.selected_target.to_string();
    let profile_id = s.selected_id.to_string();

    let packages = {
        let current_set: HashSet<_> = s.packages_text.split_whitespace().collect();
        let core = core.read().unwrap();
        let selected = s.packages_text.to_string();
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

    let extra_image_name = s.extra_image_name_text.to_string();
    let rootfs_size = s.rootfs_size_value.to_string();
    let disabled_services = s.disabled_service_text.to_string();
    let overlay_path = s.overlay_path_text.to_string();

    let extra_image_name: String = extra_image_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();

    if version.is_empty() || target.is_empty() || profile_id.is_empty() {
        eprintln!("Cannot build: Version, Target, or Profile ID is missing.");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.switch_state_to(UIState::Idle(None));
        });
        return;
    }
    tokio::spawn(clone!((ui_weak, core, get_image_builder), async move {
        println!("Initializing...");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.switch_state_to(UIState::Building {
                progress: None,
                status: Some("Initializing".into()),
            });
        });

        let build_path = core.read().unwrap().config.build_path.clone();
        let build_folder_path = build_path.join(&target);

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
                &rootfs_size,
                &disabled_services,
                &overlay_path,
            )
            .await;

        let mut stream = match stream {
            Ok(s) => s,
            Err(e) => {
                let msg = "Failed to build firmware";
                eprintln!("{msg}: {e}");
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.update_state(move |s| {
                        s.set_notification(Notification::Error, Some(msg));
                        s.switch_to(UIState::Error("Build failed".into()));
                    });
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
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.update_state(|s| {
                            s.set_notification(Notification::Error, Some(&msg));
                        });
                    });
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
                let _ = ui_weak.upgrade_in_event_loop(clone!((current_status), move |ui| {
                    ui.switch_state_to(UIState::Building {
                        progress: Some(current_progress),
                        status: if current_status.is_empty() {
                            None
                        } else {
                            Some(current_status)
                        },
                    });
                }));
                needs_update = false;
            }
        }

        drop(stream);

        if success {
            println!("Build completed");
            AsyncWriteExt::write_all(&mut log_file, current_status.as_bytes())
                .await
                .unwrap();

            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.switch_state_to(UIState::Idle(Some("Build completed".into())));
            });
        } else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.switch_state_to(UIState::Error("Build failed".into()));
            });
        }
    }));
}

#[allow(clippy::cast_precision_loss)]
fn on_download(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let (version, target, profile_id) = {
        let Some(ui) = ui_weak.upgrade() else {
            eprintln!("Error: UI handle lost during download request.");
            return;
        };
        let s = ui.get_state();
        (
            s.selected_version.to_string(),
            s.selected_target.to_string(),
            s.selected_id.to_string(),
        )
    };

    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.update_state(|s| {
            s.image_exists = false;
            s.set_notification(Notification::Info, None);
            s.switch_to(UIState::DownloadingBuilder {
                status: None,
                progress: Some(0.0),
            });
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

                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.switch_state_to(UIState::DownloadingBuilder {
                                progress: Some(current_progress),
                                status: Some(status_text),
                            });
                        });
                    }
                    Err(e) => {
                        let msg = format!("Pull error: {e}");
                        eprintln!("{msg}");
                        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                            ui.update_state(|s| {
                                s.set_notification(Notification::Error, Some(&msg));
                            });
                        });
                        break;
                    }
                }
            }

            println!();
            drop(stream);

            let exists = set_image_exists(&ui_weak, &get_image_builder, &version, &target).await;
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
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.switch_state_to(UIState::Idle(None));
                });
            }
        }
    ));
}

fn on_load_preset(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let version = {
        let Some(ui) = ui_weak.upgrade() else { return };
        let s = ui.get_state();
        s.selected_version.to_string()
    };

    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.switch_state_to(UIState::Idle(None));
    });

    tokio::spawn(clone!(
        (ui_weak, core, cache, get_image_builder),
        async move {
            let picker = rfd::AsyncFileDialog::new()
                .add_filter("JSON files", &["json"])
                .set_title("Load Preset")
                .pick_file();

            let Some(path) = picker.await else {
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.switch_state_to(UIState::Idle(None));
                });
                return;
            };

            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.switch_state_to(UIState::LoadingPreset);
            });

            let path = PathBuf::from(path);
            if let Err(e) =
                load_preset_from_path(&ui_weak, &core, &path, cache, &get_image_builder, &version)
                    .await
            {
                eprintln!("Error loading preset: {e}");
                let msg = format!("{e}");
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.update_state(|s| {
                        s.set_notification(Notification::Error, Some(&msg));
                        s.switch_to(UIState::Idle(None));
                    });
                });
            }
        }
    ));
}

fn on_save_preset(ui_weak: &slint::Weak<AppWindow>, mut preset: Preset) {
    let Some(ui) = ui_weak.upgrade() else {
        return;
    };
    let version = ui.get_state().selected_version.to_string();
    preset.release_series = get_release_series(&version);

    let filename = if preset.extra_image_name.is_empty() {
        format!("preset-{}.json", preset.profile_id)
    } else {
        format!(
            "preset-{}-{}.json",
            preset.profile_id, preset.extra_image_name
        )
    };

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.switch_state_to(UIState::SavingPreset);
    });

    tokio::spawn(clone!((ui_weak), async move {
        let picker = rfd::AsyncFileDialog::new()
            .add_filter("JSON files", &["json"])
            .set_title("Save Preset")
            .set_file_name(filename)
            .save_file();

        let Some(path) = picker.await else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.switch_state_to(UIState::Idle(None));
            });
            return;
        };

        let path = PathBuf::from(path);
        let text = serde_json::to_string_pretty(&preset).unwrap();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("Error saving preset {}: {e}", path.display());
        }

        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.switch_state_to(UIState::Idle(None));
        });
    }));
}

fn on_open_build_folder(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore) {
    let Some(ui) = ui_weak.upgrade() else {
        return;
    };
    let s = ui.get_state();
    let version = s.selected_version.to_string();
    let target = s.selected_target.to_string();
    if version.is_empty() || target.is_empty() {
        eprintln!("Cannot open folder: Version or Target is missing.");
        return;
    }
    let build_folder_path = core.read().unwrap().config.build_path.join(target);
    open_dir(&build_folder_path);
}

fn on_packages_edited(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    debounce_task: &Arc<RwLock<Option<JoinHandle<()>>>>,
    text: &SharedString,
) {
    let _ = ui_weak.upgrade_in_event_loop(clone!((text), move |ui| {
        ui.update_state(|s| {
            s.packages_text = text;
        });
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
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.update_state(|s| {
                s.removed_packages_text = removed_str.into();
            });
        });
    })));
}

fn on_profile_search_key_pressed(
    ui_weak: &slint::Weak<AppWindow>,
    event_text: &SharedString,
) -> bool {
    let Some(ui) = ui_weak.upgrade() else {
        return false;
    };
    let mut s = ui.get_state();
    let profiles = s.profiles.clone();
    let count = profiles.row_count();
    if count == 0 {
        return false;
    }

    let mut current = s.current_highlighted_profile_index;
    let count: i32 = count.try_into().unwrap();

    if *event_text == <Key as Into<SharedString>>::into(Key::DownArrow) {
        current = (current + 1) % count;
        s.current_highlighted_profile_index = current;
        ui.set_state(s);
        true
    } else if *event_text == <Key as Into<SharedString>>::into(Key::UpArrow) {
        current = if current <= 0 { count - 1 } else { current - 1 };
        s.current_highlighted_profile_index = current;
        ui.set_state(s);
        true
    } else if *event_text == <Key as Into<SharedString>>::into(Key::Return) {
        if let Some(val) = (current >= 0 && current < count)
            .then(|| profiles.row_data(current.try_into().unwrap()))
            .flatten()
        {
            ui.global::<StateBridge>().invoke_profile_selected(val);
            return true;
        }
        false
    } else {
        false
    }
}

fn on_profile_search(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore, search: &SharedString) {
    let Some(ui) = ui_weak.upgrade() else { return };
    let mut s = ui.get_state();
    s.reset_profile();
    s.current_highlighted_profile_index = -1;

    let core = core.read().unwrap();
    let filtered = if search.chars().count() >= MIN_SEARCH_CHARS {
        filter_profiles(&core.profiles, search)
            .iter()
            .map(SharedString::from)
            .collect::<Vec<_>>()
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
    name: &SharedString,
) {
    let Some(ui) = ui_weak.upgrade() else {
        return;
    };

    let profile = {
        let core = core.read().unwrap();
        find_profile_by_display_name(&core.profiles, name)
    };

    let Some(profile) = profile else {
        return;
    };

    let mut s = ui.get_state();
    s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
    s.search_text = name.clone();
    s.selected_id = profile.id.as_str().into();
    s.selected_target = profile.target.as_str().into();
    s.selected_model = get_all_models_string(&profile).as_str().into();

    s.switch_to(UIState::FetchingPackages);

    let profile_id = profile.id.clone();
    let target = profile.target.clone();
    let version = s.selected_version.to_string();
    ui.set_state(s);

    tokio::spawn(clone!(
        (ui_weak, core, cache, get_image_builder),
        async move {
            let exists = set_image_exists(&ui_weak, &get_image_builder, &version, &target).await;
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

fn on_show_rcs_toggled(ui_weak: &slint::Weak<AppWindow>, core: &SharedCore, show_rcs: bool) {
    let core = core.read().unwrap();
    let Some(ui) = ui_weak.upgrade() else { return };
    let filtered = filter_versions(&core.versions, show_rcs);

    ui.update_state(|s| {
        s.versions = Rc::new(VecModel::from(filtered)).into();

        if !show_rcs && s.selected_version.contains("-rc") {
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
    let version = version.to_string();

    let _ = ui_weak.upgrade_in_event_loop(clone!((version), move |ui| {
        ui.switch_state_to(UIState::LoadingProfiles(version));
    }));

    tokio::spawn(clone!((ui_weak, client, core), async move {
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
            let msg = format!("Failed to fetch profiles for version {version}");
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.update_state(|s| {
                    s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
                    s.profiles_fetch_failed = true;
                    s.set_notification(Notification::Error, Some(&msg));
                    s.switch_to(UIState::Idle(None));
                });
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
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.update_state(|s| {
                    s.switch_to(UIState::Idle(Some("No container engines found".into())));
                    s.set_notification(
                        Notification::Error,
                        Some(
                            "Podman or Docker not found. Please ensure either of them is running.",
                        ),
                    );
                });
            });
            return;
        }

        let versions_res = client.fetch_versions().await;
        if let Err(e) = versions_res {
            let msg = format!("Initial load error: {e}");
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.update_state(|s| {
                    s.set_notification(Notification::Error, Some(&msg));
                });
            });
            return;
        }

        let versions = versions_res.unwrap();
        if let Ok(mut c) = core.write() {
            c.versions.clone_from(&versions);
        }

        let filtered = filter_versions(&versions, show_rcs);
        let Some(first_version) = filtered.first().map(ToString::to_string) else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.update_state(|s| {
                    s.set_notification(Notification::Error, Some("No OpenWrt versions found."));
                });
            });
            return; // No versions, nothing to load
        };

        let _ = ui_weak.upgrade_in_event_loop(clone!((filtered, first_version), move |ui| {
            ui.update_state(|s| {
                s.versions = Rc::new(VecModel::from(filtered)).into();
                s.switch_to(UIState::LoadingProfiles(first_version));
            });
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
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.update_state(|s| {
                        s.profiles_fetch_failed = true;
                        s.set_notification(Notification::Error, Some(&msg));
                        s.switch_to(UIState::Idle(None));
                    });
                });
                return;
            }
        }

        // Always set busy to false and clear profiles at the end of the initial
        // load task
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.update_state(|s| {
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
    version: &str,
    target: &str,
    profile_id: &str,
) {
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.switch_state_to(UIState::FetchingPackages);
    });

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
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.update_state(|s| {
                    s.set_notification(Notification::Error, Some(&msg));
                });
            });
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
    version: &str,
    target: &str,
    profile_id: &str,
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

fn filter_versions(versions: &[String], show_rcs: bool) -> Vec<SharedString> {
    versions
        .iter()
        .filter(|v| {
            (show_rcs || !v.contains("-rc"))
                && (v
                    .split('.')
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .is_some_and(|m| m >= MIN_SERIES))
        })
        .map(SharedString::from)
        .collect()
}

fn get_build_status(line: &str) -> Option<(f32, String)> {
    BUILD_MILESTONES
        .iter()
        .find(|&&(prefix, _, _)| line.starts_with(prefix))
        .map(|&(_, progress, status)| (progress, status.to_owned()))
}

fn get_release_series(version: &str) -> String {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() >= 2 {
        format!("{}.{}", parts[0], parts[1])
    } else {
        version.to_string()
    }
}

async fn load_preset_from_path(
    ui_weak: &slint::Weak<AppWindow>,
    core: &SharedCore,
    path: &Path,
    cache: MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    version: &str,
) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(&path).await?;
    let preset: Preset = serde_json::from_str(&content)?;
    let target_id = preset.target.clone();
    let profile_id = preset.profile_id.clone();

    let found = {
        let core = core.read().unwrap();
        core.profiles
            .iter()
            .find(|p| p.id == profile_id && p.target == target_id)
            .map(|p| {
                let name = format_profile(p)
                    .first()
                    .cloned()
                    .unwrap_or_else(|| p.id.clone());
                (name, get_all_models_string(p), p.target.clone())
            })
    };

    let (name, model, target) = found.ok_or_else(|| {
        anyhow::anyhow!(
            "Profile '{profile_id}' for target '{target_id}' not found in current version"
        )
    })?;

    ui_weak.upgrade_in_event_loop({
        let overlay_path = preset.overlay_path.clone();
        clone!((profile_id, name, model, target), move |ui| {
            ui.update_state(|s| {
                s.overlay_path_text = overlay_path.into();
                s.packages_text = SharedString::new();
                s.removed_packages_text = SharedString::new();
                s.search_text = name.into();
                s.selected_id = profile_id.into();
                s.selected_model = model.into();
                s.selected_target = target.into();
            });
        })
    })?;

    let current_series = get_release_series(version);
    let preset_series = preset.release_series.clone();

    let exists = set_image_exists(ui_weak, get_image_builder, version, &target).await;
    if !exists {
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.switch_state_to(UIState::Idle(None));
        });
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

    ui_weak.upgrade_in_event_loop(clone!((original_pkgs, core), move |ui| {
        let mut s = ui.get_state();
        s.disabled_service_text = preset.disabled_services.into();
        s.extra_image_name_text = preset.extra_image_name.into();
        s.overlay_path_text = preset.overlay_path.into();
        s.packages_text = preset.packages.into();
        s.profiles = Rc::new(VecModel::<SharedString>::default()).into();
        s.removed_packages_text = removed_str.into();
        s.rootfs_size_value = preset.rootfs_size.into();
        s.switch_to(UIState::Idle(None));

        if !preset_series.is_empty() && preset_series != current_series {
            let info_msg = format!("Package list from preset series {preset_series} might be incompatible with {current_series}.");
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

fn open_dir(path: &Path) {
    #[cfg(target_os = "linux")]
    {
        eprintln!("Attempting to open folder: {}", path.display());
        let _ = Command::new("xdg-open")
            .arg(path.as_os_str())
            .spawn()
            .map_err(|e| eprintln!("Failed to open folder on Linux: {e}"));
    }
    #[cfg(target_os = "macos")]
    {
        eprintln!("Attempting to open folder: {}", path.display());
        let _ = Command::new("open")
            .arg(path.as_os_str())
            .spawn()
            .map_err(|e| eprintln!("Failed to open folder on macOS: {e}"));
    }
    #[cfg(target_os = "windows")]
    {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let path_str = path.to_string_lossy();
        let path = path_str
            .strip_prefix(r"\\?\")
            .unwrap_or(&path_str)
            .replace('/', "\\");

        eprintln!("Attempting to open folder: {path}");
        let _ = Command::new("explorer")
            .arg(path)
            .spawn()
            .map_err(|e| eprintln!("Failed to open folder: {e}"));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        eprintln!("Opening file explorer is not supported on this OS.");
    }
}

async fn set_image_exists(
    ui_weak: &slint::Weak<AppWindow>,
    get_image_builder: &GetImageBuilderFn,
    version: &str,
    target: &str,
) -> bool {
    println!("Checking if image exists: {version} {target}");

    let image_builder = get_image_builder(version, target);
    let exists = image_builder.exists().await;

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.update_state(|s| {
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
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.switch_state_to(initial_state);
    });

    let picker = rfd::AsyncFileDialog::new()
        .set_title(dialog_title)
        .pick_folder();

    let Some(path_handle) = picker.await else {
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.switch_state_to(UIState::Idle(None));
        });
        return;
    };

    let path = path_handle.path().to_path_buf();
    println!("Selected folder: {}", path.display());
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.update_state(|s| {
            update_state(s, &path);
            s.switch_to(UIState::Idle(None));
        });
    });
}
