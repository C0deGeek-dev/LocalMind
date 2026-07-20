// Review view
import { api, esc, refreshPill, toast } from '../app.js';

let rvItems = [], rvSel = null, rvChecks = new Set();

function currentReviewer() {
  return document.querySelector('#reviewer')?.value.trim() || 'ui';
}

async function renderReview() {
  view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
    <div class="toolbar">
      <input type="checkbox" id="selAll">
      <span class="pill"><span id="selN">0</span> selected</span>
      <button class="primary" id="bAcc">Accept selected</button>
      <button class="danger" id="bRej">Reject selected</button>
      <select id="catF"><option value="">all categories</option></select>
      <span class="grow"></span>
      <span class="hint">
        <span class="kbd">j/k</span> move ·
        <span class="kbd">a</span> accept ·
        <span class="kbd">r</span> reject ·
        <span class="kbd">e</span> edit ·
        <span class="kbd">d</span> defer ·
        <span class="kbd">x</span> select
      </span>
      <button id="rvRefresh">Refresh</button>
    </div>
    <div class="split" style="flex:1;min-height:0">
      <div class="listcol" id="rvList"></div>
      <div class="detailcol" id="rvDetail">
        <div class="empty">Select a pending item. New here? Click <b>ⓘ Help</b>.</div>
      </div>
    </div>
  </div>`;

  document.querySelector('#rvRefresh').onclick = loadReview;
  document.querySelector('#catF').onchange = drawReview;
  document.querySelector('#bAcc').onclick = () => bulkReview('accept');
  document.querySelector('#bRej').onclick = () => bulkReview('reject');
  document.querySelector('#selAll').onclick = e => {
    const f = document.querySelector('#catF').value;
    const sh = rvItems.filter(i => !f || i.category === f);
    if (e.target.checked) sh.forEach(i => rvChecks.add(i.id));
    else rvChecks.clear();
    drawReview();
  };
  await loadReview();
}

async function loadReview() {
  const d = await api('GET', '/api/review?state=Pending');
  rvItems = d.items;
  const cats = [...new Set(rvItems.map(i => i.category))].sort();
  const cf = document.querySelector('#catF');
  const cur = cf.value;
  cf.innerHTML = '<option value="">all categories</option>' + cats.map(c => `<option ${c === cur ? 'selected' : ''}>${c}</option>`).join('');
  drawReview();
}

function drawReview() {
  const f = document.querySelector('#catF').value;
  const list = document.querySelector('#rvList');
  list.innerHTML = '';
  const shown = rvItems.filter(i => !f || i.category === f);
  if (!shown.length) { list.innerHTML = '<div class="empty">Queue empty. 🎉</div>'; return; }

  shown.forEach(it => {
    const row = document.createElement('div');
    row.className = 'row' + (rvSel === it.id ? ' sel' : '');
    row.innerHTML = `<input type="checkbox" ${rvChecks.has(it.id) ? 'checked' : ''}><div class="txt">
      <span class="sum"><span class="chip">${esc(it.category)}</span>${esc(it.summary)}</span>
      <span class="id">${esc(it.id)}</span></div>`;
    row.querySelector('input').addEventListener('click', e => {
      e.stopPropagation();
      e.target.checked ? rvChecks.add(it.id) : rvChecks.delete(it.id);
      document.querySelector('#selN').textContent = rvChecks.size;
    });
    row.addEventListener('click', () => selReview(it.id));
    list.appendChild(row);
  });
  document.querySelector('#selN').textContent = rvChecks.size;
}

function selReview(id) {
  rvSel = id;
  drawReview();
  const it = rvItems.find(i => i.id === id);
  if (!it) return;
  document.querySelector('#rvDetail').innerHTML = `<div class="meta">
    <span class="chip">${esc(it.category)}</span>
    confidence ${(+it.confidence).toFixed(2)} · ${esc(it.id)} · session ${esc(it.session)}
  </div>
    ${it.rationale ? `<div class="meta">⚠ ${esc(it.rationale)}</div>` : ''}
    <textarea class="edit" id="rvBody">${esc(it.replacement || it.summary)}</textarea>
    <div class="actions">
      <button class="primary" id="aAcc" title="Mark good AND write to durable memory">Accept &amp; promote</button>
      <button id="aAccOnly" title="Mark accepted but do not write to memory yet">Accept only</button>
      <button id="aEdit" title="Save your edits to the text">Save edit</button>
      <button id="aDef" title="Keep pending for later">Defer</button>
      <button class="danger" id="aRej" title="Discard — never becomes memory">Reject</button>
      <span class="hint">Accept &amp; promote = the normal action.</span>
    </div>`;

  document.querySelector('#aAcc').onclick = () => actReview(id, 'accept');
  document.querySelector('#aAccOnly').onclick = () => actReview(id, 'accept_only');
  document.querySelector('#aRej').onclick = () => actReview(id, 'reject');
  document.querySelector('#aDef').onclick = () => actReview(id, 'defer');
  document.querySelector('#aEdit').onclick = () => actReview(id, 'edit', { replacement: document.querySelector('#rvBody').value });
}

async function actReview(id, action, extra) {
  try {
    await api('POST', '/api/review/' + encodeURIComponent(id) + '/' + action, Object.assign({ reviewer: currentReviewer() }, extra || {}));
    const labels = { accept: 'Accepted + promoted', accept_only: 'Accepted (not promoted)', reject: 'Rejected', defer: 'Deferred', edit: 'Edit saved', promote: 'Promoted' };
    toast(labels[action] || action);
    rvChecks.delete(id);
    if (rvSel === id) { rvSel = null; document.querySelector('#rvDetail').innerHTML = '<div class="empty">Select a pending item.</div>'; }
    await loadReview();
    refreshPill();
  } catch (e) {
    toast(e.message, true);
  }
}

async function bulkReview(action) {
  if (!rvChecks.size) return toast('Nothing selected', true);
  if (!confirm(`${action} ${rvChecks.size} item(s)?`)) return;
  try {
    const r = await api('POST', '/api/review/bulk', { action, ids: [...rvChecks], reviewer: currentReviewer() });
    toast(`${action}: ${r.done} done${r.errors.length ? ', ' + r.errors.length + ' failed' : ''}`, r.errors.length > 0);
    rvChecks.clear();
    rvSel = null;
    document.querySelector('#rvDetail').innerHTML = '<div class="empty">Select a pending item.</div>';
    await loadReview();
    refreshPill();
  } catch (e) {
    toast(e.message, true);
  }
}

function handleReviewKeydown(e) {
  if ((location.hash.slice(1) || 'review') !== 'review') return;
  if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName)) return;

  const f = document.querySelector('#catF')?.value || '';
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
}

export { handleReviewKeydown, renderReview };
