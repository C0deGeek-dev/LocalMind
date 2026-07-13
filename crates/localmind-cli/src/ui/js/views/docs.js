// Docs view
import { api, esc, toast } from '../app.js';

let docFiles = [];

async function renderDocs() {
  const examples = [
    'how does the release train work',
    'what is the review gate',
    'how do embeddings work',
    'what does LocalBox do',
    'how are secrets redacted'
  ];

  view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
    <div class="toolbar">
      <input type="text" id="docQ" placeholder="ask a question — search by meaning…" size="34">
      <button id="docGo">Search</button>
      <span class="grow"></span>
      <span class="pill" id="docN"></span>
    </div>
    <div class="split" style="flex:1;min-height:0">
      <div class="listcol">
        <div class="subhead">Browse files
          <select id="docRepo" style="width:92%;margin:6px 0;display:block"></select>
          <input type="text" id="docFilter" placeholder="filter files…" style="width:92%;display:block">
        </div>
        <div id="docFiles"></div>
      </div>
      <div class="detailcol" id="docRes">
        <div class="meta">Every repo's Markdown — READMEs, vision, ADRs, plans — searchable by <b>meaning</b>, not just keywords. Open a file on the left, or ask:</div>
        <div class="actions">${examples.map(q => `<button class="sm" data-ex="${esc(q)}">${esc(q)}</button>`).join('')}</div>
      </div>
    </div>
  </div>`;

  document.querySelector('#docGo').onclick = docSearch;
  document.querySelector('#docQ').addEventListener('keydown', e => { if (e.key === 'Enter') docSearch(); });
  document.querySelector('#docFilter').addEventListener('input', drawDocFiles);
  document.querySelectorAll('#docRes [data-ex]').forEach(b => b.onclick = () => {
    document.querySelector('#docQ').value = b.dataset.ex;
    docSearch();
  });

  let d;
  try {
    d = await api('GET', '/api/docs/files');
  } catch (e) {
    document.querySelector('#docN').textContent = 'error';
    document.querySelector('#docRes').innerHTML = `<div class="empty">${esc(e.message)}</div>`;
    return;
  }
  docFiles = d.files;
  document.querySelector('#docN').textContent = `${d.total} files`;
  if (!docFiles.length) {
    document.querySelector('#docRes').innerHTML = `<div class="empty">Nothing ingested yet — the doc index is empty.<br><br>
      Run <code>localmind ingest docs &lt;path&gt; --project &lt;project-root&gt;</code> to index a tree's Markdown
      (in a LocalPilot workspace, <code>localpilot ingest run</code> feeds this index too), then reload this tab.
      Semantic search additionally needs <code>[inference] embedding_base_url</code> +
      <code>embedding_model</code> in <code>.localmind.toml</code>; browsing works without embeddings.</div>`;
    return;
  }

  const repos = {};
  docFiles.forEach(x => { const r = x.path.split('/')[0]; repos[r] = (repos[r] || 0) + 1; });
  document.querySelector('#docRepo').innerHTML = `<option value="">all repos (${docFiles.length})</option>` +
    Object.entries(repos).sort((a, b) => b[1] - a[1]).map(([r, c]) => `<option value="${esc(r)}">${esc(r)} (${c})</option>`).join('');
  document.querySelector('#docRepo').onchange = drawDocFiles;
  drawDocFiles();
}

function drawDocFiles() {
  const f = (document.querySelector('#docFilter')?.value || '').toLowerCase();
  const repoF = document.querySelector('#docRepo')?.value || '';
  const el = document.querySelector('#docFiles');
  el.innerHTML = '';

  const files = docFiles.filter(x => (!repoF || x.path.split('/')[0] === repoF) && (!f || x.path.toLowerCase().includes(f)));
  const groups = {};
  files.forEach(x => { const repo = x.path.split('/')[0]; (groups[repo] = groups[repo] || []).push(x); });

  Object.keys(groups).sort().forEach(repo => {
    const h = document.createElement('div');
    h.className = 'subhead';
    h.style.position = 'static';
    h.innerHTML = `<b>${esc(repo)}</b> · ${groups[repo].length}`;
    el.appendChild(h);

    groups[repo].slice(0, 300).forEach(x => {
      const row = document.createElement('div');
      row.className = 'row';
      const short = x.path.slice(repo.length + 1) || x.path;
      row.innerHTML = `<div class="txt"><span class="sum">📄 ${esc(short)}</span><span class="id">${x.chunks} chunk${x.chunks > 1 ? 's' : ''}</span></div>`;
      row.addEventListener('click', () => openDocFile(x.path));
      el.appendChild(row);
    });
  });
}

async function openDocFile(path) {
  try {
    const d = await api('GET', '/api/docs/file?path=' + encodeURIComponent(path));
    document.querySelector('#docRes').innerHTML = `<div class="meta">📄 <b>${esc(path)}</b> · ${d.chunks.length} passages</div>` +
      d.chunks.map(c => `<div class="result"><div class="h">#${c.ordinal}${c.heading ? ' › ' + esc(c.heading) : ''}</div><div class="body">${esc(c.body)}</div></div>`).join('');
  } catch (e) {
    toast(e.message, true);
  }
}

async function docSearch() {
  const query = document.querySelector('#docQ').value.trim();
  if (!query) return;
  document.querySelector('#docRes').innerHTML = '<div class="empty">Searching…</div>';
  try {
    const d = await api('GET', '/api/docs?' + new URLSearchParams({ q: query, limit: '12' }));
    if (!d.embeddings_configured) {
      document.querySelector('#docN').textContent = 'semantic search unavailable';
      document.querySelector('#docRes').innerHTML = `<div class="empty">Semantic search needs an embedding endpoint, and none is configured —
        an empty result here says nothing about your docs.<br><br>Set <code>[inference] embedding_base_url</code> +
        <code>embedding_model</code> in <code>.localmind.toml</code>, start the embed server
        (<code>localbox embed-serve</code>), re-run <code>localmind ingest docs</code>, then search again.
        Browsing files on the left works without embeddings.</div>`;
      return;
    }
    document.querySelector('#docN').textContent = d.results.length + ' passages';
    document.querySelector('#docRes').innerHTML = `<div class="meta">semantic results for "${esc(query)}"</div>` +
      d.results.map(r => `<div class="result">
        <div class="h">📄 ${esc(r.path)}${r.heading ? ' › ' + esc(r.heading) : ''} · score ${(+r.score).toFixed(3)}</div>
        <div class="body">${esc(r.body)}</div></div>`).join('') || '<div class="empty">No matches.</div>';
  } catch (e) {
    document.querySelector('#docRes').innerHTML = `<div class="empty">${esc(e.message)}</div>`;
  }
}

export { renderDocs };
