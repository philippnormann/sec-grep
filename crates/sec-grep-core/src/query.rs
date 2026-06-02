//! Query language: compile user text into FTS and metadata filters.
//!
//! Grammar (loosely):
//!   or   := and ( ("OR"|"|") and )*
//!   and  := unary ( ("AND"|"&"|<implicit>) unary )*
//!   unary:= ("NOT"|"-") unary | primary
//!   primary := "(" or ")" | field | phrase | term
//!   field := ("title"|"abstract"|"author"|"venue"|"year"|"rank"|"tag"|"doi") ":" unary
//!
//! Terms are emitted as quoted FTS5 strings (trailing `*` => prefix search).
//! Negation maps to FTS5's binary `x NOT y`, so a negated term must be
//! combined with at least one positive term.

use crate::config::{Config, VenueFilter};
use crate::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub fts: Option<String>,
    pub venue_selectors: Vec<String>,
    pub rank_selectors: Vec<String>,
    pub tag_selectors: Vec<String>,
    pub doi_terms: Vec<String>,
    pub year_min: Option<i32>,
    pub year_max: Option<i32>,
}

impl ParsedQuery {
    pub fn resolve_venue_filter(&self, config: &Config) -> Result<VenueFilter> {
        let mut filter = VenueFilter::All;
        if !self.venue_selectors.is_empty() {
            filter = filter.intersect(VenueFilter::from_active_ids(
                config.resolve_venues(&self.venue_selectors)?,
            ));
        }
        if !self.rank_selectors.is_empty() {
            filter = filter.intersect(VenueFilter::from_active_ids(
                config.venues_by_rank(&self.rank_selectors),
            ));
        }
        if !self.tag_selectors.is_empty() {
            filter = filter.intersect(VenueFilter::from_active_ids(
                config.venues_by_tag(&self.tag_selectors),
            ));
        }
        Ok(filter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Field(FieldKind),
    Term(String),
    Phrase(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FieldKind {
    Fts(String),
    Metadata(MetadataField),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetadataField {
    Venue,
    Year,
    Rank,
    Tag,
    Doi,
}

impl MetadataField {
    fn label(self) -> &'static str {
        match self {
            MetadataField::Venue => "venue",
            MetadataField::Year => "year",
            MetadataField::Rank => "rank",
            MetadataField::Tag => "tag",
            MetadataField::Doi => "doi",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Term(String),
    Phrase(String),
    Field(FieldKind, Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

const FIELDS: &[(&str, &str)] = &[
    ("title", "title"),
    ("abstract", "abstract"),
    ("author", "authors"),
    ("authors", "authors"),
];

fn map_field(name: &str) -> Option<&'static str> {
    FIELDS
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, col)| *col)
}

fn map_metadata_field(name: &str) -> Option<MetadataField> {
    match name.to_ascii_lowercase().as_str() {
        "venue" => Some(MetadataField::Venue),
        "year" => Some(MetadataField::Year),
        "rank" => Some(MetadataField::Rank),
        "tag" | "tags" => Some(MetadataField::Tag),
        "doi" => Some(MetadataField::Doi),
        _ => None,
    }
}

fn is_word_delim(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '"' | '&' | '|')
}

fn tokenize(input: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '&' => {
                out.push(Tok::And);
                i += 1;
            }
            '|' => {
                out.push(Tok::Or);
                i += 1;
            }
            '"' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '"' {
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(Error::Query("unterminated quoted phrase".into()));
                }
                let phrase: String = chars[start..i].iter().collect();
                out.push(Tok::Phrase(phrase));
                i += 1; // closing quote
            }
            '-' if i + 1 < chars.len() && !is_word_delim(chars[i + 1]) => {
                out.push(Tok::Not);
                i += 1;
            }
            _ => {
                let start = i;
                while i < chars.len() && !is_word_delim(chars[i]) {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                push_word(&mut out, &word);
            }
        }
    }
    Ok(out)
}

fn push_word(out: &mut Vec<Tok>, word: &str) {
    // field prefix?
    if let Some((name, rest)) = word.split_once(':') {
        if let Some(col) = map_field(name) {
            out.push(Tok::Field(FieldKind::Fts(col.to_string())));
            if !rest.is_empty() {
                out.push(Tok::Term(rest.to_string()));
            }
            return;
        }
        if let Some(field) = map_metadata_field(name) {
            out.push(Tok::Field(FieldKind::Metadata(field)));
            if !rest.is_empty() {
                out.push(Tok::Term(rest.to_string()));
            }
            return;
        }
    }
    match word.to_ascii_uppercase().as_str() {
        "AND" => out.push(Tok::And),
        "OR" => out.push(Tok::Or),
        "NOT" => out.push(Tok::Not),
        _ => out.push(Tok::Term(word.to_string())),
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn advance(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn parse_or(&mut self) -> Result<Node> {
        let mut nodes = vec![self.parse_and()?];
        while matches!(self.peek(), Some(Tok::Or)) {
            self.advance();
            nodes.push(self.parse_and()?);
        }
        Ok(collapse(nodes, true))
    }

    fn parse_and(&mut self) -> Result<Node> {
        let mut nodes = vec![self.parse_unary()?];
        loop {
            match self.peek() {
                None | Some(Tok::RParen | Tok::Or) => break,
                Some(Tok::And) => {
                    self.advance();
                    nodes.push(self.parse_unary()?);
                }
                // implicit AND between adjacent operands
                _ => nodes.push(self.parse_unary()?),
            }
        }
        Ok(collapse(nodes, false))
    }

    fn parse_unary(&mut self) -> Result<Node> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.advance();
            return Ok(Node::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Node> {
        match self.advance() {
            Some(Tok::LParen) => {
                let inner = self.parse_or()?;
                match self.advance() {
                    Some(Tok::RParen) => Ok(inner),
                    _ => Err(Error::Query("expected ')'".into())),
                }
            }
            Some(Tok::Field(col)) => {
                let operand = self.parse_unary()?;
                Ok(Node::Field(col, Box::new(operand)))
            }
            Some(Tok::Term(t)) => Ok(Node::Term(t)),
            Some(Tok::Phrase(p)) => Ok(Node::Phrase(p)),
            other => Err(Error::Query(format!("unexpected token: {other:?}"))),
        }
    }
}

fn collapse(mut nodes: Vec<Node>, or: bool) -> Node {
    if nodes.len() == 1 {
        nodes.pop().unwrap()
    } else if or {
        Node::Or(nodes)
    } else {
        Node::And(nodes)
    }
}

fn escape(s: &str) -> String {
    s.replace('"', "\"\"")
}

fn quote_term(t: &str) -> String {
    if let Some(base) = t.strip_suffix('*') {
        if !base.is_empty() {
            return format!("\"{}\"*", escape(base));
        }
    }
    format!("\"{}\"", escape(t))
}

fn render(node: &Node) -> Result<String> {
    match node {
        Node::Term(t) => Ok(quote_term(t)),
        Node::Phrase(p) => Ok(format!("\"{}\"", escape(p))),
        Node::Field(FieldKind::Fts(col), inner) => Ok(format!("{col} : ({})", render(inner)?)),
        Node::Field(FieldKind::Metadata(field), _) => Err(Error::Query(format!(
            "{}: is a metadata filter and cannot be rendered as full-text search",
            field.label()
        ))),
        Node::Or(children) => {
            let parts: Result<Vec<_>> = children.iter().map(render).collect();
            Ok(format!("({})", parts?.join(" OR ")))
        }
        Node::Not(_) => Err(negation_error()),
        Node::And(children) => {
            let mut pos = Vec::new();
            let mut neg = Vec::new();
            for c in children {
                match c {
                    Node::Not(inner) => neg.push(render(inner)?),
                    _ => pos.push(render(c)?),
                }
            }
            if pos.is_empty() {
                return Err(negation_error());
            }
            let mut s = if pos.len() == 1 {
                pos.remove(0)
            } else {
                format!("({})", pos.join(" AND "))
            };
            for n in neg {
                s = format!("{s} NOT {n}");
            }
            Ok(s)
        }
    }
}

pub fn parse(input: &str) -> Result<ParsedQuery> {
    if input.trim().is_empty() {
        return Ok(ParsedQuery::default());
    }
    let toks = tokenize(input)?;
    if toks.is_empty() {
        return Ok(ParsedQuery::default());
    }
    let mut parser = Parser { toks, pos: 0 };
    let node = parser.parse_or()?;
    if parser.pos != parser.toks.len() {
        return Err(Error::Query(format!(
            "unexpected token: {:?}",
            parser.toks[parser.pos]
        )));
    }
    let mut parsed = ParsedQuery::default();
    if let Some(fts_node) = collect_metadata(node, &mut parsed)? {
        parsed.fts = Some(render(&fts_node)?);
    }
    Ok(parsed)
}

/// Compile a user query into an FTS5 MATCH expression.
/// Returns `None` for blank or metadata-only input.
pub fn compile(input: &str) -> Result<Option<String>> {
    Ok(parse(input)?.fts)
}

/// Parse a `--year` selector into an inclusive (min, max) bound.
/// Accepts `2020` (single), `2018-2024` (range), `2020-` (open end),
/// `-2019` (open start).
pub fn parse_year_range(s: &str) -> Result<(Option<i32>, Option<i32>)> {
    let s = s.trim();
    let bad = || {
        Error::Query(format!(
            "invalid year selector `{s}`; use `year:2020`, `year:2018-2024`, `year:2020-`, or `year:-2019`"
        ))
    };
    let year = |t: &str| -> Result<i32> { t.trim().parse::<i32>().map_err(|_| bad()) };
    match s.split_once('-') {
        None => {
            let y = year(s)?;
            Ok((Some(y), Some(y)))
        }
        Some((lo, hi)) => {
            let min = if lo.trim().is_empty() {
                None
            } else {
                Some(year(lo)?)
            };
            let max = if hi.trim().is_empty() {
                None
            } else {
                Some(year(hi)?)
            };
            if min.is_none() && max.is_none() {
                return Err(bad());
            }
            Ok((min, max))
        }
    }
}

fn collect_metadata(node: Node, parsed: &mut ParsedQuery) -> Result<Option<Node>> {
    match node {
        Node::Field(FieldKind::Fts(_), ref inner) if contains_metadata(inner) => Err(Error::Query(
            "metadata filters cannot be nested inside title:, author:, or abstract:".into(),
        )),
        Node::Term(_) | Node::Phrase(_) | Node::Field(FieldKind::Fts(_), _) => Ok(Some(node)),
        Node::Field(FieldKind::Metadata(field), inner) => {
            add_metadata_filter(field, *inner, parsed)?;
            Ok(None)
        }
        Node::And(children) => {
            let mut kept = Vec::new();
            for child in children {
                if let Some(node) = collect_metadata(child, parsed)? {
                    kept.push(node);
                }
            }
            Ok(collapse_optional(kept, false))
        }
        Node::Or(children) => {
            if children.iter().any(contains_metadata) {
                return Err(Error::Query(
                    "metadata filters cannot be combined with OR; use them as filters, e.g. `venue:ndss kernel`".into(),
                ));
            }
            Ok(Some(Node::Or(children)))
        }
        Node::Not(inner) => {
            if contains_metadata(&inner) {
                return Err(Error::Query(
                    "metadata filters cannot be negated; combine a positive metadata filter with text terms instead".into(),
                ));
            }
            Ok(Some(Node::Not(inner)))
        }
    }
}

fn collapse_optional(mut nodes: Vec<Node>, or: bool) -> Option<Node> {
    if nodes.is_empty() {
        None
    } else if nodes.len() == 1 {
        nodes.pop()
    } else if or {
        Some(Node::Or(nodes))
    } else {
        Some(Node::And(nodes))
    }
}

fn contains_metadata(node: &Node) -> bool {
    match node {
        Node::Field(FieldKind::Metadata(_), _) => true,
        Node::Field(FieldKind::Fts(_), inner) | Node::Not(inner) => contains_metadata(inner),
        Node::And(children) | Node::Or(children) => children.iter().any(contains_metadata),
        Node::Term(_) | Node::Phrase(_) => false,
    }
}

fn add_metadata_filter(field: MetadataField, node: Node, parsed: &mut ParsedQuery) -> Result<()> {
    let value = metadata_value(field, node)?;
    match field {
        MetadataField::Venue => push_csv_values(&mut parsed.venue_selectors, &value),
        MetadataField::Rank => push_csv_values(&mut parsed.rank_selectors, &value),
        MetadataField::Tag => push_csv_values(&mut parsed.tag_selectors, &value),
        MetadataField::Doi => push_csv_values(&mut parsed.doi_terms, &value),
        MetadataField::Year => {
            let range = parse_year_range(&value)?;
            merge_year_filter(parsed, range)?;
        }
    }
    Ok(())
}

fn metadata_value(field: MetadataField, node: Node) -> Result<String> {
    match node {
        Node::Term(value) | Node::Phrase(value) => Ok(value),
        _ => Err(Error::Query(format!(
            "{}: expects a single term or quoted value, e.g. `{}:ndss`",
            field.label(),
            field.label()
        ))),
    }
}

fn push_csv_values(values: &mut Vec<String>, raw: &str) {
    values.extend(
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    );
}

fn merge_year_filter(
    parsed: &mut ParsedQuery,
    (year_min, year_max): (Option<i32>, Option<i32>),
) -> Result<()> {
    let (year_min, year_max) =
        merge_year_bounds((parsed.year_min, parsed.year_max), (year_min, year_max))?;
    parsed.year_min = year_min;
    parsed.year_max = year_max;
    Ok(())
}

pub fn merge_year_bounds(
    left: (Option<i32>, Option<i32>),
    right: (Option<i32>, Option<i32>),
) -> Result<(Option<i32>, Option<i32>)> {
    let min = max_bound(left.0, right.0);
    let max = min_bound(left.1, right.1);
    if let (Some(min), Some(max)) = (min, max) {
        if min > max {
            return Err(Error::Query(format!(
                "conflicting year filters: minimum {min} is after maximum {max}"
            )));
        }
    }
    Ok((min, max))
}

fn max_bound(left: Option<i32>, right: Option<i32>) -> Option<i32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn min_bound(left: Option<i32>, right: Option<i32>) -> Option<i32> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn negation_error() -> Error {
    Error::Query(
        "a negated term must be combined with a positive term, e.g. `kernel -windows`".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> String {
        compile(s).unwrap().unwrap()
    }

    #[test]
    fn year_ranges() {
        assert_eq!(parse_year_range("2020").unwrap(), (Some(2020), Some(2020)));
        assert_eq!(
            parse_year_range("2018-2024").unwrap(),
            (Some(2018), Some(2024))
        );
        assert_eq!(parse_year_range("2020-").unwrap(), (Some(2020), None));
        assert_eq!(parse_year_range("-2019").unwrap(), (None, Some(2019)));
        assert!(parse_year_range("-").is_err());
        assert!(parse_year_range("abc").is_err());
    }

    #[test]
    fn blank_is_none() {
        assert!(compile("").unwrap().is_none());
        assert!(compile("   ").unwrap().is_none());
    }

    #[test]
    fn single_term() {
        assert_eq!(c("fuzzing"), "\"fuzzing\"");
    }

    #[test]
    fn implicit_and() {
        assert_eq!(c("fuzzing kernel"), "(\"fuzzing\" AND \"kernel\")");
    }

    #[test]
    fn explicit_and_matches_implicit() {
        assert_eq!(c("fuzzing AND kernel"), c("fuzzing kernel"));
        assert_eq!(c("fuzzing & kernel"), c("fuzzing kernel"));
    }

    #[test]
    fn or_expression() {
        assert_eq!(c("a OR b"), "(\"a\" OR \"b\")");
        assert_eq!(c("a | b"), "(\"a\" OR \"b\")");
    }

    #[test]
    fn phrase() {
        assert_eq!(c("\"side channel\""), "\"side channel\"");
    }

    #[test]
    fn field_scopes() {
        assert_eq!(c("title:fuzzing"), "title : (\"fuzzing\")");
        assert_eq!(c("author:\"jane doe\""), "authors : (\"jane doe\")");
        assert_eq!(c("abstract:rop"), "abstract : (\"rop\")");
    }

    #[test]
    fn metadata_filters_are_separated_from_fts() {
        let parsed =
            parse("venue:ndss year:2020-2024 rank:A tag:crypto doi:10.1145 fuzzing").unwrap();
        assert_eq!(parsed.fts.as_deref(), Some("\"fuzzing\""));
        assert_eq!(parsed.venue_selectors, vec!["ndss"]);
        assert_eq!(parsed.rank_selectors, vec!["A"]);
        assert_eq!(parsed.tag_selectors, vec!["crypto"]);
        assert_eq!(parsed.doi_terms, vec!["10.1145"]);
        assert_eq!(parsed.year_min, Some(2020));
        assert_eq!(parsed.year_max, Some(2024));
    }

    #[test]
    fn metadata_only_query_has_no_fts() {
        let parsed = parse("venue:ndss").unwrap();
        assert!(parsed.fts.is_none());
        assert_eq!(parsed.venue_selectors, vec!["ndss"]);
        assert!(compile("venue:ndss").unwrap().is_none());
    }

    #[test]
    fn metadata_filters_reject_or_and_negation() {
        assert!(parse("venue:ndss OR venue:ccs").is_err());
        assert!(parse("-venue:ndss").is_err());
        assert!(parse("title:(venue:ndss)").is_err());
    }

    #[test]
    fn negation() {
        assert_eq!(c("fuzzing -windows"), "\"fuzzing\" NOT \"windows\"");
        assert_eq!(c("kernel NOT windows"), "\"kernel\" NOT \"windows\"");
    }

    #[test]
    fn grouping_and_precedence() {
        assert_eq!(c("(a OR b) c"), "((\"a\" OR \"b\") AND \"c\")");
    }

    #[test]
    fn prefix_search() {
        assert_eq!(c("fuzz*"), "\"fuzz\"*");
    }

    #[test]
    fn unknown_field_is_plain_term() {
        // `foo:` is not a known field, so it stays a literal token
        assert_eq!(c("foo:bar"), "\"foo:bar\"");
    }

    #[test]
    fn errors() {
        assert!(compile("-windows").is_err());
        assert!(compile("(a OR b").is_err());
        assert!(compile("\"unterminated").is_err());
        assert!(compile("NOT alone").is_err());
    }
}
