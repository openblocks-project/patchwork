//! P4.3.1A: the configured catalogue location.
//!
//! There is no built-in production source: the user (or a build that ships
//! one) configures `owner/repo@branch` explicitly in Settings. The
//! catalogue is always read from
//! `https://raw.githubusercontent.com/<owner>/<repo>/<branch>/index.json`,
//! and every artifact URL inside it must point into that same repository
//! (P4.3.0 rules).

use super::{CatalogueSource, RAW_HOST, parse_https_url};
use crate::settings::MarketplaceSettings;

pub const INDEX_FILE: &str = "index.json";
pub const DEFAULT_BRANCH: &str = "main";

/// A validated catalogue location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogueLocation {
    source: CatalogueSource,
    branch: String,
}

/// A branch name usable as one URL path segment.
pub fn validate_branch(branch: &str) -> Result<(), String> {
    let ok = !branch.is_empty()
        && branch.len() <= 100
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !branch.starts_with(['.', '-'])
        && !branch.contains("..")
        && !branch.ends_with(".lock");
    if ok {
        Ok(())
    } else {
        Err(format!(
            "'{branch}' is not a supported branch name (letters, digits, '.', '_' and '-')."
        ))
    }
}

impl CatalogueLocation {
    /// Validate `owner/repo@branch` (an empty branch means `main`) and the
    /// resulting catalogue URL.
    pub fn new(owner: &str, repo: &str, branch: &str) -> Result<Self, String> {
        let source = CatalogueSource::new(owner.trim(), repo.trim())?;
        let branch = match branch.trim() {
            "" => DEFAULT_BRANCH,
            other => other,
        };
        validate_branch(branch)?;
        let location = Self {
            source,
            branch: branch.to_string(),
        };
        let url = location.index_url();
        let parsed = parse_https_url(&url).map_err(|e| format!("The catalogue URL {e}."))?;
        if parsed.host != RAW_HOST || parsed.has_query_or_fragment {
            return Err(format!("{url} is not an allowed catalogue address."));
        }
        Ok(location)
    }

    /// `Ok(None)` when no source is configured.
    pub fn from_settings(settings: &MarketplaceSettings) -> Result<Option<Self>, String> {
        if settings.owner.trim().is_empty() && settings.repo.trim().is_empty() {
            return Ok(None);
        }
        Self::new(&settings.owner, &settings.repo, &settings.branch).map(Some)
    }

    pub fn source(&self) -> &CatalogueSource {
        &self.source
    }

    pub fn branch(&self) -> &str {
        &self.branch
    }

    pub fn index_url(&self) -> String {
        format!(
            "https://{RAW_HOST}/{}/{}/{}/{INDEX_FILE}",
            self.source.owner(),
            self.source.repo(),
            self.branch
        )
    }

    /// `owner/repo@branch`.
    pub fn label(&self) -> String {
        format!(
            "{}/{}@{}",
            self.source.owner(),
            self.source.repo(),
            self.branch
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_are_validated_and_build_raw_urls() {
        let location = CatalogueLocation::new(" acme ", "nodes", "").unwrap();
        assert_eq!(location.branch(), "main");
        assert_eq!(
            location.index_url(),
            "https://raw.githubusercontent.com/acme/nodes/main/index.json"
        );
        assert_eq!(location.label(), "acme/nodes@main");
        assert!(CatalogueLocation::new("acme", "nodes", "release-1.x").is_ok());

        for (owner, repo, branch) in [
            ("", "nodes", "main"),
            ("acme", "", "main"),
            ("ac me", "nodes", "main"),
            ("acme", "../x", "main"),
            ("acme", "nodes", "../main"),
            ("acme", "nodes", "feature/x"),
            ("acme", "nodes", "main?x=1"),
            ("acme", "nodes", "main#x"),
            ("acme", "nodes", "-main"),
            ("acme", "nodes", "main.lock"),
            ("acme@evil.com", "nodes", "main"),
        ] {
            assert!(
                CatalogueLocation::new(owner, repo, branch).is_err(),
                "{owner}/{repo}@{branch}"
            );
        }
    }

    #[test]
    fn nothing_is_configured_by_default() {
        let settings = MarketplaceSettings::default();
        assert_eq!(CatalogueLocation::from_settings(&settings), Ok(None));
        let settings = MarketplaceSettings {
            owner: "acme".into(),
            repo: "nodes".into(),
            branch: String::new(),
        };
        assert_eq!(
            CatalogueLocation::from_settings(&settings)
                .unwrap()
                .unwrap()
                .label(),
            "acme/nodes@main"
        );
        let settings = MarketplaceSettings {
            owner: "acme".into(),
            repo: String::new(),
            branch: String::new(),
        };
        assert!(CatalogueLocation::from_settings(&settings).is_err());
    }
}
