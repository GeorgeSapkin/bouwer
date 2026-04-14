// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use slint::{ComponentHandle, SharedString, VecModel};
use std::rc::Rc;

use crate::domain::Version;
use crate::{AppState, AppWindow, StateBridge};

#[derive(Clone, PartialEq)]
pub enum UIState {
    Idle(Option<String>),
    Building {
        progress: Option<f32>,
        status: Option<String>,
    },
    DownloadingBuilder {
        progress: Option<f32>,
        status: Option<String>,
    },
    FetchingPackages,
    LoadingPreset,
    LoadingProfiles(Version),
    LoadingVersions,
    SavingPreset,
    SelectBuildFolder,
    SelectOverlayFolder,
    Error(String),
}

pub trait AppWindowExt {
    fn get_state(&self) -> AppState;
    fn set_state(&self, state: AppState);
    fn set_notification(&self, t: Notification, text: Option<&str>);
    fn switch_state_to(&self, next_state: UIState);
    fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut AppState);
}

pub trait AppWindowWeakExt {
    fn set_notification(&self, t: Notification, text: Option<&str>);
    fn switch_state_to(&self, next_state: UIState);
    fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut AppState) + Send + 'static;
}

impl AppWindowExt for AppWindow {
    fn get_state(&self) -> AppState {
        self.global::<StateBridge>().get_state()
    }

    fn set_state(&self, state: AppState) {
        self.global::<StateBridge>().set_state(state);
    }

    fn set_notification(&self, t: Notification, text: Option<&str>) {
        self.update_state(|s| s.set_notification(t, text));
    }

    fn switch_state_to(&self, next_state: UIState) {
        self.update_state(|s| s.switch_to(next_state));
    }

    fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut AppState),
    {
        let mut state = self.get_state();
        f(&mut state);
        self.set_state(state);
    }
}

impl AppWindowWeakExt for slint::Weak<AppWindow> {
    fn set_notification(&self, t: Notification, text: Option<&str>) {
        let text = text.map(ToString::to_string);
        let _ = self.upgrade_in_event_loop(move |ui| {
            ui.set_notification(t, text.as_deref());
        });
    }

    fn switch_state_to(&self, next_state: UIState) {
        self.update_state(|s| s.switch_to(next_state));
    }

    fn update_state<F>(&self, f: F)
    where
        F: FnOnce(&mut AppState) + Send + 'static,
    {
        let _ = self.upgrade_in_event_loop(move |ui| {
            ui.update_state(f);
        });
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Notification {
    Info,
    Warning,
    Error,
}

impl Notification {
    pub fn log(self, text: &str) {
        match self {
            Self::Error => eprintln!("Error: {text}"),
            Self::Warning => eprintln!("Warning: {text}"),
            Self::Info => println!("{text}"),
        }
    }
}

impl AppState {
    pub fn switch_to(&mut self, next_state: UIState) {
        match next_state {
            UIState::Idle(status) => {
                self.busy = false;
                self.progress_value = 0.0;
                self.progress_visible = false;
                if let Some(s) = status {
                    self.status_text = s.into();
                } else {
                    self.status_text = SharedString::new();
                }
            }
            UIState::Building { progress, status }
            | UIState::DownloadingBuilder { progress, status } => {
                self.busy = true;
                if let Some(p) = progress {
                    self.progress_value = p;
                } else {
                    self.progress_value = 0.0;
                }
                self.progress_visible = true;
                if let Some(s) = status {
                    self.status_text = s.into();
                }
            }
            UIState::FetchingPackages => self.set_busy("Fetching package list", true),
            UIState::LoadingPreset => self.set_busy("Loading preset", true),
            UIState::SavingPreset => self.set_busy("Saving preset", true),
            UIState::LoadingProfiles(version) => {
                self.search_text = "".into();
                self.selected_id = SharedString::new();
                self.selected_version = version.to_string().into();
                self.set_busy("Fetching profiles", true);
            }
            UIState::LoadingVersions => self.set_busy("Fetching versions", true),
            UIState::SelectBuildFolder => self.set_busy("Select build folder", false),
            UIState::SelectOverlayFolder => self.set_busy("Select overlay folder", false),
            UIState::Error(msg) => {
                self.busy = false;
                self.progress_value = 0.0;
                self.progress_visible = false;
                self.status_text = msg.into();
            }
        }
    }

    pub fn reset_profile(&mut self) {
        self.build_command_preview = SharedString::new();
        self.disabled_services_text = SharedString::new();
        self.extra_image_name_text = SharedString::new();
        self.image_exists = false;
        self.original_packages = Rc::new(VecModel::<SharedString>::default()).into();
        self.overlay_path_text = SharedString::new();
        self.packages_text = SharedString::new();
        self.profiles_fetch_failed = false;
        self.progress_value = 0.0;
        self.progress_visible = false;
        self.removed_packages_text = SharedString::new();
        self.rootfs_size_text = SharedString::new();
        self.selected_id = SharedString::new();
        self.selected_model = SharedString::new();
        self.selected_target = SharedString::new();
        self.status_text = SharedString::new();

        self.set_notification(Notification::Info, None);
    }

    pub fn set_notification(&mut self, t: Notification, text: Option<&str>) {
        if let Some(msg) = text {
            t.log(msg);
            self.notification_is_error = t == Notification::Error;
            self.notification_is_warning = t == Notification::Warning;
            self.notification = msg.into();
        } else {
            self.notification_is_error = false;
            self.notification_is_warning = false;
            self.notification = SharedString::default();
        }
    }

    fn set_busy(&mut self, status: &str, show_progress: bool) {
        self.busy = true;
        if show_progress {
            self.progress_value = 0.0;
            self.progress_visible = true;
        } else {
            self.progress_visible = false;
        }
        self.status_text = status.into();
    }
}
