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
mod config;
mod containers;
mod data;
mod profiles;

use builder::ImageBuilder;
use cache::MetadataCache;
use client::OpenWrtClient;
use config::Config;
use containers::Containers;
use data::{Preset, Profile};
use profiles::{
    filter_profiles, find_profile_by_display_name, format_profile, get_all_models_string,
};

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

#[derive(Clone, Copy, Eq, PartialEq)]
enum Notification {
    Info,
    Warning,
    Error,
}

impl Notification {
    fn log(self, text: &str) {
        match self {
            Self::Error => eprintln!("Error: {text}"),
            Self::Warning => eprintln!("Warning: {text}"),
            Self::Info => println!("{text}"),
        }
    }
}

#[derive(Default)]
struct AppInternalState {
    config: Config,
    packages: Vec<String>,
    profiles: Vec<Profile>,
    selected_profile_id: String,
    selected_target: String,
    selected_version: String,
    versions: Vec<String>,
}

type AppState = Arc<RwLock<AppInternalState>>;

trait AppStateExt {
    fn reset(&self);
}

impl AppStateExt for AppState {
    fn reset(&self) {
        if let Ok(mut s) = self.write() {
            s.packages.clear();
            s.selected_profile_id.clear();
            s.selected_target.clear();
        }
    }
}

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

    let state = Arc::new(RwLock::new(AppInternalState {
        config: config.clone(),
        ..Default::default()
    }));

    let get_image_builder: GetImageBuilderFn = {
        let state = state.clone();
        Arc::new(move |version, target| {
            let containers = Containers::new().unwrap();
            let build_path = &state.read().unwrap().config.build_path;
            ImageBuilder::new(containers.clone(), IMAGE_NAME, build_path, version, target)
        })
    };

    let cache = MetadataCache::new(&config.cache_path);
    let client = OpenWrtClient::new(BASE_URL, cache.clone());

    setup_callbacks(&ui, &state, &client, &cache, &get_image_builder);

    init(&ui, &state, client, async || match Containers::new() {
        Ok(containers) => containers.is_available().await,
        Err(_) => false,
    });

    ui.set_build_path_text(config.build_path.to_string_lossy().to_string().into());
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    ui.set_busy(true);
    ui.run()?;

    state
        .read()
        .unwrap()
        .config
        .save()
        .context("Failed to save configuration")?;

    Ok(())
}

fn setup_callbacks(
    ui: &AppWindow,
    state: &AppState,
    client: &OpenWrtClient,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let ui_weak = ui.as_weak();

    ui.on_build_path_edited({
        let state = state.clone();
        move |path| {
            if let Ok(mut s) = state.write() {
                s.config.build_path = path.as_str().into();
            }
        }
    });

    ui.on_build_requested({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let get_image_builder = get_image_builder.clone();
        move |packages| on_build(&ui_weak, &state, &get_image_builder, &packages)
    });

    ui.on_download_requested({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let cache = cache.clone();
        let get_image_builder = get_image_builder.clone();
        move || on_download(&ui_weak, &state, &cache, &get_image_builder)
    });

    ui.on_load_preset_requested({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let cache = cache.clone();
        let get_image_builder = get_image_builder.clone();
        move || on_load_preset(&ui_weak, &state, &cache, &get_image_builder)
    });

    ui.on_save_preset_requested({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move |target,
              profile_id,
              extra_image_name,
              rootfs_size,
              packages,
              disabled_services,
              overlay_path| {
            on_save_preset(
                &ui_weak,
                &state,
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
    });

    ui.on_open_build_folder_requested({
        let state = state.clone();
        move || on_open_build_folder(&state)
    });

    ui.on_select_build_folder_requested({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move || on_select_build_folder(&ui_weak, &state)
    });

    ui.on_select_overlay_folder_requested({
        let ui_weak = ui_weak.clone();
        move || on_select_overlay_folder(&ui_weak)
    });

    ui.on_packages_edited({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        // Store the handle to the pending debounce task so we can cancel it if
        // the user types again
        let debounce_task = Arc::new(RwLock::new(None::<JoinHandle<()>>));
        move |text| on_packages_edited(&ui_weak, &state, &debounce_task, &text)
    });

    ui.on_profile_search_edited({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move |search| on_profile_search(&ui_weak, &state, &search)
    });

    ui.on_profile_search_key_pressed({
        let ui_weak = ui_weak.clone();
        move |event| on_profile_search_key_pressed(&ui_weak, &event.text)
    });

    ui.on_profile_selected({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let cache = cache.clone();
        let get_image_builder = get_image_builder.clone();
        move |name| on_profile_selected(&ui_weak, &state, &cache, &get_image_builder, &name)
    });

    ui.on_show_rcs_toggled({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        move |show_rcs| on_show_rcs_toggled(&ui_weak, &state, show_rcs)
    });

    ui.on_version_changed({
        let ui_weak = ui_weak.clone();
        let state = state.clone();
        let client = client.clone();
        move |version| on_version_changed(&ui_weak, &state, &client, &version)
    });

    ui.on_open_github_link(|| {
        if let Err(e) = webbrowser::open(ABOUT_URL) {
            eprintln!("Failed to open GitHub link: {e}");
        }
    });
}

fn on_build(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    get_image_builder: &GetImageBuilderFn,
    packages: &SharedString,
) {
    let s = state.read().unwrap();
    let version = s.selected_version.clone();
    let target = s.selected_target.clone();
    let profile_id = s.selected_profile_id.clone();
    let user_pkgs: Vec<String> = packages
        .split_whitespace()
        .map(ToString::to_string)
        .collect();
    let packages = prepare_package_list(&user_pkgs, &s.packages);
    let packages = packages.join(" ");
    drop(s); // release lock early

    let (extra_image_name, rootfs_size, disabled_services, overlay_path) = {
        let ui = ui_weak.upgrade().unwrap();
        (
            ui.get_extra_image_name_text(),
            ui.get_rootfs_size_value(),
            ui.get_disabled_service_text(),
            ui.get_overlay_path_text(),
        )
    };

    let extra_image_name: String = extra_image_name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-')
        .collect();

    if version.is_empty() || target.is_empty() || profile_id.is_empty() {
        eprintln!("Cannot build: Version, Target, or Profile ID is missing.");
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_busy(false);
        });
        return;
    }
    let ui_weak = ui_weak.clone();
    let state = state.clone();
    let get_image_builder = get_image_builder.clone();
    tokio::spawn(async move {
        println!("Initializing...");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(true);
            ui.set_build_status(SharedString::from("Initializing"));
            ui.set_progress_visible(true);
            ui.set_progress_value(0.0);
        });

        let build_path = { state.read().unwrap().config.build_path.clone() };
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
                show_error(&ui_weak, "Failed to build firmware", Some(e));
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.set_build_status(SharedString::from("Build failed"));
                    ui.set_progress_visible(false);
                    ui.set_progress_value(0.0);
                    ui.set_busy(false);
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
                    show_error(&ui_weak, "Error receiving build logs", Some(e.into()));
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
                let current_status = current_status.clone();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    if !current_status.is_empty() {
                        ui.set_build_status(SharedString::from(current_status));
                    }
                    ui.set_progress_value(current_progress);
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

            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_build_status(SharedString::from("Build completed"));
                ui.set_progress_value(1.0);
                ui.set_progress_visible(false);
                ui.set_busy(false);
            });
        } else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_build_status(SharedString::from("Build failed"));
                ui.set_progress_visible(false);
                ui.set_progress_value(0.0);
                ui.set_busy(false);
            });
        }
    });
}

#[allow(clippy::cast_precision_loss)]
fn on_download(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_busy(true);
        ui.set_build_status(SharedString::from("Downloading image builder"));
        ui.set_image_exists(false);
        ui.set_progress_visible(true);
        ui.set_progress_value(0.0);

        set_notification(Notification::Info, &ui, None);
    });

    let ui_weak = ui_weak.clone();
    let state = state.clone();
    let cache = cache.clone();
    let get_image_builder = get_image_builder.clone();
    let (version, target) = {
        let s = state.read().unwrap();
        (s.selected_version.clone(), s.selected_target.clone())
    };
    tokio::spawn(async move {
        let image_builder = get_image_builder(&version, &target);
        let mut stream = image_builder.download();
        let mut current_progress = 0.0f32;
        let mut layers = HashMap::<String, (i64, i64)>::new();

        while let Some(pull_result) = stream.next().await {
            match pull_result {
                Ok(info) => {
                    if let (Some(id), Some(pd)) = (info.id.as_ref(), info.progress_detail.as_ref())
                        && let (Some(current), Some(total)) = (pd.current, pd.total)
                        && total > 0
                    {
                        layers.insert(id.clone(), (current, total));
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
                        format!("Downloading image builder: {current_mib:.2} / {total_mib:.2} MiB")
                    } else {
                        "Downloading image builder".to_string()
                    };

                    if !status_text.is_empty() {
                        use std::io::{self, Write};
                        print!("\r\x1b[K{status_text}");
                        let _ = io::stdout().flush();
                    }

                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.set_build_status(status_text.into());
                        ui.set_progress_value(current_progress);
                    });
                }
                Err(e) => {
                    let msg = format!("Pull error: {e}");
                    eprintln!("{msg}");
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        set_notification(Notification::Error, &ui, Some(&msg));
                    });
                    break;
                }
            }
        }

        println!();
        drop(stream);

        let exists = set_image_exists(&ui_weak, &get_image_builder, &version, &target).await;
        if exists {
            fetch_and_update_packages(&ui_weak, state, cache, &get_image_builder, true).await;
            ui_weak
                .upgrade_in_event_loop(move |ui| {
                    ui.set_build_status(SharedString::new());
                    ui.set_progress_visible(false);
                    ui.set_progress_value(0.0);
                    ui.set_busy(false);
                })
                .unwrap();
        } else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_build_status(SharedString::new());
                ui.set_progress_visible(false);
                ui.set_progress_value(0.0);
                ui.set_busy(false);

                let ui_weak = ui.as_weak();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.invoke_request_profile_search_focus();
                });
            });
        }
    });
}

fn on_load_preset(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) {
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_busy(true);
    });

    let ui_weak = ui_weak.clone();
    let state = state.clone();
    let cache = cache.clone();
    let get_image_builder = get_image_builder.clone();
    tokio::spawn(async move {
        let picker = rfd::AsyncFileDialog::new()
            .add_filter("JSON files", &["json"])
            .set_title("Load Preset")
            .pick_file();

        let Some(path) = picker.await else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
            });
            return;
        };

        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_build_status(SharedString::from("Loading preset"));
            ui.set_progress_visible(true);
        });

        let path = PathBuf::from(path);
        if let Err(e) =
            load_preset_from_path(&ui_weak, &state, &path, cache, &get_image_builder).await
        {
            eprintln!("Error loading preset: {e}");
            let msg = format!("{e}");
            show_error(&ui_weak, &msg, None);
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_progress_visible(false);
                ui.set_busy(false);
            });
        }
    });
}

fn on_save_preset(ui_weak: &slint::Weak<AppWindow>, state: &AppState, mut preset: Preset) {
    let version = state.read().unwrap().selected_version.clone();
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
        ui.set_busy(true);
    });

    let ui_weak = ui_weak.clone();
    tokio::spawn(async move {
        let picker = rfd::AsyncFileDialog::new()
            .add_filter("JSON files", &["json"])
            .set_title("Save Preset")
            .set_file_name(filename)
            .save_file();

        let Some(path) = picker.await else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
            });
            return;
        };

        let path = PathBuf::from(path);
        let text = serde_json::to_string_pretty(&preset).unwrap();
        if let Err(e) = std::fs::write(&path, text) {
            eprintln!("Error saving preset {}: {e}", path.display());
        }

        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_busy(false);
        });
    });
}

fn on_open_build_folder(state: &AppState) {
    let s = state.read().unwrap();
    let version = &s.selected_version;
    let target = &s.selected_target;
    if version.is_empty() || target.is_empty() {
        eprintln!("Cannot open folder: Version or Target is missing.");
        return;
    }
    let build_folder_path = s.config.build_path.join(target);
    open_dir(&build_folder_path);
}

fn on_select_overlay_folder(ui_weak: &slint::Weak<AppWindow>) {
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_busy(true);
        ui.set_build_status(SharedString::from("Select overlay folder"));
    });

    let ui_weak = ui_weak.clone();
    tokio::spawn(async move {
        let picker = rfd::AsyncFileDialog::new()
            .set_title("Select Overlay Folder")
            .pick_folder();

        let Some(path) = picker.await else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
            });
            return;
        };

        let path_str = path.path().to_string_lossy().to_string();
        println!("Selected overlay folder: {path_str}");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_overlay_path_text(path_str.into());
            ui.set_build_status(SharedString::new());
            ui.set_busy(false);
        });
    });
}

fn on_select_build_folder(ui_weak: &slint::Weak<AppWindow>, state: &AppState) {
    let _ = ui_weak.upgrade_in_event_loop(|ui| {
        ui.set_busy(true);
        ui.set_build_status(SharedString::from("Select build folder"));
    });

    let ui_weak = ui_weak.clone();
    let state = state.clone();
    tokio::spawn(async move {
        let picker = rfd::AsyncFileDialog::new()
            .set_title("Select Build Folder")
            .pick_folder();

        let Some(path) = picker.await else {
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.set_busy(false);
            });
            return;
        };

        let path = path.path().to_path_buf();
        let path_str = path.to_string_lossy().to_string();
        if let Ok(mut s) = state.write() {
            s.config.build_path = path;
        }

        println!("Selected build folder: {path_str}");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_build_path_text(path_str.into());
            ui.set_build_status(SharedString::new());
            ui.set_busy(false);
        });
    });
}

fn on_packages_edited(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    debounce_task: &Arc<RwLock<Option<JoinHandle<()>>>>,
    text: &SharedString,
) {
    {
        let text = text.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_packages_text(text);
        });
    }

    let mut handle_lock = debounce_task.write().unwrap();
    if let Some(h) = handle_lock.take() {
        h.abort();
    }

    let ui_weak = ui_weak.clone();
    let state = state.clone();
    let text = text.clone();
    *handle_lock = Some(tokio::spawn(async move {
        // Wait for 500ms of inactivity
        tokio::time::sleep(Duration::from_millis(500)).await;

        let original = state.read().unwrap().packages.clone();
        let current_set: HashSet<_> = text.split_whitespace().collect();

        // Identify which original packages are no longer in the user's list
        let removed: Vec<String> = original
            .into_iter()
            .filter(|p| !current_set.contains(p.as_str()))
            .collect();

        let removed_str = removed.join(" ");
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_removed_packages_text(removed_str.into());
        });
    }));
}

fn on_profile_search_key_pressed(
    ui_weak: &slint::Weak<AppWindow>,
    event_text: &SharedString,
) -> bool {
    let Some(ui) = ui_weak.upgrade() else {
        return false;
    };
    let model = ui.get_profiles();
    let count = model.row_count();
    if count == 0 {
        return false;
    }

    let mut current = ui.get_current_highlighted_profile_index();
    let count: i32 = count.try_into().unwrap();

    if *event_text == <Key as Into<SharedString>>::into(Key::DownArrow) {
        current = (current + 1) % count;
        ui.set_current_highlighted_profile_index(current);
        true
    } else if *event_text == <Key as Into<SharedString>>::into(Key::UpArrow) {
        current = if current <= 0 { count - 1 } else { current - 1 };
        ui.set_current_highlighted_profile_index(current);
        true
    } else if *event_text == <Key as Into<SharedString>>::into(Key::Return) {
        if let Some(val) = (current >= 0 && current < count)
            .then(|| model.row_data(current.try_into().unwrap()))
            .flatten()
        {
            ui.invoke_profile_selected(val);
            return true;
        }
        false
    } else {
        false
    }
}

fn on_profile_search(ui_weak: &slint::Weak<AppWindow>, state: &AppState, search: &SharedString) {
    let Some(ui) = ui_weak.upgrade() else { return };
    reset_profile_info(&ui, state);
    ui.set_current_highlighted_profile_index(-1);
    let s = state.read().unwrap();
    let filtered = if search.chars().count() >= MIN_SEARCH_CHARS {
        filter_profiles(&s.profiles, search)
            .iter()
            .map(SharedString::from)
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    ui.set_profiles(Rc::new(VecModel::from(filtered)).into());
}

fn on_profile_selected(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    cache: &MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    name: &SharedString,
) {
    let Some(ui) = ui_weak.upgrade() else {
        return;
    };

    let (profile, version) = {
        let s = state.read().unwrap();
        let p = find_profile_by_display_name(&s.profiles, name);
        (p, s.selected_version.clone())
    };

    let Some(profile) = profile else {
        return;
    };

    ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into());
    ui.set_search_text(name.clone());
    if let Ok(mut s) = state.write() {
        s.selected_profile_id.clone_from(&profile.id);
        s.selected_target.clone_from(&profile.target);
    }

    ui.set_selected_model(get_all_models_string(&profile).as_str().into());
    ui.set_selected_id(profile.id.as_str().into());
    ui.set_selected_target(profile.target.as_str().into());

    let ui_weak = ui_weak.clone();
    let state = state.clone();
    let get_image_builder = get_image_builder.clone();
    let cache = cache.clone();
    tokio::spawn(async move {
        let exists =
            set_image_exists(&ui_weak, &get_image_builder, &version, &profile.target).await;
        if exists {
            fetch_and_update_packages(&ui_weak, state, cache, &get_image_builder, false).await;
        } else {
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                ui.set_busy(false);

                let ui_weak = ui.as_weak();
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.invoke_request_profile_search_focus();
                });
            });
        }
    });
}

fn on_show_rcs_toggled(ui_weak: &slint::Weak<AppWindow>, state: &AppState, show_rcs: bool) {
    let Ok(s) = state.read() else {
        return;
    };
    let Some(ui) = ui_weak.upgrade() else { return };
    let filtered = filter_versions(&s.versions, show_rcs);
    ui.set_versions(Rc::new(VecModel::from(filtered)).into());

    if !show_rcs && s.selected_version.contains("-rc") {
        drop(s);
        ui.set_search_text("".into());
        ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into());
        reset_profile_info(&ui, state);
    }
}

fn on_version_changed(
    ui_weak: &slint::Weak<AppWindow>,
    state: &AppState,
    client: &OpenWrtClient,
    version: &SharedString,
) {
    let version = version.to_string();
    if let Ok(mut s) = state.write() {
        s.selected_version.clone_from(&version);
        s.selected_profile_id = String::new();
    }

    let _ = ui_weak.upgrade_in_event_loop({
        let state = state.clone();
        move |ui| {
            ui.set_busy(true);
            ui.set_search_text("".into());
            reset_profile_info(&ui, &state);
        }
    });

    let client = client.clone();
    let state = state.clone();
    let ui_weak = ui_weak.clone();
    tokio::spawn(async move {
        match client.fetch_profiles(&version).await {
            Ok(profiles) => {
                if let Ok(mut s) = state.write() {
                    s.profiles = profiles;
                }
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into());
                    ui.set_busy(false);

                    let ui_weak = ui.as_weak();
                    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                        ui.invoke_request_profile_search_focus();
                    });
                });
            }
            Err(e) => {
                let msg = format!("Failed to fetch profiles for version {version}");
                show_error(&ui_weak, &msg, Some(e));
                let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                    ui.set_profiles_fetch_failed(true);
                    ui.set_busy(false);
                    ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into()); // Clear profiles even on error
                });
            }
        }
    });
}

fn init<F, Fut>(ui: &AppWindow, state: &AppState, client: OpenWrtClient, is_containers_available: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = bool> + Send + 'static,
{
    let ui_weak = ui.as_weak();
    let state = state.clone();
    let show_rcs = ui.get_show_rcs();
    tokio::spawn(async move {
        if !is_containers_available().await {
            show_error(
                &ui_weak,
                "Podman or Docker not found. Please ensure either of them is running.",
                None,
            );
            return;
        }

        let versions_res = client.fetch_versions().await;
        if let Err(e) = versions_res {
            show_error(&ui_weak, "Initial load error", Some(e));
            return;
        }

        let versions = versions_res.unwrap();
        if let Ok(mut s) = state.write() {
            s.versions.clone_from(&versions);
        }

        let filtered = filter_versions(&versions, show_rcs);
        let Some(first_version) = filtered.first().map(ToString::to_string) else {
            show_error(&ui_weak, "No OpenWrt versions found.", None);
            return; // No versions, nothing to load
        };

        if let Ok(mut s) = state.write() {
            s.selected_version.clone_from(&first_version);
        }

        let filtered = filtered.clone();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.set_versions(Rc::new(VecModel::from(filtered)).into());
            ui.set_busy(true); // Still busy loading profiles
        });

        let profiles_res = client.fetch_profiles(&first_version).await;
        match profiles_res {
            Ok(profiles) => {
                if let Ok(mut s) = state.write() {
                    s.profiles = profiles;
                }
            }
            Err(e) => {
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    ui.set_profiles_fetch_failed(true);
                    ui.set_busy(false);
                });
                show_error(&ui_weak, "Initial load error", Some(e));
                return;
            }
        }

        // Always set busy to false and clear profiles at the end of the initial
        // load task
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into());
            ui.set_busy(false);

            let ui_weak = ui.as_weak();
            let _ = ui_weak.upgrade_in_event_loop(move |ui| {
                ui.invoke_request_profile_search_focus();
            });
        });
    });
}

async fn fetch_and_update_packages(
    ui_weak: &slint::Weak<AppWindow>,
    state: AppState,
    cache: MetadataCache,
    get_image_builder: &GetImageBuilderFn,
    already_busy: bool,
) {
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        if !already_busy {
            ui.set_busy(true);
            ui.set_progress_visible(true);
        }

        ui.set_progress_value(0.0);
        ui.set_build_status(SharedString::from("Fetching package list"));
    });

    let (version, target, profile_id) = {
        let s = state.read().unwrap();
        (
            s.selected_version.clone(),
            s.selected_target.clone(),
            s.selected_profile_id.clone(),
        )
    };

    let packages =
        fetch_packages_for_profile(&cache, get_image_builder, &version, &target, &profile_id).await;

    let packages = match packages {
        Ok(p) => {
            let packages = p + " " + EXTRA_PACKAGES;

            let mut pkgs_vec = packages
                .split_whitespace()
                .map(String::from)
                .collect::<Vec<_>>();
            pkgs_vec.sort_unstable();
            pkgs_vec.dedup();

            if let Ok(mut s) = state.write() {
                s.packages.clone_from(&pkgs_vec);
            }
            pkgs_vec.join(" ")
        }
        Err(e) => {
            show_error(ui_weak, "Error fetching package list for profile", Some(e));
            String::new()
        }
    };

    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_packages_text(packages.into());
        ui.set_removed_packages_text(SharedString::new());

        if !already_busy {
            ui.set_progress_visible(false);
            ui.set_busy(false);
        }
        ui.set_progress_value(0.0);
        ui.set_build_status(SharedString::new());

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
    state: &AppState,
    path: &Path,
    cache: MetadataCache,
    get_image_builder: &GetImageBuilderFn,
) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(&path).await?;
    let preset: Preset = serde_json::from_str(&content)?;
    let target_id = &preset.target;
    let profile_id = &preset.profile_id;

    let (found, version) = {
        let s = state.read().unwrap();
        let found = s
            .profiles
            .iter()
            .find(|p| &p.id == profile_id && &p.target == target_id)
            .map(|p| {
                let name = format_profile(p)
                    .first()
                    .cloned()
                    .unwrap_or_else(|| p.id.clone());
                (name, get_all_models_string(p), p.target.clone())
            });
        (found, s.selected_version.clone())
    };

    let (name, model, target) = found.ok_or_else(|| {
        anyhow::anyhow!(
            "Profile '{profile_id}' for target '{target_id}' not found in current version"
        )
    })?;

    ui_weak.upgrade_in_event_loop({
        let target = target.clone();
        let profile_id = preset.profile_id.clone();
        let overlay_path = preset.overlay_path.clone();
        move |ui| {
            ui.set_search_text(name.into());
            ui.set_selected_id(profile_id.clone().into());
            ui.set_selected_model(model.into());
            ui.set_selected_target(target.clone().into());
            ui.set_overlay_path_text(overlay_path.into());
            ui.set_packages_text(SharedString::new());
            ui.set_removed_packages_text(SharedString::new());
        }
    })?;

    let current_series = get_release_series(&version);
    let preset_series = preset.release_series.clone();

    let exists = set_image_exists(ui_weak, get_image_builder, &version, &target).await;
    if !exists {
        let _ = ui_weak.upgrade_in_event_loop(|ui| {
            ui.set_progress_visible(false);
            ui.set_busy(false);
        });
        return Ok(());
    }

    // Load original packages for comparison
    let fetched_pkgs =
        fetch_packages_for_profile(&cache, get_image_builder, &version, &target, profile_id)
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

    let target = target.clone();
    let profile_id: String = preset.profile_id.clone();
    let original_pkgs = original_pkgs.clone();
    let state = state.clone();

    ui_weak.upgrade_in_event_loop(move |ui| {
        ui.set_build_status(SharedString::new());
        ui.set_disabled_service_text(preset.disabled_services.into());
        ui.set_extra_image_name_text(preset.extra_image_name.into());
        ui.set_overlay_path_text(preset.overlay_path.into());
        ui.set_packages_text(preset.packages.into());
        ui.set_profiles(Rc::new(VecModel::<SharedString>::default()).into());
        ui.set_progress_visible(false);
        ui.set_rootfs_size_value(preset.rootfs_size.into());
        ui.set_removed_packages_text(removed_str.into());
        ui.set_busy(false);

        if !preset_series.is_empty() && preset_series != current_series {
            let info_msg = format!("Package list from preset series {preset_series} might be incompatible with {current_series}.");
            set_notification(Notification::Warning, &ui, Some(&info_msg));
        }

        if let Ok(mut s) = state.write() {
            s.selected_target = target;
            s.selected_profile_id = profile_id;
            s.packages = original_pkgs;
        }

        let ui_weak = ui.as_weak();
        let _ = ui_weak.upgrade_in_event_loop(move |ui| {
            ui.invoke_request_profile_search_focus();
        });
    })?;

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

/// Compares current package list with the original profile defaults.
/// Any default package missing from the user list is prefixed with `-` to
/// ensure removal.
fn prepare_package_list(user_pkgs: &[String], original_pkgs: &[String]) -> Vec<String> {
    let user_set: HashSet<&str> = user_pkgs.iter().map(AsRef::as_ref).collect();
    let mut user_pkgs = user_pkgs.to_vec();

    for pkg in original_pkgs {
        if !user_set.contains(pkg.as_str()) {
            user_pkgs.push(format!("-{pkg}"));
        }
    }

    user_pkgs
}

/// Resets the profile details section to empty/default values
fn reset_profile_info(ui: &AppWindow, state: &AppState) {
    ui.set_build_status(SharedString::new());
    ui.set_disabled_service_text(SharedString::new());
    ui.set_extra_image_name_text(SharedString::new());
    ui.set_image_exists(false);
    ui.set_overlay_path_text(SharedString::new());
    ui.set_packages_text(SharedString::new());
    ui.set_profiles_fetch_failed(false);
    ui.set_progress_value(0.0);
    ui.set_progress_visible(false);
    ui.set_removed_packages_text(SharedString::new());
    ui.set_rootfs_size_value(SharedString::new());
    ui.set_selected_id(SharedString::new());
    ui.set_selected_model(SharedString::new());
    ui.set_selected_target(SharedString::new());

    state.reset();

    set_notification(Notification::Info, ui, None);
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
        ui.set_image_exists(exists);
        set_notification(
            Notification::Info,
            &ui,
            if exists {
                None
            } else {
                Some("Image builder not found locally. Please download it first.")
            },
        );
    });

    exists
}

fn set_notification(t: Notification, ui: &AppWindow, text: Option<&str>) {
    let (is_err, is_warn, msg) = if let Some(msg) = text {
        t.log(msg);
        (
            t == Notification::Error,
            t == Notification::Warning,
            msg.into(),
        )
    } else {
        (false, false, SharedString::default())
    };

    ui.set_notification_is_error(is_err);
    ui.set_notification_is_warning(is_warn);
    ui.set_notification(msg);
}

fn show_error(ui_weak: &slint::Weak<AppWindow>, msg: &str, e: Option<anyhow::Error>) {
    if let Some(e) = e {
        eprintln!("Error: {e}");
    }

    let msg = msg.to_string();
    let _ = ui_weak.upgrade_in_event_loop(move |ui| {
        set_notification(Notification::Error, &ui, Some(&msg));
    });
}
