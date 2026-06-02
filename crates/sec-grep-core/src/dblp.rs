//! dblp SPARQL ingestion: fetch inproceedings for a venue stream.

use std::{collections::BTreeMap, time::Duration};

use serde_json::Value;

use crate::config::{DblpPublicationType, Venue};
use crate::{Paper, Result};

pub const DEFAULT_ENDPOINT: &str = "https://sparql.dblp.org/sparql";

/// Build the SPARQL query for a single venue stream, bounded by year.
/// `publ_type` is the dblp rdf:type (`Inproceedings` or `Article`).
pub fn build_query(
    stream: &str,
    publ_type: DblpPublicationType,
    min_year: i32,
    max_year: i32,
) -> String {
    let publ_type = publ_type.as_dblp_type();
    format!(
        r#"PREFIX dblp: <https://dblp.org/rdf/schema#>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
SELECT ?publ ?title ?year ?ordinal ?author ?url WHERE {{
  ?publ dblp:publishedInStream <https://dblp.org/streams/{stream}> ;
        rdf:type dblp:{publ_type} ;
        dblp:title ?title ;
        dblp:yearOfPublication ?year ;
        dblp:hasSignature ?sig .
  ?sig dblp:signatureDblpName ?author ;
       dblp:signatureOrdinal ?ordinal .
  OPTIONAL {{ ?publ dblp:primaryDocumentPage ?url . }}
  FILTER(?year >= "{min_year}"^^xsd:gYear && ?year <= "{max_year}"^^xsd:gYear)
}}
ORDER BY ?publ xsd:integer(?ordinal)"#
    )
}

/// dblp disambiguates homonyms with a trailing 4-digit id ("Name 0001");
/// strip it to keep the display name.
fn clean_author(author: &str) -> String {
    let trimmed = author.trim();
    if let Some((name, suffix)) = trimmed.rsplit_once(' ') {
        if suffix.len() == 4 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return name.trim_end().to_string();
        }
    }
    trimmed.to_string()
}

/// Extract a DOI from a publisher URL if it is a doi.org / DOI link.
fn extract_doi(url: &str) -> Option<String> {
    let lower = url.to_lowercase();
    for marker in ["doi.org/", "doi/"] {
        if let Some(idx) = lower.find(marker) {
            let doi = &url[idx + marker.len()..];
            if doi.starts_with("10.") {
                return Some(doi.trim_end_matches('/').to_string());
            }
        }
    }
    None
}

fn binding_str<'a>(binding: &'a Value, key: &str) -> Option<&'a str> {
    binding.get(key)?.get("value")?.as_str()
}

/// Parse a SPARQL JSON results document into deduplicated papers for a venue.
pub fn parse_results(json: &Value, venue_id: &str) -> Vec<Paper> {
    // Preserve first-seen ordering of publications.
    let mut order: Vec<String> = Vec::new();
    let mut by_publ: BTreeMap<String, (Paper, Vec<(i64, String)>)> = BTreeMap::new();

    let bindings = json
        .get("results")
        .and_then(|r| r.get("bindings"))
        .and_then(|b| b.as_array())
        .map(Vec::as_slice)
        .unwrap_or_default();

    for b in bindings {
        let Some(publ) = binding_str(b, "publ") else {
            continue;
        };
        let entry = by_publ.entry(publ.to_string()).or_insert_with(|| {
            order.push(publ.to_string());
            let title = binding_str(b, "title").unwrap_or_default().to_string();
            let year = binding_str(b, "year")
                .and_then(|y| y.parse::<i32>().ok())
                .unwrap_or(0);
            let url = binding_str(b, "url").map(|s| s.to_string());
            let doi = url.as_deref().and_then(extract_doi);
            (
                Paper {
                    dblp_key: publ.to_string(),
                    venue: venue_id.to_string(),
                    year,
                    title,
                    authors: String::new(),
                    doi,
                    url,
                    abstract_text: None,
                },
                Vec::new(),
            )
        });
        if let (Some(author), Some(ordinal)) = (binding_str(b, "author"), binding_str(b, "ordinal"))
        {
            let ord = ordinal.parse::<i64>().unwrap_or(i64::MAX);
            entry.1.push((ord, clean_author(author)));
        }
    }

    order
        .into_iter()
        .filter_map(|k| by_publ.remove(&k))
        .map(|(mut paper, mut authors)| {
            authors.sort_by_key(|(ord, _)| *ord);
            paper.authors = authors
                .into_iter()
                .map(|(_, a)| a)
                .collect::<Vec<_>>()
                .join(", ");
            paper
        })
        .collect()
}

/// SPARQL client over the dblp endpoint.
pub struct Dblp {
    endpoint: String,
    client: reqwest::Client,
}

impl Default for Dblp {
    fn default() -> Self {
        Self::new(DEFAULT_ENDPOINT)
    }
}

impl Dblp {
    pub fn new(endpoint: &str) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("sec-grep/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            endpoint: endpoint.to_string(),
            client,
        }
    }

    /// Fetch and parse all papers for a venue between the given years.
    pub async fn fetch_venue(
        &self,
        venue: &Venue,
        min_year: i32,
        max_year: i32,
    ) -> Result<Vec<Paper>> {
        let query = build_query(&venue.dblp_stream, venue.publ_type, min_year, max_year);
        let resp = self
            .client
            .get(&self.endpoint)
            .query(&[("query", query.as_str())])
            .header("Accept", "application/sparql-results+json")
            .send()
            .await?
            .error_for_status()?;
        let json: Value = resp.json().await?;
        Ok(parse_results(&json, &venue.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(
        publ: &str,
        title: &str,
        year: &str,
        ord: &str,
        author: &str,
        url: Option<&str>,
    ) -> Value {
        let mut b = json!({
            "publ": {"value": publ},
            "title": {"value": title},
            "year": {"value": year},
            "ordinal": {"value": ord},
            "author": {"value": author},
        });
        if let Some(u) = url {
            b["url"] = json!({"value": u});
        }
        b
    }

    #[test]
    fn query_contains_stream_and_years() {
        let q = build_query("conf/ndss", DblpPublicationType::Inproceedings, 2000, 2025);
        assert!(q.contains("https://dblp.org/streams/conf/ndss"));
        assert!(q.contains("rdf:type dblp:Inproceedings"));
        assert!(q.contains("\"2000\"^^xsd:gYear"));
        assert!(q.contains("\"2025\"^^xsd:gYear"));
    }

    #[test]
    fn clean_author_strips_homonym_suffix() {
        assert_eq!(clean_author("Jane Doe 0001"), "Jane Doe");
        assert_eq!(clean_author("Jane Doe"), "Jane Doe");
        assert_eq!(clean_author("R2 D2"), "R2 D2");
    }

    #[test]
    fn extract_doi_from_urls() {
        assert_eq!(
            extract_doi("https://doi.org/10.1145/123.456"),
            Some("10.1145/123.456".to_string())
        );
        assert_eq!(
            extract_doi("https://dl.acm.org/doi/10.1145/abc"),
            Some("10.1145/abc".to_string())
        );
        assert_eq!(extract_doi("https://www.usenix.org/x"), None);
    }

    #[test]
    fn parse_groups_authors_by_ordinal() {
        let doc = json!({
            "results": {"bindings": [
                binding("p1", "Title One", "2020", "2", "Bob B", Some("https://doi.org/10.1/a")),
                binding("p1", "Title One", "2020", "1", "Alice A 0001", Some("https://doi.org/10.1/a")),
                binding("p2", "Title Two", "2021", "1", "Carol C", None),
            ]}
        });
        let papers = parse_results(&doc, "NDSS");
        assert_eq!(papers.len(), 2);
        let p1 = papers.iter().find(|p| p.dblp_key == "p1").unwrap();
        assert_eq!(p1.authors, "Alice A, Bob B");
        assert_eq!(p1.venue, "NDSS");
        assert_eq!(p1.year, 2020);
        assert_eq!(p1.doi.as_deref(), Some("10.1/a"));
        let p2 = papers.iter().find(|p| p.dblp_key == "p2").unwrap();
        assert_eq!(p2.authors, "Carol C");
        assert!(p2.url.is_none());
    }

    #[test]
    fn parse_empty_results() {
        let doc = json!({"results": {"bindings": []}});
        assert!(parse_results(&doc, "NDSS").is_empty());
    }
}
