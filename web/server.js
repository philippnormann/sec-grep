import express from 'express';
import { spawn } from 'child_process';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

const app = express();
const PORT = process.env.PORT || 5002;

// Trust proxy so req.ip is correct behind a reverse proxy
app.set('trust proxy', true);
app.use(express.json());

// Serve the built React app static files
app.use(express.static(path.join(__dirname, 'dist')));

// Search endpoint: calls sec-grep with JSON output
app.get('/api/search', (req, res) => {
  const q = req.query.q ?? '';

  const {
    venue,
    year,
    rank,
    tag,
    sort = 'relevance',
    limit = '320',
    offset = '0',
  } = req.query;

  const args = [
    '--format',
    'json',
    '--sort',
    sort,
    '--limit',
    String(limit),
    '--offset',
    String(offset),
  ];

  if (venue) {
    const list = Array.isArray(venue) ? venue : [venue];
    list.forEach((v) => args.push('--venue', v));
  }
  if (year) args.push('--year', String(year));
  if (rank) {
    const list = Array.isArray(rank) ? rank : [rank];
    list.forEach((r) => args.push('--rank', r));
  }
  if (tag) {
    const list = Array.isArray(tag) ? tag : [tag];
    list.forEach((t) => args.push('--tag', t));
  }
  if (q) args.push(String(q));

  const child = spawn('sec-grep', args, {
    env: {
      ...process.env,
      SEC_GREP_DB:
        process.env.SEC_GREP_DB || '',
    },
  });

  let stdout = '';
  let stderr = '';
  let responded = false;

  child.stdout.on('data', (data) => {
    stdout += data;
  });

  child.stderr.on('data', (data) => {
    stderr += data;
  });

  const respondOnce = (statusOrCallback, payload) => {
    if (responded) return;
    responded = true;
    if (typeof statusOrCallback === 'function') {
      statusOrCallback();
    } else {
      res.status(statusOrCallback).json(payload);
    }
  };

  child.on('close', (code) => {
    if (code !== 0) {
      const msg = stderr.trim() || 'sec-grep failed';
      console.error('sec-grep error:', msg);
      respondOnce(200, { papers: [], total: 0, error: msg });
      return;
    }
    try {
      const papers = JSON.parse(stdout.trim()) || [];
      respondOnce(200, { papers });
    } catch (e) {
      console.error('JSON parse error:', (e && e.message) || e);
      respondOnce(500, { error: 'invalid output from sec-grep' });
    }
  });

  child.on('error', (err) => {
    console.error('Failed to start sec-grep:', err.message);
    respondOnce(500, { error: 'sec-grep not available' });
  });
});

// Fallback: serve index.html for client-side routing
app.get('*', (_req, res) => {
  res.sendFile(path.join(__dirname, 'dist', 'index.html'));
});

app.listen(PORT, () => {
  console.log(`sec-grep web UI listening on http://0.0.0.0:${PORT}`);
});
