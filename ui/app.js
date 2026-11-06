// nd-cartographer M0 viewport: pan / zoom / paint on server-baked tiles.
// No dependencies (the vendored-deps rule: nothing fetched from a CDN).
// Geometry math lives server-side; this file only transforms screen<->world
// and issues ops.
'use strict';

const PPC0 = 16;          // pixels per cell at zoom 0 (matches tiles.rs)
const TILE = 256;
const MAX_Z = 4;

// The palette is the server's material pack (GET /api/materials). If that cannot
// be fetched the editor still works with flat colours (no `material`).
const FLAT_FALLBACK = [
  { id: null, color: '#7f5539', name: 'Wood floor' },
  { id: null, color: '#8a9a5b', name: 'Grass' },
  { id: null, color: '#5a7d9a', name: 'Water' },
  { id: null, color: '#4a4a4a', name: 'Dark stone' },
];
let PALETTE = FLAT_FALLBACK;
const activeItem = () => PALETTE.find(p => p.color === state.color && (p.id || null) === (state.materialId || null)) || PALETTE[0];

const state = {
  docId: null,
  doc: null,        // last GET /api/docs/{id}
  floorId: null,
  rev: 0,
  z: 0,
  px: 0, py: 0,     // world->screen offset (screen = world*scale + offset)
  tool: 'pan',
  color: FLAT_FALLBACK[0].color,   // set from the palette at boot
  materialId: null,
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

// ---------- floors ----------
// Floors share one document; each has its own tiles, undo, lint and exports.

state.onion = false;

// The floor shown faintly underneath: the one before the current floor in the list.
function onionFloorId() {
  const i = state.doc.floors.findIndex(f => f.id === state.floorId);
  return i > 0 ? state.doc.floors[i - 1].id : null;
}
function onionParam() {
  const o = state.onion ? onionFloorId() : null;
  return o ? `&onion=${encodeURIComponent(o)}` : '';
}

function buildFloorBar() {
  const tabs = document.getElementById('floor-tabs');
  tabs.textContent = '';
  for (const f of state.doc.floors) {
    const b = document.createElement('button');
    b.setAttribute('role', 'tab');
    b.setAttribute('aria-selected', String(f.id === state.floorId));
    b.dataset.floor = f.id;
    b.textContent = f.name;           // textContent: a floor name is never HTML
    b.onclick = () => selectFloor(f.id);
    tabs.appendChild(b);
  }
  const i = state.doc.floors.findIndex(f => f.id === state.floorId);
  document.getElementById('fl-left').disabled = i <= 0;
  document.getElementById('fl-right').disabled = i >= state.doc.floors.length - 1;
  document.getElementById('fl-remove').disabled = state.doc.floors.length <= 1;
  document.getElementById('fl-onion').disabled = i <= 0;
}

async function selectFloor(id) {
  if (id === state.floorId) return;
  state.floorId = id;
  tileCache.clear();
  await refresh();
}

async function floorAction(body) {
  try {
    const out = await api('POST', `/api/docs/${state.docId}/floors`, body);
    if (out.floor) state.floorId = out.floor;
    tileCache.clear();
    await refresh();
  } catch (e) {
    statusEl.textContent = `Floor change refused: ${e.message}`;
  }
}

document.getElementById('fl-add').onclick = () => {
  const name = window.prompt('Name the new floor', `Floor ${state.doc.floors.length + 1}`);
  if (name && name.trim()) floorAction({ action: 'add', name: name.trim() });
};
document.getElementById('fl-dup').onclick = () =>
  floorAction({ action: 'add', name: `${curFloor().name} copy`, copy_from: state.floorId });
document.getElementById('fl-rename').onclick = () => {
  const name = window.prompt('Rename the floor', curFloor().name);
  if (name && name.trim() && name.trim() !== curFloor().name) floorAction({ action: 'rename', id: state.floorId, name: name.trim() });
};
const moveFloor = (d) => {
  const i = state.doc.floors.findIndex(f => f.id === state.floorId);
  floorAction({ action: 'move', id: state.floorId, index: i + d });
};
document.getElementById('fl-left').onclick = () => moveFloor(-1);
document.getElementById('fl-right').onclick = () => moveFloor(1);
document.getElementById('fl-remove').onclick = async () => {
  if (!window.confirm(`Remove "${curFloor().name}"? (The Floors undo button brings it back.)`)) return;
  const removing = state.floorId;
  const i = state.doc.floors.findIndex(f => f.id === removing);
  // Step to the neighbouring floor FIRST, so nothing asks for the floor being deleted.
  state.floorId = state.doc.floors[i > 0 ? i - 1 : 1].id;
  try {
    await api('POST', `/api/docs/${state.docId}/floors`, { action: 'remove', id: removing });
  } catch (e) {
    state.floorId = removing;               // refused: stay where we were
    statusEl.textContent = `Floor change refused: ${e.message}`;
  }
  tileCache.clear();
  await refresh();
};
document.getElementById('fl-undo').onclick = async () => {
  try {
    await api('POST', `/api/docs/${state.docId}/undo-structure`, {});
    tileCache.clear();
    await refresh();
  } catch (e) {
    statusEl.textContent = `Nothing to undo: ${e.message}`;
  }
};
document.getElementById('fl-onion').onchange = (e) => { state.onion = e.target.checked; tileCache.clear(); render(); };

// ---------- projects ----------

async function loadDocList() {
  const { docs } = await api('GET', '/api/docs');
  const sel = document.getElementById('doc-select');
  sel.textContent = '';
  for (const d of docs) {
    const o = document.createElement('option');
    o.value = d.id;
    o.textContent = d.name;       // textContent: a project name is never HTML
    o.selected = d.id === state.docId;
    sel.appendChild(o);
  }
}

async function openDoc(id) {
  state.docId = id;
  state.px = 0; state.py = 0; state.z = 0;
  tileCache.clear();
  await refresh();
  await loadDocList();
}

document.getElementById('doc-select').onchange = (e) => openDoc(e.target.value);

const newDialog = document.getElementById('new-dialog');
const ndKind = () => document.querySelector('input[name=nd-kind]:checked').value;

document.getElementById('btn-new').onclick = () => {
  document.getElementById('nd-seed').value = String(Math.floor(Math.random() * 1e6));
  newDialog.showModal();
};
for (const r of document.querySelectorAll('input[name=nd-kind]')) {
  r.onchange = () => { document.getElementById('nd-world').hidden = ndKind() !== 'region'; document.getElementById('nd-battle').hidden = ndKind() !== 'battle'; };
}
document.getElementById('nd-dice').onclick = () => { document.getElementById('nd-seed').value = String(Math.floor(Math.random() * 4294967295)); };
document.getElementById('nd-cancel').onclick = () => newDialog.close();
document.getElementById('new-form').onsubmit = async (e) => {
  e.preventDefault();
  const name = document.getElementById('nd-name').value.trim();
  if (!name) return;
  const kind = ndKind();
  newDialog.close();
  try {
    const m = await api('POST', '/api/docs', { name, kind });
    const start = kind === 'battle' ? document.getElementById('nd-start').value : '';
    if (start) {
      statusEl.textContent = `Generating a ${start}…`;
      await api('POST', `/api/docs/${m.id}/generate-map`, { kind: start, seed: Math.floor(Math.random() * 1e6), width: 64, height: 48 });
    }
    if (kind === 'region') {
      statusEl.textContent = 'Generating the world…';
      await api('POST', `/api/docs/${m.id}/generate`, worldParams());
    }
    await openDoc(m.id);
  } catch (err) {
    statusEl.textContent = `Could not create the project: ${err.message}`;
  }
};

function worldParams() {
  const n = (id) => parseFloat(document.getElementById(id).value);
  return {
    seed: n('nd-seed') >>> 0, cols: n('nd-cols'), rows: n('nd-rows'), land: n('nd-land'),
    continent: n('nd-cont'), ridges: n('nd-ridge'), rivers: n('nd-rivers'),
  };
}

document.getElementById('btn-open').onclick = () => document.getElementById('file-open').click();
document.getElementById('file-open').onchange = async (e) => {
  const file = e.target.files[0];
  e.target.value = '';
  if (!file) return;
  statusEl.textContent = `Opening ${file.name}…`;
  try {
    const res = await fetch(`/api/import/uvtt?name=${encodeURIComponent(file.name.replace(/\.(dd2vtt|uvtt|df2vtt)$/i, ''))}`, { method: 'POST', body: await file.arrayBuffer() });
    const out = await res.json();
    if (!res.ok) throw new Error(out.error || res.statusText);
    await openDoc(out.manifest.id);
    statusEl.textContent = `Opened ${file.name}: ${out.stats.walls} walls, ${out.stats.lights} lights`;
  } catch (err) {
    statusEl.textContent = `Could not open ${file.name}: ${err.message}`;
  }
};

const curFloor = () => state.doc.floors.find(f => f.id === state.floorId) || state.doc.floors[0];

async function refresh() {
  state.doc = await api('GET', `/api/docs/${state.docId}`);
  state.rev = state.doc.rev;
  // Keep the selected floor if it still exists; otherwise fall back to the first.
  if (!state.doc.floors.some(f => f.id === state.floorId)) state.floorId = state.doc.floors[0].id;
  const floor = curFloor();
  docName.textContent = state.doc.manifest.name;
  docMeta.textContent =
    `${state.doc.manifest.kind} · ${floor.digest} · ${floor.walls.length} walls · rev ${state.rev}`;
  render();
  applyKind();
  buildFloorBar();
  setExportLinks();
  buildOutline();
  refreshProblems();
  loadMiniImage();
  buildViews();
  syncMapGrid();
  loadPreview();
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
  if (!state.doc.floors.some(f => f.id === state.floorId)) return;   // floor just removed

  for (let ty = ty0; ty <= ty1; ty++) {
    for (let tx = tx0; tx <= tx1; tx++) {
      const url =
        `/tiles/${state.docId}/${state.floorId}/${state.z}/${tx}_${ty}?${cacheBust}${onionParam()}`;
      const [sx, sy] = worldToScreen(tx * TILE, ty * TILE);
      pending.push(drawTile(url, sx, sy));
    }
  }
  drawStrokePreview();
  drawLiveStroke();
  drawTerrainOverlay();
  drawOverlayDraft();
  Promise.allSettled(pending).then(() => { drawStrokePreview(); drawLiveStroke(); drawTerrainOverlay(); drawOverlayDraft(); drawPreview(); drawMinimap(); });  drawPreview();
  drawMinimap();
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
const TERRAIN_TOOLS = ['raise', 'lower', 'smooth', 'flatten', 'roughen'];

function setTool(tool) {
  state.tool = tool;
  for (const t of ['pan', 'paint', 'brush', 'erase', 'stamp', 'door', 'window', ...TERRAIN_TOOLS, 'river', 'marker', 'stairs', 'prefab', 'biome', 'region', 'route', 'label', 'pathlabel']) {
    document.getElementById(`tool-${t}`).classList.toggle('active', tool === t);
  }
  canvas.classList.toggle('painting', tool !== 'pan');
  document.getElementById('brushbar').hidden = !(tool === 'brush' || tool === 'erase');
  document.getElementById('stampbar').hidden = tool !== 'stamp';
  setTerrainBar(tool);
  document.getElementById('labelbar').hidden = !(tool === 'label' || tool === 'pathlabel');
  document.getElementById('biomebar').hidden = tool !== 'biome';
  document.getElementById('lb-hint').textContent = tool === 'pathlabel' ? 'click points along the path, Enter to finish' : tool === 'label' ? 'click to place' : '';
  statusEl.textContent =
    tool === 'paint' ? `Painting ${activeItem().name}`
    : tool === 'brush' ? `Brush: ${activeItem().name} — drag to paint (pressure and tilt count)`
    : tool === 'erase' ? 'Eraser: drag to reveal the room layer beneath'
    : tool === 'stamp' ? `Stamp: click to place a ${state.stamp?.name || 'prop'} (, . rotate · F flip)`
    : TERRAIN_TOOLS.includes(tool) ? `${tool[0].toUpperCase()}${tool.slice(1)}: drag over the map`
    : tool === 'river' ? 'River: click points from source to mouth · Enter finishes · Esc cancels'
    : tool === 'marker' ? 'Marker: click to place a point of interest'
    : tool === 'prefab' ? `Prefab ${state.prefab?.name || ''}: click to place its top-left corner (R turns it, now ${state.prefabTurns * 90}°)`
    : tool === 'biome' ? 'Biome: click the corners of the area · Enter fills it · Esc cancels'
    : tool === 'stairs' ? 'Stairs: click where they are, then say which floor they lead to'
    : tool === 'label' ? 'Label: click where the text goes'
    : tool === 'pathlabel' ? 'Text on a path: click points along the way · Enter finishes · Esc cancels'
    : tool === 'region' ? 'Region: click the corners · Enter finishes · Esc cancels'
    : tool === 'route' ? 'Route: click points along the way · Enter finishes · Esc cancels'
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
  const marks = curFloor().grid.marks || [];
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
  const item = activeItem();
  const spaceId = item.id || ('sp_' + item.color.slice(1).toLowerCase());
  const ops = [{
    t: 'batch',
    ops: [
      { t: 'upsert_space', space: { id: spaceId, name: item.name, color: item.color, kind: 'room', ...(item.id ? { material: item.id } : {}) } },
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

// Every cell on the segment between two cells (Bresenham), so a fast stroke
// paints a continuous line instead of one cell per pointer event.
function cellsOnLine(x0, y0, x1, y1) {
  const out = [];
  const dx = Math.abs(x1 - x0), dy = -Math.abs(y1 - y0);
  const sx = x0 < x1 ? 1 : -1, sy = y0 < y1 ? 1 : -1;
  let err = dx + dy, x = x0, y = y0;
  for (;;) {
    out.push([x, y]);
    if (x === x1 && y === y1) break;
    const e2 = 2 * err;
    if (e2 >= dy) { err += dy; x += sx; }
    if (e2 <= dx) { err += dx; y += sy; }
  }
  return out;
}

function paintTo(cx, cy) {
  const last = state.lastCell;
  const cells = last ? cellsOnLine(last[0], last[1], cx, cy) : [[cx, cy]];
  for (const [x, y] of cells) queuePaint(x, y);
  state.lastCell = [cx, cy];
}

// Symmetry: painting a cell also paints its mirror images about the axis
// lines (a line sits on a cell edge, so cell x mirrors to 2*ax - 1 - x).
state.sym = { v: false, h: false, ax: 20, ay: 15 };
function mirrored(x, y) {
  const { v, h, ax, ay } = state.sym;
  const xs = v ? [x, 2 * ax - 1 - x] : [x];
  const ys = h ? [y, 2 * ay - 1 - y] : [y];
  return xs.flatMap((mx) => ys.map((my) => [mx, my]));
}

function queuePaint(cellX, cellY) {
  if (!state.painting) state.painting = new Set();
  for (const [mx, my] of mirrored(cellX, cellY)) state.painting.add(`${mx},${my}`);
  drawStrokePreview();
  clearTimeout(state.pendingFlush);
  state.pendingFlush = setTimeout(flushStroke, 120);
}

for (const [id, key, num] of [['sym-v', 'v'], ['sym-h', 'h'], ['sym-ax', 'ax', true], ['sym-ay', 'ay', true]]) {
  document.getElementById(id).onchange = (e) => {
    state.sym[key] = num ? Math.round(Number(e.target.value) || 0) : e.target.checked;
    drawStrokePreview();
  };
}

// ---------- freehand brush ----------
// A stroke is sent as ONE op (points with pressure and tilt, size, opacity,
// seed). Feedback is drawn on the canvas at once; the server's baked tiles
// replace it when the op lands (plan §6: strokes composite client-side first,
// the authoritative bake trails behind).

const brushEl = (id) => document.getElementById(id);
const brushSettings = () => ({
  brush: brushEl('br-kind').value,
  size: parseFloat(brushEl('br-size').value),
  opacity: parseFloat(brushEl('br-opacity').value),
  hardness: parseFloat(brushEl('br-hardness').value),
  roughness: parseFloat(brushEl('br-rough').value),
  density: parseFloat(brushEl('br-density').value),
});

function nudgeSize(dir) {
  const el = brushEl('br-size');
  const v = Math.min(10, Math.max(0.2, parseFloat(el.value) * (dir > 0 ? 1.25 : 0.8)));
  el.value = v.toFixed(1);
  brushEl('br-size-o').textContent = el.value;
}

let liveStroke = null;   // {points:[{x,y,pressure,tilt}], erase}

function startStroke(wx, wy, e) {
  liveStroke = { points: [], erase: state.tool === 'erase' };
  addStrokePoint(wx, wy, e);
}

function addStrokePoint(wx, wy, e) {
  const s = ppc();
  const x = wx / s, y = wy / s;
  const last = liveStroke.points[liveStroke.points.length - 1];
  if (last && Math.hypot(x - last.x, y - last.y) < 0.04) return;   // thin the samples
  if (liveStroke.points.length >= 4000) return;                      // the server's cap
  // A mouse reports pressure 0.5 with no buttons quirks; treat "mouse" as full pressure.
  const pen = e.pointerType === 'pen';
  const tilt = pen ? Math.min(1, Math.hypot(e.tiltX || 0, e.tiltY || 0) / 90) : 0;
  liveStroke.points.push({ x, y, pressure: pen ? Math.min(1, Math.max(0, e.pressure)) : 1, tilt });
  render();
}

function drawLiveStroke() {
  if (!liveStroke || liveStroke.points.length === 0) return;
  const b = brushSettings();
  const s = ppc();
  ctx.save();
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';
  ctx.strokeStyle = liveStroke.erase ? '#d8e2ec' : state.color;
  ctx.fillStyle = ctx.strokeStyle;
  ctx.globalAlpha = (liveStroke.erase ? 0.35 : 0.8) * b.opacity;
  const pts = liveStroke.points;
  const w = (p) => b.size * (0.35 + 0.65 * p.pressure) * (1 + 0.5 * p.tilt) * s;
  for (let i = 0; i < pts.length; i++) {
    const [sx, sy] = worldToScreen(pts[i].x * s, pts[i].y * s);
    if (b.brush === 'scatter' && !liveStroke.erase) {
      ctx.beginPath();
      ctx.arc(sx, sy, Math.max(1.5, w(pts[i]) * 0.1), 0, Math.PI * 2);
      ctx.fill();
      continue;
    }
    if (i > 0) {
      const [px, py] = worldToScreen(pts[i - 1].x * s, pts[i - 1].y * s);
      ctx.lineWidth = Math.max(1, w(pts[i]));
      ctx.beginPath(); ctx.moveTo(px, py); ctx.lineTo(sx, sy); ctx.stroke();
    } else {
      ctx.beginPath(); ctx.arc(sx, sy, Math.max(0.5, w(pts[i]) / 2), 0, Math.PI * 2); ctx.fill();
    }
  }
  ctx.restore();
}

async function finishStroke() {
  const st = liveStroke;
  liveStroke = null;
  if (!st || st.points.length === 0) return;
  const b = brushSettings();
  const item = activeItem();
  const stroke = {
    id: 'st_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 7),
    kind: st.erase ? 'erase' : 'paint',
    brush: st.erase ? 'round' : b.brush,
    color: item.color,
    size: b.size,
    opacity: b.opacity,
    hardness: b.hardness,
    roughness: b.roughness,
    density: b.density,
    seed: Math.floor(Math.random() * 4294967295),
    points: st.points.map(p => ({ x: +p.x.toFixed(3), y: +p.y.toFixed(3), pressure: +p.pressure.toFixed(3), tilt: +p.tilt.toFixed(3) })),
  };
  if (item.id && !st.erase) stroke.material = item.id;
  await postOps([{ t: 'add_stroke', stroke }]);
}

for (const id of ['br-size']) {
  brushEl(id).oninput = () => { brushEl('br-size-o').textContent = brushEl(id).value; };
}

// ---------- stamps (props) ----------

state.stamp = null;      // {id, name, size}
let STAMPS = [];

function rotateStamp(deg) {
  const el = document.getElementById('st-rot');
  el.value = String((((parseInt(el.value, 10) + deg) % 360) + 360) % 360);
  document.getElementById('st-rot-o').textContent = `${el.value}°`;
}

async function loadStamps() {
  try {
    STAMPS = (await api('GET', '/api/stamps')).stamps;
  } catch { STAMPS = []; }
  const wrap = document.getElementById('stamp-palette');
  wrap.textContent = '';
  for (const s of STAMPS) {
    const b = document.createElement('button');
    b.title = s.tags?.length ? `${s.name} (${s.tags.join(', ')})` : s.name;
    b.dataset.find = `${s.name} ${(s.tags || []).join(' ')}`.toLowerCase();
    b.className = state.stamp?.id === s.id ? 'active' : '';
    const img = document.createElement('img');
    img.src = `/api/stamps/${s.id}/icon.png`;
    img.alt = s.name;
    b.appendChild(img);
    b.onclick = () => { state.stamp = s; loadStampsSelection(); setTool('stamp'); };
    wrap.appendChild(b);
  }
  if (!state.stamp && STAMPS.length) state.stamp = STAMPS[0];
  applyStampFilter();
}

function applyStampFilter() {
  const words = document.getElementById('stamp-filter').value.toLowerCase().split(/\s+/).filter(Boolean);
  for (const b of document.getElementById('stamp-palette').children) {
    b.hidden = !words.every(w => (b.dataset.find || '').includes(w));
  }
}
document.getElementById('stamp-filter').addEventListener('input', applyStampFilter);

function loadStampsSelection() {
  [...document.getElementById('stamp-palette').children].forEach((b, i) =>
    b.classList.toggle('active', STAMPS[i]?.id === state.stamp?.id));
}

async function placeStamp(wx, wy) {
  if (!state.stamp) return;
  const s = ppc();
  let x = wx / s, y = wy / s;
  if (document.getElementById('st-snap').checked) { x = Math.round(x * 2) / 2; y = Math.round(y * 2) / 2; }
  const stamp = {
    id: 'pp_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6),
    asset: state.stamp.id,
    x, y,
    scale: parseFloat(document.getElementById('st-scale').value),
    rotation: (parseInt(document.getElementById('st-rot').value, 10) * Math.PI) / 180,
    flip: document.getElementById('st-flip').checked,
    shadow: document.getElementById('st-shadow').checked,
  };
  await postOps([{ t: 'add_stamp', stamp }]);
}

document.getElementById('st-rot').oninput = (e) => {
  document.getElementById('st-rot-o').textContent = `${e.target.value}°`;
};

// ---------- terrain (world maps) ----------

const isRegion = () => state.doc?.manifest.kind === 'region';
function regionOnly(tool) { if (isRegion()) setTool(tool); }

function applyKind() {
  const region = isRegion();
  document.getElementById('terrain-tools').hidden = !region;
  for (const el of document.querySelectorAll('.battle-only')) el.hidden = region;
  if (region && ['paint', 'door', 'window'].includes(state.tool)) setTool('pan');
  const t = curFloor().terrain;
  if (t) syncStyleControls(t.style);
}

function setTerrainBar(tool) {
  const on = TERRAIN_TOOLS.includes(tool) || tool === 'river';
  document.getElementById('terrainbar').hidden = !(on || (isRegion() && tool === 'pan'));
  document.getElementById('tr-hint').textContent =
    tool === 'river' ? 'click source → mouth, Enter to finish' : TERRAIN_TOOLS.includes(tool) ? 'drag to sculpt' : '';
}

const trVal = (id) => parseFloat(document.getElementById(id).value);
let terrainStroke = null;   // {points:[[x,y]], mode}
let riverDraft = null;      // [[x,y]]
state.hover = null;

function startTerrainStroke(wx, wy) {
  terrainStroke = { points: [], mode: state.tool };
  addTerrainPoint(wx, wy);
}

function addTerrainPoint(wx, wy) {
  const s = ppc();
  const p = [wx / s, wy / s];
  const last = terrainStroke.points[terrainStroke.points.length - 1];
  const spacing = Math.max(0.15, trVal('tr-radius') * 0.25);
  if (last && Math.hypot(p[0] - last[0], p[1] - last[1]) < spacing) return;
  if (terrainStroke.points.length >= 4000) return;
  terrainStroke.points.push([+p[0].toFixed(3), +p[1].toFixed(3)]);
  render();
}

async function finishTerrainStroke() {
  const st = terrainStroke;
  terrainStroke = null;
  if (!st || !st.points.length) return;
  await postOps([{ t: 'terrain_stroke', stroke: {
    mode: st.mode, points: st.points, radius: trVal('tr-radius'),
    strength: trVal('tr-strength'), seed: Math.floor(Math.random() * 4294967295),
  } }]);
}

function addRiverPoint(wx, wy) {
  const s = ppc();
  (riverDraft ||= []).push([+(wx / s).toFixed(3), +(wy / s).toFixed(3)]);
  render();
}

// Catmull-Rom curve through the clicked points (the same maths as the server's
// route smoothing), so a river meanders instead of zig-zagging.
function smoothPoints(pts, steps) {
  if (pts.length < 3) return pts;
  const at = (i) => pts[Math.max(0, Math.min(pts.length - 1, i))];
  const out = [];
  for (let i = 0; i < pts.length - 1; i++) {
    const [p0, p1, p2, p3] = [at(i - 1), at(i), at(i + 1), at(i + 2)];
    for (let k = 0; k < steps; k++) {
      const t = k / steps, t2 = t * t, t3 = t2 * t;
      const f = (a, b, c, d) => 0.5 * (2 * b + (c - a) * t + (2 * a - 5 * b + 4 * c - d) * t2 + (3 * b - a - 3 * c + d) * t3);
      out.push([+f(p0[0], p1[0], p2[0], p3[0]).toFixed(3), +f(p0[1], p1[1], p2[1], p3[1]).toFixed(3)]);
    }
  }
  out.push(pts[pts.length - 1]);
  return out;
}

async function finishRiver() {
  const pts = riverDraft;
  if (state.tool !== 'river') return;
  if (!pts || pts.length < 2) { render(); return; }   // too few points: keep drawing
  riverDraft = null;
  const n = (curFloor().rivers || []).length + 1;
  const curved = smoothPoints(pts, 4);
  await postOps([{ t: 'add_river', river: {
    id: 'rv_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 5),
    name: `River ${n}`, points: curved, width: trVal('tr-rwidth'),
  } }]);
}

function drawTerrainOverlay() {
  const s = ppc();
  ctx.save();
  if (state.hover && (TERRAIN_TOOLS.includes(state.tool) || state.tool === 'river')) {
    const [sx, sy] = worldToScreen(state.hover[0] * s, state.hover[1] * s);
    const r = state.tool === 'river' ? trVal('tr-rwidth') * s * 0.5 : trVal('tr-radius') * s;
    ctx.strokeStyle = '#fff'; ctx.globalAlpha = 0.8; ctx.lineWidth = 1.5;
    ctx.beginPath(); ctx.arc(sx, sy, Math.max(2, r), 0, Math.PI * 2); ctx.stroke();
  }
  const trace = (pts, color, width) => {
    if (!pts || pts.length === 0) return;
    ctx.strokeStyle = color; ctx.lineWidth = width; ctx.lineJoin = 'round'; ctx.lineCap = 'round';
    ctx.beginPath();
    pts.forEach((p, i) => { const [x, y] = worldToScreen(p[0] * s, p[1] * s); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
    ctx.stroke();
  };
  if (terrainStroke) trace(terrainStroke.points, 'rgba(255,255,255,0.7)', 2);
  if (riverDraft) {
    trace(riverDraft, 'rgba(110,190,255,0.95)', Math.max(2, trVal('tr-rwidth') * s * 0.6));
    for (const p of riverDraft) { const [x, y] = worldToScreen(p[0] * s, p[1] * s); ctx.fillStyle = '#fff'; ctx.fillRect(x - 3, y - 3, 6, 6); }
  }
  ctx.restore();
}

// Style sliders -> one set_terrain_style (debounced, always the full style).
function syncStyleControls(st) {
  const set = (id, v) => { const el = document.getElementById(id); if (document.activeElement !== el) el.value = v; };
  set('ts-sea', st.sea_level); set('ts-hill', st.hillshade); set('ts-contour', st.contour_interval);
  set('ts-tint', st.tint); set('ts-tint-amt', st.tint_amount);
  set('ts-sat', st.saturation); set('ts-con', st.contrast); set('ts-bri', st.brightness);
}
let styleTimer = null;
function pushStyle() {
  clearTimeout(styleTimer);
  styleTimer = setTimeout(() => postOps([{ t: 'set_terrain_style', style: {
    sea_level: Math.round(trVal('ts-sea')), hillshade: trVal('ts-hill'),
    contour_interval: Math.round(trVal('ts-contour')),
    tint: document.getElementById('ts-tint').value, tint_amount: trVal('ts-tint-amt'),
    saturation: trVal('ts-sat'), contrast: trVal('ts-con'), brightness: trVal('ts-bri'),
  } }]), 250);
}
for (const id of ['ts-sea', 'ts-hill', 'ts-contour', 'ts-tint', 'ts-tint-amt', 'ts-sat', 'ts-con', 'ts-bri']) {
  document.getElementById(id).oninput = pushStyle;
}
document.getElementById('tr-radius').oninput = (e) => { document.getElementById('tr-radius-o').textContent = e.target.value; };
document.getElementById('ts-regen').onclick = () => {
  document.getElementById('nd-seed').value = String(Math.floor(Math.random() * 1e6));
  const w = curFloor().canvas;
  if (w) { document.getElementById('nd-cols').value = w[0]; document.getElementById('nd-rows').value = w[1]; }
  document.querySelector('input[name=nd-kind][value=region]').checked = true;
  document.getElementById('nd-world').hidden = false;
  document.getElementById('nd-name').value = state.doc.manifest.name;
  // Regenerate in place: the dialog's submit creates a NEW project, so handle this one separately.
  if (window.confirm('Generate a new world over this one? (Undo brings the old one back.)')) {
    api('POST', `/api/docs/${state.docId}/generate`, worldParams()).then(() => { tileCache.clear(); return refresh(); })
      .catch((err) => { statusEl.textContent = `Could not generate: ${err.message}`; });
  }
};

// ---------- labels (typography) ----------

const lbVal = (id) => parseFloat(document.getElementById(id).value);
document.getElementById('lb-size').oninput = (e) => { document.getElementById('lb-size-o').textContent = e.target.value; };
document.getElementById('lb-rot').oninput = (e) => { document.getElementById('lb-rot-o').textContent = `${e.target.value}°`; };

function labelBase(text) {
  return {
    id: newId('lb'), text: text.trim().slice(0, 200), size: lbVal('lb-size'),
    color: document.getElementById('lb-color').value, bold: document.getElementById('lb-bold').checked,
    halo: document.getElementById('lb-halo').checked, tracking: lbVal('lb-track'),
    rotation: (lbVal('lb-rot') * Math.PI) / 180, gm_only: document.getElementById('lb-gm').checked,
  };
}

async function placeLabel(wx, wy) {
  const text = window.prompt('Label text', '');
  if (text === null || !text.trim()) return;
  const s = ppc();
  await postOps([{ t: 'upsert_label', label: { ...labelBase(text), x: +(wx / s).toFixed(3), y: +(wy / s).toFixed(3) } }]);
}

async function finishBiome() {
  const pts = overlayDraft;
  if (!pts || pts.length < 3) { render(); return; }   // too few corners: keep drawing
  overlayDraft = null;
  try {
    const res = await api('POST', `/api/docs/${state.docId}/scatter`, {
      floor: state.floorId, points: pts, biome: document.getElementById('bi-kind').value,
      density: Number(document.getElementById('bi-density').value), seed: Math.floor(Math.random() * 4294967295),
    });
    state.rev = res.rev; tileCache.clear(); await refresh();
  } catch (e) { statusEl.textContent = `Error: ${e.message}`; render(); }
}

async function finishPathLabel() {
  const pts = overlayDraft;
  if (!pts || pts.length < 2) { render(); return; }   // too few points: keep drawing
  overlayDraft = null;
  const text = window.prompt('Text along the path', '');
  if (text === null || !text.trim()) { render(); return; }
  await postOps([{ t: 'upsert_label', label: { ...labelBase(text), x: pts[0][0], y: pts[0][1], rotation: 0, path: pts } }]);
}

// ---------- overlay: markers, regions, routes ----------

let overlayDraft = null;    // [[x,y]] for a region or route in progress
const newId = (p) => p + '_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 5);

async function placeMarker(wx, wy) {
  const label = window.prompt('Marker name', 'New place');
  if (label === null) return;
  const s = ppc();
  await postOps([{ t: 'put_overlay', item: {
    kind: 'marker', id: newId('mk'), x: +(wx / s).toFixed(3), y: +(wy / s).toFixed(3), label: label.trim().slice(0, 80),
  } }]);
}

// Stairs, ladders and lifts are markers that lead to another floor.
async function placeStairs(wx, wy) {
  const others = state.doc.floors.filter(f => f.id !== state.floorId);
  if (others.length === 0) { statusEl.textContent = 'Add another floor first (the + in the floor bar).'; return; }
  const label = window.prompt('Name (stairs, ladder, lift…)', 'Stairs');
  if (label === null) return;
  const list = others.map((f, i) => `${i + 1}) ${f.name}`).join('   ');
  const pick = window.prompt(`Leads to which floor?  ${list}`, '1');
  const target = others[Number(pick) - 1];
  if (!target) { statusEl.textContent = 'No such floor; nothing placed.'; return; }
  const s = ppc();
  const at = { x: +(wx / s).toFixed(3), y: +(wy / s).toFixed(3) };
  const here = state.floorId;
  const name = label.trim().slice(0, 80) || 'Stairs';
  await postOps([{ t: 'put_overlay', item: {
    kind: 'marker', id: newId('st'), ...at, label: name, links_to: { floor: target.id },
  } }]);
  // The way back, at the same spot, so the two floors line up.
  if (window.confirm(`Also add the way back on ${target.name}?`)) {
    try {
      const res = await api('POST', `/api/docs/${state.docId}/ops`, { floor: target.id, ops: [{ t: 'put_overlay', item: {
        kind: 'marker', id: newId('st'), ...at, label: name, links_to: { floor: here },
      } }] });
      state.rev = res.rev; tileCache.clear(); await refresh();
    } catch (e) { statusEl.textContent = `Error: ${e.message}`; }
  }
}

// Double-click a stair with the pan tool to follow it.
function followStairs(wx, wy) {
  const s = ppc();
  const hit = (curFloor().markers || []).find(m => m.links_to && Math.hypot(m.x * s - wx, m.y * s - wy) < 14);
  if (!hit || !state.doc.floors.some(f => f.id === hit.links_to.floor)) return false;
  selectFloor(hit.links_to.floor);
  return true;
}

function addOverlayPoint(wx, wy) {
  const s = ppc();
  (overlayDraft ||= []).push([+(wx / s).toFixed(3), +(wy / s).toFixed(3)]);
  render();
}

function finishDraft() {
  if (state.tool === 'river') return finishRiver();
  if (state.tool === 'region' || state.tool === 'route') return finishOverlayShape();
  if (state.tool === 'pathlabel') return finishPathLabel();
  if (state.tool === 'biome') return finishBiome();
}

async function finishOverlayShape() {
  const pts = overlayDraft;
  const kind = state.tool;
  if (!pts || pts.length < (kind === 'region' ? 3 : 2)) { render(); return; }   // too few: keep drawing
  overlayDraft = null;
  const name = window.prompt(kind === 'region' ? 'Region name' : 'Route name', kind === 'region' ? 'New region' : 'New route');
  if (name === null) { render(); return; }
  const item = kind === 'region'
    ? { kind, id: newId('rg'), name: name.trim().slice(0, 80), points: pts, color: '#c0504d', opacity: 0.28 }
    : { kind, id: newId('rt'), name: name.trim().slice(0, 80), points: pts, dashed: true, smooth: true };
  await postOps([{ t: 'put_overlay', item }]);
}

function drawOverlayDraft() {
  if (!overlayDraft || !overlayDraft.length) return;
  const s = ppc();
  ctx.save();
  ctx.strokeStyle = state.tool === 'region' || state.tool === 'biome' ? 'rgba(255,120,110,0.95)' : state.tool === 'pathlabel' ? 'rgba(160,220,255,0.95)' : 'rgba(255,230,140,0.95)';
  ctx.lineWidth = 2.5; ctx.lineJoin = 'round';
  ctx.beginPath();
  overlayDraft.forEach((p, i) => { const [x, y] = worldToScreen(p[0] * s, p[1] * s); i ? ctx.lineTo(x, y) : ctx.moveTo(x, y); });
  if (state.hover) { const [x, y] = worldToScreen(state.hover[0] * s, state.hover[1] * s); ctx.lineTo(x, y); }
  ctx.stroke();
  for (const p of overlayDraft) { const [x, y] = worldToScreen(p[0] * s, p[1] * s); ctx.fillStyle = '#fff'; ctx.fillRect(x - 3, y - 3, 6, 6); }
  ctx.restore();
}

// A double-click finishes a polyline only if the two clicks landed on the same
// spot; fast clicks at different corners are just fast clicks.
let lastClick = { x: -99, y: -99, t: 0 };
function isDoubleClick(e) {
  const near = Math.hypot(e.offsetX - lastClick.x, e.offsetY - lastClick.y) < 8 && e.timeStamp - lastClick.t < 500;
  lastClick = { x: e.offsetX, y: e.offsetY, t: e.timeStamp };
  return near;
}

// ---------- input ----------

let dragging = null; // {sx, sy, px, py, moved}

canvas.addEventListener('pointerdown', (e) => {
  try { canvas.setPointerCapture(e.pointerId); } catch { /* an inactive pointer id: keep going */ }
  const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
  if (state.pv.on && e.button === 0 && isDoubleClick(e)) {
    state.pv.x = wx / ppc(); state.pv.y = wy / ppc();
    loadPreview(); render();
    dragging = null;
    return;
  }
  // The player view is read-only: no tool edits the map while it is on.
  if (state.pv.on && e.button === 0 && state.tool !== 'pan') {
    statusEl.textContent = 'Switch off the player view to edit (👁 Player view).';
    dragging = null;
    return;
  }
  const wantPaint = state.tool === 'paint' && e.button === 0;
  if ((state.tool === 'brush' || state.tool === 'erase') && e.button === 0) {
    startStroke(wx, wy, e);
    dragging = { stroking: true };
    return;
  }
  if (TERRAIN_TOOLS.includes(state.tool) && e.button === 0) {
    startTerrainStroke(wx, wy);
    dragging = { terrain: true };
    return;
  }
  if (state.tool === 'label' && e.button === 0) {
    placeLabel(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'pathlabel' && e.button === 0) {
    if (isDoubleClick(e)) finishPathLabel(); else addOverlayPoint(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'prefab' && e.button === 0) {
    placePrefabAt(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'stairs' && e.button === 0) {
    placeStairs(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'pan' && e.button === 0 && isDoubleClick(e) && followStairs(wx, wy)) {
    dragging = null;
    return;
  }
  if (state.tool === 'marker' && e.button === 0) {
    placeMarker(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'biome' && e.button === 0) {
    if (isDoubleClick(e)) finishBiome(); else addOverlayPoint(wx, wy);
    dragging = null;
    return;
  }
  if ((state.tool === 'region' || state.tool === 'route') && e.button === 0) {
    if (isDoubleClick(e)) finishOverlayShape(); else addOverlayPoint(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'river' && e.button === 0) {
    if (isDoubleClick(e)) finishRiver(); else addRiverPoint(wx, wy);
    dragging = null;
    return;
  }
  if (state.tool === 'stamp' && e.button === 0) {
    placeStamp(wx, wy);
    dragging = null;
    return;
  }
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
    state.lastCell = null;
    paintTo(cx, cy);
    dragging = { painting: true };
  } else {
    dragging = { sx: e.offsetX, sy: e.offsetY, px: state.px, py: state.py, moved: false };
    canvas.classList.add('panning');
  }
});

canvas.addEventListener('pointermove', (e) => {
  const [hx, hy] = screenToWorld(e.offsetX, e.offsetY);
  state.hover = [hx / ppc(), hy / ppc()];
  if (TERRAIN_TOOLS.includes(state.tool) || ['river', 'region', 'route', 'pathlabel', 'biome'].includes(state.tool)) render();
  if (!dragging) return;
  if (dragging.terrain) {
    const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
    addTerrainPoint(wx, wy);
    return;
  }
  if (dragging.stroking) {
    // Coalesced events keep fast pen strokes smooth.
    const evs = e.getCoalescedEvents ? e.getCoalescedEvents() : [];
    for (const ev of (evs.length ? evs : [e])) {
      const [wx, wy] = screenToWorld(ev.offsetX ?? e.offsetX, ev.offsetY ?? e.offsetY);
      addStrokePoint(wx, wy, ev);
    }
  } else if (dragging.painting) {
    const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
    const [cx, cy] = worldToCell(wx, wy);
    paintTo(cx, cy);
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
  if (dragging && dragging.stroking) finishStroke();
  if (dragging && dragging.terrain) finishTerrainStroke();
  if (dragging && dragging.painting) {
    clearTimeout(state.pendingFlush);
    state.lastCell = null;
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
  { key: 'r', label: 'Brush tool (turns the prefab while placing one)', run: () => { if (state.tool === 'prefab') { state.prefabTurns = (state.prefabTurns + 1) % 4; setTool('prefab'); } else setTool('brush'); } },
  { key: 'e', label: 'Eraser tool', run: () => setTool('erase') },
  { key: 'h', label: 'Raise land (world maps)', run: () => regionOnly('raise') },
  { key: 'l', label: 'Lower land (world maps)', run: () => regionOnly('lower') },
  { key: 'm', label: 'Smooth land (world maps)', run: () => regionOnly('smooth') },
  { key: 'g', label: 'Flatten land (world maps)', run: () => regionOnly('flatten') },
  { key: 'x', label: 'Roughen land (world maps)', run: () => regionOnly('roughen') },
  { key: 'i', label: 'River tool (world maps)', run: () => regionOnly('river') },
  { key: 't', label: 'Label tool', run: () => setTool('label') },
  { key: 'y', label: 'Text along a path', run: () => setTool('pathlabel') },
  { key: 'k', label: 'Marker tool (world maps)', run: () => regionOnly('marker') },
  { key: 'u', label: 'Route tool (world maps)', run: () => regionOnly('route') },
  { key: 'Enter', label: 'Finish the river / region / route', run: () => finishDraft() },
  { key: 's', label: 'Stamp tool', run: () => setTool('stamp') },
  { key: ',', label: 'Rotate stamp left', run: () => rotateStamp(-15) },
  { key: '.', label: 'Rotate stamp right', run: () => rotateStamp(15) },
  { key: 'f', label: 'Flip stamp', run: () => { const f = document.getElementById('st-flip'); f.checked = !f.checked; } },
  { key: '[', label: 'Smaller brush', run: () => nudgeSize(-1) },
  { key: ']', label: 'Larger brush', run: () => nudgeSize(1) },
  { key: 'd', label: 'Door tool', run: () => setTool('door') },
  { key: 'w', label: 'Window tool', run: () => setTool('window') },
  { key: 'v', label: 'Pan tool', run: () => setTool('pan') },
  { key: 'p', label: 'Problems panel', run: () => toggleProblems() },
  { key: 'o', label: 'Outline (focus the tree)', run: () => toggleOutline() },
  { key: 'z', ctrl: true, label: 'Undo', run: () => undo() },
  { key: 'F1', label: 'This help', run: () => toggleHelp() },
  { key: 'Escape', label: 'Pan tool / close panels', run: () => { riverDraft = null; overlayDraft = null; toggleHelp(false); setTool('pan'); } },
];

function keyLabel(k) { return (k.ctrl ? 'Ctrl+' : '') + (k.key.length === 1 ? k.key.toUpperCase() : k.key); }

window.addEventListener('keydown', (e) => {
  // Inside the outline tree (or any text field) letters belong to it, not to the tools.
  if (e.target.closest) {
    const inField = e.target.closest('input, textarea');
    const inTree = e.target.closest('#outliner');
    const combo = e.ctrlKey || e.metaKey;
    // Text fields keep every key (including their own Ctrl+Z); the tree keeps plain
    // keys for itself but lets Ctrl-combinations (undo) through.
    if (inField && !['F1', 'Escape'].includes(e.key)) return;
    if (inTree && !combo && !['F1', 'Escape'].includes(e.key)) return;
  }
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
const treeOpen = { zones: true, rooms: true, lights: true, labels: true, stamps: true, markers: true, regions: true, routes: true };
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
  const o = curFloor().outline;
  // Rebuilding replaces the rows; if the keyboard was in the tree, put it back.
  const hadFocus = treeEl.contains(document.activeElement);
  treeEl.textContent = '';
  const groups = [
    ['zones', 'Zones', (curFloor().zones || []).length],
    ['rooms', 'Rooms', o.spaces.length],
    ['lights', 'Lights', o.lights.length],
    ['labels', 'Notes', o.labels.length],
    ['stamps', 'Props', (o.stamps || []).length],
    ['markers', 'Markers', (curFloor().markers || []).length],
    ['regions', 'Regions', (curFloor().regions || []).length],
    ['routes', 'Routes', (curFloor().routes || []).length],
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
      r.setAttribute('aria-selected', String(state.sel.has(s.id)));
      r.onclick = (e) => {
        if (e.ctrlKey || e.metaKey || e.shiftKey) { toggleSel(s.id); return; }
        focusRow(r); goToRoom(s);
      };
      r.onrenamerequest = () => renameRoom(s);
    }
    if (g === 'zones') for (const z of (curFloor().zones || [])) {
      const live = z.spaces.filter(id => o.spaces.some(s => s.id === id)).length;
      const r = addRow(treeEl, `z:${z.id}`, 2,
        `<span class="chip" style="background:${esc(z.color)}"></span>${esc(z.name)}<span class="meta">${live} rooms</span>`,
        `Zone ${z.name}, ${live} rooms`);
      r.dataset.kind = 'zone';
      r.onclick = () => { focusRow(r); setSel(z.spaces.filter(id => o.spaces.some(s => s.id === id))); };
      r.ondeleterequest = () => postOps([{ t: 'remove_zone', id: z.id }]);
      r.onrenamerequest = () => {
        const n = window.prompt('Rename zone', z.name);
        if (n === null || !n.trim() || n.trim() === z.name) return;
        postOps([{ t: 'upsert_zone', zone: { ...z, name: n.trim().slice(0, 80) } }]);
      };
    }
    if (g === 'lights') for (const l of o.lights) {
      const r = addRow(treeEl, `l:${l.id}`, 2, `${esc(l.label)}<span class="meta">${l.on ? 'on' : 'off'}</span>`,
        `Light ${l.label}, ${l.on ? 'on' : 'off'}`);
      r.dataset.kind = 'light';
      r.onclick = () => { focusRow(r); centerOn({ x: l.cell[0], y: l.cell[1] }); };
      r.ondeleterequest = () => postOps([{ t: 'remove_light', id: l.id }]);
      r.cell = l.cell;
    }
    if (g === 'markers' || g === 'regions' || g === 'routes') {
      const f = curFloor();
      const kind = g === 'markers' ? 'marker' : g === 'regions' ? 'region' : 'route';
      for (const it of f[g]) {
        const name = it.label ?? it.name ?? it.id;
        const where = kind === 'marker' ? `${it.x.toFixed(1)}, ${it.y.toFixed(1)}` : `${it.points.length} points`;
        const r = addRow(treeEl, `o:${kind}:${it.id}`, 2, `${esc(name)}<span class="meta">${where}</span>`, `${kind} ${name}, ${where}`);
        r.dataset.kind = kind;
        const pt = kind === 'marker' ? { x: it.x, y: it.y } : { x: it.points[0][0], y: it.points[0][1] };
        r.onclick = () => { focusRow(r); centerOn(pt); };
        r.ondeleterequest = () => postOps([{ t: 'remove_overlay', kind, id: it.id }]);
        r.onrenamerequest = () => {
          const n = window.prompt(`Rename ${kind}`, name);
          if (n === null || !n.trim() || n.trim() === name) return;
          const next = { ...it, kind, ...(kind === 'marker' ? { label: n.trim().slice(0, 80) } : { name: n.trim().slice(0, 80) }) };
          postOps([{ t: 'put_overlay', item: next }]);
        };
      }
    }
    if (g === 'stamps') for (const st of (o.stamps || [])) {
      const name = STAMPS.find(x => x.id === st.asset)?.name || st.asset;
      const r = addRow(treeEl, `p:${st.id}`, 2, `${esc(name)}<span class="meta">${st.cell[0]}, ${st.cell[1]}</span>`, `Prop ${name} at ${st.cell[0]}, ${st.cell[1]}`);
      r.dataset.kind = 'stamp';
      r.onclick = () => { focusRow(r); centerOn({ x: st.cell[0], y: st.cell[1] }); };
      r.ondeleterequest = () => postOps([{ t: 'remove_stamp', id: st.id }]);
    }
    if (g === 'labels') for (const l of o.labels) {
      const r = addRow(treeEl, `n:${l.id}`, 2, `${esc(l.text)}<span class="meta">${l.gm_only ? 'GM only' : ''}</span>`,
        `Note ${l.text}${l.gm_only ? ', GM only' : ''}`);
      r.dataset.kind = 'label';
      r.onclick = () => { focusRow(r); centerOn({ x: l.cell[0], y: l.cell[1] }); };
      r.ondeleterequest = () => postOps([{ t: 'remove_label', id: l.id }]);
      r.onrenamerequest = () => {
        const full = (curFloor().labels || []).find(x => x.id === l.id);
        const n = window.prompt('Label text', l.text);
        if (!full || n === null || !n.trim() || n.trim() === l.text) return;
        postOps([{ t: 'upsert_label', label: { ...full, text: n.trim().slice(0, 200) } }]);
      };
    }
  }
  updateBulk();
  const rows = visibleRows();
  const keep = rows.find(r => r.dataset.key === treeFocus) || rows[0];
  if (keep) {
    keep.tabIndex = 0;
    if (hadFocus) keep.focus();
  }
}

// ---------- map from a spec (proposal workflow) ----------
const specDialog = document.getElementById('spec-dialog');
const SPEC_EXAMPLE = {
  name: 'Abandoned metro station',
  floors: [
    { name: 'Platform', rooms: [
        { id: 'hall', name: 'Ticket Hall', w: 9, h: 6 }, { id: 'plat', name: 'Platform', w: 14, h: 4 },
        { id: 'office', name: 'Control Office', w: 5, h: 5 }, { id: 'store', name: 'Hidden Store', w: 4, h: 4 } ],
      links: [ { a: 'hall', b: 'plat', kind: 'opening' }, { a: 'hall', b: 'office' }, { a: 'office', b: 'store', kind: 'secret' } ],
      stairs: [ { room: 'plat', to_floor: 'Tunnels', to_room: 'tun' } ] },
    { name: 'Tunnels', rooms: [ { id: 'tun', name: 'Service Tunnel', w: 12, h: 3 }, { id: 'pump', name: 'Flooded Pump Room', w: 6, h: 6 } ],
      links: [ { a: 'tun', b: 'pump' } ] },
  ],
};
const WORLD_EXAMPLE = {
  name: 'The Cold Archipelago', cols: 120, rows: 80, land: 0.38, continent: 0.25, ridges: 0.7, rivers: 6,
  biomes: [ { biome: 'forest', x: 0.5, y: 0.5, rx: 0.2, ry: 0.15 }, { biome: 'rocky', x: 0.3, y: 0.35, rx: 0.12, ry: 0.1, density: 1.5 } ],
  places: [ { name: 'Port Neon', note: 'A smugglers\' harbour.', x: 0.5, y: 0.5 }, { name: 'The Drowned Abbey', x: 0.35, y: 0.4 } ],
  regions: [ { name: 'The Wild North', points: [[0.15, 0.1], [0.85, 0.1], [0.85, 0.35], [0.15, 0.35]] } ],
};
const specMode = () => document.querySelector('input[name=spec-mode]:checked').value;
let proposal = null;     // {id, seed, spec, mode}
const proposalUrl = () => `/api/docs/${state.docId}/${proposal.mode === 'world' ? 'world-proposals' : 'proposals'}/${proposal.id}`;

async function runSpec(seed) {
  const msg = document.getElementById('spec-msg');
  let spec;
  try { spec = JSON.parse(document.getElementById('spec-text').value); }
  catch (e) { msg.textContent = `That is not valid JSON: ${e.message}`; return; }
  msg.textContent = 'Laying it out…';
  try {
    if (proposal) { try { await api('DELETE', proposalUrl()); } catch { /* already gone */ } proposal = null; }
    const mode = specMode();
    if (mode === 'world') {
      if (!isRegion()) { msg.textContent = 'World maps go in a world-map project (New → World map).'; return; }
      const p = await api('POST', `/api/docs/${state.docId}/propose-world`, { spec, seed });
      proposal = { id: p.id, seed, spec, mode };
      const box = document.getElementById('spec-floors');
      box.textContent = '';
      const fig = document.createElement('figure');
      const img = document.createElement('img');
      img.alt = `Proposed world ${p.name}`;
      img.src = `/api/docs/${state.docId}/world-proposals/${p.id}/preview?ppc=4`;
      const cap = document.createElement('figcaption');
      cap.textContent = `${p.name}: ${p.biomes} biome(s), ${p.places} place(s), ${p.regions} region(s). Accepting replaces this map's terrain (Ctrl+Z undoes).`;
      fig.append(img, cap); box.appendChild(fig);
      document.getElementById('spec-result').hidden = false;
      msg.textContent = `World ${seed}. Nothing has been changed yet.`;
      return;
    }
    const p = await api('POST', `/api/docs/${state.docId}/propose`, { spec, seed });
    proposal = { id: p.id, seed, spec, mode };
    const box = document.getElementById('spec-floors');
    box.textContent = '';
    p.floors.forEach((f, i) => {
      const fig = document.createElement('figure');
      const img = document.createElement('img');
      img.alt = `Proposed floor ${f.name}`;
      img.src = `/api/docs/${state.docId}/proposals/${p.id}/preview?floor=${i}&ppc=10`;
      const cap = document.createElement('figcaption');
      cap.textContent = `${f.name}: ${f.rooms} rooms, ${f.squares} squares` + (f.problems.length ? ` · ${f.problems.length} note(s)` : '');
      if (f.problems.length) cap.title = f.problems.join('\n');
      fig.append(img, cap);
      box.appendChild(fig);
    });
    document.getElementById('spec-result').hidden = false;
    msg.textContent = `Layout ${seed}. Nothing has been added yet.`;
  } catch (e) {
    document.getElementById('spec-result').hidden = true;
    msg.textContent = e.message;       // the server says what to fix
  }
}
async function showAiStatus() {
  const el = document.getElementById('spec-ai');
  const btn = document.getElementById('spec-draft');
  try {
    const st = await api('GET', '/api/ai/status');
    const icon = st.mode === 'nd-world' ? '🌐' : st.mode === 'standalone' ? '🏠' : '⛔';
    el.textContent = `${icon} AI: ${st.mode === 'off' ? 'not set up. ' : ''}${st.detail}`;
    btn.disabled = st.mode === 'off';
    btn.title = st.mode === 'off' ? st.detail : 'Ask the AI for a spec';
    document.getElementById('spec-lore').disabled = st.mode !== 'nd-world';
  } catch { el.textContent = ''; }
}
function openSpecDialog() { document.getElementById('spec-msg').textContent = ''; specDialog.showModal(); showAiStatus(); }
document.getElementById('btn-spec').onclick = openSpecDialog;
document.getElementById('spec-draft').onclick = async () => {
  const prompt = document.getElementById('spec-prompt').value.trim();
  const msg = document.getElementById('spec-msg');
  if (!prompt) { msg.textContent = 'Describe the map first.'; return; }
  const btn = document.getElementById('spec-draft');
  btn.disabled = true; msg.textContent = 'Asking the AI (this can take a minute)…';
  try {
    const { spec } = await api('POST', specMode() === 'world' ? '/api/draft-world' : '/api/draft', { prompt, doc: state.docId, lore: document.getElementById('spec-lore').checked });
    document.getElementById('spec-text').value = JSON.stringify(spec, null, 2);
    await runSpec(Math.floor(Math.random() * 1e6));
  } catch (e) { msg.textContent = e.message; }
  finally { btn.disabled = false; }
};
document.getElementById('spec-example').onclick = () => { document.getElementById('spec-text').value = JSON.stringify(specMode() === 'world' ? WORLD_EXAMPLE : SPEC_EXAMPLE, null, 2); };
document.getElementById('spec-go').onclick = () => runSpec(Math.floor(Math.random() * 1e6));
document.getElementById('spec-again').onclick = () => runSpec(Math.floor(Math.random() * 1e6));
document.getElementById('spec-accept').onclick = async () => {
  if (!proposal) return;
  try {
    const world = proposal.mode === 'world';
    const res = await api('POST', `${proposalUrl()}/accept`);
    proposal = null; specDialog.close();
    state.rev = res.rev; tileCache.clear();
    if (world) {
      await refresh();
      statusEl.textContent = 'Drew the world. Ctrl+Z takes it back in one step.';
    } else {
      state.floorId = res.floors[0];
      await refresh();
      statusEl.textContent = `Added ${res.floors.length} floor(s). Undo floors (↶ Floors) takes them back one at a time.`;
    }
  } catch (e) { document.getElementById('spec-msg').textContent = e.message; }
};
document.getElementById('spec-reject').onclick = async () => {
  if (proposal) { try { await api('DELETE', proposalUrl()); } catch { /* gone */ } proposal = null; }
  document.getElementById('spec-result').hidden = true;
  document.getElementById('spec-msg').textContent = 'Rejected. Nothing was added.';
};
document.getElementById('spec-close').onclick = () => specDialog.close();

// ---------- make art (AI pictures → materials, props, underlays) ----------
const artDialog = document.getElementById('art-dialog');
let artJob = null, artTimer = null;

async function reloadPalettes() {
  try {
    const { materials } = await api('GET', '/api/materials');
    if (materials.length) PALETTE = materials.map(m => ({ id: m.id, color: m.color, name: m.name }));
    buildPalette();
  } catch { /* keep the old palette */ }
  await loadStamps();
}

function artItem(job, assetId, seed) {
  const fig = document.createElement('figure');
  const img = document.createElement('img');
  img.alt = `Generated picture, seed ${seed}`;
  img.src = `/api/assets/${assetId}`;
  const cap = document.createElement('figcaption');
  const prov = { prompt: job.prompt, seed, ai: true };
  const mk = (label, run) => {
    const b = document.createElement('button');
    b.textContent = label;
    b.onclick = async () => { b.disabled = true; try { await run(); b.textContent = '✓ ' + label; } catch (e) { b.disabled = false; document.getElementById('art-msg').textContent = e.message; } };
    cap.appendChild(b);
  };
  if (job.kind === 'material') mk('Keep as material', async () => {
    const name = window.prompt('Name this material', job.prompt.slice(0, 40)); if (!name) throw new Error('Cancelled.');
    const r = await api('POST', `/api/assets/${assetId}/make-material`, { name, size: 2, ...prov });
    await reloadPalettes();
    document.getElementById('art-msg').textContent = `Added "${name}" to the palette (${r.id}). Paint with it like any material.`;
  });
  if (job.kind === 'prop') mk('Keep as prop', async () => {
    const name = window.prompt('Name this prop', job.prompt.slice(0, 40)); if (!name) throw new Error('Cancelled.');
    const r = await api('POST', `/api/assets/${assetId}/make-stamp`, { name, size: 1.5, ...prov });
    await reloadPalettes();
    document.getElementById('art-msg').textContent = `Added "${name}" to the props (${r.id}).`;
  });
  if (job.kind === 'underlay') mk('Use as underlay', async () => {
    const f = curFloor(); const cols = Math.max(f.canvas?.[0] || 0, 20), rows = Math.max(f.canvas?.[1] || 0, 15);
    await api('POST', `/api/docs/${state.docId}/underlay`, { floor: state.floorId, asset: assetId, x: 0, y: 0, w: cols, h: rows, opacity: 1 });
    tileCache.clear(); await refresh();
    document.getElementById('art-msg').textContent = 'Placed behind the map. Ctrl+Z takes it back.';
  });
  fig.append(img, cap);
  return fig;
}

async function pollArt() {
  if (!artJob) return;
  let job;
  try { job = await api('GET', `/api/ai/jobs/${artJob}`); } catch (e) { document.getElementById('art-msg').textContent = e.message; return; }
  document.getElementById('art-msg').textContent = job.message;
  const box = document.getElementById('art-results');
  while (box.children.length < job.assets.length) {
    const i = box.children.length;
    box.appendChild(artItem(job, job.assets[i], job.seeds[i]));
  }
  if (job.status === 'running') { artTimer = setTimeout(pollArt, 2000); return; }
  document.getElementById('art-go').disabled = false;
  document.getElementById('art-cancel').hidden = true;
  artJob = null;
}

document.getElementById('btn-art').onclick = async () => {
  document.getElementById('art-msg').textContent = '';
  artDialog.showModal();
  try {
    const st = await api('GET', '/api/ai/status');
    const ok = st.mode === 'nd-world';
    document.getElementById('art-ai').textContent = ok ? `🌐 Pictures come from nd-world's imagegen queue (${st.detail}).` : '⛔ Picture making needs nd-world: set ND_CARTO_SERVER and ND_CARTO_EMAIL (and the password) for the studio.';
    document.getElementById('art-go').disabled = !ok || !!artJob;
  } catch { /* leave as is */ }
};
document.getElementById('art-go').onclick = async () => {
  const prompt = document.getElementById('art-prompt').value.trim();
  const msg = document.getElementById('art-msg');
  if (!prompt) { msg.textContent = 'Describe the picture first.'; return; }
  const kind = document.getElementById('art-kind').value;
  const size = Number(document.getElementById('art-size').value);
  document.getElementById('art-results').textContent = '';
  try {
    const { job } = await api('POST', '/api/ai/image', { kind, prompt, width: size, height: size, count: Number(document.getElementById('art-count').value), seed: Math.floor(Math.random() * 1e6) });
    artJob = job;
    document.getElementById('art-go').disabled = true;
    document.getElementById('art-cancel').hidden = false;
    msg.textContent = 'Queued…';
    pollArt();
  } catch (e) { msg.textContent = e.message; }
};
document.getElementById('art-cancel').onclick = async () => {
  if (artJob) { try { await api('POST', `/api/ai/jobs/${artJob}/cancel`); } catch { /* finished already */ } }
};
document.getElementById('art-close').onclick = () => artDialog.close();

// ---------- settings and health ----------
const settingsDialog = document.getElementById('settings-dialog');
document.getElementById('btn-settings').onclick = async () => {
  const list = document.getElementById('settings-list');
  list.textContent = 'Checking…';
  settingsDialog.showModal();
  try {
    const { checks } = await api('GET', '/api/doctor');
    list.textContent = '';
    for (const c of checks) {
      const li = document.createElement('li'); li.className = c.level;
      const lv = document.createElement('span'); lv.className = 'lv'; lv.textContent = c.level === 'ok' ? 'ok' : c.level === 'warn' ? 'warn' : 'FAIL';
      const t = document.createElement('span'); t.textContent = `${c.what}: ${c.detail}`;   // text, never HTML
      li.append(lv, t); list.appendChild(li);
    }
  } catch (e) { list.textContent = e.message; }
};
document.getElementById('settings-close').onclick = () => settingsDialog.close();

// ---------- import an art pack ----------
document.getElementById('btn-pack').onclick = () => document.getElementById('pack-file').click();
document.getElementById('pack-file').onchange = async (e) => {
  const file = e.target.files[0];
  e.target.value = '';
  if (!file) return;
  statusEl.textContent = `Reading ${file.name}…`;
  try {
    const res = await fetch(`/api/packs/import?name=${encodeURIComponent(file.name)}`, { method: 'POST', body: file });
    const out = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(out.error || res.statusText);
    await reloadPalettes();
    statusEl.textContent = `${out.pack}: added ${out.stamps} prop(s) and ${out.materials} material(s)` + (out.skipped ? `, ${out.skipped} picture(s) skipped` : '') + (out.notes?.length ? `. ${out.notes.join(' ')}` : '.');
  } catch (err) { statusEl.textContent = `Import failed: ${err.message}`; }
};

// ---------- house style ----------
document.getElementById('btn-style').onclick = async () => {
  const cur = state.doc.manifest.style || { tone: '', palette: [] };
  const tone = window.prompt('The look of this project, in a few words (blank clears it)', cur.tone);
  if (tone === null) return;
  const pal = window.prompt('Room colours, comma-separated #rrggbb (up to 8; blank for the default set)', cur.palette.join(', '));
  if (pal === null) return;
  const palette = pal.split(',').map(x => x.trim()).filter(Boolean);
  try {
    await api('POST', `/api/docs/${state.docId}/style`, { tone, palette });
    await refresh();
    statusEl.textContent = tone || palette.length ? 'Style saved: AI drafts and generators will follow it.' : 'Style cleared.';
  } catch (e) { statusEl.textContent = e.message; }
};

// ---------- search all maps ----------
const searchBox = document.getElementById('search-box');
const searchList = document.getElementById('search-results');
let searchTimer = null;
searchBox.addEventListener('input', () => {
  clearTimeout(searchTimer);
  const q = searchBox.value.trim();
  if (!q) { searchList.hidden = true; return; }
  searchTimer = setTimeout(async () => {
    let hits = [];
    try { hits = (await api('GET', `/api/search?q=${encodeURIComponent(q)}`)).hits; } catch { return; }
    searchList.textContent = '';
    if (hits.length === 0) { const li = document.createElement('li'); li.className = 'dim'; li.textContent = 'Nothing found'; searchList.appendChild(li); }
    for (const h of hits) {
      const li = document.createElement('li');
      li.tabIndex = 0; li.setAttribute('role', 'option');
      li.append(document.createTextNode(h.name));
      const k = document.createElement('span'); k.className = 'k';
      k.textContent = `${h.kind} · ${h.doc_name}${h.floor_name ? ' / ' + h.floor_name : ''}`;
      li.appendChild(k);
      const go = async () => {
        searchList.hidden = true;
        if (h.doc !== state.docId) await openDoc(h.doc);
        if (h.floor && state.doc.floors.some(f => f.id === h.floor) && h.floor !== state.floorId) await selectFloor(h.floor);
        if (h.at) centerOn({ x: h.at[0] - 0.5, y: h.at[1] - 0.5 });
      };
      li.onclick = go;
      li.onkeydown = (e) => { if (e.key === 'Enter') go(); };
      searchList.appendChild(li);
    }
    searchList.hidden = false;
  }, 200);
});
searchBox.addEventListener('keydown', (e) => { if (e.key === 'Escape') { searchList.hidden = true; searchBox.blur(); } });
document.addEventListener('click', (e) => { if (!e.target.closest('#searchwrap')) searchList.hidden = true; });

// ---------- player view ----------
// The fog preview: the server renders what the table sees from one spot
// (the players' copy of the floor, walls and closed doors blocking sight).
const PV_PPC = 16;
state.pv = { on: false, x: 0.5, y: 0.5, img: null, key: '' };
let pvTimer = null;

function pvParams() {
  return `floor=${state.floorId}&x=${state.pv.x.toFixed(2)}&y=${state.pv.y.toFixed(2)}&radius=${document.getElementById('pv-radius').value}` +
    `&darkness=${document.getElementById('pv-dark').checked}&ppc=${PV_PPC}&rev=${state.rev}`;
}
function loadPreview() {
  if (!state.pv.on) return;
  clearTimeout(pvTimer);
  pvTimer = setTimeout(() => {
    const key = `${state.docId}|${pvParams()}`;
    if (key === state.pv.key) return;
    pushTable();
    const img = new Image();
    img.onload = () => { state.pv.img = img; state.pv.key = key; render(); };
    img.onerror = () => { state.pv.img = null; state.pv.key = key; statusEl.textContent = 'Nothing to preview on this floor yet.'; render(); };
    img.src = `/api/docs/${state.docId}/preview?${pvParams()}`;
  }, 150);
}
function drawPreview() {
  if (!state.pv.on || !state.pv.img) return;
  const k = ppc() / PV_PPC;
  const [sx, sy] = worldToScreen(0, 0);
  ctx.drawImage(state.pv.img, sx, sy, state.pv.img.width * k, state.pv.img.height * k);
  const [vx, vy] = worldToScreen(state.pv.x * ppc(), state.pv.y * ppc());
  ctx.save();
  ctx.fillStyle = '#4cc2ff'; ctx.strokeStyle = '#06131a'; ctx.lineWidth = 2;
  ctx.beginPath(); ctx.arc(vx, vy, 6, 0, Math.PI * 2); ctx.fill(); ctx.stroke();
  ctx.restore();
}
// Share the view to the table screen (/table/<project>), kept in step as the GM moves it.
state.pv.share = false;
async function pushTable() {
  if (!state.pv.share) return;
  try {
    await api('POST', `/api/docs/${state.docId}/table`, {
      floor: state.floorId, x: state.pv.x, y: state.pv.y,
      radius: Number(document.getElementById('pv-radius').value), darkness: document.getElementById('pv-dark').checked,
    });
  } catch (e) { statusEl.textContent = `Table: ${e.message}`; }
}
async function setShare(on) {
  state.pv.share = on;
  const btn = document.getElementById('pv-share');
  btn.setAttribute('aria-pressed', String(on));
  document.getElementById('pv-link').textContent = on ? `On the table: open ${location.origin}/table/${state.docId} on the TV or tablet` : '';
  if (on) await pushTable();
  else { try { await api('DELETE', `/api/docs/${state.docId}/table`); } catch { /* already off */ } }
}
document.getElementById('pv-share').onclick = () => setShare(!state.pv.share);
function setPreview(on) {
  state.pv.on = on;
  document.getElementById('btn-preview').setAttribute('aria-pressed', String(on));
  document.getElementById('previewbar').hidden = !on;
  if (on) { state.pv.key = ''; loadPreview(); } else { if (state.pv.share) setShare(false); render(); }
}
document.getElementById('btn-preview').onclick = () => setPreview(!state.pv.on);
document.getElementById('pv-radius').oninput = (e) => { document.getElementById('pv-radius-o').textContent = e.target.value; loadPreview(); };
document.getElementById('pv-dark').onchange = loadPreview;

// ---------- map grid (square / hex) ----------
function syncMapGrid() {
  const g = curFloor().map_grid;
  document.getElementById('mg-kind').value = g ? g.kind : '';
  if (g) document.getElementById('mg-size').value = g.size;
}
function applyMapGrid() {
  const kind = document.getElementById('mg-kind').value;
  const size = Number(document.getElementById('mg-size').value);
  if (kind && !(size >= 0.5 && size <= 200)) { statusEl.textContent = 'Grid size must be 0.5 to 200 cells.'; return; }
  const prev = curFloor().map_grid;
  postOps([{ t: 'set_map_grid', grid: kind ? { kind, size, color: prev?.color || '#ffffff', opacity: prev?.opacity || 0.35 } : null }]);
}
document.getElementById('mg-kind').onchange = applyMapGrid;
document.getElementById('mg-size').onchange = () => { if (document.getElementById('mg-kind').value) applyMapGrid(); };

// ---------- minimap, saved views, copy view ----------
const mini = document.getElementById('minimap');
const miniImg = { img: null, key: '', cols: 0, rows: 0 };
let miniTimer = null;

function loadMiniImage() {
  clearTimeout(miniTimer);
  miniTimer = setTimeout(() => {
    if (!state.doc) return;
    const key = `${state.docId}/${state.floorId}/${state.rev}`;
    if (miniImg.key === key) return;
    const img = new Image();
    img.onload = () => { miniImg.img = img; miniImg.key = key; miniImg.cols = img.naturalWidth / 3; miniImg.rows = img.naturalHeight / 3; drawMinimap(); };
    img.onerror = () => { miniImg.img = null; miniImg.key = key; drawMinimap(); };   // an empty floor has no picture
    img.src = `/api/docs/${state.docId}/export/png?floor=${state.floorId}&ppc=3&format=webp&quality=50&rev=${state.rev}`;
  }, 500);
}

// Minimap geometry: the whole floor fitted into the little canvas.
function miniScale() {
  const cols = miniImg.cols || 1, rows = miniImg.rows || 1;
  const k = Math.min(mini.width / cols, mini.height / rows);
  return { k, ox: (mini.width - cols * k) / 2, oy: (mini.height - rows * k) / 2 };
}

function drawMinimap() {
  const g = mini.getContext('2d');
  g.clearRect(0, 0, mini.width, mini.height);
  if (!miniImg.img) return;
  const { k, ox, oy } = miniScale();
  g.drawImage(miniImg.img, ox, oy, miniImg.cols * k, miniImg.rows * k);
  // The part of the map the main view is showing.
  const s = ppc();
  const [wx0, wy0] = screenToWorld(0, 0);
  g.strokeStyle = '#4cc2ff'; g.lineWidth = 1.5;
  g.strokeRect(ox + (wx0 / s) * k, oy + (wy0 / s) * k, (canvas.clientWidth / s) * k, (canvas.clientHeight / s) * k);
}

function miniJump(e) {
  const r = mini.getBoundingClientRect();
  const { k, ox, oy } = miniScale();
  const px = (e.clientX - r.left) * (mini.width / r.width), py = (e.clientY - r.top) * (mini.height / r.height);
  centerOn({ x: (px - ox) / k - 0.5, y: (py - oy) / k - 0.5 });
}
let miniDrag = false;
mini.addEventListener('pointerdown', (e) => { miniDrag = true; mini.setPointerCapture(e.pointerId); miniJump(e); });
mini.addEventListener('pointermove', (e) => { if (miniDrag) miniJump(e); });
mini.addEventListener('pointerup', () => { miniDrag = false; });

// Saved views live in this browser only (a convenience, not part of the map).
function bmKey() { return `ndcart.views.${state.docId}`; }
function readViews() {
  try { const v = JSON.parse(localStorage.getItem(bmKey()) || '[]'); return Array.isArray(v) ? v : []; } catch { return []; }
}
function writeViews(v) { try { localStorage.setItem(bmKey(), JSON.stringify(v.slice(0, 50))); } catch { /* storage blocked: views just don't persist */ } }
function buildViews() {
  const sel = document.getElementById('bm-list');
  sel.textContent = '';
  const first = document.createElement('option'); first.value = ''; first.textContent = 'Saved views…'; sel.appendChild(first);
  readViews().forEach((v, i) => { const o = document.createElement('option'); o.value = String(i); o.textContent = v.name; sel.appendChild(o); });
}
document.getElementById('bm-save').onclick = () => {
  const name = window.prompt('Name this view', 'View');
  if (name === null || !name.trim()) return;
  const s = ppc();
  const [wx, wy] = screenToWorld(canvas.clientWidth / 2, canvas.clientHeight / 2);
  const v = readViews();
  v.push({ name: name.trim().slice(0, 40), floor: state.floorId, x: wx / s, y: wy / s, z: state.z });
  writeViews(v); buildViews();
};
document.getElementById('bm-list').onchange = async (e) => {
  const v = readViews()[Number(e.target.value)];
  e.target.value = '';
  if (!v) return;
  if (v.floor !== state.floorId && state.doc.floors.some(f => f.id === v.floor)) await selectFloor(v.floor);
  state.z = v.z;
  centerOn({ x: v.x - 0.5, y: v.y - 0.5 });
};
document.getElementById('bm-del').onclick = () => {
  const i = Number(document.getElementById('bm-list').value);
  const v = readViews();
  if (Number.isInteger(i) && v[i] && document.getElementById('bm-list').value !== '') { v.splice(i, 1); writeViews(v); buildViews(); }
  else statusEl.textContent = 'Pick a saved view in the list, then press ✕.';
};

document.getElementById('view-copy').onclick = () => {
  canvas.toBlob(async (blob) => {
    if (!blob) { statusEl.textContent = 'Nothing to copy yet.'; return; }
    try {
      await navigator.clipboard.write([new ClipboardItem({ 'image/png': blob })]);
      statusEl.textContent = 'Copied the view to the clipboard.';
    } catch {
      // No clipboard permission (or no secure context): offer the picture as a file instead.
      const a = document.createElement('a');
      a.href = URL.createObjectURL(blob);
      a.download = `${state.doc.manifest.name} (view).png`;
      a.click();
      setTimeout(() => URL.revokeObjectURL(a.href), 10000);
      statusEl.textContent = 'The clipboard was not available, so the view was saved as a file.';
    }
  }, 'image/png');
};

// ---------- projects gallery ----------
const galleryDialog = document.getElementById('gallery-dialog');
document.getElementById('btn-gallery').onclick = async () => {
  const grid = document.getElementById('gallery-grid');
  grid.textContent = '';
  let docs = [];
  try { docs = (await api('GET', '/api/docs')).docs; } catch (e) { statusEl.textContent = `Error: ${e.message}`; return; }
  for (const d of docs) {
    const card = document.createElement('button');
    card.className = 'card'; card.setAttribute('role', 'listitem');
    card.setAttribute('aria-current', String(d.id === state.docId));
    const img = document.createElement('img');
    img.alt = ''; img.loading = 'lazy';
    img.src = `/api/docs/${d.id}/export/png?ppc=4&format=webp&quality=50&rev=${d.rev}`;
    img.onerror = () => { img.removeAttribute('src'); };      // an empty map has no picture
    const nm = document.createElement('div'); nm.className = 'nm'; nm.textContent = d.name;
    const mt = document.createElement('div'); mt.className = 'mt';
    mt.textContent = `${d.kind} · ${d.floors.length} floor${d.floors.length === 1 ? '' : 's'}`;
    card.append(img, nm, mt);
    card.onclick = () => { galleryDialog.close(); if (d.id !== state.docId) openDoc(d.id); };
    grid.appendChild(card);
  }
  galleryDialog.showModal();
};
document.getElementById('gallery-close').onclick = () => galleryDialog.close();

// ---------- live refresh ----------
// If the project changes from outside (another tab, the HTTP API, a script),
// pick the change up instead of editing a stale copy.
setInterval(async () => {
  if (!state.doc || document.hidden || state.painting || dragging || document.querySelector('dialog[open]')) return;
  try {
    const { docs } = await api('GET', '/api/docs');
    const mine = docs.find(d => d.id === state.docId);
    if (mine && mine.rev > state.rev) {
      tileCache.clear();
      await refresh();
      statusEl.textContent = 'Updated: this project was changed elsewhere.';
    }
  } catch { /* server briefly away: try again next tick */ }
}, 3000);

// ---------- generate onto a floor ----------
const genDialog = document.getElementById('gen-dialog');
document.getElementById('fl-gen').onclick = () => {
  document.getElementById('gn-seed').value = Math.floor(Math.random() * 1e6);
  genDialog.showModal();
};
document.getElementById('gn-dice').onclick = () => { document.getElementById('gn-seed').value = Math.floor(Math.random() * 4294967295); };
document.getElementById('gn-cancel').onclick = () => genDialog.close();
document.getElementById('gen-form').onsubmit = async () => {
  const body = {
    floor: state.floorId,
    kind: document.getElementById('gn-kind').value,
    seed: Math.max(0, Math.floor(Number(document.getElementById('gn-seed').value) || 0)),
    width: Number(document.getElementById('gn-w').value),
    height: Number(document.getElementById('gn-h').value),
  };
  try {
    const res = await api('POST', `/api/docs/${state.docId}/generate-map`, body);
    state.rev = res.rev; tileCache.clear(); await refresh();
    statusEl.textContent = `Generated a ${body.kind} (seed ${body.seed}). Ctrl+Z takes it back.`;
  } catch (e) { statusEl.textContent = `Error: ${e.message}`; }
};

// ---------- prefabs ----------
state.prefab = null;       // {id, name, w, h} being placed
state.prefabTurns = 0;
async function loadPrefabs() {
  const list = document.getElementById('pf-list');
  let ps = [];
  try { ps = (await api('GET', '/api/prefabs')).prefabs; } catch { return; }
  list.textContent = '';
  if (ps.length === 0) { const li = document.createElement('li'); li.className = 'dim'; li.textContent = 'None yet'; list.appendChild(li); }
  for (const p of ps) {
    const li = document.createElement('li');
    li.setAttribute('aria-current', String(state.prefab?.id === p.id));
    const name = document.createElement('span');
    name.className = 'pf-name'; name.textContent = p.name;
    name.title = `${p.w}×${p.h} squares, ${p.rooms} room(s)`;
    const use = document.createElement('button'); use.textContent = state.prefab?.id === p.id ? 'Stop' : 'Place';
    use.onclick = () => {
      state.prefab = state.prefab?.id === p.id ? null : p;
      state.prefabTurns = 0;
      if (state.prefab) setTool('prefab'); else setTool('pan');
      document.activeElement?.blur();      // so R (turn) reaches the map, not the panel
      loadPrefabs();
    };
    const del = document.createElement('button'); del.textContent = 'Delete';
    del.onclick = async () => {
      try { await api('DELETE', `/api/prefabs/${p.id}`); if (state.prefab?.id === p.id) { state.prefab = null; setTool('pan'); } await loadPrefabs(); }
      catch (e) { statusEl.textContent = `Error: ${e.message}`; }
    };
    li.append(name, use, del);
    list.appendChild(li);
  }
}
document.getElementById('tool-prefab').onclick = () => {
  const d = document.getElementById('ppanel');
  d.open = true; loadPrefabs();
  if (state.prefab) setTool('prefab'); else statusEl.textContent = 'Pick a prefab in the Outline panel (Prefabs), then click the map.';
};
document.getElementById('ppanel').addEventListener('toggle', (e) => { if (e.target.open) loadPrefabs(); });
document.getElementById('pf-save').onclick = async () => {
  if (state.sel.size === 0) { statusEl.textContent = 'Select some rooms in the outline first (Ctrl+click).'; return; }
  const name = window.prompt('Prefab name', selSpaces()[0]?.name || 'My prefab');
  if (name === null || !name.trim()) return;
  try {
    await api('POST', `/api/docs/${state.docId}/prefabs`, { floor: state.floorId, name: name.trim().slice(0, 80), spaces: [...state.sel] });
    await loadPrefabs();
    statusEl.textContent = `Saved "${name.trim()}" to the prefab library.`;
  } catch (e) { statusEl.textContent = `Error: ${e.message}`; }
};
async function placePrefabAt(wx, wy) {
  if (!state.prefab) return;
  const [cx, cy] = worldToCell(wx, wy);
  try {
    const res = await api('POST', `/api/docs/${state.docId}/place-prefab`, { floor: state.floorId, prefab: state.prefab.id, x: cx, y: cy, turns: state.prefabTurns });
    state.rev = res.rev; tileCache.clear(); await refresh();
  } catch (e) { statusEl.textContent = `Error: ${e.message}`; }
}

// ---------- checkpoints ----------
async function loadCheckpoints() {
  const list = document.getElementById('cp-list');
  let cps = [];
  try { cps = (await api('GET', `/api/docs/${state.docId}/checkpoints`)).checkpoints; } catch { return; }
  list.textContent = '';
  if (cps.length === 0) { const li = document.createElement('li'); li.className = 'dim'; li.textContent = 'None yet'; list.appendChild(li); }
  for (const c of cps.slice().reverse()) {
    const li = document.createElement('li');
    const name = document.createElement('span');
    name.className = 'cp-name'; name.textContent = c.name;     // a name is never HTML
    name.title = `${c.created_at} · ${c.floors} floor(s), ${c.cells} squares`;
    const restore = document.createElement('button'); restore.textContent = 'Restore';
    restore.onclick = async () => {
      if (!window.confirm(`Restore "${c.name}"? The map as it is now is saved as a checkpoint first; undo history starts over.`)) return;
      try {
        await api('POST', `/api/docs/${state.docId}/checkpoints/${c.id}/restore`);
        tileCache.clear(); state.sel = new Set(); await refresh(); await loadCheckpoints();
      } catch (e) { statusEl.textContent = `Error: ${e.message}`; }
    };
    const del = document.createElement('button'); del.textContent = 'Delete';
    del.onclick = async () => {
      try { await api('DELETE', `/api/docs/${state.docId}/checkpoints/${c.id}`); await loadCheckpoints(); }
      catch (e) { statusEl.textContent = `Error: ${e.message}`; }
    };
    li.append(name, restore, del);
    list.appendChild(li);
  }
}
document.getElementById('cp-save').onclick = async () => {
  const name = window.prompt('Checkpoint name', 'Before the big change');
  if (name === null || !name.trim()) return;
  try { await api('POST', `/api/docs/${state.docId}/checkpoints`, { name: name.trim().slice(0, 80) }); await loadCheckpoints(); }
  catch (e) { statusEl.textContent = `Error: ${e.message}`; }
};
document.getElementById('cpanel').addEventListener('toggle', (e) => { if (e.target.open) loadCheckpoints(); });

// ---------- selection and bulk edits ----------
// A selection is a set of room ids. Every bulk edit is one batch op, so a
// single Ctrl+Z puts everything back.
state.sel = new Set();

function setSel(ids) { state.sel = new Set(ids); buildOutline(); updateBulk(); }
function toggleSel(id) {
  if (state.sel.has(id)) state.sel.delete(id); else state.sel.add(id);
  buildOutline(); updateBulk();
}
function selSpaces() {
  const all = curFloor().outline.spaces;
  return all.filter(s => state.sel.has(s.id));
}
function spaceOp(s, patch) {
  const { id, name, color, kind, material } = { ...s, ...patch };
  return { t: 'upsert_space', space: { id, name, color, kind, ...(material ? { material } : {}) } };
}

function updateBulk() {
  // Drop ids of rooms that no longer exist.
  const have = new Set(curFloor().outline.spaces.map(s => s.id));
  for (const id of [...state.sel]) if (!have.has(id)) state.sel.delete(id);
  const n = state.sel.size;
  document.getElementById('sel-count').textContent = `${n} selected`;
  for (const id of ['sel-none', 'bk-zone', 'bk-color', 'bk-kind', 'bk-prefix', 'bk-furnish', 'bk-delete']) {
    document.getElementById(id).disabled = n === 0;
  }
  const sel = document.getElementById('sel-by');
  const o = curFloor().outline;
  const opts = [['', 'Select by…'], ['all', 'All rooms']];
  for (const m of new Set(o.spaces.map(s => s.material).filter(Boolean))) opts.push([`m:${m}`, `material: ${m}`]);
  for (const k of new Set(o.spaces.map(s => s.kind))) opts.push([`k:${k}`, `kind: ${k}`]);
  for (const c of new Set(o.spaces.map(s => s.color))) opts.push([`c:${c}`, `colour ${c}`]);
  for (const z of (curFloor().zones || [])) opts.push([`z:${z.id}`, `zone: ${z.name}`]);
  opts.push(['inv', 'Invert selection']);
  sel.textContent = '';
  for (const [v, t] of opts) { const op = document.createElement('option'); op.value = v; op.textContent = t; sel.appendChild(op); }
}

document.getElementById('sel-by').onchange = (e) => {
  const v = e.target.value; e.target.value = '';
  const sp = curFloor().outline.spaces;
  let ids = [];
  if (v === 'all') ids = sp.map(s => s.id);
  else if (v === 'inv') ids = sp.filter(s => !state.sel.has(s.id)).map(s => s.id);
  else if (v.startsWith('m:')) ids = sp.filter(s => s.material === v.slice(2)).map(s => s.id);
  else if (v.startsWith('k:')) ids = sp.filter(s => s.kind === v.slice(2)).map(s => s.id);
  else if (v.startsWith('c:')) ids = sp.filter(s => s.color === v.slice(2)).map(s => s.id);
  else if (v.startsWith('z:')) ids = (curFloor().zones || []).find(z => z.id === v.slice(2))?.spaces || [];
  else return;
  setSel(ids);
};
document.getElementById('sel-none').onclick = () => setSel([]);
document.getElementById('bk-zone').onclick = () => {
  const name = window.prompt('Zone name', 'New zone');
  if (name === null || !name.trim()) return;
  const color = selSpaces()[0]?.color || '#8a8f98';
  postOps([{ t: 'upsert_zone', zone: { id: newId('zn'), name: name.trim().slice(0, 80), color, spaces: [...state.sel] } }]);
};
document.getElementById('bk-color').onclick = () => {
  const c = window.prompt('Colour for the selected rooms (#rrggbb)', selSpaces()[0]?.color || '#7f5539');
  if (c === null) return;
  if (!/^#[0-9a-fA-F]{6}$/.test(c.trim())) { statusEl.textContent = 'Colour must look like #7f5539'; return; }
  postOps([{ t: 'batch', ops: selSpaces().map(s => spaceOp(s, { color: c.trim() })) }]);
};
document.getElementById('bk-kind').onclick = () => {
  const k = window.prompt('Kind for the selected rooms (room, corridor or area)', selSpaces()[0]?.kind || 'room');
  if (k === null || !k.trim()) return;
  postOps([{ t: 'batch', ops: selSpaces().map(s => spaceOp(s, { kind: k.trim() })) }]);
};
document.getElementById('bk-prefix').onclick = () => {
  const p = window.prompt('Text to put before each name', 'East – ');
  if (p === null || !p) return;
  postOps([{ t: 'batch', ops: selSpaces().map(s => spaceOp(s, { name: (p + s.name).slice(0, 60) })) }]);
};
document.getElementById('bk-refine').onclick = async () => {
  const instruction = window.prompt(`What should change? (${state.sel.size} room(s) selected stay exactly as they are)`, 'Add a barracks next to the entrance');
  if (instruction === null || !instruction.trim()) return;
  statusEl.textContent = 'Asking the AI to revise this floor…';
  try {
    const { spec } = await api('POST', `/api/docs/${state.docId}/refine`, { floor: state.floorId, pins: [...state.sel], instruction: instruction.trim() });
    document.querySelector('input[name=spec-mode][value=battle]').checked = true;
    document.getElementById('spec-text').value = JSON.stringify(spec, null, 2);
    openSpecDialog();
    await runSpec(Math.floor(Math.random() * 1e6));
    statusEl.textContent = 'Revision ready to preview. Accepting adds it as a new floor; this floor is untouched.';
  } catch (e) { statusEl.textContent = e.message; }
};
document.getElementById('bk-furnish').onclick = async () => {
  try {
    const res = await api('POST', `/api/docs/${state.docId}/furnish`, { floor: state.floorId, spaces: [...state.sel], seed: Math.floor(Math.random() * 4294967295) });
    state.rev = res.rev; tileCache.clear(); await refresh();
    statusEl.textContent = 'Furnished the selected rooms. Ctrl+Z takes it back in one step.';
  } catch (e) { statusEl.textContent = e.message; }
};
document.getElementById('bk-delete').onclick = () => {
  const n = state.sel.size;
  if (!window.confirm(`Delete ${n} room${n === 1 ? '' : 's'}? Ctrl+Z brings them back.`)) return;
  postOps([{ t: 'batch', ops: [...state.sel].map(id => ({ t: 'remove_space', id })) }]);
  state.sel = new Set();
};

function focusKey(key) { focusRow(visibleRows().find(r => r.dataset.key === key)); }

function goToRoom(s) {
  if (!s.bbox) return;
  centerOn({ x: (s.bbox[0] + s.bbox[2]) / 2 - 0.5, y: (s.bbox[1] + s.bbox[3]) / 2 - 0.5 });
}

function renameRoom(s) {
  const name = window.prompt('Rename room', s.name);
  if (name === null || !name.trim() || name.trim() === s.name) return;
  postOps([{ t: 'upsert_space', space: { id: s.id, name: name.trim().slice(0, 60), color: s.color, kind: s.kind, ...(s.material ? { material: s.material } : {}) } }]);
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
  document.getElementById('exp-pdf').href = `${base}/pdf?${q}`;
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
document.getElementById('tool-brush').onclick = () => setTool('brush');
document.getElementById('tool-erase').onclick = () => setTool('erase');
document.getElementById('tool-stamp').onclick = () => setTool('stamp');
document.getElementById('tool-door').onclick = () => setTool('door');
document.getElementById('tool-window').onclick = () => setTool('window');
// Every other tool button (terrain, river, marker, region, route, …) is wired the
// same way, by its id (`tool-raise` -> `raise`), so a new button cannot be left dead.
for (const btn of document.querySelectorAll('button.tool')) {
  if (!btn.onclick) btn.onclick = () => setTool(btn.id.slice('tool-'.length));
}
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
    const active = item.color === state.color && (item.id || null) === (state.materialId || null);
    sw.className = 'swatch' + (active ? ' active' : '');
    sw.style.background = item.color;
    sw.title = item.name;
    sw.onclick = () => {
      state.color = item.color;
      state.materialId = item.id || null;
      buildPalette();
      // Picking a material keeps a paint-like tool (cell paint, brush, eraser);
      // from any other tool it starts cell painting.
      setTool(['paint', 'brush', 'erase'].includes(state.tool) ? state.tool : 'paint');
    };
    wrap.appendChild(sw);
  }
}

window.addEventListener('resize', resizeCanvas);

// ---------- boot ----------

(async function boot() {
  try {
    const { materials } = await api('GET', '/api/materials');
    if (materials.length) {
      PALETTE = materials.map(m => ({ id: m.id, color: m.color, name: m.name }));
      state.color = PALETTE[0].color;
      state.materialId = PALETTE[0].id;
    }
  } catch { /* flat palette */ }
  buildPalette();
  loadStamps();
  resizeCanvas();
  try {
    state.docId = await ensureDoc();
    await refresh();
    await loadDocList();
    setTool('paint');
  } catch (e) {
    statusEl.textContent = `Cannot reach the server: ${e.message}`;
  }
})();
