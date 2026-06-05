pub mod abstracts;
pub mod config;
pub mod db;
pub mod dblp;
pub mod output;
pub mod query;

mod error;
mod model;

pub use error::{Error, Result};
pub use model::Paper;

use crate::db::{Search, Sort};
use crate::config::Config;

/// Build a search query from a raw query string and options.
///
/// This is the main entry point for constructing searches, used by both
/// the CLI and web interface.
pub fn build_search(
    raw_query: &str,
    config: &Config,
    options: SearchOptions,
) -> Result<Search> {
    let parsed = query::parse(raw_query)?;
    let mut venue_selectors = parsed.venue_selectors;
    venue_selectors.extend_from_slice(options.venues);
    let mut rank_selectors = parsed.rank_selectors;
    rank_selectors.extend_from_slice(options.ranks);
    let mut tag_selectors = parsed.tag_selectors;
    tag_selectors.extend_from_slice(options.tags);
    let mut year_ranges = parsed.year_ranges;
    year_ranges.extend_from_slice(options.years);
    let venue_filter =
        config.resolve_venue_filter(&venue_selectors, &rank_selectors, &tag_selectors)?;

    Ok(Search {
        fts: parsed.fts,
        venue_filter,
        doi_terms: parsed.doi_terms,
        year_ranges,
        sort: options.sort,
        limit: options.limit,
        offset: options.offset,
    })
}

/// Options for building a search query.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions<'a> {
    pub venues: &'a [String],
    pub ranks: &'a [String],
    pub tags: &'a [String],
    pub years: &'a [query::YearRange],
    pub sort: Sort,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}
