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

function setTool(tool) {
  state.tool = tool;
  document.getElementById('tool-pan').classList.toggle('active', tool === 'pan');
  document.getElementById('tool-paint').classList.toggle('active', tool === 'paint');
  canvas.classList.toggle('painting', tool === 'paint');
  statusEl.textContent = tool === 'paint'
    ? `Painting ${PALETTE.find(p => p.color === state.color)?.name || state.color}`
    : 'Pan (drag) · wheel zooms';
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
      { t: 'upsert_space', space: { id: spaceId, name: spaceId, color: state.color, kind: 'room' } },
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
  const [wx, wy] = screenToWorld(e.offsetX, e.offsetY);
  state.z = nz;
  state.px = e.offsetX - wx * 2 ** nz;
  state.py = e.offsetY - wy * 2 ** nz;
  render();
}, { passive: false });

window.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') {
    e.preventDefault();
    undo();
  } else if (e.key === 'b') setTool('paint');
  else if (e.key === 'v' || e.key === 'Escape') setTool('pan');
});

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
document.getElementById('btn-undo').onclick = undo;

function buildPalette() {
  const wrap = document.getElementById('palette');
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
