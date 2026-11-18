//! P4.3.1A: searching and labelling the catalogue (UI-independent).

use std::collections::BTreeMap;

use super::cache::{CatalogueView, RefreshOutcome};
use super::{
    Capability, Catalogue, CataloguePackage, CatalogueVersion, Compatibility, DeclaredCapability,
    MARKETPLACE_SOURCE,
};
use crate::library::index::InstalledPackage;

/// How a label should be coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Good,
    Warning,
    Error,
}

// ── Installed state ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallState {
    /// In the Library and verified. `from_marketplace` is false for a
    /// version that got there another way (copied by hand).
    Installed { from_marketplace: bool },
    /// In the Library but failed verification.
    Damaged(String),
}

/// Installed versions by package id, from the Library's existing scan
/// (no extra disk access or hashing).
#[derive(Debug, Clone, Default)]
pub struct InstalledIndex(BTreeMap<String, BTreeMap<semver::Version, InstallState>>);

impl InstalledIndex {
    pub fn from_packages(packages: &[InstalledPackage]) -> Self {
        let mut map: BTreeMap<String, BTreeMap<semver::Version, InstallState>> = BTreeMap::new();
        for package in packages {
            let versions = map.entry(package.id.clone()).or_default();
            for installed in &package.versions {
                let state = match &installed.integrity {
                    Ok(()) => InstallState::Installed {
                        from_marketplace: installed.manifest.source == MARKETPLACE_SOURCE,
                    },
                    Err(problem) => InstallState::Damaged(problem.clone()),
                };
                versions.insert(installed.version.clone(), state);
            }
        }
        Self(map)
    }

    pub fn state(&self, id: &str, version: &semver::Version) -> Option<&InstallState> {
        self.0.get(id)?.get(version)
    }

    /// True when any version of `id` is in the Library.
    pub fn has_any(&self, id: &str) -> bool {
        self.0.get(id).is_some_and(|v| !v.is_empty())
    }
}

pub fn install_label(state: Option<&InstallState>) -> (String, Tone) {
    match state {
        None => ("Not installed".to_string(), Tone::Normal),
        Some(InstallState::Installed {
            from_marketplace: true,
        }) => ("Installed".to_string(), Tone::Good),
        Some(InstallState::Installed {
            from_marketplace: false,
        }) => (
            "In Library (not installed from the marketplace)".to_string(),
            Tone::Warning,
        ),
        Some(InstallState::Damaged(_)) => ("In Library, but damaged".to_string(), Tone::Error),
    }
}

// ── Compatibility and capabilities ──────────────────────────────────────

pub fn compatibility_label(version: &CatalogueVersion, app: &semver::Version) -> (String, Tone) {
    if let Some(reason) = &version.yanked {
        return (format!("Withdrawn: {reason}"), Tone::Error);
    }
    match version.compatibility(app) {
        Compatibility::Compatible => ("Compatible".to_string(), Tone::Good),
        Compatibility::NeedsNewerPatchwork(min) => {
            (format!("Needs Patchwork {min} or newer"), Tone::Warning)
        }
        Compatibility::UnsupportedFormat(what) => (
            format!("Not supported by this Patchwork ({what})"),
            Tone::Warning,
        ),
        Compatibility::UnknownCapabilities(names) => (
            format!(
                "Needs a newer Patchwork (unknown capabilities: {})",
                names.join(", ")
            ),
            Tone::Warning,
        ),
    }
}

pub fn capability_label(capability: &DeclaredCapability) -> String {
    match capability {
        DeclaredCapability::Known(c) => match c {
            Capability::Filesystem => "Files on this computer",
            Capability::Network => "Network access",
            Capability::Devices => "Hardware devices",
            Capability::MediaCapture => "Camera or microphone",
            Capability::Scripting => "Runs scripts or shaders",
            Capability::Process => "Runs other programs on this computer",
            Capability::SystemInfo => "Reads system information",
            Capability::AiCredentials => "Uses your AI service keys",
            Capability::NativeCode => "Native code",
        }
        .to_string(),
        DeclaredCapability::Unknown(name) => format!("Unknown capability '{name}'"),
    }
}

// ── Search ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// Whitespace-separated terms; all must match. `tag:<name>` matches an
    /// exact tag.
    pub text: String,
    /// Only packages with at least one version installable here.
    pub compatible_only: bool,
    /// Only packages with a version in the Library.
    pub installed_only: bool,
}

fn term_matches(catalogue: &Catalogue, package: &CataloguePackage, term: &str) -> bool {
    if let Some(tag) = term.strip_prefix("tag:") {
        return package.tags.iter().any(|t| t == tag);
    }
    let publisher = catalogue.publishers.get(&package.namespace);
    package.name.to_lowercase().contains(term)
        || package.id.contains(term)
        || package.tags.iter().any(|t| t.contains(term))
        || package.author.to_lowercase().contains(term)
        || publisher.is_some_and(|p| {
            p.display_name.to_lowercase().contains(term) || p.github.to_lowercase().contains(term)
        })
}

/// Matching packages, sorted by display name then id.
pub fn search<'a>(
    catalogue: &'a Catalogue,
    query: &SearchQuery,
    app: &semver::Version,
    installed: &InstalledIndex,
) -> Vec<&'a CataloguePackage> {
    let text = query.text.to_lowercase();
    let terms: Vec<&str> = text.split_whitespace().collect();
    let mut found: Vec<&CataloguePackage> = catalogue
        .packages
        .values()
        .filter(|p| terms.iter().all(|t| term_matches(catalogue, p, t)))
        .filter(|p| !query.compatible_only || !p.installable_versions(app).is_empty())
        .filter(|p| !query.installed_only || installed.has_any(&p.id))
        .collect();
    found.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    found
}

// ── Status line ─────────────────────────────────────────────────────────

/// "just now", "5 minutes ago", "3 hours ago", "2 days ago".
pub fn describe_age(seconds: u64) -> String {
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match seconds {
        0..=59 => "just now".to_string(),
        60..=3_599 => plural(seconds / 60, "minute"),
        3_600..=86_399 => plural(seconds / 3_600, "hour"),
        _ => plural(seconds / 86_400, "day"),
    }
}

/// One line describing where the shown catalogue came from and how fresh
/// it is. Cached and out-of-date data are always labelled as such.
pub fn status_line(
    view: &CatalogueView,
    outcome: Option<&RefreshOutcome>,
    now: u64,
) -> (String, Tone) {
    let age = view
        .checked_at
        .map(|t| describe_age(now.saturating_sub(t)))
        .unwrap_or_default();
    let stale = if view.is_stale(now) {
        " It may be out of date."
    } else {
        ""
    };
    let cached = |prefix: &str, tone: Tone| -> (String, Tone) {
        if view.catalogue.is_some() {
            (
                format!("{prefix} Showing the cached catalogue (last checked {age}).{stale}"),
                tone,
            )
        } else {
            (
                format!("{prefix} No cached catalogue is available."),
                Tone::Error,
            )
        }
    };
    match outcome {
        None if view.catalogue.is_none() => match &view.cache_problem {
            Some(problem) => (
                format!("The cached catalogue could not be used ({problem}). Press Refresh."),
                Tone::Warning,
            ),
            None => ("No catalogue yet. Press Refresh.".to_string(), Tone::Normal),
        },
        None => (
            format!("Offline copy: cached catalogue, last checked {age}.{stale}"),
            if stale.is_empty() {
                Tone::Normal
            } else {
                Tone::Warning
            },
        ),
        Some(RefreshOutcome::Updated) => ("Catalogue updated just now.".to_string(), Tone::Good),
        Some(RefreshOutcome::UpdatedNotSaved(error)) => (
            format!("Catalogue updated, but it could not be saved for offline use: {error}"),
            Tone::Warning,
        ),
        Some(RefreshOutcome::Unchanged) => ("Catalogue is up to date.".to_string(), Tone::Good),
        Some(RefreshOutcome::FetchFailed(error)) => {
            cached(&format!("Could not refresh: {error}"), Tone::Warning)
        }
        Some(RefreshOutcome::Rejected(issues)) => cached(
            &format!(
                "The downloaded catalogue was refused ({}).",
                issues.first().map(String::as_str).unwrap_or("invalid")
            ),
            Tone::Error,
        ),
        Some(RefreshOutcome::UpdateRequired(format)) => cached(
            &format!("The catalogue uses format {format}; update Patchwork to read it."),
            Tone::Warning,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::index::InstalledVersion;
    use crate::library::marketplace::cache::tests::{EXAMPLE, location};
    use crate::library::marketplace::fetch::FetchError;
    use crate::library::marketplace::parse_catalogue;
    use crate::library::package::PackageManifest;

    fn example() -> Catalogue {
        parse_catalogue(EXAMPLE.as_bytes(), location().source()).unwrap()
    }

    fn app() -> semver::Version {
        semver::Version::new(0, 0, 10)
    }

    fn ids(found: &[&CataloguePackage]) -> Vec<String> {
        found.iter().map(|p| p.id.clone()).collect()
    }

    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            ..SearchQuery::default()
        }
    }

    fn installed(
        id: &str,
        version: &str,
        source: &str,
        integrity: Result<(), String>,
    ) -> InstalledPackage {
        let manifest = PackageManifest {
            format: 1,
            id: id.into(),
            name: "x".into(),
            version: version.into(),
            description: String::new(),
            author: String::new(),
            tags: Vec::new(),
            entry: "Gain.pwnode".into(),
            sha256: "ab".repeat(32),
            pwnode_version: 1,
            min_app_version: None,
            dependencies: Vec::new(),
            source: source.into(),
            published_at: 0,
            files: Default::default(),
            bundle_format: 0,
        };
        InstalledPackage {
            id: id.into(),
            versions: vec![InstalledVersion {
                manifest,
                version: semver::Version::parse(version).unwrap(),
                dir: Default::default(),
                entry_path: Default::default(),
                integrity,
            }],
        }
    }

    #[test]
    fn search_matches_name_id_publisher_and_tags() {
        let catalogue = example();
        let none = InstalledIndex::default();
        let find = |text: &str| ids(&search(&catalogue, &query(text), &app(), &none));
        assert_eq!(
            find(""),
            vec![
                "acme-audio.gain",
                "patchwork.lfo",
                "acme-audio.stereo-delay"
            ],
            "all, sorted by display name"
        );
        assert_eq!(
            find("STEREO"),
            vec!["acme-audio.stereo-delay"],
            "name, any case"
        );
        assert_eq!(find("patchwork.lfo"), vec!["patchwork.lfo"], "id");
        assert_eq!(find("acme audio").len(), 2, "publisher display name");
        assert_eq!(
            find("patchwork-nodes"),
            vec!["patchwork.lfo"],
            "publisher login"
        );
        assert_eq!(find("modulation"), vec!["patchwork.lfo"], "tag");
        assert_eq!(find("tag:audio").len(), 2, "exact tag");
        assert!(find("tag:aud").is_empty(), "tag: needs an exact tag");
        assert_eq!(
            find("acme utility"),
            vec!["acme-audio.gain"],
            "all terms must match"
        );
        assert!(find("nothing-like-this").is_empty());
    }

    #[test]
    fn search_filters_by_compatibility_and_installed_state() {
        let catalogue = example();
        let none = InstalledIndex::default();
        let compatible = SearchQuery {
            compatible_only: true,
            ..SearchQuery::default()
        };
        // gain has 1.2.0 installable; all packages qualify on 0.0.10…
        assert_eq!(search(&catalogue, &compatible, &app(), &none).len(), 3);
        // …and none on an app older than every min_patchwork.
        assert!(
            search(
                &catalogue,
                &compatible,
                &semver::Version::new(0, 0, 1),
                &none
            )
            .is_empty()
        );

        let index = InstalledIndex::from_packages(&[installed(
            "acme-audio.gain",
            "1.2.0",
            MARKETPLACE_SOURCE,
            Ok(()),
        )]);
        let installed_only = SearchQuery {
            installed_only: true,
            ..SearchQuery::default()
        };
        assert_eq!(
            ids(&search(&catalogue, &installed_only, &app(), &index)),
            vec!["acme-audio.gain"]
        );
    }

    #[test]
    fn installed_state_is_detected_from_the_library_scan() {
        let index = InstalledIndex::from_packages(&[
            installed("acme-audio.gain", "1.2.0", MARKETPLACE_SOURCE, Ok(())),
            installed("patchwork.lfo", "1.0.0", "local", Ok(())),
            installed(
                "acme-audio.stereo-delay",
                "1.0.0",
                MARKETPLACE_SOURCE,
                Err("checksum".into()),
            ),
        ]);
        let v = |s: &str| semver::Version::parse(s).unwrap();
        assert_eq!(
            install_label(index.state("acme-audio.gain", &v("1.2.0"))),
            ("Installed".to_string(), Tone::Good)
        );
        assert_eq!(
            install_label(index.state("acme-audio.gain", &v("2.0.0"))).0,
            "Not installed",
            "versions are exact"
        );
        assert_eq!(
            install_label(index.state("patchwork.lfo", &v("1.0.0"))).1,
            Tone::Warning
        );
        assert_eq!(
            install_label(index.state("acme-audio.stereo-delay", &v("1.0.0"))).1,
            Tone::Error
        );
        assert!(index.has_any("acme-audio.gain"));
        assert!(!index.has_any("acme-audio.other"));
    }

    #[test]
    fn compatibility_and_capability_labels() {
        let catalogue = example();
        let gain = catalogue.package("acme-audio.gain").unwrap();
        let label = |v: &str| {
            compatibility_label(&gain.versions[&semver::Version::parse(v).unwrap()], &app())
        };
        assert_eq!(label("1.2.0"), ("Compatible".to_string(), Tone::Good));
        assert_eq!(
            label("2.0.0"),
            ("Needs Patchwork 0.1.0 or newer".to_string(), Tone::Warning)
        );
        let (text, tone) = label("1.1.0");
        assert!(text.starts_with("Withdrawn: Clipped above 0 dB"));
        assert_eq!(tone, Tone::Error);

        let mut unknown = gain.versions[&semver::Version::new(1, 2, 0)].clone();
        unknown.capabilities = vec![DeclaredCapability::Unknown("teleport".into())];
        assert!(compatibility_label(&unknown, &app()).0.contains("teleport"));
        unknown.capabilities.clear();
        unknown.bundle_format = 99;
        assert!(
            compatibility_label(&unknown, &app())
                .0
                .contains("bundle format 99")
        );

        assert_eq!(
            capability_label(&DeclaredCapability::Known(Capability::Network)),
            "Network access"
        );
        assert!(capability_label(&DeclaredCapability::Unknown("x".into())).contains("'x'"));
    }

    #[test]
    fn status_lines_label_cached_and_stale_data() {
        let catalogue = Some(example());
        let cached = CatalogueView {
            catalogue: catalogue.clone(),
            fetched_at: Some(0),
            checked_at: Some(0),
            from_cache: true,
            cache_problem: None,
            catalogue_sha256: None,
        };
        let (text, tone) = status_line(&cached, None, 7_200);
        assert!(
            text.contains("cached catalogue, last checked 2 hours ago"),
            "{text}"
        );
        assert_eq!(tone, Tone::Normal);
        let (text, tone) = status_line(&cached, None, 30 * 86_400);
        assert!(text.contains("may be out of date"), "{text}");
        assert_eq!(tone, Tone::Warning);

        let (text, _) = status_line(
            &cached,
            Some(&RefreshOutcome::FetchFailed(FetchError::Timeout)),
            60,
        );
        assert!(
            text.contains("Could not refresh") && text.contains("Showing the cached catalogue")
        );
        let (text, tone) = status_line(
            &cached,
            Some(&RefreshOutcome::Rejected(vec!["bad".into()])),
            60,
        );
        assert!(text.contains("refused (bad)") && text.contains("cached"));
        assert_eq!(tone, Tone::Error);
        assert!(
            status_line(&cached, Some(&RefreshOutcome::UpdateRequired(2)), 60)
                .0
                .contains("update Patchwork")
        );

        let empty = CatalogueView::default();
        assert_eq!(
            status_line(&empty, None, 0).0,
            "No catalogue yet. Press Refresh."
        );
        let (text, tone) = status_line(
            &empty,
            Some(&RefreshOutcome::FetchFailed(FetchError::Status(404))),
            0,
        );
        assert!(text.contains("No cached catalogue"));
        assert_eq!(tone, Tone::Error);
        let damaged = CatalogueView {
            cache_problem: Some("checksum".into()),
            ..CatalogueView::default()
        };
        assert!(
            status_line(&damaged, None, 0)
                .0
                .contains("could not be used (checksum)")
        );

        assert_eq!(describe_age(30), "just now");
        assert_eq!(describe_age(60), "1 minute ago");
        assert_eq!(describe_age(3 * 86_400), "3 days ago");
    }
}
