//! Render papers as a table, JSON, CSV, or BibTeX.

use std::str::FromStr;

use crate::{Error, Paper, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
    Csv,
    Bibtex,
}

impl FromStr for Format {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "table" => Ok(Format::Table),
            "json" => Ok(Format::Json),
            "csv" => Ok(Format::Csv),
            "bibtex" | "bib" => Ok(Format::Bibtex),
            other => Err(Error::Other(format!("unknown format: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Key,
    Venue,
    Year,
    Title,
    Authors,
    Doi,
    Url,
    Abstract,
}

impl Column {
    fn header(self) -> &'static str {
        match self {
            Column::Key => "key",
            Column::Venue => "venue",
            Column::Year => "year",
            Column::Title => "title",
            Column::Authors => "authors",
            Column::Doi => "doi",
            Column::Url => "url",
            Column::Abstract => "abstract",
        }
    }

    fn value(self, p: &Paper) -> String {
        match self {
            Column::Key => p.dblp_key.clone(),
            Column::Venue => p.venue.clone(),
            Column::Year => p.year.to_string(),
            Column::Title => p.title.clone(),
            Column::Authors => p.authors.clone(),
            Column::Doi => p.doi.clone().unwrap_or_default(),
            Column::Url => p.url.clone().unwrap_or_default(),
            Column::Abstract => p.abstract_text.clone().unwrap_or_default(),
        }
    }
}

impl FromStr for Column {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "key" | "dblp_key" => Ok(Column::Key),
            "venue" => Ok(Column::Venue),
            "year" => Ok(Column::Year),
            "title" => Ok(Column::Title),
            "authors" | "author" => Ok(Column::Authors),
            "doi" => Ok(Column::Doi),
            "url" => Ok(Column::Url),
            "abstract" => Ok(Column::Abstract),
            other => Err(Error::Other(format!("unknown column: {other}"))),
        }
    }
}

const DEFAULT_TABLE_COLS: &[Column] =
    &[Column::Venue, Column::Year, Column::Title, Column::Authors];
const ALL_COLS: &[Column] = &[
    Column::Key,
    Column::Venue,
    Column::Year,
    Column::Title,
    Column::Authors,
    Column::Doi,
    Column::Url,
    Column::Abstract,
];

/// Render papers in the requested format. `columns` overrides the default
/// column set for table/csv output (ignored for json/bibtex).
pub fn render(papers: &[Paper], format: Format, columns: Option<&[Column]>) -> Result<String> {
    match format {
        Format::Table => Ok(table(papers, columns.unwrap_or(DEFAULT_TABLE_COLS))),
        Format::Csv => csv(papers, columns.unwrap_or(ALL_COLS)),
        Format::Json => Ok(serde_json::to_string_pretty(papers)?),
        Format::Bibtex => Ok(bibtex(papers)),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn table(papers: &[Paper], cols: &[Column]) -> String {
    // Per-column display cap so the title column doesn't blow up the width.
    let cap = |col: Column| match col {
        Column::Title => 70,
        Column::Authors => 40,
        Column::Abstract => 60,
        Column::Url => 50,
        _ => usize::MAX,
    };

    let mut rows: Vec<Vec<String>> = Vec::with_capacity(papers.len());
    for p in papers {
        rows.push(
            cols.iter()
                .map(|c| truncate(&c.value(p), cap(*c)))
                .collect(),
        );
    }

    let mut widths: Vec<usize> = cols.iter().map(|c| c.header().chars().count()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let fmt_row = |cells: &[String]| -> String {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = widths[i] - c.chars().count();
                format!("{c}{}", " ".repeat(pad))
            })
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };

    let mut out = String::new();
    let header: Vec<String> = cols.iter().map(|c| c.header().to_string()).collect();
    out.push_str(&fmt_row(&header));
    out.push('\n');
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    out.push_str(&fmt_row(&sep));
    for row in &rows {
        out.push('\n');
        out.push_str(&fmt_row(row));
    }
    out
}

fn csv(papers: &[Paper], cols: &[Column]) -> Result<String> {
    let mut wtr = csv::Writer::from_writer(Vec::new());
    wtr.write_record(cols.iter().map(|c| c.header()))?;
    for p in papers {
        wtr.write_record(cols.iter().map(|c| c.value(p)))?;
    }
    let bytes = wtr.into_inner().map_err(|e| e.into_error())?;
    Ok(String::from_utf8(bytes)?)
}

fn bibtex(papers: &[Paper]) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    for (i, p) in papers.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let _ = writeln!(out, "@inproceedings{{{},", bibtex_key(&p.cite_key()));
        let _ = writeln!(out, "  title     = {{{}}},", bibtex_value(&p.title));
        let _ = writeln!(
            out,
            "  author    = {{{}}},",
            bibtex_value(&p.authors_bibtex())
        );
        let _ = writeln!(out, "  booktitle = {{{}}},", bibtex_value(&p.venue));
        let _ = writeln!(out, "  year      = {{{}}},", p.year);
        if let Some(doi) = &p.doi {
            let _ = writeln!(out, "  doi       = {{{}}},", bibtex_value(doi));
        }
        if let Some(url) = &p.url {
            let _ = writeln!(out, "  url       = {{{}}},", bibtex_value(url));
        }
        out.push_str("}\n");
    }
    out
}

fn bibtex_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.') {
            out.push(c);
        } else if c.is_whitespace() {
            out.push('_');
        }
    }
    if out.is_empty() {
        "paper".to_string()
    } else {
        out
    }
}

fn bibtex_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str(r"\textbackslash{}"),
            '{' => out.push_str(r"\{"),
            '}' => out.push_str(r"\}"),
            '&' => out.push_str(r"\&"),
            '%' => out.push_str(r"\%"),
            '$' => out.push_str(r"\$"),
            '#' => out.push_str(r"\#"),
            '_' => out.push_str(r"\_"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Paper> {
        vec![Paper {
            dblp_key: "conf/ndss/Smith20".into(),
            venue: "NDSS".into(),
            year: 2020,
            title: "Fuzzing the Linux kernel".into(),
            authors: "Alice Smith, Bob Jones".into(),
            doi: Some("10.1/x".into()),
            url: Some("https://doi.org/10.1/x".into()),
            abstract_text: Some("we fuzz kernels".into()),
        }]
    }

    #[test]
    fn format_from_str() {
        assert_eq!(Format::from_str("JSON").unwrap(), Format::Json);
        assert_eq!(Format::from_str("bib").unwrap(), Format::Bibtex);
        assert!(Format::from_str("xml").is_err());
    }

    #[test]
    fn table_has_header_and_row() {
        let t = render(&sample(), Format::Table, None).unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].contains("venue"));
        assert!(lines[0].contains("title"));
        assert!(lines
            .iter()
            .any(|l| l.contains("NDSS") && l.contains("2020")));
    }

    #[test]
    fn json_uses_abstract_key() {
        let j = render(&sample(), Format::Json, None).unwrap();
        assert!(j.contains("\"abstract\""));
        assert!(!j.contains("abstract_text"));
        // round-trips back into Paper
        let back: Vec<Paper> = serde_json::from_str(&j).unwrap();
        assert_eq!(back, sample());
    }

    #[test]
    fn csv_header_and_values() {
        let c = render(&sample(), Format::Csv, None).unwrap();
        let mut lines = c.lines();
        assert_eq!(
            lines.next().unwrap(),
            "key,venue,year,title,authors,doi,url,abstract"
        );
        assert!(lines
            .next()
            .unwrap()
            .contains("NDSS,2020,Fuzzing the Linux kernel"));
    }

    #[test]
    fn bibtex_structure() {
        let b = render(&sample(), Format::Bibtex, None).unwrap();
        assert!(b.contains("@inproceedings{ndss:2020:smith,"));
        assert!(b.contains("author    = {Alice Smith and Bob Jones}"));
        assert!(b.contains("booktitle = {NDSS}"));
        assert!(b.contains("doi       = {10.1/x}"));
    }

    #[test]
    fn bibtex_escapes_values_and_sanitizes_key() {
        let paper = Paper {
            dblp_key: "x".into(),
            venue: "S&P".into(),
            year: 2024,
            title: "A {broken} 100%_safe #1".into(),
            authors: "Alice A, Bob B".into(),
            doi: Some("10.1/a_b".into()),
            url: Some(r"https://example.com/a\b".into()),
            abstract_text: None,
        };
        let b = render(&[paper], Format::Bibtex, None).unwrap();
        assert!(b.contains("@inproceedings{sp:2024:a,"));
        assert!(b.contains(r"title     = {A \{broken\} 100\%\_safe \#1},"));
        assert!(b.contains(r"booktitle = {S\&P},"));
        assert!(b.contains(r"doi       = {10.1/a\_b},"));
        assert!(b.contains(r"url       = {https://example.com/a\textbackslash{}b},"));
    }

    #[test]
    fn custom_columns() {
        let cols = [Column::Year, Column::Title];
        let c = csv(&sample(), &cols).unwrap();
        assert_eq!(c.lines().next().unwrap(), "year,title");
    }
}
