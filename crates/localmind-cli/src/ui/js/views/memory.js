// Memory view
import { api, esc, refreshPill, toast } from '../app.js';

let memAll = [];
async function renderMemory() {
  view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
    <div class="toolbar" style="gap:6px">
      <input type="text" id="memQ" placeholder="search by meaning…" size="20">
      <button id="memGo">Search</button>
      <select id="fScope"></select>
      <select id="fCat"></select>
      <select id="fLang"></select>
      <select id="fStatus"></select>
      <label class="hint"><input type="checkbox" id="fStale"> stale</label>
      <label class="hint"><input type="checkbox" id="fConf"> conflict</label>
      <button class="sm" id="memClear">clear</button>
      <span class="grow"></span>
      <span class="pill" id="memN"></span>
    </div>
    <div class="split" style="flex:1;min-height:0">
      <div class="listcol" id="memList"></div>
      <div class="detailcol" id="memDetail">
        <div class="empty">Pick filters above (each shows how many), or search by meaning. Click a memory to open it.</div>
      </div>
    </div>
  </div>`;

  const f = await api('GET', '/api/memory/facets');
  const fill = (sel, label, obj) => {
    const el = document.querySelector(sel);
    const total = Object.values(obj).reduce((a, b) => a + b, 0);
    el.innerHTML = `<option value="">${label}: all (${total})</option>` +
      Object.entries(obj).sort((a, b) => b[1] - a[1]).map(([k, v]) =>
        `<option value="${esc(k)}">${esc(k)} (${v})</option>`).join('');
    el.onchange = applyMem;
  };
  fill('#fScope', 'scope', f.scope);
  fill('#fCat', 'category', f.category);
  fill('#fLang', 'language', f.language);
  fill('#fStatus', 'status', f.status);

  document.querySelector('#fStale').onchange = applyMem;
  document.querySelector('#fConf').onchange = applyMem;
  document.querySelector('#memGo').onclick = () => loadMemory(true);
  document.querySelector('#memQ').addEventListener('keydown', e => { if (e.key === 'Enter') loadMemory(true); });
  document.querySelector('#memClear').onclick = () => {
    ['#fScope', '#fCat', '#fLang', '#fStatus'].forEach(s => document.querySelector(s).value = '');
    document.querySelector('#fStale').checked = false;
    document.querySelector('#fConf').checked = false;
    document.querySelector('#memQ').value = '';
    loadMemory(false);
  };
  loadMemory(false);
}

async function loadMemory(search) {
  const q = search ? document.querySelector('#memQ').value.trim() : '';
  const d = await api('GET', '/api/memory?' + new URLSearchParams(q ? { query: q } : {}));
  memAll = d.items;
  applyMem();
}

function applyMem() {
  const sc = document.querySelector('#fScope').value, ca = document.querySelector('#fCat').value, la = document.querySelector('#fLang').value, st = document.querySelector('#fStatus').value;
  const stale = document.querySelector('#fStale').checked, conf = document.querySelector('#fConf').checked;
  const has = (v, fv) => !fv || v == null || String(v) === fv;
  const lang = m => !la || m.language === undefined || (la === '(agnostic)' ? !m.language : m.language === la);
  const items = memAll.filter(m => has(m.scope, sc) && has(m.category, ca) && has(m.status, st) && lang(m) && (!stale || m.stale) && (!conf || m.contradicted));

  document.querySelector('#memN').textContent = `${items.length} of ${memAll.length}`;
  const list = document.querySelector('#memList');
  list.innerHTML = items.length ? '' : '<div class="empty">Nothing matches these filters.</div>';

  items.forEach(it => {
    const row = document.createElement('div');
    row.className = 'row';
    row.innerHTML = `<div class="txt"><span class="sum">
      <span class="chip">${esc(it.category)}</span>${esc(it.snippet)}
      ${it.stale ? '<span class="badge stale">stale</span>' : ''}${it.contradicted ? '<span class="badge conflict">conflict</span>' : ''}
    </span>
      <span class="id">${esc(it.id)}${it.scope ? ' · ' + esc(it.scope) : ''}${it.language ? ' · ' + esc(it.language) : ''}${it.score != null ? ' · score ' + it.score : ''} · used ${it.hit_count ?? 0}</span></div>`;
    row.addEventListener('click', () => selMemory(it.id));
    list.appendChild(row);
  });
}

async function selMemory(id) {
  try {
    const m = await api('GET', '/api/memory/' + encodeURIComponent(id));
    const p = m.provenance;
    document.querySelector('#memDetail').innerHTML = `<div class="meta">
      <span class="chip">${esc(m.category)}</span>${esc(m.scope)} · ${esc(m.status)}${m.language ? ' · ' + esc(m.language) : ''} · used ${m.hit_count}
      ${m.stale ? '<span class="badge stale">stale</span>' : ''}${m.contradicted ? '<span class="badge conflict">conflict</span>' : ''}
    </div>
    <div class="meta">${esc(m.id)}</div>
    <div class="body">${esc(m.body)}</div>
    ${p ? `<div class="meta" style="margin-top:12px">provenance — confidence ${(+p.confidence).toFixed(2)} · ${esc(p.epistemic_status)}
      ${p.source_session ? ' · session ' + esc(p.source_session) : ''}${p.contradicts.length ? ' · contradicts ' + p.contradicts.map(esc).join(', ') : ''}</div>` : ''}
    <div class="actions"><button class="danger" id="memDel">Delete permanently</button></div>`;

    document.querySelector('#memDel').onclick = async () => {
      if (!confirm('Delete this memory permanently? (recorded in Audit)')) return;
      try {
        await api('DELETE', '/api/memory/' + encodeURIComponent(id));
        toast('Deleted');
        document.querySelector('#memDetail').innerHTML = '<div class="empty">Select a memory.</div>';
        await loadMemory();
        refreshPill();
      } catch (e) {
        toast(e.message, true);
      }
    };
  } catch (e) {
    toast(e.message, true);
  }
}

export { renderMemory, memAll };
