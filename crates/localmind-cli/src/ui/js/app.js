// LocalMind — shared utilities, API layer, routing
import { renderDashboard } from './views/dashboard.js';
import { handleReviewKeydown, renderReview } from './views/review.js';
import { renderMemory } from './views/memory.js';
import { renderDocs } from './views/docs.js';
import { renderGraph } from './graph.js';
import { renderAudit } from './views/audit.js';

// ── Token / API helpers ──
const token = new URLSearchParams(location.search).get('token');
const q = u => token ? u + (u.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(token) : u;

export const esc = s => {
  if (s == null) return '';
  return String(s).replace(/[&<>"']/g, c => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;'
  }[c]));
};

export async function api(m, url, body) {
  const r = await fetch(q(url), {
    method: m,
    headers: { 'Content-Type': 'application/json' },
    body: body ? JSON.stringify(body) : undefined
  });
  const j = await r.json().catch(() => ({ error: 'bad response' }));
  if (!r.ok || j.error) throw new Error(j.error || ('HTTP ' + r.status));
  return j;
}

export function toast(msg, err) {
  const t = document.querySelector('#toast');
  t.textContent = msg;
  t.className = 'toast show' + (err ? ' err' : '');
  setTimeout(() => t.className = 'toast', 2200);
}

export async function refreshPill() {
  try {
    const s = await api('GET', '/api/stats');
    document.querySelector('#pill').innerHTML = `<b>${s.pending}</b> pending · <b>${s.accepted}</b> memory · <b>${s.doc_chunks}</b> doc chunks`;
  } catch (e) {
    document.querySelector('#pill').textContent = e.message;
  }
}

document.querySelector('#helpBtn').onclick = () => document.querySelector('#help').classList.add('on');

// ── Routing ──
const routes = {
  dashboard: renderDashboard,
  review: renderReview,
  memory: renderMemory,
  docs: renderDocs,
  graph: renderGraph,
  audit: renderAudit
};

function route() {
  const tab = (location.hash.slice(1) || 'review');
  if (typeof gToken !== 'undefined') gToken++;
  document.querySelectorAll('#nav a').forEach(a => a.classList.toggle('on', a.hash === '#' + tab));
  (routes[tab] || renderReview)();
  refreshPill();
}

window.addEventListener('hashchange', route);

// ── Keyboard shortcuts (review page) ──
document.addEventListener('keydown', e => {
  if (e.key === 'Escape') document.querySelector('#help').classList.remove('on');
  handleReviewKeydown(e);
});

// ── Boot ──
route();
