// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    cmp::Ordering,
    fmt::{Display, Formatter},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::BuildData;

#[derive(Clone)]
pub struct ImageTag(pub String);

impl ImageTag {
    pub fn new(target: &Target, version: &Version, image_base: &str) -> Self {
        Self(format!("{image_base}:{}-{version}", target.to_slug()))
    }
}

impl Display for ImageTag {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&ImageTag> for Option<String> {
    fn from(tag: &ImageTag) -> Self {
        Some(tag.0.clone())
    }
}

#[derive(Deserialize)]
pub struct OpenWrtOverview {
    pub profiles: Vec<Profile>,
}

#[derive(Deserialize)]
pub struct OpenWrtVersions {
    pub versions_list: Vec<Version>,
}

#[derive(Serialize, Deserialize)]
pub struct Preset {
    pub release_series: ReleaseSeries,
    pub target: Target,
    pub profile_id: ProfileId,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub extra_image_name: String,
    #[serde(
        skip_serializing_if = "is_zero",
        default,
        deserialize_with = "deserialize_u32_or_string"
    )]
    pub rootfs_size: u32,
    pub packages: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub disabled_services: String,
    #[serde(skip_serializing_if = "is_path_empty", default)]
    pub overlay_path: PathBuf,
}

impl From<BuildData> for Preset {
    fn from(data: BuildData) -> Self {
        Self {
            release_series: data.version.as_str().into(),
            target: data.target.as_str().into(),
            profile_id: data.profile_id.as_str().into(),
            extra_image_name: data.extra_image_name.into(),
            rootfs_size: data.rootfs_size.cast_unsigned(),
            packages: data.packages.into(),
            disabled_services: data.disabled_services.into(),
            overlay_path: data.overlay_path.as_str().into(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct ProfileId(pub String);

impl Display for ProfileId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ProfileId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ProfileId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Profile {
    pub id: ProfileId,
    pub titles: Vec<ProfileTitle>,
    pub target: Target,
}

impl Profile {
    pub fn format(&self) -> Vec<String> {
        self.titles
            .iter()
            .map(|t| format!("{t} ({})", self.id))
            .collect()
    }

    pub fn format_all_models(&self) -> String {
        self.titles
            .iter()
            .map(ToString::to_string)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    }

    pub fn matches(&self, query: &str) -> bool {
        self.id.0.to_lowercase().contains(query)
            || self
                .titles
                .iter()
                .any(|t| t.to_string().to_lowercase().contains(query))
    }
}

pub trait ProfileSliceExt {
    fn filter(&self, query: &str) -> Vec<String>;
    fn find_by_display_name(&self, name: &str) -> Option<Profile>;
}

impl ProfileSliceExt for [Profile] {
    fn filter(&self, query: &str) -> Vec<String> {
        let query = query.trim().to_lowercase();
        self.iter()
            .filter(|p| p.matches(&query))
            .flat_map(Profile::format)
            .collect()
    }

    fn find_by_display_name(&self, name: &str) -> Option<Profile> {
        let name = name.trim().to_lowercase();
        self.iter()
            .find(|p| p.format().iter().any(|dn| dn.to_lowercase() == name))
            .cloned()
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ProfileTitle {
    pub model: Option<String>,
    pub vendor: Option<String>,
    pub variant: Option<String>,
    pub title: Option<String>,
}

impl Display for ProfileTitle {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(full_title) = &self.title {
            return write!(f, "{full_title}");
        }
        let vendor = self.vendor.as_deref().unwrap_or("").trim();
        let model = self.model.as_deref().unwrap_or("").trim();
        let mut s = format!("{vendor} {model}").trim().to_owned();

        if let Some(variant) = self.variant.as_deref()
            && !variant.is_empty()
        {
            s = format!("{s} {variant}");
        }

        write!(f, "{s}")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct ReleaseSeries {
    pub major: u8,
    pub minor: u8,
}

impl Display for ReleaseSeries {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02}", self.major, self.minor)
    }
}

impl From<String> for ReleaseSeries {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&str> for ReleaseSeries {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split('.').collect();
        let major = parts
            .first()
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        let minor = parts
            .get(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        Self { major, minor }
    }
}

impl From<ReleaseSeries> for String {
    fn from(rs: ReleaseSeries) -> String {
        rs.to_string()
    }
}

impl From<Version> for ReleaseSeries {
    fn from(v: Version) -> Self {
        Self::from(&v)
    }
}

impl From<&Version> for ReleaseSeries {
    fn from(v: &Version) -> Self {
        Self {
            major: v.major,
            minor: v.minor,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(from = "String", into = "String")]
pub struct Target {
    pub target: String,
    pub subtarget: String,
}

impl Target {
    pub fn to_path(&self) -> PathBuf {
        let mut p = PathBuf::new();
        p.push(&self.target);
        p.push(&self.subtarget);
        p
    }

    pub fn to_slug(&self) -> String {
        format!("{}-{}", self.target, self.subtarget)
    }
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.subtarget.is_empty() {
            write!(f, "{}", self.target)
        } else {
            write!(f, "{}/{}", self.target, self.subtarget)
        }
    }
}

impl From<Target> for String {
    fn from(t: Target) -> Self {
        t.to_string()
    }
}

impl From<String> for Target {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&str> for Target {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split('/').collect();
        Self {
            target: parts.first().copied().unwrap_or_default().to_string(),
            subtarget: parts.get(1).copied().unwrap_or_default().to_string(),
        }
    }
}

impl TryFrom<&ImageTag> for Target {
    type Error = &'static str;

    fn try_from(tag: &ImageTag) -> Result<Self, Self::Error> {
        let (_, stripped) = tag.0.split_once(':').ok_or("invalid tag format")?;
        let (idx, _) = stripped
            .rmatch_indices('-')
            .find(|&(i, _)| {
                let suffix = &stripped[i + 1..];
                suffix.starts_with(|c: char| c.is_ascii_digit()) && suffix.contains('.')
            })
            .ok_or("could not parse target and version")?;

        let target_slug = &stripped[..idx];
        Ok(Target::from(target_slug.replace('-', "/").as_str()))
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(from = "String")]
pub struct Version {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
    pub rc: Option<u8>,
}
impl Version {
    pub fn to_release_series(&self) -> ReleaseSeries {
        ReleaseSeries::from(self)
    }

    pub fn same_release_series(&self, other: &ReleaseSeries) -> bool {
        &self.to_release_series() == other
    }
}

impl Display for Version {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{:02}.{}", self.major, self.minor, self.patch)?;
        if let Some(rc) = self.rc {
            write!(f, "-rc{rc}")?;
        }
        Ok(())
    }
}

impl From<String> for Version {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&str> for Version {
    fn from(s: &str) -> Self {
        let parts: Vec<&str> = s.split('.').collect();
        let major = parts
            .first()
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        let minor = parts
            .get(1)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default();
        let last = parts.get(2).unwrap_or(&"");

        let (patch, rc) = if let Some((p_str, rc_str)) = last.split_once("-rc") {
            (p_str.parse().unwrap_or_default(), rc_str.parse().ok())
        } else {
            (last.parse().unwrap_or_default(), None)
        };

        Self {
            major,
            minor,
            patch,
            rc,
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then(match (self.rc, other.rc) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TryFrom<&ImageTag> for Version {
    type Error = &'static str;

    fn try_from(tag: &ImageTag) -> Result<Self, Self::Error> {
        let (_, stripped) = tag.0.split_once(':').ok_or("invalid tag format")?;
        let (idx, _) = stripped
            .rmatch_indices('-')
            .find(|&(i, _)| {
                let suffix = &stripped[i + 1..];
                suffix.starts_with(|c: char| c.is_ascii_digit()) && suffix.contains('.')
            })
            .ok_or("could not parse target and version")?;

        Ok(Version::from(&stripped[idx + 1..]))
    }
}

fn deserialize_u32_or_string<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U32OrString {
        String(String),
        U32(u32),
    }

    match U32OrString::deserialize(deserializer)? {
        U32OrString::U32(v) => Ok(v),
        U32OrString::String(s) => {
            if s.is_empty() {
                Ok(0)
            } else {
                s.parse::<u32>().map_err(serde::de::Error::custom)
            }
        }
    }
}

fn is_path_empty(p: &Path) -> bool {
    p.as_os_str().is_empty()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &u32) -> bool {
    *v == 0
}
