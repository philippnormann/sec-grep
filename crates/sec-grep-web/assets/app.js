const LOAD_BATCH = 120;
const SEARCH_DEBOUNCE_MS = 300;
const SORTS = ['year', 'relevance', 'venue'];

const dom = {
  searchInput: null,
  results: null,
  status: null,
  detail: null,
  sortButtons: [],
  sentinel: null,
};

const state = {
  query: '',
  sort: 'year',
  results: [],
  selected: 0,
  loading: false,
  hasMore: false,
  error: '',
};

let abortController = null;
let debounceTimer = null;
let sentinelObserver = null;

function init() {
  cacheDom();
  bindEvents();
  setupInfiniteScroll();
  renderSortButtons();
  renderStatus();

  runSearch({ reset: true }).then(() => {
    dom.searchInput.focus();
  });
}

function cacheDom() {
  dom.searchInput = document.getElementById('search');
  dom.results = document.getElementById('results');
  dom.status = document.getElementById('status');
  dom.detail = document.getElementById('detail');
  dom.sortButtons = Array.from(document.querySelectorAll('.sort-btn'));

  dom.sentinel = document.createElement('div');
  dom.sentinel.className = 'sentinel';
  dom.results.appendChild(dom.sentinel);
}

function bindEvents() {
  dom.searchInput.addEventListener('input', onSearchInput);
  dom.searchInput.addEventListener('keydown', onSearchKeyDown);
  dom.results.addEventListener('click', onResultsClick);
  dom.results.addEventListener('dblclick', onResultsDoubleClick);
  window.addEventListener('keydown', onGlobalKeyDown);

  dom.sortButtons.forEach((button) => {
    button.addEventListener('click', () => setSort(button.dataset.sort));
  });
}

function setupInfiniteScroll() {
  sentinelObserver = new IntersectionObserver(
    (entries) => {
      if (entries[0]?.isIntersecting) {
        runSearch({ reset: false });
      }
    },
    {
      root: dom.results,
      rootMargin: '100px',
    },
  );

  sentinelObserver.observe(dom.sentinel);
}

function onSearchInput() {
  state.query = dom.searchInput.value;
  scheduleSearch();
}

function onSearchKeyDown(event) {
  if (event.key === 'Enter') {
    event.preventDefault();
    openSelected();
  }
}

function onResultsClick(event) {
  const index = getEventRowIndex(event);
  if (index === null) {
    return;
  }

  setSelected(index);
}

function onResultsDoubleClick(event) {
  const index = getEventRowIndex(event);
  if (index === null) {
    return;
  }

  setSelected(index);
  openSelected();
}

function onGlobalKeyDown(event) {
  if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) {
    return;
  }

  switch (event.key) {
    case 'ArrowDown': {
      if (!state.results.length) {
        return;
      }
      event.preventDefault();
      setSelected(state.selected + 1, { scrollIntoView: true });
      maybeLoadMore();
      break;
    }
    case 'ArrowUp': {
      if (!state.results.length) {
        return;
      }
      event.preventDefault();
      setSelected(state.selected - 1, { scrollIntoView: true });
      break;
    }
    case 'Enter': {
      event.preventDefault();
      openSelected();
      break;
    }
    case 'Tab': {
      event.preventDefault();
      cycleSort(event.shiftKey ? -1 : 1);
      break;
    }
  }
}

function setSort(sort) {
  if (!SORTS.includes(sort) || state.sort === sort) {
    return;
  }

  state.sort = sort;
  renderSortButtons();
  scheduleSearch();
}

function cycleSort(direction) {
  const currentIndex = SORTS.indexOf(state.sort);
  const nextIndex = (currentIndex + direction + SORTS.length) % SORTS.length;
  setSort(SORTS[nextIndex]);
}

function scheduleSearch() {
  if (debounceTimer) {
    clearTimeout(debounceTimer);
  }

  debounceTimer = setTimeout(() => {
    debounceTimer = null;
    runSearch({ reset: true });
  }, SEARCH_DEBOUNCE_MS);
}

async function runSearch({ reset }) {
  if (!reset && (state.loading || !state.hasMore)) {
    return;
  }

  const offset = reset ? 0 : state.results.length;
  const response = await requestPapers({
    query: state.query,
    sort: state.sort,
    offset,
    limit: LOAD_BATCH,
  });

  if (!response) {
    return;
  }

  const { papers } = response;

  if (reset) {
    state.results = papers;
    state.selected = 0;
    state.hasMore = papers.length >= LOAD_BATCH;
    dom.results.scrollTop = 0;
    renderResults();
  } else {
    if (papers.length === 0) {
      state.hasMore = false;
      renderStatus();
      return;
    }

    const startIndex = state.results.length;
    state.results = state.results.concat(papers);
    state.hasMore = papers.length >= LOAD_BATCH;
    appendResults(papers, startIndex);
  }

  renderDetail();
  renderStatus();
}

async function requestPapers({ query, sort, offset, limit }) {
  if (abortController) {
    abortController.abort();
  }

  const controller = new AbortController();
  abortController = controller;
  state.loading = true;
  state.error = '';
  renderStatus();

  try {
    const params = new URLSearchParams({
      q: query,
      sort,
      limit: String(limit),
      offset: String(offset),
    });

    const response = await fetch(`/api/search?${params.toString()}`, {
      signal: controller.signal,
    });

    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }

    const data = await response.json();
    state.error = typeof data.error === 'string' ? data.error : '';

    return {
      papers: Array.isArray(data.papers) ? data.papers : [],
    };
  } catch (error) {
    if (error.name === 'AbortError') {
      return null;
    }

    state.error = error.message || 'search failed';
    return null;
  } finally {
    if (abortController === controller) {
      abortController = null;
      state.loading = false;
      renderStatus();
    }
  }
}

function renderSortButtons() {
  dom.sortButtons.forEach((button) => {
    button.classList.toggle('active', button.dataset.sort === state.sort);
  });
}

function renderResults() {
  dom.results.querySelectorAll('.result-row').forEach((row) => row.remove());
  appendResults(state.results, 0);
}

function appendResults(papers, startIndex) {
  const fragment = document.createDocumentFragment();

  papers.forEach((paper, offset) => {
    fragment.appendChild(createResultRow(paper, startIndex + offset));
  });

  dom.results.insertBefore(fragment, dom.sentinel);
}

function createResultRow(paper, index) {
  const row = document.createElement('div');
  row.className = 'result-row';
  row.dataset.index = String(index);

  if (index === state.selected) {
    row.classList.add('selected');
  }

  row.appendChild(createTextElement('span', 'col-venue', paper.venue || ''));
  row.appendChild(createTextElement('span', 'col-year', formatYear(paper.year)));
  row.appendChild(createTextElement('span', 'col-title', paper.title || ''));
  row.appendChild(createTextElement('span', 'col-authors', paper.authors || ''));

  return row;
}

function setSelected(index, options = {}) {
  if (!state.results.length) {
    return;
  }

  const nextIndex = clamp(index, 0, state.results.length - 1);
  const previousIndex = state.selected;

  if (nextIndex === previousIndex) {
    if (options.scrollIntoView) {
      scrollRowIntoView(nextIndex);
    }
    return;
  }

  state.selected = nextIndex;
  updateRowSelection(previousIndex, false);
  updateRowSelection(nextIndex, true);
  renderDetail();
  renderStatus();

  if (options.scrollIntoView) {
    scrollRowIntoView(nextIndex);
  }
}

function updateRowSelection(index, selected) {
  const row = getRow(index);
  if (row) {
    row.classList.toggle('selected', selected);
  }
}

function scrollRowIntoView(index) {
  const row = getRow(index);
  if (row) {
    row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
  }
}

function renderDetail() {
  dom.detail.replaceChildren();

  const paper = state.results[state.selected];
  if (!paper) {
    dom.detail.appendChild(createTextElement('div', 'detail-empty', 'No paper selected'));
    return;
  }

  const content = document.createElement('div');
  content.className = 'detail-content';

  content.appendChild(createTextElement('h2', 'detail-title', paper.title || ''));

  const meta = createTextElement(
    'div',
    'detail-meta',
    [paper.venue || '', formatYear(paper.year)].filter(Boolean).join(' · '),
  );
  content.appendChild(meta);

  content.appendChild(createLabeledTextBlock('detail-authors', 'Authors', paper.authors || ''));

  if (paper.doi) {
    content.appendChild(createLabeledTextBlock('detail-link', 'DOI', paper.doi));
  }

  const url = paperUrl(paper);
  if (url) {
    const linkBlock = document.createElement('div');
    linkBlock.className = 'detail-link';
    linkBlock.appendChild(createLabel('URL'));
    linkBlock.appendChild(document.createTextNode(' '));

    const link = document.createElement('a');
    link.href = url;
    link.target = '_blank';
    link.rel = 'noreferrer';
    link.textContent = url;
    linkBlock.appendChild(link);

    content.appendChild(linkBlock);
  }

  const abstractBlock = document.createElement('div');
  abstractBlock.className = 'detail-abstract';
  abstractBlock.appendChild(createLabel('Abstract'));

  const abstractText = document.createElement('p');
  abstractText.textContent = paper.abstract || 'No abstract available.';
  abstractBlock.appendChild(abstractText);

  content.appendChild(abstractBlock);
  dom.detail.appendChild(content);
}

function renderStatus() {
  dom.status.replaceChildren();

  if (state.error) {
    dom.status.appendChild(createTextElement('span', 'error', state.error));
    return;
  }

  if (state.results.length > 0 || state.loading) {
    const selection = state.results.length ? ` · ${state.selected + 1}/${state.results.length}` : '';
    const count = `${state.results.length}${state.hasMore ? '+' : ''} papers`;
    dom.status.appendChild(createTextElement('span', '', `${count} · sort ${state.sort}${selection}`));
  }

  if (state.loading) {
    dom.status.appendChild(createTextElement('span', 'spinner', 'Loading…'));
  }
}

function openSelected() {
  const paper = state.results[state.selected];
  if (!paper) {
    return;
  }

  const url = paperUrl(paper);
  if (url) {
    window.open(url, '_blank');
  }
}

function maybeLoadMore() {
  if (state.selected >= state.results.length - 5) {
    runSearch({ reset: false });
  }
}

function getEventRowIndex(event) {
  const target = event.target instanceof Element ? event.target : null;
  const row = target ? target.closest('.result-row') : null;
  if (!row) {
    return null;
  }

  const index = Number.parseInt(row.dataset.index || '', 10);
  return Number.isNaN(index) ? null : index;
}

function getRow(index) {
  return dom.results.querySelector(`.result-row[data-index="${index}"]`);
}

function paperUrl(paper) {
  if (paper.url) {
    return paper.url;
  }

  if (paper.doi) {
    return `https://doi.org/${paper.doi}`;
  }

  return '';
}

function formatYear(year) {
  return year == null ? '' : String(year);
}

function clamp(value, min, max) {
  return Math.max(min, Math.min(value, max));
}

function createTextElement(tag, className, text) {
  const element = document.createElement(tag);
  if (className) {
    element.className = className;
  }
  element.textContent = text;
  return element;
}

function createLabel(text) {
  const label = document.createElement('strong');
  label.textContent = text;
  return label;
}

function createLabeledTextBlock(className, label, text) {
  const block = document.createElement('div');
  block.className = className;
  block.appendChild(createLabel(label));
  block.appendChild(document.createTextNode(` ${text}`));
  return block;
}

init();
