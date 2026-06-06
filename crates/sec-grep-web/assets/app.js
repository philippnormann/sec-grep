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
  modal: null,
  modalCode: null,
  modalClose: null,
  modalCopy: null,
  modalDownload: null,
};

const state = {
  query: '',
  sort: 'year',
  results: [],
  selected: 0,
  loading: false,
  hasMore: false,
  error: '',
  bibtex: '',
  bibtexKey: '',
};

let debounceTimer = null;
let sentinelObserver = null;

function init() {
  dom.searchInput = document.getElementById('search');
  dom.results = document.getElementById('results');
  dom.status = document.getElementById('status');
  dom.detail = document.getElementById('detail');
  dom.sortButtons = Array.from(document.querySelectorAll('.sort-btn'));

  dom.sentinel = document.createElement('div');
  dom.sentinel.className = 'sentinel';
  dom.results.appendChild(dom.sentinel);

  dom.modal = document.getElementById('bibtex-modal');
  dom.modalCode = document.getElementById('bibtex-code');
  dom.modalClose = document.getElementById('modal-close');
  dom.modalCopy = document.getElementById('modal-copy');
  dom.modalDownload = document.getElementById('modal-download');

  bindEvents();
  setupInfiniteScroll();
  renderSortButtons();
  renderStatus();

  runSearch({ reset: true }).then(() => {
    dom.searchInput.focus();
  });
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

  dom.modalClose.addEventListener('click', closeModal);
  dom.modal.addEventListener('click', (e) => {
    if (e.target === dom.modal) closeModal();
  });
  dom.modalCopy.addEventListener('click', copyBibTeX);
  dom.modalDownload.addEventListener('click', downloadBibTeX);
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
  if (event.key === 'Escape' && !dom.modal.classList.contains('hidden')) {
    event.preventDefault();
    closeModal();
    return;
  }

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

  state.loading = true;
  state.error = '';
  renderStatus();

  const offset = reset ? 0 : state.results.length;
  const params = new URLSearchParams({
    q: state.query,
    sort: state.sort,
    limit: String(LOAD_BATCH),
    offset: String(offset),
  });

  try {
    const response = await fetch(`/api/search?${params.toString()}`);
    const data = await response.json();
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}: ${data.error}`);
    }
    const papers = Array.isArray(data.papers) ? data.papers : [];

    if (reset) {
      state.results = papers;
      state.selected = 0;
      state.hasMore = papers.length >= LOAD_BATCH;
      dom.results.scrollTop = 0;
      dom.results.querySelectorAll('.result-row').forEach((row) => row.remove());
      appendResults(papers, 0);
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
  } catch (error) {
    state.error = error.message || 'search failed';
    renderStatus();
  } finally {
    state.loading = false;
    renderStatus();
  }
}

function renderSortButtons() {
  dom.sortButtons.forEach((button) => {
    button.classList.toggle('active', button.dataset.sort === state.sort);
  });
}

function appendResults(papers, startIndex) {
  const fragment = document.createDocumentFragment();

  papers.forEach((paper, offset) => {
    const index = startIndex + offset;
    const row = document.createElement('div');
    row.className = 'result-row';
    row.dataset.index = String(index);
    if (index === state.selected) {
      row.classList.add('selected');
    }
    row.appendChild(el('span', 'col-venue', paper.venue || ''));
    row.appendChild(el('span', 'col-year', paper.year ?? ''));
    row.appendChild(el('span', 'col-title', paper.title || ''));
    row.appendChild(el('span', 'col-authors', paper.authors || ''));
    fragment.appendChild(row);
  });

  dom.results.insertBefore(fragment, dom.sentinel);
}

function setSelected(index, options = {}) {
  if (!state.results.length) {
    return;
  }

  const nextIndex = Math.max(0, Math.min(index, state.results.length - 1));
  const previousIndex = state.selected;

  if (nextIndex === previousIndex) {
    if (options.scrollIntoView) {
      const row = dom.results.querySelector(`.result-row[data-index="${nextIndex}"]`);
      if (row) {
        row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }
    }
    return;
  }

  state.selected = nextIndex;
  updateRowSelection(previousIndex, false);
  updateRowSelection(nextIndex, true);
  renderDetail();
  renderStatus();

  if (options.scrollIntoView) {
    const row = dom.results.querySelector(`.result-row[data-index="${nextIndex}"]`);
    if (row) {
      row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    }
  }
}

function updateRowSelection(index, selected) {
  const row = dom.results.querySelector(`.result-row[data-index="${index}"]`);
  if (row) {
    row.classList.toggle('selected', selected);
  }
}

function renderDetail() {
  dom.detail.replaceChildren();

  const paper = state.results[state.selected];
  if (!paper) {
    dom.detail.appendChild(el('div', 'detail-empty', 'No paper selected'));
    return;
  }

  const content = document.createElement('div');
  content.className = 'detail-content';

  content.appendChild(el('h2', 'detail-title', paper.title || ''));

  const metaParts = [paper.venue || '', paper.year ?? ''].filter(Boolean);
  content.appendChild(el('div', 'detail-meta', metaParts.join(' · ')));

  content.appendChild(labeledBlock('detail-authors', 'Authors', paper.authors || ''));

  if (paper.doi) {
    content.appendChild(labeledBlock('detail-link', 'DOI', paper.doi));
  }

  const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : '');
  if (url) {
    const linkBlock = document.createElement('div');
    linkBlock.className = 'detail-link';
    linkBlock.appendChild(el('strong', '', 'URL'));
    linkBlock.appendChild(document.createTextNode(' '));

    const link = document.createElement('a');
    link.href = url;
    link.target = '_blank';
    link.rel = 'noreferrer';
    link.textContent = url;
    linkBlock.appendChild(link);

    content.appendChild(linkBlock);
  }

  const citationBlock = document.createElement('div');
  citationBlock.className = 'detail-link';
  citationBlock.appendChild(el('strong', '', 'Citation'));

  const bibtexBtn = document.createElement('button');
  bibtexBtn.className = 'download-btn';
  bibtexBtn.textContent = 'BibTeX';
  bibtexBtn.onclick = async () => {
    state.error = '';
    renderStatus();
    try {
      const paper = state.results[state.selected];
      const response = await fetch(`/api/bibtex?key=${encodeURIComponent(paper.dblp_key)}`);
      if (response.ok) {
        const bibtex = await response.text();
        openModal(bibtex, paper.dblp_key);
      } else {
        state.error = 'Failed to fetch BibTeX';
        renderStatus();
      }
    } catch (e) {
      state.error = e.message || 'Error fetching BibTeX';
      renderStatus();
    }
  };
  citationBlock.appendChild(bibtexBtn);
  content.appendChild(citationBlock);

  const abstractBlock = document.createElement('div');
  abstractBlock.className = 'detail-abstract';
  abstractBlock.appendChild(el('strong', '', 'Abstract'));

  const abstractText = document.createElement('p');
  abstractText.textContent = paper.abstract || 'No abstract available.';
  abstractBlock.appendChild(abstractText);

  content.appendChild(abstractBlock);
  dom.detail.appendChild(content);
}

function renderStatus() {
  dom.status.replaceChildren();

  if (state.error) {
    dom.status.appendChild(el('span', 'error', state.error));
    return;
  }

  if (state.results.length > 0 || state.loading) {
    const selection = state.results.length ? ` · ${state.selected + 1}/${state.results.length}` : '';
    const count = `${state.results.length}${state.hasMore ? '+' : ''} papers`;
    dom.status.appendChild(el('span', '', `${count} · sort ${state.sort}${selection}`));
  }

  if (state.loading) {
    dom.status.appendChild(el('span', 'spinner', 'Loading…'));
  }
}

function openSelected() {
  const paper = state.results[state.selected];
  if (!paper) {
    return;
  }

  const url = paper.url || (paper.doi ? `https://doi.org/${paper.doi}` : '');
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

function el(tag, className, text) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text) e.textContent = text;
  return e;
}

function labeledBlock(className, label, text) {
  const block = document.createElement('div');
  block.className = className;
  block.appendChild(el('strong', '', label));
  block.appendChild(document.createTextNode(` ${text}`));
  return block;
}

function openModal(bibtex, key) {
  state.bibtex = bibtex;
  state.bibtexKey = key;
  dom.modalCode.textContent = bibtex;
  dom.modal.classList.remove('hidden');
}

function closeModal() {
  dom.modal.classList.add('hidden');
  state.bibtex = '';
  state.bibtexKey = '';
}

async function copyBibTeX() {
  try {
    await navigator.clipboard.writeText(state.bibtex);
    const original = dom.modalCopy.textContent;
    dom.modalCopy.textContent = 'Copied!';
    setTimeout(() => {
      dom.modalCopy.textContent = original;
    }, 1200);
  } catch (e) {
    state.error = `Copy failed: ${e}`;
    renderStatus();
  }
}

function downloadBibTeX() {
  const blob = new Blob([state.bibtex], { type: 'text/plain' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `bibtex_${state.bibtexKey}.bib`;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

init();
