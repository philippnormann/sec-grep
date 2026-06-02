mod tui;

use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use futures::stream::{self, StreamExt};

use sec_grep_core::abstracts::Enricher;
use sec_grep_core::config::{Config, Paths, Secrets};
use sec_grep_core::db::{Database, Search, Sort};
use sec_grep_core::dblp::Dblp;
use sec_grep_core::output::{self, Column, Format};
use sec_grep_core::query;

/// Upper bound for dblp year filters; papers never exceed this.
const MAX_YEAR: i32 = 2100;

#[derive(Parser)]
#[command(
    name = "sec-grep",
    about = "Search security conference papers beyond the top-4 venues",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Query string (default action is search). Supports AND/OR/NOT, "phrases",
    /// title:/author:/abstract: text fields, metadata filters
    /// (venue:/year:/rank:/tag:/doi:), and prefix*.
    #[arg(value_name = "QUERY")]
    query: Vec<String>,

    /// Restrict to venues (id or alias), comma- or space-separated.
    #[arg(long, value_delimiter = ',')]
    venue: Vec<String>,

    /// Restrict by year: 2020, 2018-2024, 2020-, or -2019.
    #[arg(long)]
    year: Option<String>,

    /// Restrict by rank label (e.g. A*, A, B).
    #[arg(long, value_delimiter = ',')]
    rank: Vec<String>,

    /// Restrict by tag (e.g. crypto, systems).
    #[arg(long, value_delimiter = ',')]
    tag: Vec<String>,

    /// Result ordering (default: relevance).
    #[arg(long, value_enum)]
    sort: Option<SortArg>,

    /// Output format (default: table).
    #[arg(long, value_parser = parse_format_arg)]
    format: Option<Format>,

    /// Limit number of results.
    #[arg(long)]
    limit: Option<usize>,

    /// Columns for table/csv output (comma-separated).
    #[arg(long, value_delimiter = ',')]
    fields: Vec<String>,

    /// Launch the interactive TUI.
    #[arg(long)]
    tui: bool,

    /// Override database path.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Override user venues.yaml path.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

impl Cli {
    fn has_search_args(&self) -> bool {
        !self.query.is_empty()
            || !self.venue.is_empty()
            || self.year.is_some()
            || !self.rank.is_empty()
            || !self.tag.is_empty()
            || self.sort.is_some()
            || self.format.is_some()
            || self.limit.is_some()
            || !self.fields.is_empty()
            || self.tui
    }
}

#[derive(Subcommand)]
enum Command {
    /// Create the data/config directories and an empty database.
    Init,
    /// Fetch paper metadata from dblp (incremental, idempotent).
    Update(UpdateArgs),
    /// Fill missing abstracts on the existing database (no dblp re-fetch).
    Enrich(EnrichArgs),
}

/// Default number of concurrent abstract fetches.
const DEFAULT_JOBS: usize = 8;
const MIN_ENRICH_BATCH: usize = 64;
const MAX_ENRICH_BATCH: usize = 512;

#[derive(clap::Args)]
struct UpdateArgs {
    /// Only ingest from these venues (id or alias).
    #[arg(long, value_delimiter = ',')]
    venue: Vec<String>,
    /// Minimum year (overrides config default).
    #[arg(long)]
    since: Option<i32>,
    /// Also fetch abstracts (slower; uses API keys, then static scrapers).
    #[arg(long)]
    abstracts: bool,
    /// Concurrent abstract fetches (only with --abstracts).
    #[arg(long, default_value_t = DEFAULT_JOBS)]
    jobs: usize,
}

#[derive(clap::Args)]
struct EnrichArgs {
    /// Only enrich these venues (id or alias); default is all.
    #[arg(long, value_delimiter = ',')]
    venue: Vec<String>,
    /// Concurrent abstract fetches.
    #[arg(long, default_value_t = DEFAULT_JOBS)]
    jobs: usize,
    /// Stop after this many papers (useful for sampling / validation).
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum SortArg {
    Relevance,
    Year,
    Venue,
}

impl From<SortArg> for Sort {
    fn from(s: SortArg) -> Self {
        match s {
            SortArg::Relevance => Sort::Relevance,
            SortArg::Year => Sort::Year,
            SortArg::Venue => Sort::Venue,
        }
    }
}

pub(crate) struct SearchOptions<'a> {
    pub(crate) venues: &'a [String],
    pub(crate) ranks: &'a [String],
    pub(crate) tags: &'a [String],
    pub(crate) year: Option<&'a str>,
    pub(crate) sort: Sort,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .with_target(false)
        .without_time()
        .init();

    let cli = Cli::parse();
    reject_search_args_for_subcommands(&cli)?;
    let paths = Paths::resolve()?;
    let config = load_config(&cli, &paths)?;

    match &cli.command {
        Some(Command::Init) => cmd_init(&cli, &paths),
        Some(Command::Update(args)) => cmd_update(args, &cli, &paths, &config).await,
        Some(Command::Enrich(args)) => cmd_enrich(args, &cli, &paths, &config).await,
        None if cli.tui => {
            let db = open_db(&cli, &paths)?;
            tui::run(db, config)
        }
        None => cmd_search(&cli, &paths, &config),
    }
}

fn reject_search_args_for_subcommands(cli: &Cli) -> Result<()> {
    if cli.command.is_some() && cli.has_search_args() {
        anyhow::bail!(
            "search query/options cannot be used with subcommands; put command-specific options after the subcommand"
        );
    }
    Ok(())
}

fn log_header(title: &str) {
    eprintln!("{title}");
}

fn log_field(label: &str, value: impl std::fmt::Display) {
    eprintln!("  {label:<10} {value}");
}

fn log_blank() {
    eprintln!();
}

fn write_stdout(out: &str) {
    if out.is_empty() {
        return;
    }
    print!("{out}");
    if !out.ends_with('\n') {
        println!();
    }
}

fn flush_stderr() {
    let _ = std::io::stderr().flush();
}

fn load_config(cli: &Cli, paths: &Paths) -> Result<Config> {
    let user_path = config_path(cli, paths);
    Config::load(Some(&user_path)).context("loading venue config")
}

fn config_path(cli: &Cli, paths: &Paths) -> PathBuf {
    cli.config
        .clone()
        .unwrap_or_else(|| paths.user_venues_path())
}

fn db_path(cli: &Cli, paths: &Paths) -> PathBuf {
    cli.db.clone().unwrap_or_else(|| paths.db_path())
}

fn open_db(cli: &Cli, paths: &Paths) -> Result<Database> {
    let path = db_path(cli, paths);
    Database::open_existing(&path).with_context(|| {
        format!(
            "no database at {}; run `sec-grep init` then `sec-grep update`",
            path.display()
        )
    })
}

fn cmd_init(cli: &Cli, paths: &Paths) -> Result<()> {
    paths.ensure_dirs()?;
    let path = db_path(cli, paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Database::open(&path).context("creating database")?;
    log_header("sec-grep initialized");
    log_field("database", path.display());
    log_field("config", config_path(cli, paths).display());
    log_blank();
    log_field("next", "`sec-grep update`");
    Ok(())
}

fn cmd_search(cli: &Cli, paths: &Paths, config: &Config) -> Result<()> {
    let db = open_db(cli, paths)?;

    let raw = cli.query.join(" ");
    let search = build_search(
        &raw,
        config,
        SearchOptions {
            venues: &cli.venue,
            ranks: &cli.rank,
            tags: &cli.tag,
            year: cli.year.as_deref(),
            sort: cli.sort.unwrap_or(SortArg::Relevance).into(),
            limit: cli.limit,
            offset: None,
        },
    )?;
    let papers = db.search(&search)?;

    let columns = parse_columns(&cli.fields)?;
    let format = cli.format.unwrap_or(Format::Table);
    let out =
        output::render(&papers, format, columns.as_deref()).map_err(|e| anyhow::anyhow!(e))?;
    write_stdout(&out);
    if matches!(format, Format::Table) {
        eprintln!("results    {}", papers.len());
    }
    Ok(())
}

pub(crate) fn build_search(
    raw_query: &str,
    config: &Config,
    options: SearchOptions<'_>,
) -> sec_grep_core::Result<Search> {
    let parsed = query::parse(raw_query)?;
    let cli_year = match options.year {
        Some(year) => query::parse_year_range(year)?,
        None => (None, None),
    };
    let (year_min, year_max) =
        query::merge_year_bounds((parsed.year_min, parsed.year_max), cli_year)?;
    let venue_filter = config
        .resolve_venue_filter(options.venues, options.ranks, options.tags)?
        .intersect(parsed.resolve_venue_filter(config)?);

    Ok(Search {
        fts: parsed.fts,
        venue_filter,
        doi_terms: parsed.doi_terms,
        year_min,
        year_max,
        sort: options.sort,
        limit: options.limit,
        offset: options.offset,
    })
}

fn parse_columns(fields: &[String]) -> Result<Option<Vec<Column>>> {
    if fields.is_empty() {
        return Ok(None);
    }
    let cols = fields
        .iter()
        .map(|f| f.parse::<Column>())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!(e))?;
    Ok(Some(cols))
}

fn parse_format_arg(value: &str) -> std::result::Result<Format, String> {
    value.parse::<Format>().map_err(|e| e.to_string())
}

async fn cmd_update(args: &UpdateArgs, cli: &Cli, paths: &Paths, config: &Config) -> Result<()> {
    paths.ensure_dirs()?;
    let path = db_path(cli, paths);
    let mut db = Database::open(&path).context("opening database")?;

    let venue_ids = if args.venue.is_empty() {
        config.all_venue_ids()
    } else {
        config.resolve_venues(&args.venue)?
    };
    let min_year = args.since.unwrap_or(config.defaults.min_year);

    log_header("sec-grep update");
    log_field("venues", venue_ids.len());
    log_field("since", min_year);
    log_blank();

    let dblp = Dblp::default();
    let mut total = 0usize;
    let mut failed = Vec::new();
    for id in &venue_ids {
        let venue = config.venue(id).expect("resolved venue");
        eprint!("  {id:<12} ");
        flush_stderr();
        match dblp.fetch_venue(venue, min_year, MAX_YEAR).await {
            Ok(papers) => {
                let n = db.upsert_papers(&papers)?;
                total += papers.len();
                eprintln!("fetched {:>5} papers, {:>5} upserted", papers.len(), n);
            }
            Err(e) => {
                eprintln!("failed   {e}");
                failed.push(id.clone());
            }
        }
    }
    log_blank();
    log_header("summary");
    log_field("fetched", format_args!("{total} papers"));
    log_field("failed", failed.len());
    log_field("database", format_args!("{} papers", db.count()?));

    if !failed.is_empty() {
        anyhow::bail!("failed to fetch venue(s): {}", failed.join(", "));
    }

    if args.abstracts {
        log_blank();
        enrich_abstracts(&mut db, config, &venue_ids, args.jobs, None).await?;
    }
    Ok(())
}

async fn cmd_enrich(args: &EnrichArgs, cli: &Cli, paths: &Paths, config: &Config) -> Result<()> {
    let mut db = open_db(cli, paths)?;
    let venue_ids = if args.venue.is_empty() {
        Vec::new()
    } else {
        config.resolve_venues(&args.venue)?
    };
    enrich_abstracts(&mut db, config, &venue_ids, args.jobs, args.limit).await
}

/// Fill missing abstracts, running up to `jobs` fetches concurrently.
/// `venue_ids` empty means all venues; `limit` caps how many are attempted.
async fn enrich_abstracts(
    db: &mut Database,
    config: &Config,
    venue_ids: &[String],
    jobs: usize,
    limit: Option<usize>,
) -> Result<()> {
    let enricher = Enricher::new(Secrets::load());
    let pending = db.count_missing_abstracts(venue_ids)?;
    let total = limit.map_or(pending, |limit| limit.min(pending));
    let jobs = jobs.max(1);
    let batch_size = enrich_batch_size(jobs);
    log_header("abstract enrichment");
    log_field("pending", format_args!("{total} abstracts"));
    log_field("jobs", jobs);
    log_field("strategy", "DOI APIs, then static scrape");
    log_blank();

    let enricher = &enricher;
    let mut filled = 0usize;
    let mut processed = 0usize;
    let mut after_id = 0;
    while processed < total {
        let remaining = total - processed;
        let batch =
            db.papers_missing_abstract_batch(venue_ids, after_id, remaining.min(batch_size))?;
        let Some(next_after_id) = batch.last().map(|paper| paper.id) else {
            break;
        };
        after_id = next_after_id;

        let mut stream = stream::iter(batch.into_iter().map(|missing| async move {
            let paper = missing.paper;
            let source = config.venue(&paper.venue).and_then(|v| v.abstract_source);
            let dblp_key = paper.dblp_key.clone();
            (dblp_key, enricher.enrich(&paper, source).await)
        }))
        .buffer_unordered(jobs);

        while let Some((dblp_key, res)) = stream.next().await {
            processed += 1;
            match res {
                Ok(Some(abs)) => {
                    db.set_abstract(&dblp_key, &abs)?;
                    filled += 1;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("abstract fetch failed for {dblp_key}: {e}"),
            }
            if processed % 25 == 0 {
                log_field(
                    "progress",
                    format_args!("{processed}/{total} processed, {filled} filled"),
                );
            }
        }
    }
    log_blank();
    log_header("summary");
    log_field("filled", format_args!("{filled}/{total} abstracts"));
    Ok(())
}

fn enrich_batch_size(jobs: usize) -> usize {
    jobs.saturating_mul(4)
        .clamp(MIN_ENRICH_BATCH, MAX_ENRICH_BATCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_search_preserves_empty_cli_venue_filter() {
        let config = Config::defaults().unwrap();
        let ranks = vec!["does-not-exist".to_string()];
        let search = build_search(
            "",
            &config,
            SearchOptions {
                venues: &[],
                ranks: &ranks,
                tags: &[],
                year: None,
                sort: Sort::Relevance,
                limit: None,
                offset: None,
            },
        )
        .unwrap();

        assert!(search.venue_filter.is_empty());
    }
}
