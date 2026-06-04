use std::process::Command;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::{
    extract::Query,
    http::header,
    response::{Html, Json, IntoResponse},
    routing::get,
    Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

const INDEX_HTML: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/index.html"));
const STYLES_CSS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/styles.css"));
const APP_JS: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/app.js"));

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
    #[serde(default = "default_sort")]
    sort: String,
    #[serde(default = "default_limit")]
    limit: String,
    #[serde(default = "default_offset")]
    offset: String,
}

fn default_sort() -> String {
    "year".into()
}
fn default_limit() -> String {
    "320".into()
}
fn default_offset() -> String {
    "0".into()
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    papers: Vec<Value>,
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
    let db = args.db.clone();

    let app = Router::new()
        .route("/api/search", get(move |query| api_search(query, db.clone())))
        .route("/static/styles.css", get(serve_css))
        .route("/static/app.js", get(serve_js))
        .fallback(fallback);

    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!("sec-grep web UI listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind to {}", addr))?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn api_search(Query(params): Query<SearchParams>, db: Option<PathBuf>) -> impl IntoResponse {
    let mut cmd = Command::new("sec-grep");
    cmd.arg("--format").arg("json");
    cmd.arg("--sort").arg(&params.sort);
    cmd.arg("--limit").arg(&params.limit);
    cmd.arg("--offset").arg(&params.offset);

    if let Some(db) = &db {
        cmd.arg("--db").arg(db);
    }

    if let Some(q) = &params.q {
        if !q.is_empty() {
            cmd.arg(q);
        }
    }

    match tokio::task::spawn_blocking(move || cmd.output()).await {
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let msg = if stderr.trim().is_empty() {
                    "sec-grep failed".to_string()
                } else {
                    stderr.trim().to_string()
                };
                warn!("sec-grep error: {}", msg);
                let papers = parse_papers(&output.stdout).unwrap_or_default();
                let res = SearchResponse {
                    papers,
                    error: Some(msg),
                };
                return (axum::http::StatusCode::OK, Json(res)).into_response();
            }

            let papers = parse_papers(&output.stdout).unwrap_or_default();
            let res = SearchResponse {
                papers,
                error: None,
            };
            (axum::http::StatusCode::OK, Json(res)).into_response()
        }
        Ok(Err(e)) => {
            warn!("Failed to start sec-grep: {}", e);
            let res = SearchResponse {
                papers: vec![],
                error: Some("sec-grep not available".to_string()),
            };
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(res)).into_response()
        }
        Err(e) => {
            warn!("Blocking task failed: {}", e);
            let res = SearchResponse {
                papers: vec![],
                error: Some("internal server error".to_string()),
            };
            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(res)).into_response()
        }
    }
}

fn parse_papers(stdout: &[u8]) -> Option<Vec<Value>> {
    let text = std::str::from_utf8(stdout).ok()?.trim();
    if text.is_empty() {
        return Some(vec![]);
    }
    serde_json::from_str(text).ok()
}

async fn serve_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], STYLES_CSS)
}

async fn serve_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], APP_JS)
}

async fn fallback() -> Html<&'static str> {
    Html(INDEX_HTML)
}
