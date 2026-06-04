const LOAD_BATCH = 120;

let state = {
  query: '',
  sort: 'year',
  results: [],
  total: null,
  selected: 0,
  error: '',
  loading: false,
  hasMore: false,
  abortController: null,
  debounceTimer: null,
  searchInput: null,
  resultsEl: null,
  sentinel: null,
  statusEl: null,
  detailEl: null,
  sentinelObserver: null,
  renderedCount: 0,
};

function init() {
  state.searchInput = document.getElementById('search');
  state.resultsEl = document.getElementById('results');
  state.statusEl = document.getElementById('status');
  state.detailEl = document.getElementById('detail');

  // Create persistent sentinel element once
  state.sentinel = document.createElement('div');
  state.sentinel.className = 'sentinel';
  state.resultsEl.appendChild(state.sentinel);

  state.searchInput.addEventListener('input', onInput);
  state.searchInput.addEventListener('keydown', onSearchKey);
  window.addEventListener('keydown', onGlobalKey);

  document.querySelectorAll('.sort-btn').forEach(btn => {
    btn.addEventListener('click', () => setSort(btn.dataset.sort));
  });

  // Set up IntersectionObserver once
  state.sentinelObserver = new IntersectionObserver((entries) => {
    if (entries[0].isIntersecting) loadMore();
  }, { rootMargin: '100px' });
  state.sentinelObserver.observe(state.sentinel);

  refresh().then(() => {
    updateSortButtons();
    state.searchInput.focus();
  });
}



function onInput() {
  state.query = state.searchInput.value;
  if (state.debounceTimer) clearTimeout(state.debounceTimer);
  state.debounceTimer = setTimeout(() => refresh(), 300);
}

function setSort(sort) {
  if (state.sort === sort) return;
  state.sort = sort;
  updateSortButtons();
  if (state.debounceTimer) clearTimeout(state.debounceTimer);
  state.debounceTimer = setTimeout(() => refresh(), 300);
}

function updateSortButtons() {
  document.querySelectorAll('.sort-btn').forEach(btn => {
    btn.classList.toggle('active', btn.dataset.sort === state.sort);
  });
}

async function performSearch(q, s, offset = 0, limit = LOAD_BATCH) {
  if (state.abortController) {
    state.abortController.abort();
  }
  const controller = new AbortController();
  state.abortController = controller;

  state.loading = true;
  state.error = '';
  updateStatus();

  try {
    const params = new URLSearchParams();
    params.set('q', q);
    params.set('sort', s);
    params.set('limit', String(limit));
    params.set('offset', String(offset));
    const res = await fetch(`/api/search?${params.toString()}`, { signal: controller.signal });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const data = await res.json();
    return data.papers || [];
  } catch (e) {
    if (e.name === 'AbortError') return null;
    state.error = e.message || 'search failed';
    return null;
  } finally {
    if (state.abortController === controller) {
      state.abortController = null;
    }
    state.loading = false;
  }
}

async function refresh() {
  state.hasMore = true;
  state.selected = 0;
  state.renderedCount = 0;
  const papers = await performSearch(state.query, state.sort, 0);
  if (!papers) return;
  state.results = papers;
  state.hasMore = papers.length >= LOAD_BATCH;
  state.total = null;
  renderResults(false);
  renderDetail();
  updateStatus();
}

async function loadMore() {
  if (state.loading) return;
  if (!state.hasMore) return;
  if (state.total !== null && state.results.length >= state.total) {
    state.hasMore = false;
    updateStatus();
    return;
  }
  const papers = await performSearch(state.query, state.sort, state.results.length);
  if (!papers) return;
  if (papers.length === 0) {
    state.hasMore = false;
    state.total = state.results.length;
    updateStatus();
    return;
  }
  const prevLength = state.results.length;
  state.results = state.results.concat(papers);
  state.hasMore = papers.length >= LOAD_BATCH;
  renderResults(true);
  renderDetail();
  updateStatus();
}

function setSelected(idx) {
  prevSelected = state.selected;
  state.selected = idx;
  renderResults();
  renderDetail();
  updateStatus();
}

function openSelected() {
  const paper = state.results[state.selected];
  if (!paper) return;
  const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : '');
  if (url) window.open(url, '_blank');
}

function onSearchKey(e) {
  if (e.key === 'Enter') {
    e.preventDefault();
    openSelected();
  }
}

function onGlobalKey(e) {
  if (e.key === 'ArrowDown') {
    e.preventDefault();
    const bound = (state.total !== null ? state.total : state.results.length) - 1;
    const nextIdx = Math.min(state.selected + 1, bound);
    setSelected(nextIdx);
    // Trigger loadMore if we're near the end (last 5 items)
    if (nextIdx >= state.results.length - 5 && state.hasMore && !state.loading) {
      loadMore();
    }
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    setSelected(Math.max(state.selected - 1, 0));
  } else if (e.key === 'Enter') {
    e.preventDefault();
    openSelected();
  } else if (e.key === 'Tab' && !e.shiftKey) {
    e.preventDefault();
    cycleSort(1);
  } else if (e.key === 'Tab' && e.shiftKey) {
    e.preventDefault();
    cycleSort(-1);
  }
}

function cycleSort(dir) {
  const sorts = ['year', 'relevance', 'venue'];
  const idx = sorts.indexOf(state.sort);
  const next = (idx + dir + sorts.length) % sorts.length;
  setSort(sorts[next]);
}

function sanitize(str) {
  const el = document.createElement('div');
  el.textContent = str;
  return el.innerHTML;
}

let prevSelected = -1;

function renderResults(appendOnly = false) {
  const container = state.resultsEl;
  const fragment = document.createDocumentFragment();

  if (appendOnly) {
    // Only render new rows that haven't been rendered yet
    for (let i = state.renderedCount; i < state.results.length; i++) {
      const paper = state.results[i];
      const row = createRow(paper, i);
      fragment.appendChild(row);
    }
    // Insert new rows before the sentinel
    while (fragment.firstChild) {
      container.insertBefore(fragment.firstChild, state.sentinel);
    }
  } else {
    // Full re-render: clear all rows and render everything
    while (container.firstChild && container.firstChild !== state.sentinel) {
      container.removeChild(container.firstChild);
    }
    for (let i = 0; i < state.results.length; i++) {
      const paper = state.results[i];
      const row = createRow(paper, i);
      fragment.appendChild(row);
    }
    while (fragment.firstChild) {
      container.insertBefore(fragment.firstChild, state.sentinel);
    }
  }

  state.renderedCount = state.results.length;
  prevSelected = state.selected;
}

function createRow(paper, idx) {
  const row = document.createElement('div');
  row.className = idx === state.selected ? 'result-row selected' : 'result-row';
  row.innerHTML = `
    <span class="col-venue">${sanitize(paper.venue.padEnd(10, ' '))}</span>
    <span class="col-year">${sanitize(String(paper.year))}</span>
    <span class="col-title">${sanitize(paper.title)}</span>
    <span class="col-authors">${sanitize(paper.authors)}</span>
  `;
  row.addEventListener('click', () => setSelected(idx));
  row.addEventListener('dblclick', openSelected);
  if (idx === state.selected && state.selected !== prevSelected) {
    requestAnimationFrame(() => {
      row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    });
  }
  return row;
}

function renderDetail() {
  const paper = state.results[state.selected];
  const el = state.detailEl;
  if (!paper) {
    el.innerHTML = '<div class="detail-empty">No paper selected</div>';
    return;
  }
  const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : undefined);
  el.innerHTML = `
    <div class="detail-content">
      <h2 class="detail-title">${sanitize(paper.title)}</h2>
      <div class="detail-meta">
        <span>${sanitize(paper.venue)}</span> · <span>${sanitize(String(paper.year))}</span>
      </div>
      <div class="detail-authors">
        <strong>Authors</strong> ${sanitize(paper.authors)}
      </div>
      ${paper.doi ? `<div class="detail-link"><strong>DOI</strong> ${sanitize(paper.doi)}</div>` : ''}
      ${url ? `<div class="detail-link"><strong>URL</strong> <a href="${sanitize(url)}" target="_blank" rel="noreferrer">${sanitize(url)}</a></div>` : ''}
      <div class="detail-abstract">
        <strong>Abstract</strong>
        <p>${sanitize(paper.abstract || 'No abstract available.')}</p>
      </div>
    </div>
  `;
}

function updateStatus() {
  const el = state.statusEl;
  if (state.error) {
    el.innerHTML = `<span class="error">${sanitize(state.error)}</span>`;
    return;
  }
  let html = '';
  if (state.total !== null && state.results.length > 0) {
    html = `<span>${state.total} papers · sort ${state.sort} · ${state.selected + 1}/${state.total}</span>`;
  } else if (state.results.length > 0) {
    html = `<span>${state.results.length}${state.hasMore ? '+' : ''} papers · sort ${state.sort} · ${state.selected + 1}/${state.results.length}</span>`;
  }
  if (state.loading) {
    html += `<span class="spinner">Loading…</span>`;
  }
  el.innerHTML = html;
}

init();
