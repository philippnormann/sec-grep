/**
 * Main web UI for sec-grep.
 * Search bug fix: prevent infinite load loop by tracking hasMore state.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { Paper } from './types';

const LOAD_BATCH = 120;

export default function App() {
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<'relevance' | 'year' | 'venue'>('year');
  const [results, setResults] = useState<Paper[]>([]);
  const [total, setTotal] = useState<number | null>(null);
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState('');
  const [loading, setLoading] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const listEndRef = useRef<HTMLDivElement>(null);
  const selectedRef = useRef<HTMLDivElement>(null);
  const abortRef = useRef<AbortController | null>(null);

  const performSearch = useCallback(
    async (q: string, s: 'relevance' | 'year' | 'venue', offset = 0, limit = LOAD_BATCH) => {
      // Cancel any in-flight request before starting a new one
      if (abortRef.current) {
        abortRef.current.abort();
      }
      const controller = new AbortController();
      abortRef.current = controller;

      setLoading(true);
      setError('');
      try {
        const params = new URLSearchParams();
        params.set('q', q);
        params.set('sort', s);
        params.set('limit', String(limit));
        params.set('offset', String(offset));
        const res = await fetch(`/api/search?${params.toString()}`, {
          signal: controller.signal,
        });
        if (!res.ok) throw new Error(`HTTP ${res.status}`);
        const data = await res.json();
        return data.papers as Paper[];
      } catch (e: any) {
        if (e.name === 'AbortError') return null; // silently ignore cancelled requests
        setError(e.message || 'search failed');
        return null;
      } finally {
        setLoading(false);
      }
    },
    [],
  );

  const refresh = useCallback(
    async () => {
      setHasMore(true);
      setSelected(0);
      const papers = await performSearch(query, sort, 0);
      if (!papers) return;
      setResults(papers);
      // If first batch is smaller than load batch, we know there is no more.
      setHasMore(papers.length >= LOAD_BATCH);
      // We don't have total from backend; infer it later or leave null.
      setTotal(null);
    },
    [query, sort, performSearch],
  );

  // Initial load
  useEffect(() => {
    refresh();
    inputRef.current?.focus();
    return () => {
      abortRef.current?.abort();
    };
  }, []);

  // Debounced search on query / sort change
  useEffect(() => {
    const t = setTimeout(() => {
      refresh();
    }, 300);
    return () => clearTimeout(t);
  }, [query, sort, refresh]);

  // Infinite scroll loader
  const loadMore = useCallback(async () => {
    if (loading) return;
    if (!hasMore) return;
    if (total !== null && results.length >= total) {
      setHasMore(false);
      return;
    }
    const papers = await performSearch(query, sort, results.length);
    if (!papers) return;
    if (papers.length === 0) {
      setHasMore(false);
      setTotal(results.length);
      return;
    }
    setResults((prev) => [...prev, ...papers]);
    setHasMore(papers.length >= LOAD_BATCH);
  }, [query, sort, results.length, total, loading, hasMore, performSearch]);

  // IntersectionObserver for infinite scroll
  useEffect(() => {
    const el = listEndRef.current;
    if (!el) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries[0].isIntersecting) {
          loadMore();
        }
      },
      { rootMargin: '100px' },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [loadMore]);

  // Keyboard navigation
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        setSelected((prev) => Math.min(prev + 1, (total ?? results.length) - 1));
      } else if (e.key === 'ArrowUp') {
        e.preventDefault();
        setSelected((prev) => Math.max(prev - 1, 0));
      } else if (e.key === 'Enter' && document.activeElement !== inputRef.current) {
        e.preventDefault();
        openSelected();
      } else if (e.key === 'Tab' && !e.shiftKey) {
        e.preventDefault();
        setSort((prev) => (prev === 'year' ? 'relevance' : prev === 'relevance' ? 'venue' : 'year'));
      } else if (e.key === 'Tab' && e.shiftKey) {
        e.preventDefault();
        setSort((prev) => (prev === 'year' ? 'venue' : prev === 'venue' ? 'relevance' : 'year'));
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [total, results.length]);

  // Scroll selected item into view
  useEffect(() => {
    selectedRef.current?.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }, [selected]);

  const openSelected = () => {
    const paper = results[selected];
    if (!paper) return;
    const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : '');
    if (url) window.open(url, '_blank');
  };

  const detailPaper = results[selected] ?? undefined;

  return (
    <div className="app">
      <header className="header">
        <div className="search-row">
          <input
            ref={inputRef}
            className="search-input"
            type="text"
            placeholder="all papers"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <div className="sort-bar">
            {(['year', 'relevance', 'venue'] as const).map((s) => (
              <button
                key={s}
                className={sort === s ? 'sort-btn active' : 'sort-btn'}
                onClick={() => setSort(s)}
              >
                sort {s}
              </button>
            ))}
          </div>
        </div>
        <div className="status">
          {error && <span className="error">{error}</span>}
          {total !== null && !error && (
            <span>
              {total} papers · sort {sort} · {selected + 1}/{total}
            </span>
          )}
          {total === null && !error && results.length > 0 && (
            <span>
              {results.length}{hasMore ? '+' : ''} papers · sort {sort} · {selected + 1}/{results.length}
            </span>
          )}
          {loading && <span className="spinner">Loading…</span>}
        </div>
      </header>

      <main className="main">
        <section className="results">
          {results.map((paper, idx) => {
            const isSelected = idx === selected;
            return (
              <div
                key={paper.dblp_key}
                ref={isSelected ? selectedRef : undefined}
                className={isSelected ? 'result-row selected' : 'result-row'}
                onClick={() => setSelected(idx)}
                onDoubleClick={openSelected}
              >
                <span className="col-venue">{paper.venue.padEnd(10, ' ')}</span>
                <span className="col-year">{paper.year}</span>
                <span className="col-title">{paper.title}</span>
                <span className="col-authors">{paper.authors}</span>
              </div>
            );
          })}
          <div ref={listEndRef} className="sentinel" />
        </section>

        <aside className="detail">{<Detail paper={detailPaper} />}</aside>
      </main>

      <footer className="footer">
        <span>
          Tab sort · Enter open · ↑↓ move · {`"phrase"`} · title:term · venue:ndss · year:2020
        </span>
      </footer>
    </div>
  );
}

function Detail({ paper }: { paper?: Paper }) {
  if (!paper) return <div className="detail-empty">No paper selected</div>;
  const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : undefined);
  return (
    <div className="detail-content">
      <h2 className="detail-title">{paper.title}</h2>
      <div className="detail-meta">
        <span>{paper.venue}</span> · <span>{paper.year}</span>
      </div>
      <div className="detail-authors">
        <strong>Authors</strong> {paper.authors}
      </div>
      {paper.doi && (
        <div className="detail-link">
          <strong>DOI</strong> {paper.doi}
        </div>
      )}
      {url && (
        <div className="detail-link">
          <strong>URL</strong>{' '}
          <a href={url} target="_blank" rel="noreferrer">
            {url}
          </a>
        </div>
      )}
      <div className="detail-abstract">
        <strong>Abstract</strong>
        <p>{paper.abstract || 'No abstract available.'}</p>
      </div>
    </div>
  );
}
