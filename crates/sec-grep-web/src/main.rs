use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::header,
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

/// Application state shared across all requests
#[derive(Clone)]
struct AppState {
    config: Arc<Config>,
    db: Arc<Mutex<Database>>,
}

#[derive(Parser, Debug)]
#[command(name = "sec-grep-web", version)]
struct Args {
    /// Port to listen on
    #[arg(short, long, default_value = "5002")]
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

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    papers: Vec<Paper>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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

    let state = AppState { config, db };

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
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
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
    // Parse parameters with defaults
    let sort = match params.sort.as_deref().unwrap_or("year") {
        "relevance" => Sort::Relevance,
        "venue" => Sort::Venue,
        _ => Sort::Year,
    };
    let limit = params.limit.as_deref().unwrap_or("320").parse::<usize>().ok();
    let offset = params.offset.as_deref().unwrap_or("0").parse::<usize>().ok();
    let query = params.q.as_deref().unwrap_or("");

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
            return (axum::http::StatusCode::BAD_REQUEST, Json(SearchResponse {
                papers: vec![],
                error: Some(e.to_string()),
            })).into_response();
        }
    };

    // Execute search (acquire lock)
    let db = match state.db.lock() {
        Ok(db) => db,
        Err(e) => {
            warn!("db lock error: {}", e);
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(SearchResponse {
                papers: vec![],
                error: Some("internal server error".to_string()),
            })).into_response();
        }
    };

    let papers = match db.search(&search) {
        Ok(papers) => papers,
        Err(e) => {
            warn!("db search error: {}", e);
            return (axum::http::StatusCode::BAD_REQUEST, Json(SearchResponse {
                papers: vec![],
                error: Some(e.to_string()),
            })).into_response();
        }
    };

    (axum::http::StatusCode::OK, Json(SearchResponse {
        papers,
        error: None,
    })).into_response()
}

async fn api_bibtex(
    State(state): State<AppState>,
    Query(params): Query<BibtexParams>,
) -> impl IntoResponse {
    let db = match state.db.lock() {
        Ok(db) => db,
        Err(e) => {
            warn!("db lock error: {}", e);
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(SearchResponse {
                papers: vec![],
                error: Some("internal server error".to_string()),
            })).into_response();
        }
    };

    let paper = match db.get_by_key(&params.key) {
        Ok(Some(p)) => p,
        Ok(None) => {
            return (axum::http::StatusCode::NOT_FOUND, Json(SearchResponse {
                papers: vec![],
                error: Some("paper not found".to_string()),
            })).into_response();
        }
        Err(e) => {
            warn!("db error: {}", e);
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(SearchResponse {
                papers: vec![],
                error: Some("internal server error".to_string()),
            })).into_response();
        }
    };

    let bibtex = output::render(&[paper], output::Format::Bibtex, None).unwrap();

    (axum::http::StatusCode::OK, bibtex).into_response()
}
