// LocalMind — shared utilities, API layer, routing
import { renderDashboard } from './views/dashboard.js';
import { handleReviewKeydown, renderReview } from './views/review.js';
import { renderMemory } from './views/memory.js';
import { renderDocs } from './views/docs.js';
import { renderGraph } from './graph.js';
import { renderAudit } from './views/audit.js';
import { renderSkills } from './views/skills.js';

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
    const awaiting = s.accepted_awaiting_promotion
      ? ` · <b>${s.accepted_awaiting_promotion}</b> awaiting promotion`
      : '';
    document.querySelector('#pill').innerHTML = `<b>${s.pending}</b> pending${awaiting} · <b>${s.accepted}</b> memory · <b>${s.doc_chunks}</b> doc chunks`;
  } catch (e) {
    document.querySelector('#pill').textContent = e.message;
  }
}

document.querySelector('#helpBtn').onclick = () => document.querySelector('#help').classList.add('on');

// ── Reviewer identity ──
// Stored per browser origin, not per project — the human is the same across
// projects. The name is written into review items and the append-only audit
// log as the acting `actor`, so nothing may default it to a person.
const REVIEWER_KEY = 'localmind.reviewer';

export function normalizeReviewer(name) {
  return (name || '').trim().slice(0, 64);
}

export function reviewerName() {
  let stored = '';
  try { stored = localStorage.getItem(REVIEWER_KEY) || ''; } catch { /* storage unavailable */ }
  return normalizeReviewer(stored);
}

function storeReviewer(name) {
  try { localStorage.setItem(REVIEWER_KEY, name); } catch { /* storage unavailable */ }
}

// Opens the blocking identity modal. It has no close control and Escape does
// not dismiss it: decision actions stay disabled until a name is saved.
export function askReviewer() {
  const modal = document.querySelector('#ident');
  modal.classList.add('on');
  const field = document.querySelector('#identName');
  field.value = reviewerName();
  field.focus();
}

document.querySelector('#identForm').onsubmit = e => {
  e.preventDefault();
  const name = normalizeReviewer(document.querySelector('#identName').value);
  if (!name) return toast('Enter a name — decisions are recorded under it', true);
  storeReviewer(name);
  document.querySelector('#reviewer').value = name;
  document.querySelector('#ident').classList.remove('on');
  route(); // re-render so decision controls pick up the identity
};

// Editing the header field updates the stored identity; clearing it re-opens
// the modal instead of silently falling back to an anonymous actor.
document.querySelector('#reviewer').onchange = e => {
  const name = normalizeReviewer(e.target.value);
  if (!name) return askReviewer();
  storeReviewer(name);
  e.target.value = name;
};

// ── Routing ──
const routes = {
  dashboard: renderDashboard,
  review: renderReview,
  memory: renderMemory,
  docs: renderDocs,
  skills: renderSkills,
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
const bootName = reviewerName();
if (bootName) document.querySelector('#reviewer').value = bootName;
else askReviewer();
route();
