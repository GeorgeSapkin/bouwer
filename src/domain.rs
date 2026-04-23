// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use std::cmp::Ordering;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::ops::Deref;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::BuildData;

#[derive(Clone)]
pub struct ImageTag(String);

impl ImageTag {
    pub fn new(target: &Target, version: &Version, image_base: &str) -> Self {
        Self(format!("{image_base}:{}-{version}", target.to_slug()))
    }
}

impl AsRef<str> for ImageTag {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Deref for ImageTag {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
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

impl From<&str> for ImageTag {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for ImageTag {
    fn from(s: String) -> Self {
        Self(s)
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Package {
    pub name: String,
    pub enabled: bool,
}

impl Display for Package {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.enabled {
            write!(f, "{}", self.name)
        } else {
            write!(f, "-{}", self.name)
        }
    }
}

impl From<String> for Package {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<&str> for Package {
    fn from(s: &str) -> Self {
        if let Some(name) = s.strip_prefix('-') {
            Self {
                name: name.to_string(),
                enabled: false,
            }
        } else {
            Self {
                name: s.to_string(),
                enabled: true,
            }
        }
    }
}

impl Ord for Package {
    fn cmp(&self, other: &Self) -> Ordering {
        // Sort by enabled (true first), then by name
        self.enabled
            .cmp(&other.enabled)
            .reverse()
            .then_with(|| self.name.cmp(&other.name))
    }
}

impl PartialOrd for Package {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(from = "String", into = "String")]
pub struct PackageList(Vec<Package>);

impl PackageList {
    /// Get all packages in self that are not mentioned in other. Ignores enabled state.
    pub fn diff(&self, other: &PackageList) -> PackageList {
        let other_names: HashSet<_> = other.iter().map(|p| p.name.as_str()).collect();
        self.iter()
            .filter(|p| !other_names.contains(p.name.as_str()))
            .cloned()
            .collect()
    }

    /// Extends self with packages from other. Overrides enabled state.
    pub fn extend(&mut self, other: &PackageList, enabled: bool) {
        let self_names: HashSet<&str> = self.iter().map(|p| p.name.as_str()).collect();
        let extras: Vec<Package> = other
            .iter()
            .filter(|p| !self_names.contains(p.name.as_str()))
            .map(|p| Package {
                name: p.name.clone(),
                enabled,
            })
            .collect();
        self.0.extend(extras);
        self.0.sort_unstable();
        self.0.dedup();
    }
}

impl Deref for PackageList {
    type Target = [Package];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Display for PackageList {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for (i, package) in self.iter().enumerate() {
            if i > 0 {
                write!(f, " ")?;
            }
            write!(f, "{package}")?;
        }
        Ok(())
    }
}

impl FromIterator<Package> for PackageList {
    fn from_iter<I: IntoIterator<Item = Package>>(iter: I) -> Self {
        Self::from(iter.into_iter().collect::<Vec<_>>())
    }
}

impl From<Vec<Package>> for PackageList {
    fn from(mut v: Vec<Package>) -> Self {
        v.sort_unstable();
        v.dedup();
        Self(v)
    }
}

impl From<&str> for PackageList {
    fn from(s: &str) -> Self {
        s.split_whitespace().map(Package::from).collect()
    }
}

impl From<String> for PackageList {
    fn from(s: String) -> Self {
        Self::from(s.as_str())
    }
}

impl From<PackageList> for String {
    fn from(list: PackageList) -> Self {
        list.to_string()
    }
}

impl IntoIterator for PackageList {
    type Item = Package;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'iter> IntoIterator for &'iter PackageList {
    type Item = &'iter Package;
    type IntoIter = std::slice::Iter<'iter, Package>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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
    pub packages: PackageList,
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
            packages: data.packages.as_str().into(),
            disabled_services: data.disabled_services.into(),
            overlay_path: data.overlay_path.as_str().into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl AsRef<str> for ProfileId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Deref for ProfileId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Display for ProfileId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for ProfileId {
    fn from(s: &str) -> Self {
        Self(s.replace(',', "_"))
    }
}

impl From<String> for ProfileId {
    fn from(s: String) -> Self {
        Self(s.replace(',', "_"))
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
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

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
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
        U32OrString::String(s) if s.is_empty() => Ok(0),
        U32OrString::String(s) => s.parse::<u32>().map_err(serde::de::Error::custom),
    }
}

fn is_path_empty(p: &Path) -> bool {
    p.as_os_str().is_empty()
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(v: &u32) -> bool {
    *v == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_id_normalization() {
        let cases = [
            ("zyxel,ex5601-t0-ubootmod", "zyxel_ex5601-t0-ubootmod"),
            ("zyxel_ex5601-t0-ubootmod", "zyxel_ex5601-t0-ubootmod"),
        ];
        for (input, expected) in cases {
            assert_eq!(ProfileId::from(input).0, expected);
        }
    }

    #[test]
    fn test_version_parsing_and_display() {
        let cases = [
            ("23.05.2", 23, 5, 2, None),
            ("23.05.0-rc3", 23, 5, 0, Some(3)),
            ("21.02.0-rc1", 21, 2, 0, Some(1)),
            ("25.12.0-rc12", 25, 12, 0, Some(12)),
            ("19.07.10", 19, 7, 10, None),
            ("0.00.0", 0, 0, 0, None),
        ];

        for (input, major, minor, patch, rc) in cases {
            let v = Version::from(input);
            assert_eq!(v.major, major);
            assert_eq!(v.minor, minor);
            assert_eq!(v.patch, patch);
            assert_eq!(v.rc, rc);
            assert_eq!(v.to_string(), input);
        }
    }

    #[test]
    fn test_version_ordering() {
        let v1 = Version::from("23.05.0-rc1");
        let v2 = Version::from("23.05.0-rc2");
        let v3 = Version::from("23.05.0");
        let v4 = Version::from("23.05.1");
        let v5 = Version::from("24.10.0");

        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v3 < v4);
        assert!(v4 < v5);
    }

    #[test]
    fn test_target_logic() {
        let t = Target::from("ath79/generic");
        assert_eq!(t.target, "ath79");
        assert_eq!(t.subtarget, "generic");
        assert_eq!(t.to_slug(), "ath79-generic");
        assert_eq!(t.to_path(), PathBuf::from("ath79").join("generic"));

        let t2 = Target::from("realtek/rtl931x_nand");
        assert_eq!(t2.to_slug(), "realtek-rtl931x_nand");
    }

    #[test]
    fn test_image_tag_conversions() {
        let tag = ImageTag("openwrt/imagebuilder:ramips-mt7621-23.05.2".to_string());
        let t = Target::try_from(&tag).expect("Failed to parse target from tag");
        let v = Version::try_from(&tag).expect("Failed to parse version from tag");
        assert_eq!(t.to_string(), "ramips/mt7621");
        assert_eq!(v.to_string(), "23.05.2");

        let tag = ImageTag("base:x86-64-21.02.0-rc1".to_string());
        let t = Target::try_from(&tag).unwrap();
        let v = Version::try_from(&tag).unwrap();
        assert_eq!(t.to_string(), "x86/64");
        assert_eq!(v.to_string(), "21.02.0-rc1");
    }

    #[test]
    fn test_profile_title_formatting() {
        let pt1 = ProfileTitle {
            model: Some("C200".into()),
            vendor: Some("Ctera".into()),
            variant: Some("V1".into()),
            title: None,
        };
        assert_eq!(pt1.to_string(), "Ctera C200 V1");

        let pt2 = ProfileTitle {
            model: Some("LoongArch64".into()),
            vendor: Some("Generic".into()),
            variant: None,
            title: None,
        };
        assert_eq!(pt2.to_string(), "Generic LoongArch64");

        let pt3 = ProfileTitle {
            model: None,
            vendor: None,
            variant: None,
            title: Some("Generic EFI Boot".into()),
        };
        assert_eq!(pt3.to_string(), "Generic EFI Boot");
    }

    #[test]
    fn test_profile_filtering() {
        let profiles = [
            Profile {
                id: "zyxel,ex5601-t0-ubootmod".into(),
                titles: vec![ProfileTitle {
                    model: Some("EX5601-T0".into()),
                    vendor: Some("Zyxel".into()),
                    variant: Some("(OpenWrt U-Boot layout)".into()),
                    title: None,
                }],
                target: Target::from("mediatek/filogic"),
            },
            Profile {
                id: "generic".into(),
                titles: vec![ProfileTitle {
                    model: Some("x86/64".into()),
                    vendor: Some("Generic".into()),
                    variant: None,
                    title: Some("Generic thing".into()),
                }],
                target: Target::from("x86/64"),
            },
        ];

        // Match by ID
        assert_eq!(profiles.filter("ex5601-t0-ubootmod").len(), 1);
        // Match by title
        assert_eq!(profiles.filter("generic thing").len(), 1);
        // Normalized case matching
        assert_eq!(profiles.filter("EX5601-T0").len(), 1);
        assert_eq!(profiles.filter("nomatch").len(), 0);

        let display_name = "Zyxel EX5601-T0 (OpenWrt U-Boot layout) (zyxel_ex5601-t0-ubootmod)";
        let found = profiles.find_by_display_name(display_name);
        assert!(found.is_some());
        assert_eq!(found.unwrap().id.0, "zyxel_ex5601-t0-ubootmod");
    }

    #[test]
    fn test_release_series_parsing() {
        let rs = ReleaseSeries::from("23.05.2");
        assert_eq!(rs.major, 23);
        assert_eq!(rs.minor, 5);
        assert_eq!(rs.to_string(), "23.05");

        let rs_rc = ReleaseSeries::from("25.12.0-rc5");
        assert_eq!(rs_rc.major, 25);
        assert_eq!(rs_rc.minor, 12);
    }

    #[test]
    fn test_preset_serialization() {
        let preset = Preset {
            release_series: ReleaseSeries {
                major: 23,
                minor: 5,
            },
            target: "ath79/generic".into(),
            profile_id: "id".into(),
            extra_image_name: "openssl".into(),
            rootfs_size: 5000,
            packages: "luci".into(),
            disabled_services: "dnsmasq".into(),
            overlay_path: "/path/to/overlay".into(),
        };

        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"release_series\":\"23.05\""));
        assert!(json.contains("\"extra_image_name\":\"openssl\""));
        assert!(json.contains("\"packages\":\"luci\""));
        assert!(json.contains("\"rootfs_size\":5000"));

        let deserialized: Preset = serde_json::from_str(&json).unwrap();
        assert_eq!(preset, deserialized);
    }

    #[test]
    fn test_preset_serialization_skips() {
        let preset = Preset {
            release_series: ReleaseSeries {
                major: 23,
                minor: 5,
            },
            target: "ath79/generic".into(),
            profile_id: "id".into(),
            extra_image_name: String::new(),
            rootfs_size: 0,
            packages: "luci".into(),
            disabled_services: String::new(),
            overlay_path: PathBuf::new(),
        };

        let json = serde_json::to_string(&preset).unwrap();
        assert!(json.contains("\"release_series\":\"23.05\""));
        assert!(json.contains("\"packages\":\"luci\""));

        assert!(!json.contains("extra_image_name"));
        assert!(!json.contains("disabled_services"));
        assert!(!json.contains("overlay_path"));
        assert!(!json.contains("rootfs_size"));

        let deserialized: Preset = serde_json::from_str(&json).unwrap();
        assert_eq!(preset, deserialized);
    }

    #[test]
    fn test_version_deserialization() {
        let versions_json = include_str!("../tests/versions.json");
        let versions: OpenWrtVersions =
            serde_json::from_str(versions_json).expect("Failed to deserialize versions fixture");
        assert!(!versions.versions_list.is_empty());

        let first = &versions.versions_list[0];
        assert_eq!(first.to_string(), "25.12.2");

        let rc_version = &versions.versions_list[2];
        assert_eq!(rc_version.to_string(), "25.12.0-rc5");
        assert_eq!(rc_version.rc, Some(5));
    }

    #[test]
    fn test_overview_deserialization() {
        let overview_json = include_str!("../tests/overview.json");
        let overview: OpenWrtOverview =
            serde_json::from_str(overview_json).expect("Failed to deserialize overview fixture");
        assert_eq!(overview.profiles.len(), 3);

        let multitle_profile = overview
            .profiles
            .iter()
            .find(|p| p.id.0 == "zyxel_ex5601-t0-ubootmod")
            .unwrap();
        assert_eq!(multitle_profile.titles.len(), 3);
        assert_eq!(
            multitle_profile.format_all_models(),
            "Zyxel EX5601-T0 (OpenWrt U-Boot layout) / Zyxel EX5601-T1 / Zyxel T-56"
        );

        let multitle_profile = overview
            .profiles
            .iter()
            .find(|p| p.id.0 == "generic")
            .unwrap();
        assert_eq!(multitle_profile.titles.len(), 1);
        assert_eq!(multitle_profile.format_all_models(), "Generic EFI Boot");
    }

    #[test]
    fn test_deserialize_u32_or_string_logic() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Wrapper {
            #[serde(deserialize_with = "deserialize_u32_or_string")]
            val: u32,
        }

        let cases = [
            (r#"{"val": 5000}"#, 5000),
            (r#"{"val": "5000"}"#, 5000),
            (r#"{"val": ""}"#, 0),
        ];

        for (json, expected) in cases {
            let w: Wrapper = serde_json::from_str(json).unwrap();
            assert_eq!(w.val, expected);
        }
    }

    #[test]
    fn test_package_display() {
        let p1 = Package {
            name: "luci".into(),
            enabled: true,
        };
        let p2 = Package {
            name: "dnsmasq".into(),
            enabled: false,
        };
        assert_eq!(p1.to_string(), "luci");
        assert_eq!(p2.to_string(), "-dnsmasq");
    }

    #[test]
    fn test_package_list_display() {
        let list = PackageList::from(vec![
            Package {
                name: "pkg1".into(),
                enabled: true,
            },
            Package {
                name: "pkg2".into(),
                enabled: false,
            },
            Package {
                name: "pkg3".into(),
                enabled: true,
            },
        ]);
        assert_eq!(list.to_string(), "pkg1 pkg3 -pkg2");
    }

    #[test]
    fn test_package_list_from_str() {
        let input = "  pkg1   -pkg2  pkg3  ";
        let list = PackageList::from(input);
        assert_eq!(list.len(), 3);

        assert_eq!(list[0].name, "pkg1");
        assert!(list[0].enabled);

        assert_eq!(list[1].name, "pkg3");
        assert!(list[1].enabled);

        assert_eq!(list[2].name, "pkg2");
        assert!(!list[2].enabled);

        assert_eq!(list.to_string(), "pkg1 pkg3 -pkg2");
    }

    #[test]
    fn test_package_conversions() {
        let p1 = Package::from("abc");
        assert_eq!(p1.name, "abc");
        assert!(p1.enabled);

        let p2 = Package::from("def".to_string());
        assert_eq!(p2.name, "def");
        assert!(p2.enabled);
    }

    #[test]
    fn test_package_list_default() {
        let list = PackageList::default();
        assert!(list.is_empty());
    }

    #[test]
    fn test_package_list_diff() {
        let left = PackageList::from("pkg1 pkg2 pkg3");
        let right = PackageList::from("pkg2 pkg4");

        let diff = left.diff(&right);

        // pkg1 and pkg3 should remain from list1
        assert_eq!(diff.len(), 2);
        assert_eq!(diff.to_string(), "pkg1 pkg3");

        let empty = PackageList::default();
        assert_eq!(left.diff(&empty).to_string(), "pkg1 pkg2 pkg3");
        assert_eq!(empty.diff(&left).to_string(), "");

        // Test ignoring enabled state in both lists
        let left = PackageList::from("pkg1 -pkg2");
        let right = PackageList::from("pkg2 pkg5");
        let diff = left.diff(&right);

        // pkg2 is removed even if disabled in list3 or enabled in list4
        assert_eq!(diff.to_string(), "pkg1");
    }

    #[test]
    fn test_package_list_extend_enabled() {
        let mut left = PackageList::from("pkg1 -pkg2");
        let right = PackageList::from("pkg2 pkg3");

        left.extend(&right, true);
        assert_eq!(left.len(), 3);

        let p1 = left.iter().find(|p| p.name == "pkg1").unwrap();
        let p2 = left.iter().find(|p| p.name == "pkg2").unwrap();
        let p3 = left.iter().find(|p| p.name == "pkg3").unwrap();

        assert!(p1.enabled);
        assert!(!p2.enabled);
        assert!(p3.enabled);

        let mut left: PackageList = "pkg1 pkg3".into();
        let right: PackageList = "pkg1 pkg2 pkg3".into();
        left.extend(&right, true);

        let p1 = left.iter().find(|p| p.name == "pkg1").unwrap();
        let p2 = left.iter().find(|p| p.name == "pkg2").unwrap();
        let p3 = left.iter().find(|p| p.name == "pkg3").unwrap();

        assert!(p1.enabled);
        assert!(p2.enabled);
        assert!(p3.enabled);
    }

    #[test]
    fn test_package_list_extend_disabled() {
        let mut left = PackageList::from("pkg1 -pkg2");
        let right = PackageList::from("pkg2 pkg3");

        left.extend(&right, false);
        assert_eq!(left.len(), 3);

        let p1 = left.iter().find(|p| p.name == "pkg1").unwrap();
        let p2 = left.iter().find(|p| p.name == "pkg2").unwrap();
        let p3 = left.iter().find(|p| p.name == "pkg3").unwrap();

        assert!(p1.enabled);
        assert!(!p2.enabled);
        assert!(!p3.enabled);

        let mut left: PackageList = "pkg1 pkg3".into();
        let right: PackageList = "pkg1 pkg2 pkg3".into();
        left.extend(&right, false);

        let p1 = left.iter().find(|p| p.name == "pkg1").unwrap();
        let p2 = left.iter().find(|p| p.name == "pkg2").unwrap();
        let p3 = left.iter().find(|p| p.name == "pkg3").unwrap();

        assert!(p1.enabled);
        assert!(!p2.enabled);
        assert!(p3.enabled);
    }

    #[test]
    fn test_package_list_conversions() {
        let s = "pkg1 -pkg2".to_string();
        let list = PackageList::from(s);
        assert_eq!(list.to_string(), "pkg1 -pkg2");

        let s2: String = list.into();
        assert_eq!(s2, "pkg1 -pkg2");
    }
}
