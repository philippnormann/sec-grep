//! End-to-end pipeline: dblp JSON -> upsert -> parse query -> search -> render.

use sec_grep_core::db::{Database, Search};
use sec_grep_core::output::{render, Format};
use sec_grep_core::query::YearRange;
use sec_grep_core::{dblp, query};
use serde_json::json;

fn fixture() -> serde_json::Value {
    let row = |publ: &str, title: &str, year: &str, ord: &str, author: &str, url: &str| {
        json!({
            "publ": {"value": publ},
            "title": {"value": title},
            "year": {"value": year},
            "ordinal": {"value": ord},
            "author": {"value": author},
            "url": {"value": url},
        })
    };
    json!({
        "results": {"bindings": [
            row("p1", "Fuzzing the Linux kernel", "2024", "1", "Alice Smith 0001", "https://doi.org/10.1/a"),
            row("p1", "Fuzzing the Linux kernel", "2024", "2", "Bob Jones", "https://doi.org/10.1/a"),
            row("p2", "Side channel attacks on caches", "2023", "1", "Carol Chen", "https://doi.org/10.1/b"),
            row("p3", "A formal model of TLS", "2022", "1", "Dave Diaz", "https://doi.org/10.1/c"),
        ]}
    })
}

#[test]
fn full_pipeline() {
    // ingest
    let papers = dblp::parse_results(&fixture(), "NDSS");
    assert_eq!(papers.len(), 3);
    let mut db = Database::open_in_memory().unwrap();
    let n = db.upsert_papers(&papers).unwrap();
    assert_eq!(n, 3);
    assert_eq!(db.count().unwrap(), 3);

    // boolean full-text search
    let hits = db
        .search(&Search {
            fts: query::fts("fuzzing AND kernel").unwrap(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].title, "Fuzzing the Linux kernel");
    assert_eq!(hits[0].authors, "Alice Smith, Bob Jones");
    assert_eq!(hits[0].doi.as_deref(), Some("10.1/a"));

    // phrase search
    let hits = db
        .search(&Search {
            fts: query::fts("\"side channel\"").unwrap(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].dblp_key, "p2");

    // year filter
    let recent = db
        .search(&Search {
            year_ranges: vec![YearRange::new(Some(2023), None).unwrap()],
            ..Default::default()
        })
        .unwrap();
    assert_eq!(recent.len(), 2);

    // bibtex render
    let bib = render(&papers, Format::Bibtex, None).unwrap();
    assert!(bib.contains("@inproceedings{ndss:2024:smith,"));
    assert!(bib.contains("author    = {Alice Smith and Bob Jones}"));
}

#[test]
fn idempotent_reingest() {
    let papers = dblp::parse_results(&fixture(), "NDSS");
    let mut db = Database::open_in_memory().unwrap();
    db.upsert_papers(&papers).unwrap();
    db.upsert_papers(&papers).unwrap();
    assert_eq!(db.count().unwrap(), 3);
}
