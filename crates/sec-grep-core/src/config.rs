//! Venue catalog, secrets, and filesystem paths.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Bundled default catalog, embedded at compile time.
const DEFAULT_VENUES_YAML: &str = include_str!("../venues.yaml");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Venue {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub dblp_stream: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub rank: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub abstract_source: Option<AbstractSource>,
    /// dblp publication type for this stream (`Inproceedings` for conferences,
    /// `Article` for journals like PoPETs).
    #[serde(default = "default_publ_type")]
    pub publ_type: DblpPublicationType,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AbstractSource {
    Acm,
    Ieee,
    Ndss,
    Springer,
    Usenix,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum DblpPublicationType {
    Inproceedings,
    Article,
}

impl DblpPublicationType {
    pub fn as_dblp_type(self) -> &'static str {
        match self {
            DblpPublicationType::Inproceedings => "Inproceedings",
            DblpPublicationType::Article => "Article",
        }
    }
}

fn default_publ_type() -> DblpPublicationType {
    DblpPublicationType::Inproceedings
}

impl Venue {
    /// Case-insensitive match against the venue id or any alias.
    pub fn matches(&self, needle: &str) -> bool {
        self.id.eq_ignore_ascii_case(needle)
            || self.aliases.iter().any(|a| a.eq_ignore_ascii_case(needle))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Defaults {
    #[serde(default = "default_min_year")]
    pub min_year: i32,
}

fn default_min_year() -> i32 {
    2000
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            min_year: default_min_year(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub venues: Vec<Venue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VenueFilter {
    #[default]
    All,
    Only(Vec<String>),
    Empty,
}

impl VenueFilter {
    pub fn from_active_ids(ids: Vec<String>) -> Self {
        if ids.is_empty() {
            Self::Empty
        } else {
            Self::Only(ids)
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    pub fn intersect(self, other: Self) -> Self {
        match (self, other) {
            (Self::Empty, _) | (_, Self::Empty) => Self::Empty,
            (Self::All, filter) | (filter, Self::All) => filter,
            (Self::Only(left), Self::Only(right)) => {
                let ids = left
                    .into_iter()
                    .filter(|id| right.iter().any(|other| other == id))
                    .collect::<Vec<_>>();
                Self::from_active_ids(ids)
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigOverride {
    defaults: Option<Defaults>,
    #[serde(default)]
    venues: Vec<Venue>,
}

impl ConfigOverride {
    fn from_yaml(yaml: &str) -> Result<Self> {
        Ok(serde_yaml::from_str(yaml)?)
    }
}

impl Config {
    /// Parse the embedded default catalog.
    pub fn defaults() -> Result<Self> {
        Self::from_yaml(DEFAULT_VENUES_YAML)
    }

    pub fn from_yaml(yaml: &str) -> Result<Self> {
        Ok(serde_yaml::from_str(yaml)?)
    }

    /// Load defaults, then deep-merge a user override file if it exists.
    /// User venues replace defaults sharing the same id (case-insensitive) and
    /// new ids are appended. A provided user `defaults` block overrides wholesale.
    pub fn load(user_override: Option<&Path>) -> Result<Self> {
        let mut cfg = Self::defaults()?;
        if let Some(path) = user_override {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let user = ConfigOverride::from_yaml(&text)?;
                    cfg.merge(user);
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(cfg)
    }

    fn merge(&mut self, user: ConfigOverride) {
        if let Some(defaults) = user.defaults {
            self.defaults = defaults;
        }
        for uv in user.venues {
            match self
                .venues
                .iter_mut()
                .find(|v| v.id.eq_ignore_ascii_case(&uv.id))
            {
                Some(existing) => *existing = uv,
                None => self.venues.push(uv),
            }
        }
    }

    /// Resolve a venue by id or alias.
    pub fn venue(&self, needle: &str) -> Option<&Venue> {
        self.venues.iter().find(|v| v.matches(needle))
    }

    /// Resolve venue selectors (id or alias) to canonical ids.
    /// Unknown selectors produce an error listing them.
    pub fn resolve_venues(&self, selectors: &[String]) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        let mut unknown = Vec::new();
        for sel in selectors {
            match self.venue(sel) {
                Some(v) => {
                    if seen.insert(v.id.clone()) {
                        ids.push(v.id.clone());
                    }
                }
                None => unknown.push(sel.clone()),
            }
        }
        if !unknown.is_empty() {
            return Err(Error::Config(format!(
                "unknown venue(s): {}",
                unknown.join(", ")
            )));
        }
        Ok(ids)
    }

    /// All configured venue ids in catalog order.
    pub fn all_venue_ids(&self) -> Vec<String> {
        self.venues.iter().map(|v| v.id.clone()).collect()
    }

    /// Resolve the union of explicit venue selectors, rank filters, and tag
    /// filters, preserving first-seen catalog/user order.
    pub fn resolve_venue_filter(
        &self,
        venues: &[String],
        ranks: &[String],
        tags: &[String],
    ) -> Result<VenueFilter> {
        if venues.is_empty() && ranks.is_empty() && tags.is_empty() {
            return Ok(VenueFilter::All);
        }
        let mut ids = Vec::new();
        let mut seen = HashSet::new();
        Self::extend_unique(&mut ids, &mut seen, self.resolve_venues(venues)?);
        Self::extend_unique(&mut ids, &mut seen, self.venues_by_rank(ranks));
        Self::extend_unique(&mut ids, &mut seen, self.venues_by_tag(tags));
        Ok(VenueFilter::from_active_ids(ids))
    }

    fn extend_unique(ids: &mut Vec<String>, seen: &mut HashSet<String>, new_ids: Vec<String>) {
        for id in new_ids {
            if seen.insert(id.clone()) {
                ids.push(id);
            }
        }
    }

    /// Venue ids matching the given rank labels (case-insensitive).
    pub fn venues_by_rank(&self, ranks: &[String]) -> Vec<String> {
        if ranks.is_empty() {
            return Vec::new();
        }
        self.venues
            .iter()
            .filter(|v| {
                v.rank
                    .as_deref()
                    .is_some_and(|r| ranks.iter().any(|q| q.eq_ignore_ascii_case(r)))
            })
            .map(|v| v.id.clone())
            .collect()
    }

    /// Venue ids carrying any of the given tags (case-insensitive).
    pub fn venues_by_tag(&self, tags: &[String]) -> Vec<String> {
        if tags.is_empty() {
            return Vec::new();
        }
        self.venues
            .iter()
            .filter(|v| {
                v.tags
                    .iter()
                    .any(|t| tags.iter().any(|q| q.eq_ignore_ascii_case(t)))
            })
            .map(|v| v.id.clone())
            .collect()
    }
}

/// API keys, read from the environment (optionally seeded from a `.env` file).
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub openalex_api_key: Option<String>,
    pub semantic_scholar_key: Option<String>,
}

impl Secrets {
    /// Best-effort load: source `.env` if present, then read known vars.
    pub fn load() -> Self {
        let _ = dotenvy::dotenv();
        Self {
            openalex_api_key: non_empty_env("OPENALEX_API_KEY"),
            semantic_scholar_key: non_empty_env("SEMANTIC_SCHOLAR_S2_KEY"),
        }
    }
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

/// Resolved on-disk locations for the database and user config.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "sec-grep")
            .ok_or_else(|| Error::Config("cannot determine home directory".into()))?;
        Ok(Self {
            data_dir: dirs.data_dir().to_path_buf(),
            config_dir: dirs.config_dir().to_path_buf(),
        })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("papers.db")
    }

    pub fn user_venues_path(&self) -> PathBuf {
        self.config_dir.join("venues.yaml")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.config_dir)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_catalog_parses_and_has_top4() {
        let cfg = Config::defaults().unwrap();
        assert!(cfg.venues.len() >= 4);
        for v in ["NDSS", "USENIX-SEC", "SP", "CCS"] {
            assert!(cfg.venue(v).is_some(), "missing {v}");
        }
        assert_eq!(cfg.defaults.min_year, 2000);
    }

    #[test]
    fn lookup_by_alias_is_case_insensitive() {
        let cfg = Config::defaults().unwrap();
        assert_eq!(cfg.venue("oakland").unwrap().id, "SP");
        assert_eq!(cfg.venue("USENIX").unwrap().id, "USENIX-SEC");
        assert_eq!(cfg.venue("Ndss").unwrap().id, "NDSS");
        assert!(cfg.venue("nope").is_none());
    }

    #[test]
    fn merge_replaces_existing_and_adds_new() {
        let mut cfg = Config::defaults().unwrap();
        let before = cfg.venues.len();
        let user = ConfigOverride::from_yaml(
            r#"
defaults:
  min_year: 2015
venues:
  - id: NDSS
    dblp_stream: conf/ndss
    rank: B
    aliases: [ndss]
  - id: MYVENUE
    dblp_stream: conf/myv
    aliases: [myv]
"#,
        )
        .unwrap();
        cfg.merge(user);
        assert_eq!(cfg.defaults.min_year, 2015);
        assert_eq!(cfg.venue("NDSS").unwrap().rank.as_deref(), Some("B"));
        assert_eq!(cfg.venues.len(), before + 1);
        assert!(cfg.venue("myv").is_some());
    }

    #[test]
    fn merge_without_defaults_preserves_existing_defaults() {
        let mut cfg = Config::defaults().unwrap();
        cfg.defaults.min_year = 1997;
        let user = ConfigOverride::from_yaml(
            r#"
venues:
  - id: MYVENUE
    dblp_stream: conf/myv
"#,
        )
        .unwrap();
        cfg.merge(user);
        assert_eq!(cfg.defaults.min_year, 1997);
        assert!(cfg.venue("MYVENUE").is_some());
    }

    #[test]
    fn rank_and_tag_filters() {
        let cfg = Config::defaults().unwrap();
        let astar = cfg.venues_by_rank(&["a*".into()]);
        assert!(astar.contains(&"NDSS".to_string()));
        let crypto = cfg.venues_by_tag(&["crypto".into()]);
        assert!(crypto.contains(&"CCS".to_string()));
    }

    #[test]
    fn resolve_venues_reports_unknown() {
        let cfg = Config::defaults().unwrap();
        let ok = cfg
            .resolve_venues(&["ndss".into(), "oakland".into()])
            .unwrap();
        assert_eq!(ok, vec!["NDSS".to_string(), "SP".to_string()]);
        assert!(cfg.resolve_venues(&["bogus".into()]).is_err());
    }

    #[test]
    fn combined_venue_filter_deduplicates_in_order() {
        let cfg = Config::defaults().unwrap();
        let filter = cfg
            .resolve_venue_filter(&["ndss".into()], &["A*".into()], &["crypto".into()])
            .unwrap();
        let VenueFilter::Only(ids) = filter else {
            panic!("expected active venue filter");
        };
        assert_eq!(ids.first().map(String::as_str), Some("NDSS"));
        assert_eq!(ids.iter().filter(|id| id.as_str() == "NDSS").count(), 1);
        assert!(ids.contains(&"CCS".to_string()));
    }

    #[test]
    fn combined_venue_filter_preserves_active_empty_filter() {
        let cfg = Config::defaults().unwrap();
        let filter = cfg
            .resolve_venue_filter(&[], &["does-not-exist".into()], &[])
            .unwrap();
        assert_eq!(filter, VenueFilter::Empty);
    }
}
