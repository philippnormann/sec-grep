use serde::{Deserialize, Serialize};

/// A conference paper record as stored and queried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paper {
    pub dblp_key: String,
    pub venue: String,
    pub year: i32,
    pub title: String,
    /// Authors joined with ", " in signature order.
    pub authors: String,
    pub doi: Option<String>,
    pub url: Option<String>,
    #[serde(rename = "abstract")]
    pub abstract_text: Option<String>,
}

impl Paper {
    /// Generate a dblp-style BibTeX citation key, e.g. `NDSS:2021:smith`.
    pub fn cite_key(&self) -> String {
        let first_author = self
            .authors
            .split(',')
            .next()
            .unwrap_or("")
            .split_whitespace()
            .last()
            .unwrap_or("anon")
            .to_lowercase();
        let venue = self.venue.replace([' ', '&'], "").to_lowercase();
        format!("{venue}:{}:{first_author}", self.year)
    }

    /// Authors formatted for BibTeX (" and " separated).
    pub fn authors_bibtex(&self) -> String {
        self.authors
            .split(", ")
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" and ")
    }
}
