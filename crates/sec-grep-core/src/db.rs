//! SQLite storage with an FTS5 full-text index over papers.

use std::path::Path;

use rusqlite::{params_from_iter, types::Value, Connection, OpenFlags, OptionalExtension, Row};

use crate::config::VenueFilter;
use crate::query::YearRange;
use crate::{Paper, Result};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS papers (
    id         INTEGER PRIMARY KEY,
    dblp_key   TEXT UNIQUE NOT NULL,
    venue      TEXT NOT NULL,
    year       INTEGER NOT NULL,
    title      TEXT NOT NULL,
    authors    TEXT NOT NULL,
    doi        TEXT,
    url        TEXT,
    abstract   TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_papers_venue_year ON papers(venue, year);
CREATE INDEX IF NOT EXISTS idx_papers_year_venue ON papers(year DESC, venue ASC, id);

CREATE VIRTUAL TABLE IF NOT EXISTS papers_fts USING fts5(
    title, authors, abstract,
    content = 'papers',
    content_rowid = 'id',
    tokenize = 'porter unicode61'
);

CREATE TRIGGER IF NOT EXISTS papers_ai AFTER INSERT ON papers BEGIN
    INSERT INTO papers_fts(rowid, title, authors, abstract)
    VALUES (new.id, new.title, new.authors, new.abstract);
END;

CREATE TRIGGER IF NOT EXISTS papers_ad AFTER DELETE ON papers BEGIN
    INSERT INTO papers_fts(papers_fts, rowid, title, authors, abstract)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract);
END;

CREATE TRIGGER IF NOT EXISTS papers_au AFTER UPDATE ON papers BEGIN
    INSERT INTO papers_fts(papers_fts, rowid, title, authors, abstract)
    VALUES ('delete', old.id, old.title, old.authors, old.abstract);
    INSERT INTO papers_fts(rowid, title, authors, abstract)
    VALUES (new.id, new.title, new.authors, new.abstract);
END;
"#;

const PAPER_COLUMNS_WITH_ALIAS: &str =
    "p.dblp_key, p.venue, p.year, p.title, p.authors, p.doi, p.url, p.abstract";

/// How to order search results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sort {
    /// BM25 relevance when a full-text query is present, else by year.
    #[default]
    Relevance,
    Year,
    Venue,
}

/// A compiled search request. `fts` is an FTS5 MATCH expression (already
/// validated by the query module); the rest are SQL-side metadata filters.
#[derive(Debug, Clone, Default)]
pub struct Search {
    pub fts: Option<String>,
    pub venue_filter: VenueFilter,
    pub doi_terms: Vec<String>,
    pub year_ranges: Vec<YearRange>,
    pub sort: Sort,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

pub struct Database {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct MissingPaper {
    pub id: i64,
    pub paper: Paper,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_existing(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM papers", [], |r| r.get(0))?)
    }

    /// Insert or update papers keyed by `dblp_key`. Returns rows affected.
    pub fn upsert_papers(&mut self, papers: &[Paper]) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut n = 0;
        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO papers (dblp_key, venue, year, title, authors, doi, url, abstract, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, datetime('now'))
                ON CONFLICT(dblp_key) DO UPDATE SET
                    venue = excluded.venue,
                    year = excluded.year,
                    title = excluded.title,
                    authors = excluded.authors,
                    doi = excluded.doi,
                    url = excluded.url,
                    abstract = COALESCE(excluded.abstract, papers.abstract),
                    updated_at = datetime('now')
                WHERE
                    papers.venue IS NOT excluded.venue OR
                    papers.year IS NOT excluded.year OR
                    papers.title IS NOT excluded.title OR
                    papers.authors IS NOT excluded.authors OR
                    papers.doi IS NOT excluded.doi OR
                    papers.url IS NOT excluded.url OR
                    (excluded.abstract IS NOT NULL AND papers.abstract IS NOT excluded.abstract)
                "#,
            )?;
            for p in papers {
                n += stmt.execute(rusqlite::params![
                    p.dblp_key,
                    p.venue,
                    p.year,
                    p.title,
                    p.authors,
                    p.doi,
                    p.url,
                    p.abstract_text,
                ])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Update only the abstract for a given dblp key.
    pub fn set_abstract(&mut self, dblp_key: &str, abstract_text: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE papers SET abstract = ?2, updated_at = datetime('now') WHERE dblp_key = ?1",
            rusqlite::params![dblp_key, abstract_text],
        )?;
        Ok(())
    }

    pub fn papers_missing_abstract_batch(
        &self,
        venues: &[String],
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<MissingPaper>> {
        let mut parts = missing_abstract_parts(venues, Some(after_id));
        let next = parts.args.len() + 1;
        let sql = format!(
            "SELECT p.id, {PAPER_COLUMNS_WITH_ALIAS} FROM papers p {} \
             ORDER BY p.id ASC LIMIT ?{next}",
            parts.where_sql
        );
        parts.args.push((limit as i64).into());
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(parts.args.iter()), row_to_missing_paper)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn count_missing_abstracts(&self, venues: &[String]) -> Result<usize> {
        let parts = missing_abstract_parts(venues, None);
        let sql = format!("SELECT COUNT(*) FROM papers p {}", parts.where_sql);
        let count: i64 = self
            .conn
            .query_row(&sql, params_from_iter(parts.args.iter()), |r| r.get(0))?;
        Ok(count as usize)
    }

    pub fn search(&self, q: &Search) -> Result<Vec<Paper>> {
        if q.venue_filter.is_empty() {
            return Ok(Vec::new());
        }
        let mut parts = search_query_parts(q);
        let order = match q.sort {
            Sort::Relevance if q.fts.is_some() => {
                "ORDER BY bm25(papers_fts), p.year DESC".to_string()
            }
            Sort::Venue => "ORDER BY p.venue ASC, p.year DESC".to_string(),
            _ => "ORDER BY p.year DESC, p.venue ASC".to_string(),
        };

        let mut sql = format!(
            "SELECT {PAPER_COLUMNS_WITH_ALIAS} FROM {} {} {order}",
            parts.from, parts.where_sql
        );
        if let Some(n) = q.limit {
            let next = parts.args.len() + 1;
            sql.push_str(&format!(" LIMIT ?{next}"));
            parts.args.push((n as i64).into());
        }
        if let Some(n) = q.offset {
            if q.limit.is_none() {
                sql.push_str(" LIMIT -1");
            }
            let next = parts.args.len() + 1;
            sql.push_str(&format!(" OFFSET ?{next}"));
            parts.args.push((n as i64).into());
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(parts.args.iter()), row_to_paper)?
            .collect::<std::result::Result<_, _>>()?;
        Ok(rows)
    }

    pub fn search_count(&self, q: &Search) -> Result<usize> {
        if q.venue_filter.is_empty() {
            return Ok(0);
        }
        let parts = search_query_parts(q);
        let sql = format!("SELECT COUNT(*) FROM {} {}", parts.from, parts.where_sql);
        let count: i64 = self
            .conn
            .query_row(&sql, params_from_iter(parts.args.iter()), |r| r.get(0))?;
        Ok(count as usize)
    }

    pub fn get_by_key(&self, dblp_key: &str) -> Result<Option<Paper>> {
        Ok(self
            .conn
            .query_row(
                "SELECT dblp_key, venue, year, title, authors, doi, url, abstract \
                 FROM papers WHERE dblp_key = ?1",
                [dblp_key],
                row_to_paper,
            )
            .optional()?)
    }
}

fn in_clause(column: &str, start: usize, value_count: usize) -> String {
    format!("{column} IN ({})", placeholders(start, value_count))
}

fn placeholders(start: usize, count: usize) -> String {
    (start..start + count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn append_string_args(args: &mut Vec<Value>, values: &[String]) {
    args.extend(values.iter().cloned().map(Value::from));
}

fn year_ranges_clause(column: &str, start: usize, ranges: &[YearRange]) -> String {
    let mut next = start;
    let clauses = ranges
        .iter()
        .map(|range| match range.bounds() {
            (Some(min), Some(max)) if min == max => {
                let placeholder = next;
                next += 1;
                format!("{column} = ?{placeholder}")
            }
            (Some(_), Some(_)) => {
                let min_placeholder = next;
                let max_placeholder = next + 1;
                next += 2;
                format!("({column} >= ?{min_placeholder} AND {column} <= ?{max_placeholder})")
            }
            (Some(_), None) => {
                let placeholder = next;
                next += 1;
                format!("{column} >= ?{placeholder}")
            }
            (None, Some(_)) => {
                let placeholder = next;
                next += 1;
                format!("{column} <= ?{placeholder}")
            }
            (None, None) => unreachable!("year parser rejects empty ranges"),
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("({clauses})")
}

fn append_year_range_args(args: &mut Vec<Value>, ranges: &[YearRange]) {
    for range in ranges {
        match range.bounds() {
            (Some(min), Some(max)) if min == max => args.push((min as i64).into()),
            (Some(min), Some(max)) => {
                args.push((min as i64).into());
                args.push((max as i64).into());
            }
            (Some(min), None) => args.push((min as i64).into()),
            (None, Some(max)) => args.push((max as i64).into()),
            (None, None) => unreachable!("year parser rejects empty ranges"),
        }
    }
}

struct SearchQueryParts {
    from: String,
    where_sql: String,
    args: Vec<Value>,
}

fn search_query_parts(q: &Search) -> SearchQueryParts {
    let mut args: Vec<Value> = Vec::new();
    let mut where_clauses: Vec<String> = Vec::new();

    let from = if let Some(fts) = &q.fts {
        args.push(fts.clone().into());
        where_clauses.push("papers_fts MATCH ?1".to_string());
        "papers_fts f JOIN papers p ON p.id = f.rowid".to_string()
    } else {
        "papers p".to_string()
    };

    if let VenueFilter::Only(venues) = &q.venue_filter {
        where_clauses.push(in_clause("p.venue", args.len() + 1, venues.len()));
        append_string_args(&mut args, venues);
    }
    if !q.year_ranges.is_empty() {
        where_clauses.push(year_ranges_clause("p.year", args.len() + 1, &q.year_ranges));
        append_year_range_args(&mut args, &q.year_ranges);
    }
    if !q.doi_terms.is_empty() {
        where_clauses.push(like_any_clause("p.doi", args.len() + 1, q.doi_terms.len()));
        append_like_args(&mut args, &q.doi_terms);
    }

    let where_sql = if where_clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", where_clauses.join(" AND "))
    };

    SearchQueryParts {
        from,
        where_sql,
        args,
    }
}

fn like_any_clause(column: &str, start: usize, value_count: usize) -> String {
    let clauses = (start..start + value_count)
        .map(|i| format!("{column} LIKE ?{i} ESCAPE '\\'"))
        .collect::<Vec<_>>()
        .join(" OR ");
    format!("({clauses})")
}

fn append_like_args(args: &mut Vec<Value>, values: &[String]) {
    args.extend(
        values
            .iter()
            .map(|value| Value::from(format!("%{}%", escape_like(value)))),
    );
}

fn escape_like(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn missing_abstract_parts(venues: &[String], after_id: Option<i64>) -> SearchQueryParts {
    let mut args: Vec<Value> = Vec::new();
    let mut where_clauses = vec![
        "p.url IS NOT NULL".to_string(),
        "(p.abstract IS NULL OR p.abstract = '')".to_string(),
    ];
    if !venues.is_empty() {
        where_clauses.push(in_clause("p.venue", args.len() + 1, venues.len()));
        append_string_args(&mut args, venues);
    }
    if let Some(after_id) = after_id {
        let next = args.len() + 1;
        where_clauses.push(format!("p.id > ?{next}"));
        args.push(after_id.into());
    }

    SearchQueryParts {
        from: String::new(),
        where_sql: format!("WHERE {}", where_clauses.join(" AND ")),
        args,
    }
}

fn row_to_paper(row: &Row) -> rusqlite::Result<Paper> {
    Ok(Paper {
        dblp_key: row.get(0)?,
        venue: row.get(1)?,
        year: row.get(2)?,
        title: row.get(3)?,
        authors: row.get(4)?,
        doi: row.get(5)?,
        url: row.get(6)?,
        abstract_text: row.get(7)?,
    })
}

fn row_to_missing_paper(row: &Row) -> rusqlite::Result<MissingPaper> {
    Ok(MissingPaper {
        id: row.get(0)?,
        paper: Paper {
            dblp_key: row.get(1)?,
            venue: row.get(2)?,
            year: row.get(3)?,
            title: row.get(4)?,
            authors: row.get(5)?,
            doi: row.get(6)?,
            url: row.get(7)?,
            abstract_text: row.get(8)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper(key: &str, venue: &str, year: i32, title: &str, abs: Option<&str>) -> Paper {
        Paper {
            dblp_key: key.into(),
            venue: venue.into(),
            year,
            title: title.into(),
            authors: "Alice Smith, Bob Jones".into(),
            doi: Some("10.1/x".into()),
            url: Some("https://example.com".into()),
            abstract_text: abs.map(|s| s.into()),
        }
    }

    fn seeded() -> Database {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_papers(&[
            paper(
                "k1",
                "NDSS",
                2020,
                "Fuzzing the Linux kernel",
                Some("we fuzz kernels"),
            ),
            paper("k2", "CCS", 2021, "Side channel attacks on caches", None),
            paper(
                "k3",
                "SP",
                2019,
                "Kernel exploitation techniques",
                Some("rop chains"),
            ),
        ])
        .unwrap();
        db
    }

    #[test]
    fn schema_and_count() {
        let db = seeded();
        assert_eq!(db.count().unwrap(), 3);
    }

    #[test]
    fn upsert_is_idempotent_and_updates() {
        let mut db = seeded();
        db.upsert_papers(&[paper(
            "k1",
            "NDSS",
            2020,
            "Fuzzing the Linux kernel v2",
            None,
        )])
        .unwrap();
        assert_eq!(db.count().unwrap(), 3);
        let p = db.get_by_key("k1").unwrap().unwrap();
        assert_eq!(p.title, "Fuzzing the Linux kernel v2");
        assert_eq!(p.abstract_text.as_deref(), Some("we fuzz kernels"));
    }

    #[test]
    fn upsert_skips_unchanged_rows() {
        let mut db = seeded();
        let rows = db
            .upsert_papers(&[paper("k1", "NDSS", 2020, "Fuzzing the Linux kernel", None)])
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn fts_search_matches_title_and_abstract() {
        let db = seeded();
        let hits = db
            .search(&Search {
                fts: Some("kernel".into()),
                ..Default::default()
            })
            .unwrap();
        let keys: Vec<_> = hits.iter().map(|p| p.dblp_key.as_str()).collect();
        assert!(keys.contains(&"k1"));
        assert!(keys.contains(&"k3"));
        assert!(!keys.contains(&"k2"));
    }

    #[test]
    fn fts_boolean_and_phrase() {
        let db = seeded();
        let and_hits = db
            .search(&Search {
                fts: Some("fuzzing AND kernel".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(and_hits.len(), 1);
        assert_eq!(and_hits[0].dblp_key, "k1");

        let phrase = db
            .search(&Search {
                fts: Some("\"side channel\"".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(phrase.len(), 1);
        assert_eq!(phrase[0].dblp_key, "k2");
    }

    #[test]
    fn metadata_filters() {
        let db = seeded();
        let by_venue = db
            .search(&Search {
                venue_filter: VenueFilter::Only(vec!["NDSS".into(), "SP".into()]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_venue.len(), 2);

        let by_year = db
            .search(&Search {
                year_ranges: vec![YearRange::new(Some(2020), None).unwrap()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_year.len(), 2);
    }

    #[test]
    fn repeated_year_filters_match_any_range() {
        let db = seeded();
        let hits = db
            .search(&Search {
                year_ranges: vec![YearRange::single(2019), YearRange::single(2021)],
                ..Default::default()
            })
            .unwrap();
        let keys = hits
            .iter()
            .map(|paper| paper.dblp_key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["k2", "k3"]);
    }

    #[test]
    fn doi_filter_matches_substrings() {
        let db = seeded();
        let hits = db
            .search(&Search {
                doi_terms: vec!["10.1/x".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn search_count_and_offset() {
        let db = seeded();
        let search = Search {
            limit: Some(1),
            offset: Some(1),
            ..Default::default()
        };
        assert_eq!(db.search_count(&search).unwrap(), 3);
        let hits = db.search(&search).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].dblp_key, "k1");
    }

    #[test]
    fn no_match_short_circuits_search() {
        let db = seeded();
        let search = Search {
            venue_filter: VenueFilter::Empty,
            ..Default::default()
        };
        assert_eq!(db.search_count(&search).unwrap(), 0);
        assert!(db.search(&search).unwrap().is_empty());
    }

    #[test]
    fn missing_abstract_batch_uses_keyset_bound() {
        let db = seeded();
        assert_eq!(db.count_missing_abstracts(&[]).unwrap(), 1);
        let missing = db.papers_missing_abstract_batch(&[], 0, 10).unwrap();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].paper.dblp_key, "k2");
        let next = db
            .papers_missing_abstract_batch(&[], missing[0].id, 10)
            .unwrap();
        assert!(next.is_empty());
    }

    #[test]
    fn set_abstract_updates_fts() {
        let mut db = seeded();
        db.set_abstract("k2", "a microarchitectural timing leak")
            .unwrap();
        let hits = db
            .search(&Search {
                fts: Some("microarchitectural".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].dblp_key, "k2");
    }
}
