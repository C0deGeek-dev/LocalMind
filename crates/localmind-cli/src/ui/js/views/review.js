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
      <select id="stateF"><option value="" ${rvState === '' ? 'selected' : ''}>All states</option>${RV_STATES.map(s => `<option ${s === rvState ? 'selected' : ''}>${s}</option>`).join('')}</select>
      <input type="checkbox" id="selAll">
      <span class="pill" id="rvSelPill"><span id="selN">0</span> selected</span>
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
        <span class="kbd">d</span> defer<span id="rvSelHint"> · <span class="kbd">x</span> select</span>
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
  // The All view (empty rvState) omits the state parameter entirely, which the
  // backend answers with every state; a named state filters as before.
  const url = rvState ? '/api/review?state=' + encodeURIComponent(rvState) : '/api/review';
  const d = await api('GET', url);
  rvItems = d.items;
  const cats = [...new Set(rvItems.map(i => i.category))].sort();
  const cf = document.querySelector('#catF');
  const cur = cf.value;
  cf.innerHTML = '<option value="">all categories</option>' + cats.map(c => `<option ${c === cur ? 'selected' : ''}>${c}</option>`).join('');
  // Bulk actions follow the view: accept/reject act on pending items, promote
  // on accepted/edited ones still awaiting promotion.
  const pending = rvState === 'Pending';
  // The All view has no bulk-selection surface: a bulk decide over a mixed
  // selection is not a safe state transition, so hide select-all, the count
  // pill and the x-select hint there (the row checkboxes are dropped in
  // drawReview and the x shortcut is inert).
  const allView = rvState === '';
  for (const s of ['#selAll', '#rvSelPill', '#rvSelHint']) {
    document.querySelector(s).style.display = allView ? 'none' : '';
  }
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
      : rvState === ''
        ? '<div class="empty">No review items.</div>'
        : `<div class="empty">No ${esc(rvState.toLowerCase())} items.</div>`;
    return;
  }

  const allView = rvState === '';
  shown.forEach(it => {
    const row = document.createElement('div');
    row.className = 'row' + (rvSel === it.id ? ' sel' : '');
    // In the mixed All view each row carries its own state; named-state views
    // already know their state from the filter.
    const stateChip = allView ? `<span class="chip">${esc(it.state)}</span>` : '';
    const badge = (it.state === 'Accepted' || it.state === 'Edited')
      ? (it.promoted ? '<span class="chip">promoted</span>' : '<span class="chip">awaiting promotion</span>')
      : '';
    const check = allView ? '' : `<input type="checkbox" ${rvChecks.has(it.id) ? 'checked' : ''}>`;
    row.innerHTML = `${check}<div class="txt">
      <span class="sum">${stateChip}<span class="chip">${esc(it.category)}</span>${badge}${esc(it.summary)}</span>
      <span class="id">${esc(it.id)}</span></div>`;
    const cb = row.querySelector('input');
    if (cb) {
      cb.addEventListener('click', e => {
        e.stopPropagation();
        e.target.checked ? rvChecks.add(it.id) : rvChecks.delete(it.id);
        document.querySelector('#selN').textContent = rvChecks.size;
      });
    }
    row.addEventListener('click', () => selReview(it.id));
    list.appendChild(row);
  });
  document.querySelector('#selN').textContent = rvChecks.size;
}

// The hindsight and experiment cards. The server builds them — the same data
// and the same wording the command line prints — and this only lays them out.
// Every signal is a word: a status label is text first, and its colour only
// repeats what the text already says.
function row(label, value) {
  return value ? `<div class="crow"><span class="clabel">${esc(label)}</span><span>${esc(value)}</span></div>` : '';
}

function causeHtml(label, c) {
  const cites = c.cites.length ? `cites ${c.cites.join(', ')}` : 'cites no fact';
  return row(label, `${c.claim} (confidence ${(+c.confidence).toFixed(2)}; ${cites})`);
}

function hindsightHtml(h, missing) {
  if (!h) return `<section class="card"><h4>Hindsight</h4><p class="cnote">${esc(missing || '')}</p></section>`;
  const facts = h.facts.map(f => `<li><span class="cid">${esc(f.id)}</span> ${esc(f.kind)}: ${esc(f.label)}${f.redacted ? ' <span class="ctag">redacted</span>' : ''}
      ${f.excerpt ? `<pre class="cexcerpt">${esc(f.excerpt)}</pre>` : ''}</li>`).join('');
  return `<section class="card">
    <h4>Hindsight${h.safe_abstention ? ' <span class="ctag ok">safe outcome: nothing proposed for memory</span>' : ''}</h4>
    ${row('intended', h.intended)}
    ${row('observed', h.observed)}
    ${row('outcome', h.outcome)}
    ${row('analysis', h.analysis)}
    <div class="crow"><span class="clabel">facts</span><span>${h.facts.length} recorded by the run, not editable<ul class="cfacts">${facts}</ul></span></div>
    ${h.cause ? causeHtml('cause', h.cause) : row('cause', 'none established')}
    ${h.alternatives.map(a => causeHtml('alternative', a)).join('')}
    ${h.missed_signals.map(s => row('missed signal', s)).join('')}
    ${row('intervention', h.intervention)}
    ${row('had it been applied', h.counterfactual)}
    ${row('applies', h.applicability)}
    ${h.preconditions.map(p => row('needs', p)).join('')}
    ${row('stops being true when', h.invalidation)}
  </section>`;
}

// Milliseconds under a second, and nothing at all for an unmeasured zero — the
// same rule the text cards use.
function took(ms) {
  if (!ms) return '';
  return ms < 1000 ? `${ms} ms` : (ms / 1000).toFixed(1) + ' s';
}

function experimentHtml(e, n) {
  const tags = (e.stale ? '<span class="ctag warn">stale: about an earlier version — does not count until rerun</span>' : '')
    + (e.harmful ? '<span class="ctag bad">harmful result: held for a person</span>' : '');
  const arms = e.arms.map(a => row('arm ' + a.arm,
    `${a.passed} of ${a.attempts} passed${took(a.wall_ms) ? ', ' + took(a.wall_ms) : ''}${a.cancelled ? ', cancelled' : ''}${a.truncated ? ', output truncated' : ''}${a.observations.length ? ' — ' + a.observations.join('; ') : ''}`)).join('');
  const state = { available: 'available', no_longer_retained: 'no longer retained; the result itself still stands', not_checked: 'not checked from here' };
  const run = `revision ${e.source_revision.slice(0, 11)}${e.model ? ', model ' + e.model : ''}${e.repetitions ? `, ${e.repetitions} attempt(s)${took(e.wall_ms) ? ', ' + took(e.wall_ms) + ' in total' : ''}` : ''}`;
  return `<section class="card">
    <h4>${n}. ${esc(e.tier)} — ${esc(e.verdict)} ${tags}</h4>
    <p class="cmeaning"><b>${esc(e.verdict)}</b> — ${esc(e.meaning)}</p>
    <p class="cnote">${esc(e.tier)} ${esc(e.tier_meaning)}.</p>
    ${e.reasons.map(r => row('reason', r)).join('')}
    ${row('tested', e.task)}
    ${row('from', e.assignment_source)}
    ${row('decided by', e.oracle)}
    ${arms}
    ${row('injection', e.injection)}
    ${row('run', run)}
    ${e.limitations.map(l => row('limit', l)).join('')}
    ${e.details.map(d => row('details', `${d.locator} (${d.summary}) — ${state[d.state] || d.state}`)).join('')}
  </section>`;
}

function cardsHtml(c) {
  if (!c) return '';
  const lineage = c.lineage.length
    ? `<section class="card"><h4>Lineage</h4>${c.lineage.map(l => `<p class="cnote">${esc(l)}</p>`).join('')}</section>` : '';
  const hold = c.hold ? `<div class="chold">${esc(c.hold)}</div>` : '';
  const untested = c.untested ? `<section class="card"><h4>Experiments</h4><p class="cnote">${esc(c.untested)}</p></section>` : '';
  return `<div class="cards">
    ${lineage}${hold}
    ${hindsightHtml(c.hindsight, c.no_hindsight)}
    ${untested}${c.experiments.map((e, i) => experimentHtml(e, i + 1)).join('')}
    <div class="cnext"><b>Next.</b> ${esc(c.next)}</div>
  </div>`;
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
    ${cardsHtml(it.cards)}
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
    else if (e.key === 'x' && rvState !== '') { rvChecks.has(rvSel) ? rvChecks.delete(rvSel) : rvChecks.add(rvSel); drawReview(); }
  }
}

export { handleReviewKeydown, renderReview };
