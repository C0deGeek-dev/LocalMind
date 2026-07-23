// Review view
import { api, askReviewer, esc, refreshPill, reviewerName, toast } from '../app.js';

let rvItems = [], rvSel = null, rvChecks = new Set(), rvState = 'Pending';

// Review states a reviewer can browse. "Accepted" holds both items awaiting
// promotion and already-promoted ones — the list badges the difference and
// only awaiting items get a promote control.
const RV_STATES = ['Pending', 'Accepted', 'Edited', 'Deferred', 'Rejected', 'Merged'];

// The stored identity, or '' when none is set. Decision actions must not fire
// without one — the name lands in the append-only audit log as the actor, so
// there is deliberately no anonymous fallback here (the server keeps `ui`
// only as a defensive default for direct API callers).
function currentReviewer() {
  return reviewerName();
}

// Gate for every decision path (buttons, bulk, keyboard): with no identity,
// re-open the blocking modal instead of acting.
function requireReviewer() {
  if (currentReviewer()) return true;
  askReviewer();
  toast('Set your reviewer name first — decisions are recorded under it', true);
  return false;
}

function promotable(it) {
  return (it.state === 'Accepted' || it.state === 'Edited') && !it.promoted;
}

async function renderReview() {
  view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
    <div class="toolbar">
      <select id="stateF">${RV_STATES.map(s => `<option ${s === rvState ? 'selected' : ''}>${s}</option>`).join('')}</select>
      <input type="checkbox" id="selAll">
      <span class="pill"><span id="selN">0</span> selected</span>
      <button class="primary" id="bAcc" ${currentReviewer() ? '' : 'disabled'}>Accept selected</button>
      <button class="danger" id="bRej" ${currentReviewer() ? '' : 'disabled'}>Reject selected</button>
      <button id="bProm" style="display:none" ${currentReviewer() ? '' : 'disabled'}>Promote selected</button>
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
        <div class="empty">Select an item. New here? Click <b>ⓘ Help</b>.</div>
      </div>
    </div>
  </div>`;

  document.querySelector('#rvRefresh').onclick = loadReview;
  document.querySelector('#stateF').onchange = e => {
    rvState = e.target.value;
    rvChecks.clear();
    rvSel = null;
    loadReview();
  };
  document.querySelector('#catF').onchange = drawReview;
  document.querySelector('#bAcc').onclick = () => bulkReview('accept');
  document.querySelector('#bRej').onclick = () => bulkReview('reject');
  document.querySelector('#bProm').onclick = () => bulkReview('promote');
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
  const d = await api('GET', '/api/review?state=' + encodeURIComponent(rvState));
  rvItems = d.items;
  const cats = [...new Set(rvItems.map(i => i.category))].sort();
  const cf = document.querySelector('#catF');
  const cur = cf.value;
  cf.innerHTML = '<option value="">all categories</option>' + cats.map(c => `<option ${c === cur ? 'selected' : ''}>${c}</option>`).join('');
  // Bulk actions follow the view: accept/reject act on pending items, promote
  // on accepted/edited ones still awaiting promotion.
  const pending = rvState === 'Pending';
  document.querySelector('#bAcc').style.display = pending ? '' : 'none';
  document.querySelector('#bRej').style.display = pending ? '' : 'none';
  document.querySelector('#bProm').style.display =
    (rvState === 'Accepted' || rvState === 'Edited') ? '' : 'none';
  drawReview();
}

function drawReview() {
  const f = document.querySelector('#catF').value;
  const list = document.querySelector('#rvList');
  list.innerHTML = '';
  const shown = rvItems.filter(i => !f || i.category === f);
  if (!shown.length) {
    list.innerHTML = rvState === 'Pending'
      ? '<div class="empty">Queue empty. 🎉</div>'
      : `<div class="empty">No ${esc(rvState.toLowerCase())} items.</div>`;
    return;
  }

  shown.forEach(it => {
    const row = document.createElement('div');
    row.className = 'row' + (rvSel === it.id ? ' sel' : '');
    const badge = (it.state === 'Accepted' || it.state === 'Edited')
      ? (it.promoted ? '<span class="chip">promoted</span>' : '<span class="chip">awaiting promotion</span>')
      : '';
    row.innerHTML = `<input type="checkbox" ${rvChecks.has(it.id) ? 'checked' : ''}><div class="txt">
      <span class="sum"><span class="chip">${esc(it.category)}</span>${badge}${esc(it.summary)}</span>
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
  const pending = it.state === 'Pending';
  const stateLine = pending ? '' : `<span class="chip">${esc(it.state)}${it.promoted ? ' · promoted' : ''}</span>`;
  const dis = currentReviewer() ? '' : 'disabled';
  const actions = pending
    ? `<button class="primary" id="aAcc" ${dis} title="Mark good AND write to durable memory">Accept &amp; promote</button>
      <button id="aAccOnly" ${dis} title="Mark accepted but do not write to memory yet">Accept only</button>
      <button id="aEdit" ${dis} title="Save your edits to the text">Save edit</button>
      <button id="aDef" ${dis} title="Keep pending for later">Defer</button>
      <button class="danger" id="aRej" ${dis} title="Discard — never becomes memory">Reject</button>
      <span class="hint">Accept &amp; promote = the normal action.</span>`
    : promotable(it)
      ? `<button class="primary" id="aProm" ${dis} title="Write this accepted item to durable memory">Promote to memory</button>
        <button id="aEdit" ${dis} title="Save your edits to the text">Save edit</button>
        <span class="hint">Accepted earlier with "Accept only" — promote when ready.</span>`
      : it.promoted
        ? '<span class="hint">Already promoted to durable memory.</span>'
        : '';
  const editNeeded = it.requires_edit && !it.replacement
    ? '<div class="meta">⚠ Source excerpt — edit it into a standalone lesson (Save edit) before promoting.</div>'
    : '';
  const evidence = it.evidence_text
    ? `<details><summary>Full source evidence (review-only — never promoted into memory)</summary>
        <pre style="white-space:pre-wrap;max-height:16em;overflow:auto">${esc(it.evidence_text)}</pre></details>`
    : '';
  document.querySelector('#rvDetail').innerHTML = `<div class="meta">
    <span class="chip">${esc(it.category)}</span>${stateLine}
    confidence ${(+it.confidence).toFixed(2)} · ${esc(it.id)} · session ${esc(it.session)}
  </div>
    ${it.rationale ? `<div class="meta">⚠ ${esc(it.rationale)}</div>` : ''}
    ${editNeeded}
    <textarea class="edit" id="rvBody">${esc(it.replacement || it.summary)}</textarea>
    ${evidence}
    <div class="actions">${actions}</div>`;

  const on = (sel, fn) => { const el = document.querySelector(sel); if (el) el.onclick = fn; };
  on('#aAcc', () => actReview(id, 'accept'));
  on('#aAccOnly', () => actReview(id, 'accept_only'));
  on('#aProm', () => actReview(id, 'promote'));
  on('#aRej', () => actReview(id, 'reject'));
  on('#aDef', () => actReview(id, 'defer'));
  on('#aEdit', () => actReview(id, 'edit', { replacement: document.querySelector('#rvBody').value }));
}

async function actReview(id, action, extra) {
  if (!requireReviewer()) return;
  try {
    await api('POST', '/api/review/' + encodeURIComponent(id) + '/' + action, Object.assign({ reviewer: currentReviewer() }, extra || {}));
    const labels = { accept: 'Accepted + promoted', accept_only: 'Accepted (not promoted)', reject: 'Rejected', defer: 'Deferred', edit: 'Edit saved', promote: 'Promoted' };
    toast(labels[action] || action);
    rvChecks.delete(id);
    if (rvSel === id) { rvSel = null; document.querySelector('#rvDetail').innerHTML = '<div class="empty">Select an item.</div>'; }
    await loadReview();
    refreshPill();
  } catch (e) {
    toast(e.message, true);
  }
}

async function bulkReview(action) {
  if (!requireReviewer()) return;
  if (!rvChecks.size) return toast('Nothing selected', true);
  if (!confirm(`${action} ${rvChecks.size} item(s)?`)) return;
  try {
    const r = await api('POST', '/api/review/bulk', { action, ids: [...rvChecks], reviewer: currentReviewer() });
    toast(`${action}: ${r.done} done${r.errors.length ? ', ' + r.errors.length + ' failed' : ''}`, r.errors.length > 0);
    rvChecks.clear();
    rvSel = null;
    document.querySelector('#rvDetail').innerHTML = '<div class="empty">Select an item.</div>';
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
    const it = rvItems.find(i => i.id === rvSel);
    // Accept/reject act on pending items only; on other views the keys are
    // inert rather than firing actions the backend would reject.
    if (e.key === 'a' && it?.state === 'Pending') actReview(rvSel, 'accept');
    else if (e.key === 'r' && it?.state === 'Pending') actReview(rvSel, 'reject');
    else if (e.key === 'd' && it?.state === 'Pending') actReview(rvSel, 'defer');
    else if (e.key === 'e') { const t = document.querySelector('#rvBody'); if (t) t.focus(); }
    else if (e.key === 'x') { rvChecks.has(rvSel) ? rvChecks.delete(rvSel) : rvChecks.add(rvSel); drawReview(); }
  }
}

export { handleReviewKeydown, renderReview };
