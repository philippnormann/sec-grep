//! Optional abstract enrichment.
//!
//! Two tiers, each a fallback for the previous:
//!   1. API by DOI: OpenAlex -> Semantic Scholar -> Crossref
//!   2. Static HTML scrape (publisher page)
//!
//! The pure parsing/extraction helpers are unit-tested; the networked
//! orchestration is exercised end-to-end via the CLI.

use std::{net::IpAddr, time::Duration};

use reqwest::{header, Url};
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use crate::config::{AbstractSource, Secrets};
use crate::{Paper, Result};

const MAX_JSON_BODY_BYTES: usize = 2 * 1024 * 1024;
const MAX_HTML_BODY_BYTES: usize = 4 * 1024 * 1024;
const MAX_STATIC_REDIRECTS: usize = 5;

/// Reconstruct plain text from an OpenAlex `abstract_inverted_index`,
/// or read a plain `abstract` string if present.
pub fn abstract_from_openalex(work: &Value) -> Option<String> {
    if let Some(s) = work.get("abstract").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            return Some(s.trim().to_string());
        }
    }
    let idx = work.get("abstract_inverted_index")?.as_object()?;
    let mut positioned: Vec<(u64, &str)> = Vec::new();
    for (word, positions) in idx {
        for p in positions.as_array()? {
            if let Some(pos) = p.as_u64() {
                positioned.push((pos, word.as_str()));
            }
        }
    }
    if positioned.is_empty() {
        return None;
    }
    positioned.sort_by_key(|(p, _)| *p);
    Some(
        positioned
            .into_iter()
            .map(|(_, w)| w)
            .collect::<Vec<_>>()
            .join(" "),
    )
}

pub fn abstract_from_semantic_scholar(paper: &Value) -> Option<String> {
    paper
        .get("abstract")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Crossref returns JATS-flavored XML in `message.abstract`; strip tags.
pub fn abstract_from_crossref(message: &Value) -> Option<String> {
    let raw = message.get("abstract").and_then(|v| v.as_str())?;
    let doc = Html::parse_fragment(raw);
    element_text(doc.root_element())
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract an abstract from a publisher HTML page, trying source-specific
/// selectors first, then generic `og:description` / `description` meta tags.
pub fn extract_abstract_html(html: &str, source: Option<AbstractSource>) -> Option<String> {
    let doc = Html::parse_document(html);

    let source_hit = match source {
        Some(AbstractSource::Acm) => {
            first_selector_text(&doc, &["div.abstractInFull", "div.abstractSection"])
        }
        Some(AbstractSource::Ieee) => first_selector_text(&doc, &["div.abstract-text"]),
        Some(AbstractSource::Ndss) => extract_ndss_abstract(&doc),
        Some(AbstractSource::Neurips) => extract_neurips_abstract(&doc),
        Some(AbstractSource::Springer) => first_selector_text(
            &doc,
            &[
                "section[data-title='Abstract'] div.c-article-section__content",
                "#Abs1-content",
            ],
        ),
        Some(AbstractSource::Usenix) => first_selector_text(
            &doc,
            &[
                "div.field-name-field-paper-description",
                "div.field-type-text-with-summary",
            ],
        ),
        _ => None,
    };

    source_hit.or_else(|| first_meta_content(&doc))
}

fn extract_ndss_abstract(doc: &Html) -> Option<String> {
    let mut text = first_selector_text(doc, &["div.paper-data"])?;
    if let Some(authors) = first_selector_text(doc, &["div.paper-data strong"]) {
        if let Some(rest) = text.strip_prefix(&authors) {
            text = rest
                .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '-'))
                .to_string();
        }
    }
    non_empty_text(text)
}

fn extract_neurips_abstract(doc: &Html) -> Option<String> {
    let selector = Selector::parse("section.paper-section").ok()?;
    for section in doc.select(&selector) {
        let text = element_text(section)?;
        if let Some(abstract_text) = text.strip_prefix("Abstract") {
            return non_empty_text(abstract_text);
        }
    }
    None
}

fn first_selector_text(doc: &Html, selectors: &[&str]) -> Option<String> {
    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        for element in doc.select(&selector) {
            if let Some(text) = element_text(element) {
                return Some(text);
            }
        }
    }
    None
}

fn first_meta_content(doc: &Html) -> Option<String> {
    let selectors = [
        "meta[name='citation_abstract']",
        "meta[property='og:description']",
        "meta[name='description']",
    ];

    for selector in selectors {
        let Ok(selector) = Selector::parse(selector) else {
            continue;
        };
        for element in doc.select(&selector) {
            if let Some(content) = element.value().attr("content") {
                if let Some(text) = non_empty_text(content) {
                    return Some(text);
                }
            }
        }
    }
    None
}

fn element_text(element: ElementRef) -> Option<String> {
    non_empty_text(element.text().collect::<Vec<_>>().join(" "))
}

fn non_empty_text(text: impl AsRef<str>) -> Option<String> {
    let decoded = decode_html_entities(text.as_ref());
    let decoded = decode_html_entities(&decoded);
    let text = collapse_ws(&decoded);
    (!text.is_empty()).then_some(text)
}

fn decode_html_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;

    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        let entity_start = idx + 1;
        let Some(entity_len) = rest[entity_start..].find(';') else {
            out.push_str(&rest[idx..]);
            return out;
        };
        let entity_end = entity_start + entity_len;
        let entity = &rest[entity_start..entity_end];
        match decode_entity(entity) {
            Some(decoded) => out.push_str(&decoded),
            None => out.push_str(&rest[idx..=entity_end]),
        }
        rest = &rest[entity_end + 1..];
    }

    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<String> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        let code = u32::from_str_radix(hex, 16).ok()?;
        return char::from_u32(code).map(|c| c.to_string());
    }
    if let Some(dec) = entity.strip_prefix('#') {
        let code = dec.parse::<u32>().ok()?;
        return char::from_u32(code).map(|c| c.to_string());
    }
    match entity {
        "amp" => Some("&".to_string()),
        "apos" => Some("'".to_string()),
        "gt" => Some(">".to_string()),
        "lt" => Some("<".to_string()),
        "nbsp" => Some(" ".to_string()),
        "quot" => Some("\"".to_string()),
        _ => None,
    }
}

/// Networked abstract enrichment using the configured API keys.
pub struct Enricher {
    client: reqwest::Client,
    secrets: Secrets,
}

impl Enricher {
    pub fn new(secrets: Secrets) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("sec-grep/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client");
        Self { client, secrets }
    }

    /// Try, in order: API-by-DOI, then static scrape. Returns the first hit.
    pub async fn enrich(
        &self,
        paper: &Paper,
        source: Option<AbstractSource>,
    ) -> Result<Option<String>> {
        if let Some(doi) = &paper.doi {
            if let Some(abs) = self.api_by_doi(doi).await? {
                return Ok(Some(abs));
            }
        }
        if let Some(url) = &paper.url {
            if let Some(abs) = self.static_scrape(url, source).await? {
                return Ok(Some(abs));
            }
        }
        Ok(None)
    }

    async fn api_by_doi(&self, doi: &str) -> Result<Option<String>> {
        let openalex_req = {
            let req = self
                .client
                .get(format!("https://api.openalex.org/works/doi:{doi}"));
            match &self.secrets.openalex_api_key {
                Some(key) => req.query(&[("api_key", key.as_str())]),
                None => req,
            }
        };
        if let Some(abs) = self
            .fetch_abstract(openalex_req, abstract_from_openalex)
            .await
        {
            return Ok(Some(abs));
        }

        let s2 =
            format!("https://api.semanticscholar.org/graph/v1/paper/DOI:{doi}?fields=abstract");
        let s2_req = {
            let req = self.client.get(&s2);
            match &self.secrets.semantic_scholar_key {
                Some(key) => req.header("x-api-key", key),
                None => req,
            }
        };
        if let Some(abs) = self
            .fetch_abstract(s2_req, abstract_from_semantic_scholar)
            .await
        {
            return Ok(Some(abs));
        }

        let cr = self
            .client
            .get(format!("https://api.crossref.org/works/{doi}"));
        Ok(self
            .fetch_abstract(cr, |json| {
                json.get("message").and_then(abstract_from_crossref)
            })
            .await)
    }

    /// Send a request and run `extract` over the JSON body, swallowing any
    /// transport, status, or decode failure as a `None` (try the next source).
    async fn fetch_abstract(
        &self,
        req: reqwest::RequestBuilder,
        extract: impl Fn(&Value) -> Option<String>,
    ) -> Option<String> {
        let resp = req.send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = read_body_limited(resp, MAX_JSON_BODY_BYTES).await?;
        let json = serde_json::from_slice::<Value>(&bytes).ok()?;
        extract(&json)
    }

    async fn static_scrape(
        &self,
        url: &str,
        source: Option<AbstractSource>,
    ) -> Result<Option<String>> {
        let Some(mut url) = allowed_static_url(url).await else {
            return Ok(None);
        };

        for _ in 0..=MAX_STATIC_REDIRECTS {
            let resp = match self.client.get(url.clone()).send().await {
                Ok(resp) => resp,
                Err(_) => return Ok(None),
            };
            if resp.status().is_redirection() {
                let Some(next_url) = redirect_url(&url, &resp) else {
                    return Ok(None);
                };
                let Some(next_url) = allowed_static_url(next_url.as_str()).await else {
                    return Ok(None);
                };
                url = next_url;
                continue;
            }
            if !resp.status().is_success() {
                return Ok(None);
            }
            let Some(html) = read_text_limited(resp, MAX_HTML_BODY_BYTES).await else {
                return Ok(None);
            };
            return Ok(extract_abstract_html(&html, source));
        }

        Ok(None)
    }
}

async fn allowed_static_url(raw: &str) -> Option<Url> {
    let url = parse_static_url(raw)?;
    let host = url.host_str()?;
    let port = url.port_or_known_default()?;
    if let Some(ip) = parse_host_ip(host) {
        return is_public_ip(ip).then_some(url);
    }
    let addrs = tokio::net::lookup_host((host, port)).await.ok()?;
    let mut has_addr = false;
    for addr in addrs {
        has_addr = true;
        if !is_public_ip(addr.ip()) {
            return None;
        }
    }
    has_addr.then_some(url)
}

fn parse_static_url(raw: &str) -> Option<Url> {
    let url = Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?;
    if is_localhost(host) {
        return None;
    }
    if let Some(ip) = parse_host_ip(host) {
        return is_public_ip(ip).then_some(url);
    }
    Some(url)
}

fn parse_host_ip(host: &str) -> Option<IpAddr> {
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse().ok()
}

fn is_localhost(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || ip.is_documentation())
        }
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            let first = segments[0];
            let is_unique_local = (first & 0xfe00) == 0xfc00;
            let is_link_local = (first & 0xffc0) == 0xfe80;
            let is_documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
            !(ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || is_unique_local
                || is_link_local
                || is_documentation)
        }
    }
}

fn redirect_url(base: &Url, resp: &reqwest::Response) -> Option<Url> {
    let location = resp.headers().get(header::LOCATION)?.to_str().ok()?;
    base.join(location).ok()
}

async fn read_text_limited(resp: reqwest::Response, limit: usize) -> Option<String> {
    let bytes = read_body_limited(resp, limit).await?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

async fn read_body_limited(mut resp: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    if resp.content_length().is_some_and(|len| len > limit as u64) {
        return None;
    }

    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        if body.len().checked_add(chunk.len())? > limit {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openalex_inverted_index() {
        let work = json!({
            "abstract_inverted_index": {
                "We": [0], "fuzz": [1], "the": [2], "kernel": [3]
            }
        });
        assert_eq!(
            abstract_from_openalex(&work).as_deref(),
            Some("We fuzz the kernel")
        );
    }

    #[test]
    fn openalex_plain_abstract() {
        let work = json!({ "abstract": "  direct text  " });
        assert_eq!(
            abstract_from_openalex(&work).as_deref(),
            Some("direct text")
        );
    }

    #[test]
    fn semantic_scholar_abstract() {
        assert_eq!(
            abstract_from_semantic_scholar(&json!({"abstract": "hello"})).as_deref(),
            Some("hello")
        );
        assert!(abstract_from_semantic_scholar(&json!({"abstract": null})).is_none());
    }

    #[test]
    fn crossref_strips_jats() {
        let msg = json!({"abstract": "<jats:p>A <jats:bold>bold</jats:bold> claim.</jats:p>"});
        assert_eq!(
            abstract_from_crossref(&msg).as_deref(),
            Some("A bold claim.")
        );
    }

    #[test]
    fn html_acm_source_specific_selector() {
        let html = r#"<html><body>
            <div class="abstractInFull"><p>This is the ACM abstract.</p></div>
        </body></html>"#;
        assert_eq!(
            extract_abstract_html(html, Some(AbstractSource::Acm)).as_deref(),
            Some("This is the ACM abstract.")
        );
    }

    #[test]
    fn html_ndss_strips_authors_from_paper_data() {
        let html = r#"<html><body>
            <div class="paper-data">
                <p><strong><p>Alice A (Example U), Bob B (Example Labs)</p></strong></p>
                <p>
                    <p>First abstract paragraph.</p>
                    <p>Second abstract paragraph.</p>
                </p>
            </div>
        </body></html>"#;
        let abstract_text = extract_abstract_html(html, Some(AbstractSource::Ndss)).unwrap();
        assert_eq!(
            abstract_text,
            "First abstract paragraph. Second abstract paragraph."
        );
        assert!(!abstract_text.contains("Alice A"));
        assert!(!abstract_text.contains("Bob B"));
    }

    #[test]
    fn html_usenix_source_specific_selector() {
        let html = r#"<html><body>
            <div class="field field-name-field-paper-people-text"><p>Alice A and Bob B</p></div>
            <div class="field field-name-field-paper-description"><p>This is the USENIX abstract.</p></div>
        </body></html>"#;
        assert_eq!(
            extract_abstract_html(html, Some(AbstractSource::Usenix)).as_deref(),
            Some("This is the USENIX abstract.")
        );
    }

    #[test]
    fn html_neurips_source_specific_selector() {
        let html = r#"<html><body>
            <section class="paper-section">
                <h2 class="section-label">Abstract</h2>
                <p class="paper-abstract"><p>NeurIPS abstract text.</p></p>
            </section>
        </body></html>"#;
        assert_eq!(
            extract_abstract_html(html, Some(AbstractSource::Neurips)).as_deref(),
            Some("NeurIPS abstract text.")
        );
    }

    #[test]
    fn html_ieee_prefers_abstract_text_selector() {
        let html = r#"<html><head>
            <meta property="og:description" content="IEEE fallback abstract.">
        </head><body>
            <div class="abstract-text">This is the IEEE abstract.</div>
        </body></html>"#;
        assert_eq!(
            extract_abstract_html(html, Some(AbstractSource::Ieee)).as_deref(),
            Some("This is the IEEE abstract.")
        );
    }

    #[test]
    fn html_springer_prefers_full_abstract_section_over_truncated_meta() {
        let html = r#"<html><head>
            <meta property="og:description" content="Truncated Springer abstract...">
        </head><body>
            <section data-title="Abstract">
                <div class="c-article-section__content">
                    <p>This is the full Springer abstract.</p>
                    <p>It has a second sentence.</p>
                </div>
            </section>
        </body></html>"#;
        assert_eq!(
            extract_abstract_html(html, Some(AbstractSource::Springer)).as_deref(),
            Some("This is the full Springer abstract. It has a second sentence.")
        );
    }

    #[test]
    fn html_generic_meta_fallback() {
        let html = r#"<html><head>
            <meta property="og:description" content="Fallback abstract here.">
        </head><body></body></html>"#;
        assert_eq!(
            extract_abstract_html(html, None).as_deref(),
            Some("Fallback abstract here.")
        );
    }

    #[test]
    fn html_meta_fallback_decodes_entities() {
        let html = r#"<html><head>
            <meta property="og:description" content="A&amp;#160;B &amp;amp; C &#8217;">
        </head><body></body></html>"#;
        assert_eq!(
            extract_abstract_html(html, None).as_deref(),
            Some("A B & C ’")
        );
    }

    #[test]
    fn html_no_abstract() {
        assert!(extract_abstract_html("<html></html>", None).is_none());
    }

    #[test]
    fn static_url_rejects_local_or_non_http_targets() {
        assert!(parse_static_url("https://example.com/paper").is_some());
        assert!(parse_static_url("file:///etc/passwd").is_none());
        assert!(parse_static_url("http://localhost/paper").is_none());
        assert!(parse_static_url("http://127.0.0.1/paper").is_none());
        assert!(parse_static_url("http://[::1]/paper").is_none());
        assert!(parse_static_url("https://user@example.com/paper").is_none());
    }
}
