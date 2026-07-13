// LocalMind — shared utilities, API layer, routing
import { renderDashboard } from './views/dashboard.js';
import { renderReview } from './views/review.js';
import { renderMemory } from './views/memory.js';
import { renderDocs } from './views/docs.js';
import { renderGraph, graphShow, graphOverview, renderGlobal, graphDetails, renderGraphResult } from './graph.js';
import { renderAudit } from './views/audit.js';

const view = document.querySelector('#view');

// ── Token / API helpers ──
const token = new URLSearchParams(location.search).get('token');
const q = u => token ? u + (u.includes('?') ? '&' : '?') + 'token=' + encodeURIComponent(token) : u;
const reviewer = () => document.querySelector('#reviewer').value.trim() || 'ui';

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

async function refreshPill() {
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

  if ((location.hash.slice(1) || 'review') !== 'review') return;
  if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName)) return;

  const f = document.querySelector('#catF') ? document.querySelector('#catF').value : '';
  const shown = rvItems.filter(i => !f || i.category === f);
  const idx = shown.findIndex(i => i.id === rvSel);

  if (e.key === 'j' || e.key === 'k') {
    e.preventDefault();
    const n = e.key === 'j' ? idx + 1 : idx - 1;
    if (shown[n]) selReview(shown[n].id);
  } else if (rvSel) {
    if (e.key === 'a') actReview(rvSel, 'accept');
    else if (e.key === 'r') actReview(rvSel, 'reject');
    else if (e.key === 'd') actReview(rvSel, 'defer');
    else if (e.key === 'e') { const t = document.querySelector('#rvBody'); if (t) t.focus(); }
    else if (e.key === 'x') { rvChecks.has(rvSel) ? rvChecks.delete(rvSel) : rvChecks.add(rvSel); drawReview(); }
  }
});

// ── Boot ──
route();
