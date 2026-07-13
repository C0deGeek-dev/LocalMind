// Dashboard view
import { api, esc } from '../app.js';
async function renderDashboard() {
  view.innerHTML = '<div class="pad"><div class="empty">Loading…</div></div>';
  try {
    const s = await api('GET', '/api/stats');
    const bars = (t, o) => {
      const es = Object.entries(o).sort((a, b) => b[1] - a[1]);
      const mx = Math.max(1, ...es.map(e => e[1]));
      return `<div class="barwrap"><h3>${t}</h3>${es.map(([k, v]) =>
        `<div class="bar"><span class="lbl">${esc(k)}</span>
          <span class="track"><span class="fill" style="width:${v / mx * 100}%"></span></span><span class="v">${v}</span></div>`
      ).join('') || '<div class="empty">none</div>'}</div>`;
    };
    const empty = (s.pending + s.accepted + s.doc_chunks) === 0;
    const store = `<div class="storebar">Store: <code>${esc(s.store_path || '?')}</code>${empty ? ' · <b>this store is empty</b> (nothing ingested or reviewed here yet)' : ''}</div>`;
    view.innerHTML = `<div class="pad">${store}<div class="cards">
      <div class="card"><div class="n">${s.pending}</div><div class="l">pending review</div></div>
      <div class="card"><div class="n">${s.accepted}</div><div class="l">accepted memory</div></div>
      <div class="card"><div class="n">${s.doc_chunks}</div><div class="l">doc chunks</div></div></div>
      ${bars('Pending by category', s.pending_by_category)}${bars('Accepted by scope', s.accepted_by_scope)}${bars('Accepted by category', s.accepted_by_category)}</div>`;
  } catch (e) {
    view.innerHTML = `<div class="pad"><div class="empty">${esc(e.message)}</div></div>`;
  }
}

export { renderDashboard };
