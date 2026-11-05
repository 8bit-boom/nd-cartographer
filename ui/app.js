// nd-cartographer M0 viewport: pan / zoom / paint on server-baked tiles.
// No dependencies (the vendored-deps rule: nothing fetched from a CDN).
// Geometry math lives server-side; this file only transforms screen<->world
// and issues ops.
'use strict';

const PPC0 = 16;          // pixels per cell at zoom 0 (matches tiles.rs)
const TILE = 256;
const MAX_Z = 4;

const PALETTE = [
  { color: '#7f5539', name: 'Wood floor' },
  { color: '#8a9a5b', name: 'Grass' },
  { color: '#5a7d9a', name: 'Water' },
  { color: '#b5651d', name: 'Road' },
  { color: '#4a4a4a', name: 'Dark stone' },
  { color: '#262626', name: 'Void' },
];

const state = {
  docId: null,
  doc: null,        // last GET /api/docs/{id}
  floorId: null,
  rev: 0,
  z: 0,
  px: 0, py: 0,     // world->screen offset (screen = world*scale + offset)
  tool: 'pan',
  color: PALETTE[0].color,
  painting: null,   // in-progress stroke: Set of "x,y"
  pendingFlush: null,
};

const canvas = document.getElementById('canvas');
const ctx = canvas.getContext('2d');
const statusEl = document.getElementById('status');
const docName = document.getElementById('doc-name');
const docMeta = document.getElementById('doc-meta');

// ---------- API ----------

async function api(method, url, body) {
  const res = await fetch(url, {
    method,
    headers: body ? { 'Content-Type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!res.ok) {
    let msg = res.statusText;
    try { msg = (await res.json()).error || msg; } catch { /* keep */ }
    throw new Error(msg);
  }
  return res.json();
}

async function ensureDoc() {
  const { docs } = await api('GET', '/api/docs');
  if (docs.length > 0) return docs[0].id;
  const manifest = await api('POST', '/api/docs', { name: 'Untitled Battle Map', kind: 'battle' });
  return manifest.id;
}

async function refresh() {
  state.doc = await api('GET', `/api/docs/${state.docId}`);
  state.rev = state.doc.rev;
  const floor = state.doc.floors[0];
  state.floorId = floor.id;
  docName.textContent = state.doc.manifest.name;
  docMeta.textContent =
    `battle · ${floor.digest} · ${floor.walls.length} walls · rev ${state.rev}`;
  render();
  setExportLinks();
  buildOutline();
  refreshProblems();
}

// ---------- screen <-> world ----------

function ppc() { return PPC0 * (2 ** state.z); }        // pixels per cell at z
function screenToWorld(sx, sy) { return [sx - state.px, sy - state.py]; }
function worldToScreen(wx, wy) { return [wx + state.px, wy + state.py]; }
function worldToCell(wx, wy) {
  return [Math.floor(wx / ppc()), Math.floor(wy / ppc())];
}

// ---------- rendering ----------

function resizeCanvas() {
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.floor(canvas.clientWidth * dpr);
  canvas.height = Math.floor(canvas.clientHeight * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  render();
}

function render() {
  if (!state.doc) return;
  const w = canvas.clientWidth, h = canvas.clientHeight;
  ctx.clearRect(0, 0, w, h);
  ctx.fillStyle = '#14161a';
  ctx.fillRect(0, 0, w, h);

  const scale = 2 ** state.z;
  const [wx0, wy0] = screenToWorld(0, 0);
  const [wx1, wy1] = screenToWorld(w, h);
  const tx0 = Math.floor(wx0 / TILE), tx1 = Math.floor(wx1 / TILE);
  const ty0 = Math.floor(wy0 / TILE), ty1 = Math.floor(wy1 / TILE);
  const cacheBust = `rev=${state.rev}`;
  const pending = [];

  for (let ty = ty0; ty <= ty1; ty++) {
    for (let tx = tx0; tx <= tx1; tx++) {
      const url =
        `/tiles/${state.docId}/${state.floorId}/${state.z}/${tx}_${ty}?${cacheBust}`;
      const [sx, sy] = worldToScreen(tx * TILE, ty * TILE);
      pending.push(drawTile(url, sx, sy));
    }
  }
  drawStrokePreview();
  Promise.allSettled(pending).then(drawStrokePreview);
}

const tileCache = new Map(); // url -> Image
function drawTile(url, sx, sy) {
  return new Promise((resolve) => {
    let img = tileCache.get(url);
    if (!img) {
      img = new Image();
      img.onload = () => { ctx.drawImage(img, sx, sy); resolve(); };
      img.onerror = () => resolve();
      img.src = url;
      if (tileCache.size > 600) tileCache.clear();
      tileCache.set(url, img);
    } else if (img.complete && img.naturalWidth > 0) {
      ctx.drawImage(img, sx, sy);
      resolve();
    } else {
      img.onload = () => { ctx.drawImage(img, sx, sy); resolve(); };
      resolve();
    }
  });
}

function drawStrokePreview() {
  if (!state.painting) return;
  const s = ppc();
  ctx.save();
  ctx.fillStyle = state.color;
  ctx.globalAlpha = 0.55;
  for (const key of state.painting) {
    const [x, y] = key.split(',').map(Number);
    const [sx, sy] = worldToScreen(x * s, y * s);
    ctx.fillRect(sx, sy, s, s);
  }
  ctx.restore();
}

// ---------- tools ----------

const EDGE_TOOLS = { door: 'door', window: 'window' };

function setTool(tool) {
  state.tool = tool;
  for (const t of ['pan', 'paint', 'door', 'window']) {
    document.getElementById(`tool-${t}`).classList.toggle('active', tool === t);
  }
  canvas.classList.toggle('painting', tool !== 'pan');
  statusEl.textContent =
    tool === 'paint' ? `Painting ${PALETTE.find(p => p.color === state.color)?.name || state.color}`
    : tool in EDGE_TOOLS ? `Click a cell edge to place a ${tool}; click it again to remove`
    : 'Pan (drag) · wheel zooms';
}

// The nearest cell edge to a world point, or null if the pointer is mid-cell.
function pickEdge(wx, wy) {
  const s = ppc();
  const cx = Math.floor(wx / s), cy = Math.floor(wy / s);
  const fx = (wx - cx * s) / s, fy = (wy - cy * s) / s;
  const d = [['west', fx], ['east', 1 - fx], ['north', fy], ['south', 1 - fy]]
    .sort((a, b) => a[1] - b[1])[0];
  return d[1] < 0.35 ? { cell: { x: cx, y: cy }, edge: d[0] } : null;
}

// The same edge seen from the neighbouring cell: `east` of (x,y) is `west` of (x+1,y).
const NEIGHBOUR = { west: [-1, 0, 'east'], east: [1, 0, 'west'], north: [0, -1, 'south'], south: [0, 1, 'north'] };

function markAt(edge) {
  const [dx, dy, back] = NEIGHBOUR[edge.edge];
  const marks = state.doc.floors[0].grid.marks || [];
  return marks.find(m => {
    const c = m.edge.cell;
    return (c.x === edge.cell.x && c.y === edge.cell.y && m.edge.edge === edge.edge) ||
           (c.x === edge.cell.x + dx && c.y === edge.cell.y + dy && m.edge.edge === back);
  });
}

async function toggleMark(edge, kind) {
  const existing = markAt(edge);
  const target = existing ? existing.edge : edge;
  const mark = existing && existing.mark.kind === kind ? null : { kind, state: 'closed' };
  await postOps([{ t: 'set_mark', edge: target, mark }]);
}

async function postOps(ops) {
  try {
    const res = await api('POST', `/api/docs/${state.docId}/ops`, { floor: state.floorId, ops });
    state.rev = res.rev;
    tileCache.clear();
    await refresh();
  } catch (e) {
    statusEl.textContent = `Error: ${e.message}`;
  }
}

async function flushStroke() {
  if (!state.painting || state.painting.size === 0) return;
  const cells = [...state.painting].map((k) => {
    const [x, y] = k.split(',').map(Number);
    return { x, y };
  });
  state.painting = null;
  const spaceId = 'sp_' + state.color.slice(1).toLowerCase();
  const ops = [{
    t: 'batch',
    ops: [
      { t: 'upsert_space', space: { id: spaceId, name: PALETTE.find(p => p.color === state.color)?.name || spaceId, color: state.color, kind: 'room' } },
      { t: 'paint_cells', cells, space: spaceId },
    ],
  }];
  try {
    const res = await api('POST', `/api/docs/${state.docId}/ops`, { floor: state.floorId, ops });
    state.rev = res.rev;
    tileCache.clear();
    await refresh();
  } catch (e) {
    statusEl.textContent = `Error: ${e.message}`;
  }
}

function queuePaint(cellX, cellY) {
  if (!state.painting) state.painting = new Set();
  state.painting.add(`${cellX},${cellY}`);
  drawStrokePreview();
  clearTimeout(state.pendingFlush);
  state.pendingFlush = setTimeout(flushStroke, 120);
}

// ---------- input ----------

let dragging = null; // {sx, sy, px, py, moved}

canvas.addEventListener('pointerdown', (e) => {
  canvas.setPointerCapture(e.pointerId);
  const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
  const wantPaint = state.tool === 'paint' && e.button === 0;
  if (state.tool in EDGE_TOOLS && e.button === 0) {
    const edge = pickEdge(wx, wy);
    if (edge) toggleMark(edge, EDGE_TOOLS[state.tool]);
    else statusEl.textContent = 'Click closer to a cell edge';
    dragging = null;
    return;
  }
  if (wantPaint) {
    const [cx, cy] = worldToCell(wx, wy);
    state.painting = new Set();
    queuePaint(cx, cy);
    dragging = { painting: true };
  } else {
    dragging = { sx: e.offsetX, sy: e.offsetY, px: state.px, py: state.py, moved: false };
    canvas.classList.add('panning');
  }
});

canvas.addEventListener('pointermove', (e) => {
  if (!dragging) return;
  if (dragging.painting) {
    const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
    const [cx, cy] = worldToCell(wx, wy);
    queuePaint(cx, cy);
  } else {
    const dx = e.offsetX - dragging.sx;
    const dy = e.offsetY - dragging.sy;
    if (Math.abs(dx) + Math.abs(dy) > 2) dragging.moved = true;
    state.px = dragging.px + dx;
    state.py = dragging.py + dy;
    render();
  }
});

canvas.addEventListener('pointerup', (e) => {
  if (dragging && dragging.painting) {
    clearTimeout(state.pendingFlush);
    flushStroke();
  }
  dragging = null;
  canvas.classList.remove('panning');
});

canvas.addEventListener('wheel', (e) => {
  e.preventDefault();
  const dir = e.deltaY < 0 ? 1 : -1;
  const nz = Math.min(MAX_Z, Math.max(0, state.z + dir));
  if (nz === state.z) return;
  // Zoom at the cursor: keep the world point under it fixed.
  // (wx, wy) are in the CURRENT zoom's pixels, so rescale by the zoom *change*.
  const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
  const k = 2 ** (nz - state.z);
  state.z = nz;
  state.px = e.offsetX - wx * k;
  state.py = e.offsetY - wy * k;
  render();
}, { passive: false });

// ONE table is both the key handler and the F1 help (they cannot drift).
const KEYS = [
  { key: 'b', label: 'Paint tool', run: () => setTool('paint') },
  { key: 'd', label: 'Door tool', run: () => setTool('door') },
  { key: 'w', label: 'Window tool', run: () => setTool('window') },
  { key: 'v', label: 'Pan tool', run: () => setTool('pan') },
  { key: 'p', label: 'Problems panel', run: () => toggleProblems() },
  { key: 'o', label: 'Outline (focus the tree)', run: () => toggleOutline() },
  { key: 'z', ctrl: true, label: 'Undo', run: () => undo() },
  { key: 'F1', label: 'This help', run: () => toggleHelp() },
  { key: 'Escape', label: 'Pan tool / close panels', run: () => { toggleHelp(false); setTool('pan'); } },
];

function keyLabel(k) { return (k.ctrl ? 'Ctrl+' : '') + (k.key.length === 1 ? k.key.toUpperCase() : k.key); }

window.addEventListener('keydown', (e) => {
  // Inside the outline tree (or any text field) letters belong to it, not to the tools.
  if (e.target.closest && e.target.closest('#outliner, input, textarea') && !['F1', 'Escape'].includes(e.key)) return;
  const k = KEYS.find(x => x.key.toLowerCase() === e.key.toLowerCase() && !!x.ctrl === (e.ctrlKey || e.metaKey));
  if (!k) return;
  e.preventDefault();
  k.run();
});

function toggleHelp(force) {
  const el = document.getElementById('help');
  const show = force === undefined ? el.hidden : force;
  if (show) {
    document.getElementById('help-table').innerHTML =
      KEYS.map(k => `<tr><td><kbd>${keyLabel(k)}</kbd></td><td>${k.label}</td></tr>`).join('');
  }
  el.hidden = !show;
}

// ---------- problems panel (the linter) ----------

function toggleProblems(force) {
  const el = document.getElementById('problems');
  el.hidden = force === undefined ? !el.hidden : !force;
}

function centerOn(cell) {
  const s = ppc();
  state.px = canvas.clientWidth / 2 - (cell.x + 0.5) * s;
  state.py = canvas.clientHeight / 2 - (cell.y + 0.5) * s;
  render();
}

async function refreshProblems() {
  let lint;
  try {
    lint = await api('GET', `/api/docs/${state.docId}/lint?floor=${state.floorId}`);
  } catch { return; }
  const btn = document.getElementById('btn-problems');
  const n = lint.problems.length;
  btn.textContent = n ? `⚠ Problems (${n})` : '✓ No problems';
  btn.classList.toggle('has-problems', n > 0);
  btn.classList.toggle('has-errors', lint.blocks_publish);
  const list = document.getElementById('problem-list');
  list.innerHTML = '';
  if (!n) {
    list.innerHTML = '<li class="none">Nothing to fix.</li>';
    return;
  }
  for (const p of lint.problems) {
    const li = document.createElement('li');
    const msg = document.createElement('div');
    const dot = document.createElement('span');
    dot.className = `sev ${p.severity}`;
    msg.append(dot, document.createTextNode(p.message));   // textContent: never HTML
    const actions = document.createElement('div');
    actions.className = 'row-actions';
    if (p.at) {
      const go = document.createElement('button');
      go.textContent = 'Go to';
      go.onclick = () => centerOn(p.at);
      actions.appendChild(go);
    }
    if (p.fix) {
      const fix = document.createElement('button');
      fix.textContent = 'Fix';
      fix.onclick = () => postOps([p.fix]);
      actions.appendChild(fix);
    }
    li.append(msg, actions);
    list.appendChild(li);
  }
}

// ---------- outliner ----------
// ARIA tree: roving tabindex, arrow keys, Enter = go to, F2 = rename a room,
// Delete = remove a light or note. Data comes from the server's `outline`.

const treeEl = document.getElementById('tree');
const treeOpen = { rooms: true, lights: true, labels: true };
let treeFocus = null;   // data-key of the focused row, kept across rebuilds

function toggleOutline(force) {
  const el = document.getElementById('outliner');
  const show = force === undefined ? el.hidden : force;
  el.hidden = !show;
  resizeCanvas();
  if (show) focusRow(visibleRows()[0]);
}

function visibleRows() { return [...treeEl.querySelectorAll('li[role=treeitem]')]; }

function focusRow(row) {
  if (!row) return;
  for (const r of visibleRows()) r.tabIndex = -1;
  row.tabIndex = 0;
  row.focus();
  treeFocus = row.dataset.key;
}

function addRow(parent, key, level, html, label, handlers = {}) {
  const li = document.createElement('li');
  li.setAttribute('role', 'treeitem');
  li.setAttribute('aria-level', String(level));
  li.setAttribute('aria-label', label);
  li.dataset.key = key;
  li.tabIndex = -1;
  li.innerHTML = html;                       // html is built only from escaped text below
  Object.assign(li, handlers);
  parent.appendChild(li);
  return li;
}

const esc = (t) => String(t).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function buildOutline() {
  const o = state.doc.floors[0].outline;
  // Rebuilding replaces the rows; if the keyboard was in the tree, put it back.
  const hadFocus = treeEl.contains(document.activeElement);
  treeEl.textContent = '';
  const groups = [
    ['rooms', 'Rooms', o.spaces.length],
    ['lights', 'Lights', o.lights.length],
    ['labels', 'Notes', o.labels.length],
  ];
  for (const [g, title, n] of groups) {
    const head = addRow(treeEl, `g:${g}`, 1, `${esc(title)} <span class="meta">${n}</span>`, `${title}, ${n}`);
    head.setAttribute('aria-expanded', String(treeOpen[g]));
    head.onclick = () => { treeOpen[g] = !treeOpen[g]; buildOutline(); focusKey(`g:${g}`); };
    if (!treeOpen[g]) continue;
    if (g === 'rooms') for (const s of o.spaces) {
      const where = s.bbox ? `${s.cells} squares` : 'no squares yet';
      const r = addRow(treeEl, `s:${s.id}`, 2,
        `<span class="chip" style="background:${esc(s.color)}"></span>${esc(s.name)}<span class="meta">${where} · ${esc(s.kind)}</span>`,
        `Room ${s.name}, ${where}, ${s.kind}`);
      r.dataset.kind = 'space';
      r.onclick = () => { focusRow(r); goToRoom(s); };
      r.onrenamerequest = () => renameRoom(s);
    }
    if (g === 'lights') for (const l of o.lights) {
      const r = addRow(treeEl, `l:${l.id}`, 2, `${esc(l.label)}<span class="meta">${l.on ? 'on' : 'off'}</span>`,
        `Light ${l.label}, ${l.on ? 'on' : 'off'}`);
      r.dataset.kind = 'light';
      r.onclick = () => { focusRow(r); centerOn({ x: l.cell[0], y: l.cell[1] }); };
      r.ondeleterequest = () => postOps([{ t: 'remove_light', id: l.id }]);
      r.cell = l.cell;
    }
    if (g === 'labels') for (const l of o.labels) {
      const r = addRow(treeEl, `n:${l.id}`, 2, `${esc(l.text)}<span class="meta">${l.gm_only ? 'GM only' : ''}</span>`,
        `Note ${l.text}${l.gm_only ? ', GM only' : ''}`);
      r.dataset.kind = 'label';
      r.onclick = () => { focusRow(r); centerOn({ x: l.cell[0], y: l.cell[1] }); };
      r.ondeleterequest = () => postOps([{ t: 'remove_label', id: l.id }]);
    }
  }
  const rows = visibleRows();
  const keep = rows.find(r => r.dataset.key === treeFocus) || rows[0];
  if (keep) {
    keep.tabIndex = 0;
    if (hadFocus) keep.focus();
  }
}

function focusKey(key) { focusRow(visibleRows().find(r => r.dataset.key === key)); }

function goToRoom(s) {
  if (!s.bbox) return;
  centerOn({ x: (s.bbox[0] + s.bbox[2]) / 2 - 0.5, y: (s.bbox[1] + s.bbox[3]) / 2 - 0.5 });
}

function renameRoom(s) {
  const name = window.prompt('Rename room', s.name);
  if (name === null || !name.trim() || name.trim() === s.name) return;
  postOps([{ t: 'upsert_space', space: { id: s.id, name: name.trim().slice(0, 60), color: s.color, kind: s.kind } }]);
}

treeEl.addEventListener('keydown', (e) => {
  const rows = visibleRows();
  const i = rows.indexOf(document.activeElement);
  if (i < 0) return;
  const row = rows[i];
  let used = true;
  switch (e.key) {
    case 'ArrowDown': focusRow(rows[Math.min(rows.length - 1, i + 1)]); break;
    case 'ArrowUp': focusRow(rows[Math.max(0, i - 1)]); break;
    case 'Home': focusRow(rows[0]); break;
    case 'End': focusRow(rows[rows.length - 1]); break;
    case 'ArrowRight': if (row.getAttribute('aria-expanded') === 'false') row.click(); else focusRow(rows[Math.min(rows.length - 1, i + 1)]); break;
    case 'ArrowLeft':
      if (row.getAttribute('aria-expanded') === 'true') row.click();
      else { const head = rows.slice(0, i).reverse().find(r => r.getAttribute('aria-level') === '1'); focusRow(head); }
      break;
    case 'Enter': case ' ': row.click(); break;
    case 'F2': if (row.onrenamerequest) row.onrenamerequest(); break;
    case 'Delete': case 'Backspace': if (row.ondeleterequest) row.ondeleterequest(); break;
    default: used = false;
  }
  if (used) e.preventDefault();
});

function setExportLinks() {
  const base = `/api/docs/${state.docId}/export`;
  const player = document.getElementById('exp-player').checked;
  const q = `floor=${state.floorId}` + (player ? '&audience=player' : '');
  const suffix = player ? ' (players)' : '';
  document.getElementById('exp-png').href = `${base}/png?${q}&format=png`;
  document.getElementById('exp-webp').href = `${base}/png?${q}&format=webp`;
  document.getElementById('exp-avif').href = `${base}/png?${q}&format=avif`;
  document.getElementById('exp-uvtt').href = `${base}/uvtt?${q}`;
  document.getElementById('exp-png').download = `${state.doc.manifest.name}${suffix}.png`;
  document.getElementById('exp-webp').download = `${state.doc.manifest.name}${suffix}.webp`;
  document.getElementById('exp-avif').download = `${state.doc.manifest.name}${suffix}.avif`;
  document.getElementById('exp-uvtt').download = `${state.doc.manifest.name}${suffix}.dd2vtt`;
}

async function undo() {
  try {
    const res = await api('POST', `/api/docs/${state.docId}/undo`, { floor: state.floorId });
    state.rev = res.rev;
    tileCache.clear();
    await refresh();
  } catch (err) {
    statusEl.textContent = err.message;
  }
}

// ---------- toolbar wiring ----------

document.getElementById('tool-pan').onclick = () => setTool('pan');
document.getElementById('tool-paint').onclick = () => setTool('paint');
document.getElementById('tool-door').onclick = () => setTool('door');
document.getElementById('tool-window').onclick = () => setTool('window');
document.getElementById('btn-problems').onclick = () => toggleProblems();
document.getElementById('btn-outline').onclick = () => toggleOutline();
document.getElementById('problems-close').onclick = () => toggleProblems(false);
document.getElementById('btn-help').onclick = () => toggleHelp();
document.getElementById('exp-player').onchange = setExportLinks;
document.getElementById('btn-undo').onclick = undo;

function buildPalette() {
  const wrap = document.getElementById('palette');
  wrap.textContent = '';   // rebuilt on every pick: without this the swatches multiplied
  for (const item of PALETTE) {
    const sw = document.createElement('span');
    sw.className = 'swatch' + (item.color === state.color ? ' active' : '');
    sw.style.background = item.color;
    sw.title = item.name;
    sw.onclick = () => {
      state.color = item.color;
      buildPalette();
      setTool('paint');
    };
    wrap.appendChild(sw);
  }
}

window.addEventListener('resize', resizeCanvas);

// ---------- boot ----------

(async function boot() {
  buildPalette();
  resizeCanvas();
  try {
    state.docId = await ensureDoc();
    await refresh();
    setTool('paint');
  } catch (e) {
    statusEl.textContent = `Cannot reach the server: ${e.message}`;
  }
})();
