// Audit view
import { api, esc } from '../app.js';

let auditItems = [];
async function renderAudit() {
  view.innerHTML = '<div class="pad"><div class="empty">Loading…</div></div>';
  try {
    const d = await api('GET', '/api/audit?limit=300');
    auditItems = d.items;
    const counts = {};
    auditItems.forEach(r => counts[r.kind] = (counts[r.kind] || 0) + 1);

    view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
      <div class="toolbar">
        <span class="meta">Append-only log — entries are never edited or deleted.</span>
        <select id="akF"><option value="">all kinds (${auditItems.length})</option>${Object.entries(counts).sort((a, b) => b[1] - a[1]).map(([k, c]) => `<option value="${esc(k)}">${esc(k)} (${c})</option>`).join('')}</select>
        <span class="grow"></span>
        <span class="pill">${auditItems.length} events</span>
      </div>
      <div class="pad" style="padding-top:0"><table>
        <thead><tr><th>#</th><th>when</th><th>kind</th><th>actor</th><th>subject</th></tr></thead>
        <tbody id="akBody"></tbody>
      </table></div>
    </div>`;

    document.querySelector('#akF').onchange = drawAudit;
    drawAudit();
  } catch (e) {
    view.innerHTML = `<div class="pad"><div class="empty">${esc(e.message)}</div></div>`;
  }
}

function drawAudit() {
  const f = document.querySelector('#akF').value;
  const body = document.querySelector('#akBody');
  body.innerHTML = '';

  auditItems.filter(r => !f || r.kind === f).forEach(r => {
    const tr = document.createElement('tr');
    tr.className = 'audit';
    tr.innerHTML = `<td class="mono">${r.id}</td><td class="mono">${esc(r.at)}</td><td>${esc(r.kind)}</td><td>${esc(r.actor)}</td><td class="mono">${esc(r.subject)}</td>`;
    tr.addEventListener('click', () => {
      const has = tr.nextSibling && tr.nextSibling.classList && tr.nextSibling.classList.contains('detail');
      if (has) { tr.nextSibling.remove(); return; }
      const dr = document.createElement('tr');
      dr.className = 'detail';
      dr.innerHTML = `<td></td><td colspan="4"><div class="body">${esc(r.metadata || '{}')}</div></td>`;
      tr.after(dr);
    });
    body.appendChild(tr);
  });
}

export { renderAudit };
