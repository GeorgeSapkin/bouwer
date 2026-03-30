// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use crate::client::{Profile, ProfileTitle};

pub fn filter_profiles(profiles: &[Profile], query: &str) -> Vec<String> {
    let query = query.trim().to_lowercase();

    profiles
        .iter()
        .filter(profile_matches(&query))
        .flat_map(format_profile)
        .collect()
}

pub fn find_profile_by_display_name(profiles: &[Profile], name: &str) -> Option<Profile> {
    let name = name.trim().to_lowercase();
    profiles
        .iter()
        .find(|p| format_profile(p).iter().any(|dn| dn.to_lowercase() == name))
        .cloned()
}

fn format_model(t: &ProfileTitle) -> String {
    let vendor = t.vendor.as_deref().unwrap_or("").trim();
    let model = t.model.as_deref().unwrap_or("").trim();
    let mut s = format!("{vendor} {model}").trim().to_owned();

    if let Some(variant) = t.variant.as_deref()
        && !variant.is_empty()
    {
        s = format!("{s} {variant}");
    }

    s
}

pub fn format_profile(p: &Profile) -> Vec<String> {
    p.titles
        .iter()
        .map(|t| {
            format!(
                "{} ({})",
                if let Some(full_title) = &t.title {
                    full_title.clone()
                } else {
                    format_model(t)
                },
                p.id
            )
        })
        .collect()
}

pub fn get_all_models_string(p: &Profile) -> String {
    p.titles
        .iter()
        .map(|t| {
            if let Some(full_title) = &t.title {
                full_title.clone()
            } else {
                format_model(t)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

fn profile_matches(query: &str) -> impl Fn(&&Profile) -> bool {
    move |p| {
        if p.id.to_lowercase().contains(query) {
            return true;
        }
        p.titles.iter().any(|t| {
            if let Some(title) = &t.title
                && title.to_lowercase().contains(query)
            {
                return true;
            }
            format_model(t).to_lowercase().contains(query)
        })
    }
}
