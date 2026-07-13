// Graph view — search, overview, force-directed canvas, details
import { api, esc } from './app.js';

const GKIND = {
  function: '#00f0ff',
  test: '#10b981',
  file: '#f59e0b',
  type: '#a855f7',
  module: '#ef4444',
  dependency: '#5a6a80',
  repository: '#3a4a60'
};

let gToken = 0, gHistory = [];
// ── Public entry ──
async function renderGraph() {
  view.innerHTML = `<div style="display:flex;flex-direction:column;height:100%">
    <div class="toolbar">
      <input type="text" id="gSym" placeholder="search a function or test by name…" size="26">
      <button id="gGo">Search</button>
      <button class="sm" id="gHome">overview</button>
      <button class="sm" id="gGlobal">global map</button>
      <span class="grow"></span>
      <span class="hint" id="gLegend"></span>
      <span class="pill" id="gN">code graph</span>
    </div>
    <div class="split" style="flex:1;min-height:0">
      <div class="listcol" id="gList"><div class="empty">Search a symbol, or open a hotspot from the overview →</div></div>
      <div class="detailcol" id="gRes" style="padding:0;display:flex;flex-direction:column"></div>
    </div>
  </div>`;

  document.querySelector('#gLegend').innerHTML = ['function', 'test', 'file', 'type', 'module'].map(k => `<span style="color:${GKIND[k]}">●</span>${k}`).join(' ');

  const go = async () => {
    const query = document.querySelector('#gSym').value.trim();
    const d = await api('GET', '/api/graph/symbols?' + new URLSearchParams({ q: query, limit: '150' }));
    document.querySelector('#gN').textContent = `${d.symbols.length} symbols`;
    const el = document.querySelector('#gList');
    el.innerHTML = d.symbols.length ? '' : '<div class="empty">No symbols match.</div>';

    d.symbols.forEach(s => {
      const row = document.createElement('div');
      row.className = 'row';
      row.innerHTML = `<div class="txt"><span class="sum"><span style="color:${GKIND[s.kind] || '#5a6a80'}">●</span> ${esc(s.qualified_name.split('::').pop())}</span><span class="id">${esc(s.qualified_name)}</span></div>`;
      row.addEventListener('click', () => graphShow(s.qualified_name));
      el.appendChild(row);
    });
  };

  document.querySelector('#gGo').onclick = go;
  document.querySelector('#gSym').addEventListener('keydown', e => { if (e.key === 'Enter') go(); });
  document.querySelector('#gHome').onclick = graphOverview;
  document.querySelector('#gGlobal').onclick = renderGlobal;
  graphOverview();
}

function gBack() {
  gHistory.pop();
  const prev = gHistory[gHistory.length - 1];
  if (prev) graphShow(prev, false);
  else graphOverview();
}

// ── Overview ──
async function graphOverview() {
  gToken++;
  gHistory = [];
  const el = document.querySelector('#gRes');
  el.style.padding = '20px 24px';
  el.innerHTML = '<div class="empty">Loading overview…</div>';

  try {
    const o = await api('GET', '/api/graph/overview');
    const clickList = (t, items, label, val) => `<div class="barwrap"><h3>${t}</h3>${(items || []).map(x => {
      const v = val(x);
      return `<div class="bar" data-sym="${x.qualified_name ? esc(x.qualified_name) : ''}" style="${x.qualified_name ? 'cursor:pointer' : ''}">
        <span class="lbl">${esc(label(x))}</span><span class="track"><span class="fill" style="width:${v / Math.max(1, val(items[0])) * 100}%"></span></span><span class="v">${v}</span></div>`;
    }).join('') || '<div class="empty">none</div>'}</div>`;

    el.innerHTML = `<div class="meta" style="max-width:660px">The <b>code graph</b> maps every function, file and test across your repos and how they connect — calls, imports, tests. Ask it: what calls this? what tests this? what did we learn about this code? <b>Click a name below to open its graph</b>, or search one.</div>
      <div class="cards"><div class="card"><div class="n">${o.file_count}</div><div class="l">files</div></div>
        <div class="card"><div class="n">${o.symbol_count}</div><div class="l">symbols</div></div>
      ${clickList('Most-called functions — click to explore', o.hotspots, h => h.qualified_name.split('::').pop() + '  ·  ' + h.qualified_name.split('/')[0], h => h.in_degree)}
      ${clickList('Entry points — called by nothing else', o.entry_points, h => h.qualified_name.split('::').pop() + '  ·  ' + h.qualified_name.split('/')[0], h => h.out_degree)}
      <div class="barwrap"><h3>Languages by files</h3>${o.languages.map(l => `<div class="bar"><span class="lbl">${esc(l.language)}</span><span class="track"><span class="fill" style="width:${l.file_count / Math.max(1, o.languages[0].file_count) * 100}%"></span></span><span class="v">${l.file_count}</span></div>`).join('')}</div>`;

    el.querySelectorAll('.bar[data-sym]').forEach(b => { if (b.dataset.sym) b.onclick = () => graphShow(b.dataset.sym); });
  } catch (e) {
    el.innerHTML = `<div class="empty">${esc(e.message)}</div>`;
  }
}

// ── Symbol neighborhood graph ──
async function graphShow(symbol, record = true) {
  gToken++;
  const my = gToken;
  if (record) gHistory.push(symbol);

  const el = document.querySelector('#gRes');
  el.style.padding = '0';
  el.innerHTML = `<div class="toolbar" style="z-index:1">
    <button class="sm" id="gBack" title="Back">←</button>
    <b style="font-family:var(--font-mono);font-size:12px">${esc(symbol.split('::').pop())}</b>
    <span class="hint">${esc(symbol)}</span><span class="grow"></span>
    <span class="hint">drag pan · scroll zoom · click a node to recenter</span>
    <button class="sm" id="gInfo">details</button>
  </div>
    <div id="graphHost" style="flex:1;min-height:0;position:relative"><div class="empty">Loading…</div></div>`;

  document.querySelector('#gBack').onclick = gBack;
  document.querySelector('#gInfo').onclick = () => graphDetails(symbol);

  try {
    const d = await api('GET', '/api/graph/local?' + new URLSearchParams({ symbol, depth: '1' }));
    if (my !== gToken) return;
    if (!d.nodes.length) { document.querySelector('#graphHost').innerHTML = '<div class="empty">Not in the graph.</div>'; return; }
    forceGraph(document.querySelector('#graphHost'), d, graphShow, my);
  } catch (e) {
    document.querySelector('#graphHost').innerHTML = `<div class="empty">${esc(e.message)}</div>`;
  }
}

// ── Global file map ──
async function renderGlobal() {
  gToken++;
  const my = gToken;
  gHistory = [];
  const el = document.querySelector('#gRes');
  el.style.padding = '0';
  el.innerHTML = `<div class="toolbar" style="z-index:1">
    <b>Global map</b>
    <select id="glRepo"><option value="">all repos</option></select>
    <span class="hint" id="glNote"></span><span class="grow"></span>
    <span class="hint">files as nodes · click a file to open it</span>
  </div>
    <div id="graphHost" style="flex:1;min-height:0;position:relative"><div class="empty">Loading the map…</div></div>`;

  const load = async (prefix) => {
    const d = await api('GET', '/api/graph/global?' + new URLSearchParams({ path: prefix || '', limit: '200' }));
    if (my !== gToken) return;
    document.querySelector('#glNote').textContent = `showing ${d.shown} of ${d.total_connected} connected files`;
    if (!d.nodes.length) { document.querySelector('#graphHost').innerHTML = '<div class="empty">No connected files here.</div>'; return; }
    forceGraph(document.querySelector('#graphHost'), d, s => graphShow(s), my);
  };

  try {
    const first = await api('GET', '/api/graph/global?limit=400');
    if (my !== gToken) return;
    const repos = [...new Set(first.nodes.map(n => n.path.split('/')[0]))].sort();
    document.querySelector('#glRepo').innerHTML = '<option value="">all repos</option>' + repos.map(r => `<option>${esc(r)}</option>`).join('');
    document.querySelector('#glRepo').onchange = () => load(document.querySelector('#glRepo').value);
    load('');
  } catch (e) {
    document.querySelector('#graphHost').innerHTML = `<div class="empty">${esc(e.message)}</div>`;
  }
}

// ── Details panel ──
async function graphDetails(symbol) {
  gToken++;
  const el = document.querySelector('#gRes');
  el.style.padding = '16px 20px';
  el.innerHTML = `<div class="actions" style="margin-bottom:12px">
    <button class="sm" id="dBack">← graph</button>
    <button class="sm primary" id="dNb">neighbors</button>
    <button class="sm" id="dCov">tests</button>
    <button class="sm" id="dKn">knowledge</button>
  </div><div id="dBody"></div>`;

  document.querySelector('#dBack').onclick = () => graphShow(symbol);
  const run = async (tool, btn) => {
    document.querySelectorAll('#gRes .actions .sm').forEach(b => b.classList.remove('primary'));
    btn.classList.add('primary');
    document.querySelector('#dBody').innerHTML = '<div class="empty">…</div>';
    const d = await api('GET', '/api/graph?' + new URLSearchParams({ tool, symbol, depth: '1' }));
    document.querySelector('#dBody').innerHTML = renderGraphResult(d);
  };

  document.querySelector('#dNb').onclick = e => run('neighborhood', e.target);
  document.querySelector('#dCov').onclick = e => run('coverage', e.target);
  document.querySelector('#dKn').onclick = e => run('knowledge', e.target);
  run('neighborhood', document.querySelector('#dNb'));
}

// ── Graph result rendering ──
function sym(s) {
  return `<div class="result"><div class="h"><span style="color:${GKIND[s.kind] || '#5a6a80'}">●</span> ${esc(s.kind)} · ${esc(s.qualified_name)}</div>${s.skeleton ? `<div class="body">${esc(s.skeleton)}</div>` : ''}</div>`;
}

export function renderGraphResult(d) {
  if (d.graph_error) return `<div class="empty">${esc(d.graph_error)}</div>`;
  if (d.result === 'neighborhood') return `<div class="meta">Directly connected — by calls, imports, or tests.</div>${sym(d.symbol)}${(d.neighbors.map(sym).join('') || '<div class="empty">none</div>')}`;
  if (d.result === 'coverage') return `<div class="meta">Tests that exercise this symbol.</div>${sym(d.symbol)}${(d.tests.map(sym).join('') || '<div class="empty">no tests attached</div>')}`;
  if (d.result === 'knowledge') return `<div class="meta">Accepted lessons anchored to this code.</div>${sym(d.symbol)}${(d.knowledge.map(k => `<div class="result">memory ${esc(k.memory_id)} · confidence ${(+k.confidence).toFixed(2)}</div>`).join('') || '<div class="empty">no memory anchored here yet</div>')}</div>`;
  return `<div class="body">${esc(JSON.stringify(d, null, 2))}</div>`;
}

// ── Force-directed graph (SVG) ──
function forceGraph(host, data, onRecenter, token) {
  const W = host.clientWidth || 700, H = host.clientHeight || 460, cx = W / 2, cy = H / 2;
  const NS = 'http://www.w3.org/2000/svg';

  const nodes = data.nodes.map((n, i) => {
    const a = i / data.nodes.length * 6.283;
    return { ...n, x: cx + Math.cos(a) * 130 + ((i * 29) % 40), y: cy + Math.sin(a) * 130 + ((i * 17) % 40), vx: 0, vy: 0, pin: false };
  });

  const by = {};
  nodes.forEach(n => by[n.id] = n);
  const links = data.edges.map(e => ({ s: by[e.from], t: by[e.to] })).filter(l => l.s && l.t);

  const svg = document.createElementNS(NS, 'svg');
  svg.setAttribute('width', '100%');
  svg.setAttribute('height', '100%');
  svg.style.cssText = 'display:block;cursor:grab;touch-action:none';
  const g = document.createElementNS(NS, 'g');
  svg.appendChild(g);

  host.innerHTML = '';
  host.appendChild(svg);

  // Edges
  const lineEls = links.map(() => {
    const l = document.createElementNS(NS, 'line');
    l.setAttribute('stroke', 'rgba(0, 240, 255, 0.18)');
    l.setAttribute('stroke-width', '1');
    g.appendChild(l);
    return l;
  });

  // Drag state
  const drag = { node: null, moved: false, pan: null };
  let tx = 0, ty = 0, scale = 1;
  const applyView = () => g.setAttribute('transform', `translate(${tx},${ty}) scale(${scale})`);
  const showLabels = nodes.length <= 55;

  // Nodes
  const nodeEls = nodes.map(n => {
    const grp = document.createElementNS(NS, 'g');
    grp.style.cursor = 'pointer';
    const r = n.focus ? 10 : 6;
    const c = document.createElementNS(NS, 'circle');
    c.setAttribute('r', r);
    c.setAttribute('fill', GKIND[n.kind] || '#5a6a80');
    if (n.focus) {
      c.setAttribute('stroke', '#00f0ff');
      c.setAttribute('stroke-width', '2.5');
      c.style.filter = 'drop-shadow(0 0 7px rgba(0, 240, 255, 0.9))';
    }
    const t = document.createElementNS(NS, 'text');
    t.textContent = n.name;
    t.setAttribute('x', r + 4);
    t.setAttribute('y', 4);
    t.setAttribute('font-size', '11');
    t.setAttribute('fill', '#e0e8f0');
    t.style.pointerEvents = 'none';
    t.style.opacity = showLabels ? '1' : '0';
    grp.appendChild(c);
    grp.appendChild(t);
    g.appendChild(grp);

    grp.addEventListener('mousedown', ev => { ev.stopPropagation(); drag.node = n; drag.moved = false; n.pin = true; });
    grp.addEventListener('mouseenter', () => { t.style.opacity = '1'; t.setAttribute('font-weight', '700'); });
    grp.addEventListener('mouseleave', () => { t.style.opacity = showLabels ? '1' : '0'; t.setAttribute('font-weight', '400'); });

    return { n, grp };
  });

  const toGraph = ev => {
    const b = svg.getBoundingClientRect();
    return { x: (ev.clientX - b.left - tx) / scale, y: (ev.clientY - b.top - ty) / scale };
  };

  svg.addEventListener('mousedown', ev => {
    if (drag.node) return;
    drag.pan = { x: ev.clientX, y: ev.clientY, tx, ty };
    svg.style.cursor = 'grabbing';
  });

  const move = ev => {
    if (drag.node) {
      drag.moved = true;
      const p = toGraph(ev);
      drag.node.x = p.x;
      drag.node.y = p.y;
    } else if (drag.pan) {
      tx = drag.pan.tx + (ev.clientX - drag.pan.x);
      ty = drag.pan.ty + (ev.clientY - drag.pan.y);
      applyView();
    }
  };

  const up = () => {
    if (drag.node) {
      const n = drag.node;
      n.pin = false;
      drag.node = null;
      if (!drag.moved) onRecenter(n.qualified_name);
    }
    drag.pan = null;
    svg.style.cursor = 'grab';
  };

  window.addEventListener('mousemove', move);
  window.addEventListener('mouseup', up);

  svg.addEventListener('wheel', ev => {
    ev.preventDefault();
    scale = Math.max(0.25, Math.min(4, scale * (ev.deltaY < 0 ? 1.12 : 0.89)));
    applyView();
  }, { passive: false });

  // Physics
  const KR = 2400, KS = 0.035, REST = 95, CEN = 0.004, DAMP = 0.85;
  let frame = 0;

  (function tick() {
    if (token !== gToken) {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      return;
    }
    for (let i = 0; i < nodes.length; i++) {
      const a = nodes[i];
      for (let j = 0; j < nodes.length; j++) {
        if (i === j) continue;
        const b = nodes[j];
        let dx = a.x - b.x, dy = a.y - b.y, d2 = dx * dx + dy * dy || 0.01, d = Math.sqrt(d2), f = KR / d2;
        a.vx += dx / d * f; a.vy += dy / d * f;
      }
    }
    links.forEach(l => {
      let dx = l.t.x - l.s.x, dy = l.t.y - l.s.y, d = Math.hypot(dx, dy) || 0.01, f = (d - REST) * KS, ux = dx / d, uy = dy / d;
      l.s.vx += ux * f; l.s.vy += uy * f;
      l.t.vx -= ux * f; l.t.vy -= uy * f;
    });
    nodes.forEach(n => {
      n.vx += (cx - n.x) * CEN;
      n.vy += (cy - n.y) * CEN;
      n.vx *= DAMP; n.vy *= DAMP;
      if (!n.pin) { n.x += n.vx; n.y += n.vy; }
    });

    links.forEach((l, i) => {
      lineEls[i].setAttribute('x1', l.s.x);
      lineEls[i].setAttribute('y1', l.s.y);
      lineEls[i].setAttribute('x2', l.t.x);
      lineEls[i].setAttribute('y2', l.t.y);
    });
    nodeEls.forEach(ne => ne.grp.setAttribute('transform', `translate(${ne.n.x},${ne.n.y})`));

    if (frame++ < 700) requestAnimationFrame(tick);
  })();
}

export { renderGraph, graphShow, graphOverview, renderGlobal, graphDetails };
