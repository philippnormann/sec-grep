use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use clap::Parser;
use sec_grep_core::config::Config;
use sec_grep_core::db::Database;
use sec_grep_core::{build_search, Paper, SearchOptions, output};
use sec_grep_core::db::Sort;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const INDEX_HTML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.html"));
const STYLES_CSS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/styles.css"));
const APP_JS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.js"));

/// Access log middleware - logs method, path, status, latency
async fn access_log(
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    let start = Instant::now();
    let method = req.method().clone();
    let uri = req.uri().to_string();
    let response = next.run(req).await;
    let latency = start.elapsed().as_millis();
    info!("{} {} {} {}ms", method, uri, response.status().as_u16(), latency);
    response
}

/// Application state shared across all requests
#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    db: Arc<Mutex<Database>>,
    ranks: Arc<HashMap<String, String>>,
}

#[derive(Parser, Debug)]
#[command(name = "sec-grep-web", version)]
struct Args {
    /// Host to listen on
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Port to listen on
    #[arg(short, long, default_value_t = 5002)]
    port: u16,
    /// Override database path
    #[arg(long)]
    db: Option<PathBuf>,
}

#[derive(Deserialize, Debug)]
struct SearchParams {
    q: Option<String>,
    sort: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
}

#[derive(Deserialize, Debug)]
struct BibtexParams {
    key: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    papers: Vec<Paper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ranks: Option<HashMap<String, String>>,
}

/// Build a JSON error response for search endpoints.
fn search_error(status: StatusCode, message: &str) -> axum::response::Response {
    (
        status,
        Json(SearchResponse {
            papers: vec![],
            error: Some(message.to_string()),
            ranks: None,
        }),
    )
        .into_response()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .with_target(false)
        .without_time()
        .init();

    let args = Args::parse();

    // Resolve paths
    let paths = sec_grep_core::config::Paths::resolve()
        .context("failed to resolve paths")?;
    let db_path = args.db.unwrap_or_else(|| paths.db_path());
    let config_path = paths.user_venues_path();

    // Load config and open database once at startup
    let config = Arc::new(
        Config::load(Some(&config_path))
            .context("loading venue config")?
    );
    let db = Arc::new(Mutex::new(
        Database::open_existing(&db_path)
            .with_context(|| format!("no database at {}", db_path.display()))?
    ));

    let mut ranks = HashMap::new();
    for venue in &config.venues {
        if let Some(rank) = venue.rank.as_deref().filter(|r| !r.is_empty()) {
            ranks.insert(venue.id.clone(), rank.to_string());
        }
    }
    let state = AppState { config, db, ranks: Arc::new(ranks) };

    let app = Router::new()
        .route("/api/search", get(api_search))
        .route(
            "/static/styles.css",
            get(|| async { ([(header::CONTENT_TYPE, "text/css")], STYLES_CSS) }),
        )
        .route(
            "/static/app.js",
            get(|| async { ([(header::CONTENT_TYPE, "application/javascript")], APP_JS) }),
        )
        .route("/api/bibtex", get(api_bibtex))
        .fallback(|| async { Html(INDEX_HTML) })
        .layer(middleware::from_fn(access_log))
        .with_state(state);

    let addr: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid address: {}:{}", args.host, args.port))?;
    info!("sec-grep web UI listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {}", addr))?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn api_search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> impl IntoResponse {
    let sort = parse_sort(params.sort.as_deref(), &state.config);
    let query = params.q.as_deref().unwrap_or("");
    let limit = params.limit.as_deref().unwrap_or("120").parse::<usize>().ok();
    let offset = params.offset.as_deref().unwrap_or("0").parse::<usize>().ok();

    // Build search
    let search = match build_search(
        query,
        &state.config,
        SearchOptions {
            venues: &[],
            ranks: &[],
            tags: &[],
            years: &[],
            sort,
            limit,
            offset,
        },
    ) {
        Ok(search) => search,
        Err(e) => {
            warn!("search build error: {}", e);
            return search_error(StatusCode::BAD_REQUEST, &e.to_string());
        }
    };

    // Execute search (acquire lock)
    let db = match state.db.lock() {
        Ok(db) => db,
        Err(e) => {
            warn!("db lock error: {}", e);
            return search_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
        }
    };

    let papers = match db.search(&search) {
        Ok(papers) => papers,
        Err(e) => {
            warn!("db search error: {}", e);
            return search_error(StatusCode::BAD_REQUEST, &e.to_string());
        }
    };

    (
        StatusCode::OK,
        Json(SearchResponse {
            papers,
            error: None,
            ranks: Some((*state.ranks).clone()),
        }),
    )
        .into_response()
}

async fn api_bibtex(
    State(state): State<AppState>,
    Query(params): Query<BibtexParams>,
) -> impl IntoResponse {
    let db = match state.db.lock() {
        Ok(db) => db,
        Err(e) => {
            warn!("db lock error: {}", e);
            return search_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
        }
    };

    let paper = match db.get_by_key(&params.key) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return search_error(StatusCode::NOT_FOUND, "paper not found");
        }
        Err(e) => {
            warn!("db error: {}", e);
            return search_error(StatusCode::INTERNAL_SERVER_ERROR, "internal server error");
        }
    };

    let bibtex = output::render(&[paper], output::Format::Bibtex, None).unwrap();

    (StatusCode::OK, bibtex).into_response()
}

/// Parse sort string into Sort enum.
fn parse_sort(sort_str: Option<&str>, config: &Config) -> Sort {
    match sort_str.unwrap_or("year") {
        "relevance" => Sort::Relevance,
        "venue" => Sort::Venue,
        "rank" => Sort::Rank(config.rank_sort_order()),
        _ => Sort::Year,
    }
}

/// Minimal `oneshot` replacement — run a single request against an axum Router
/// without binding a network port. Avoids a direct `tower` dev-dependency.
fn oneshot<S>(svc: S, req: axum::http::Request<axum::body::Body>) -> S::Response
where
    S: axum::Service<axum::http::Request<axum::body::Body>, Response = axum::response::Response> + Send + 'static,
    S::Error: Send,
{
    let svc = std::sync::Arc::new(svc);
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let res = svc.ready().await.and_then(|svc| svc.call(req));
        let _ = tx.send(res);
    });
    futures::executor::block_on(rx).expect("response channel closed").expect("service call failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sec_grep_core::config::Config;
    use sec_grep_core::db::Database;
    use sec_grep_core::Paper;
    use std::collections::HashMap;

    fn paper(key: &str, venue: &str, year: i32, title: &str, abs: Option<&str>) -> Paper {
        Paper {
            dblp_key: key.into(),
            venue: venue.into(),
            year,
            title: title.into(),
            authors: "Alice Smith".into(),
            doi: Some("10.1/x".into()),
            url: Some("https://example.com".into()),
            abstract_text: abs.map(|s| s.into()),
        }
    }

    fn test_state() -> AppState {
        let mut db = Database::open_in_memory().unwrap();
        db.upsert_papers(&[
            paper("k1", "NDSS", 2020, "Fuzzing the Linux kernel", Some("we fuzz kernels")),
            paper("k2", "CCS", 2021, "Side channel attacks", None),
            paper("k3", "SP", 2019, "Kernel exploitation", Some("rop chains")),
        ])
        .unwrap();

        let config = Config::defaults().unwrap();
        let ranks = HashMap::new();

        AppState {
            config: Arc::new(config),
            db: Arc::new(Mutex::new(db)),
            ranks: Arc::new(ranks),
        }
    }

    fn create_test_app(state: AppState) -> Router {
        Router::new()
            .route("/api/search", get(api_search))
            .route(
                "/static/styles.css",
                get(|| async { ([(header::CONTENT_TYPE, "text/css")], STYLES_CSS) }),
            )
            .route(
                "/static/app.js",
                get(|| async {
                    (
                        [(header::CONTENT_TYPE, "application/javascript")],
                        APP_JS,
                    )
                }),
            )
            .route("/api/bibtex", get(api_bibtex))
            .fallback(|| async { Html(INDEX_HTML) })
            .with_state(state)
    }

    #[test]
    fn search_empty_query_returns_all_papers() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(app, Request::builder().uri("/api/search").body(Body::empty()).unwrap());

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn search_with_query_filters_papers() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/search?q=fuzzing")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: SearchResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(json.papers.len(), 1);
        assert_eq!(json.papers[0].title, "Fuzzing the Linux kernel");
    }

    #[test]
    fn search_with_invalid_query_returns_error() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/search?q=year:notanumber")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: SearchResponse = serde_json::from_slice(&body).unwrap();
        assert!(json.error.is_some());
        assert!(json.papers.is_empty());
    }

    #[test]
    fn search_with_limit_returns_bounded_results() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/search?limit=2")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: SearchResponse = serde_json::from_slice(&body).unwrap();
        assert!(json.papers.len() <= 2);
    }

    #[test]
    fn search_with_offset_skips_results() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/search?offset=2&limit=10")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: SearchResponse = serde_json::from_slice(&body).unwrap();
        // With 3 papers and offset=2, we should get at most 1
        assert!(json.papers.len() <= 1);
    }

    #[test]
    fn search_with_sort_parameter() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/search?sort=year")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: SearchResponse = serde_json::from_slice(&body).unwrap();
        assert!(!json.papers.is_empty());
    }

    #[test]
    fn bibtex_returns_plain_text() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/bibtex?key=k1")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("@"));
    }

    #[test]
    fn bibtex_missing_key_returns_404() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/api/bibtex?key=nonexistent")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn static_css_returns_200_with_correct_content_type() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/static/styles.css")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(content_type, "text/css");
    }

    #[test]
    fn static_js_returns_200_with_correct_content_type() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/static/app.js")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(content_type, "application/javascript");
    }

    #[test]
    fn fallback_returns_html() {
        let state = test_state();
        let app = create_test_app(state);

        let response = oneshot(
            app,
            Request::builder()
                .uri("/some/unknown/path")
                .body(Body::empty())
                .unwrap(),
        );

        assert_eq!(response.status(), StatusCode::OK);
        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/html"));
    }

    #[test]
    fn parse_sort_defaults_to_year() {
        let config = Config::defaults().unwrap();
        assert_eq!(parse_sort(None, &config), Sort::Year);
    }

    #[test]
    fn parse_sort_handles_relevance() {
        let config = Config::defaults().unwrap();
        assert_eq!(parse_sort(Some("relevance"), &config), Sort::Relevance);
    }

    #[test]
    fn parse_sort_handles_venue() {
        let config = Config::defaults().unwrap();
        assert_eq!(parse_sort(Some("venue"), &config), Sort::Venue);
    }

    #[test]
    fn parse_sort_handles_rank() {
        let config = Config::defaults().unwrap();
        assert_eq!(
            parse_sort(Some("rank"), &config),
            Sort::Rank(config.rank_sort_order())
        );
    }

    #[test]
    fn parse_sort_unknown_defaults_to_year() {
        let config = Config::defaults().unwrap();
        assert_eq!(parse_sort(Some("unknown"), &config), Sort::Year);
    }
}

