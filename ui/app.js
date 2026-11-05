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

document.getElementById('btn-new').onclick = async () => {
  const name = window.prompt('Name the new project', 'Untitled Battle Map');
  if (name === null || !name.trim()) return;
  const m = await api('POST', '/api/docs', { name: name.trim(), kind: 'battle' });
  await openDoc(m.id);
};

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
  drawLiveStroke();
  Promise.allSettled(pending).then(() => { drawStrokePreview(); drawLiveStroke(); });
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
  for (const t of ['pan', 'paint', 'brush', 'erase', 'stamp', 'door', 'window']) {
    document.getElementById(`tool-${t}`).classList.toggle('active', tool === t);
  }
  canvas.classList.toggle('painting', tool !== 'pan');
  document.getElementById('brushbar').hidden = !(tool === 'brush' || tool === 'erase');
  document.getElementById('stampbar').hidden = tool !== 'stamp';
  statusEl.textContent =
    tool === 'paint' ? `Painting ${activeItem().name}`
    : tool === 'brush' ? `Brush: ${activeItem().name} — drag to paint (pressure and tilt count)`
    : tool === 'erase' ? 'Eraser: drag to reveal the room layer beneath'
    : tool === 'stamp' ? `Stamp: click to place a ${state.stamp?.name || 'prop'} (, . rotate · F flip)`
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

function queuePaint(cellX, cellY) {
  if (!state.painting) state.painting = new Set();
  state.painting.add(`${cellX},${cellY}`);
  drawStrokePreview();
  clearTimeout(state.pendingFlush);
  state.pendingFlush = setTimeout(flushStroke, 120);
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
    b.title = s.name;
    b.className = state.stamp?.id === s.id ? 'active' : '';
    const img = document.createElement('img');
    img.src = `/api/stamps/${s.id}/icon.png`;
    img.alt = s.name;
    b.appendChild(img);
    b.onclick = () => { state.stamp = s; loadStampsSelection(); setTool('stamp'); };
    wrap.appendChild(b);
  }
  if (!state.stamp && STAMPS.length) state.stamp = STAMPS[0];
}

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

// ---------- input ----------

let dragging = null; // {sx, sy, px, py, moved}

canvas.addEventListener('pointerdown', (e) => {
  try { canvas.setPointerCapture(e.pointerId); } catch { /* an inactive pointer id: keep going */ }
  const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
  const wantPaint = state.tool === 'paint' && e.button === 0;
  if ((state.tool === 'brush' || state.tool === 'erase') && e.button === 0) {
    startStroke(wx, wy, e);
    dragging = { stroking: true };
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
  if (!dragging) return;
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
  { key: 'r', label: 'Brush tool', run: () => setTool('brush') },
  { key: 'e', label: 'Eraser tool', run: () => setTool('erase') },
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
const treeOpen = { rooms: true, lights: true, labels: true, stamps: true };
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
    ['stamps', 'Props', (o.stamps || []).length],
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
