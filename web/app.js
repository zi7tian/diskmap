'use strict';
/* DiskMap 前端：嵌套方框树图（squarified treemap）+ 重要程度着色 */

const TOKEN = window.DISKMAP_TOKEN;
const $ = (id) => document.getElementById(id);
const CLASS_KEYS = ['core', 'running', 'system', 'config', 'user', 'cache', 'other'];
const RUNNING = 1;
const HDR = 17;            // 文件夹标题栏高度
const MIN_PX_AREA = 24;    // 小于该像素面积的项目由后端合并

const S = {
  meta: null, status: null,
  root: null, viewPath: null, tree: null, totals: null,
  drawList: [], hover: null, selected: null, selectedPath: null,
  hl: null, pinned: null, flash: null,
  W: 0, H: 0, dpr: 1, mouse: null,
  fetchSeq: 0, lastLive: 0, liveBusy: false, pollTimer: 0, wasScanning: false,
};

const map = $('map'), overlay = $('overlay');
const ctx = map.getContext('2d', { alpha: false });
const octx = overlay.getContext('2d');

/* ================================================================ 工具 */
async function api(path, body) {
  const opts = { headers: { 'X-DiskMap-Token': TOKEN } };
  if (body !== undefined) {
    opts.method = 'POST';
    opts.headers['Content-Type'] = 'application/json';
    opts.body = JSON.stringify(body);
  }
  const r = await fetch(path, opts);
  const data = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(data.error || `请求失败（${r.status}）`);
  return data;
}

function fmtBytes(n) {
  if (n == null) return '—';
  if (n < 1024) return `${n} B`;
  const u = ['KB', 'MB', 'GB', 'TB', 'PB'];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return `${n >= 100 ? n.toFixed(0) : n >= 10 ? n.toFixed(1) : n.toFixed(2)} ${u[i]}`;
}
const fmtInt = (n) => (n || 0).toLocaleString('zh-CN');
function pct(a, b) {
  if (!b) return '0%';
  const p = (a / b) * 100;
  if (p > 0 && p < 0.1) return '<0.1%';
  return `${p >= 10 ? p.toFixed(0) : p.toFixed(1)}%`;
}
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
const clsVar = (id) => `var(--c-${CLASS_KEYS[id]})`;

/* ---- 路径：Linux/macOS 用 /，Windows 用 \ 且根目录形如 C:\ ---- */
let SEP = '/';
const isRootPath = (p) => p === SEP || /^[A-Za-z]:\\$/.test(p);
const joinPath = (p, n) => (p.endsWith(SEP) ? p + n : p + SEP + n);
function parentPath(p) {
  if (isRootPath(p)) return p;
  const i = p.lastIndexOf(SEP);
  if (i < 0) return p;
  const par = p.slice(0, i);
  if (par === '') return SEP;
  if (/^[A-Za-z]:$/.test(par)) return par + SEP;
  return par;
}
/* anc 的直接子路径中，通向 desc 的那一个 */
function childToward(anc, desc) {
  const rel = desc.slice(anc.endsWith(SEP) ? anc.length : anc.length + 1);
  return joinPath(anc, rel.split(SEP)[0]);
}

/* ---- 各系统不同的提示文字 ---- */
const OS_TEXT = {
  linux: {
    elevate: '用 sudo 运行可扫描全部',
    hint: '普通用户无法读取部分系统文件夹，用 <code>sudo</code> 运行可扫描全部内容。',
    system: '系统文件：请通过包管理器卸载对应软件，而不是直接删除。',
  },
  macos: {
    elevate: '在「隐私与安全性 → 完全磁盘访问权限」中允许 DiskMap 可扫描更多',
    hint: '部分文件夹受 macOS 隐私保护。在「系统设置 → 隐私与安全性 → 完全磁盘访问权限」中添加 DiskMap 后可以扫描更多内容。',
    system: '系统或应用程序文件：卸载应用请把它从「应用程序」拖到废纸篓；系统文件不要改动。',
  },
  windows: {
    elevate: '右键「以管理员身份运行」可扫描全部',
    hint: '部分系统文件夹需要管理员权限才能读取。右键 DiskMap 选择「以管理员身份运行」可扫描全部内容。',
    system: '系统或程序文件：请在「设置 → 应用 → 已安装的应用」中卸载对应程序，而不是直接删除。',
  },
};
const osText = () => OS_TEXT[(S.meta && S.meta.os) || 'linux'] || OS_TEXT.linux;

let toastTimer = 0;
function toast(msg, ms = 2600, agent = false) {
  const t = $('toast');
  t.textContent = msg;
  t.classList.toggle('agent', agent);
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, ms);
}

/* ---- 把"用户正在看什么"告诉后台，供 AI 助手读取 ----
   立即发送（不用定时器防抖：后台窗口里的定时器可能被浏览器大幅推迟），并保证按顺序到达 */
let uiStateChain = Promise.resolve();
function reportUiState() {
  const sel = S.selected && S.selected.t !== 'a' ? S.selected.path : null;
  const body = { view: S.viewPath, selected: sel, pinned: S.pinned != null ? CLASS_KEYS[S.pinned] : null };
  uiStateChain = uiStateChain.then(() => api('/api/ui-state', body)).catch(() => {});
}

/* ================================================================ 配色 */
function hexToRgb(hex) {
  hex = hex.trim().replace('#', '');
  if (hex.length === 3) hex = hex.split('').map((c) => c + c).join('');
  const n = parseInt(hex, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}
function rgbToHsl([r, g, b]) {
  r /= 255; g /= 255; b /= 255;
  const max = Math.max(r, g, b), min = Math.min(r, g, b);
  let h = 0, s = 0;
  const l = (max + min) / 2;
  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
    if (max === r) h = (g - b) / d + (g < b ? 6 : 0);
    else if (max === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60;
  }
  return [h, s, l];
}
function hslToRgb(h, s, l) {
  s = Math.max(0, Math.min(1, s)); l = Math.max(0, Math.min(1, l));
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  return [Math.round(f(0) * 255), Math.round(f(8) * 255), Math.round(f(4) * 255)];
}
const rgbCss = ([r, g, b]) => `rgb(${r},${g},${b})`;
function luminance([r, g, b]) {
  const c = [r, g, b].map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
}
const contrast = (a, b) => (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);

let PAL = null;
function buildPalette() {
  const cs = getComputedStyle(document.documentElement);
  const bg = hexToRgb(cs.getPropertyValue('--bg'));
  const dark = luminance(bg) < 0.2;
  const font = cs.getPropertyValue('--font').trim() || 'sans-serif';
  const cls = CLASS_KEYS.map((key) => {
    const base = cs.getPropertyValue(`--c-${key}`).trim();
    const [h, s, l] = rgbToHsl(hexToRgb(base));
    const file = [], fileText = [];
    for (let i = 0; i < 7; i++) {
      const rgb = hslToRgb(h, s, l + (i - 3) * 0.035);
      file.push(rgbCss(rgb));
      const L = luminance(rgb);
      fileText.push(contrast(L, 0) > contrast(L, 1) ? 'rgba(12,14,18,.92)' : 'rgba(255,255,255,.96)');
    }
    return {
      base, file, fileText,
      dirBg: rgbCss(dark ? hslToRgb(h, s * 0.28, 0.115) : hslToRgb(h, s * 0.35, 0.955)),
      dirBg2: rgbCss(dark ? hslToRgb(h, s * 0.28, 0.15) : hslToRgb(h, s * 0.35, 0.915)),
      dirHead: rgbCss(dark ? hslToRgb(h, s * 0.42, 0.235) : hslToRgb(h, s * 0.5, 0.85)),
      dirBorder: rgbCss(dark ? hslToRgb(h, s * 0.45, 0.33) : hslToRgb(h, s * 0.45, 0.68)),
      dirText: rgbCss(dark ? hslToRgb(h, s * 0.3, 0.9) : hslToRgb(h, s * 0.6, 0.2)),
      dirSub: dark ? 'rgba(255,255,255,.55)' : 'rgba(0,0,0,.5)',
      hatchLine: rgbCss(hslToRgb(h, s * 0.8, dark ? 0.42 : 0.62)),
      pat: null,
    };
  });
  PAL = {
    dark, cls, font,
    mapBg: cs.getPropertyValue('--map-bg').trim(),
    sel: dark ? '#ffffff' : '#0b0d10',
    selHalo: dark ? 'rgba(0,0,0,.75)' : 'rgba(255,255,255,.85)',
    hover: dark ? 'rgba(255,255,255,.75)' : 'rgba(0,0,0,.65)',
    hoverParent: dark ? 'rgba(255,255,255,.28)' : 'rgba(0,0,0,.28)',
  };
}

function hatchPattern(P) {
  if (P.pat) return P.pat;
  const d = S.dpr, sz = Math.round(6 * d);
  const c = document.createElement('canvas');
  c.width = c.height = sz;
  const g = c.getContext('2d');
  g.fillStyle = P.dirBg2;
  g.fillRect(0, 0, sz, sz);
  g.strokeStyle = P.hatchLine;
  g.lineWidth = 1.3 * d;
  g.beginPath();
  g.moveTo(-1, sz + 1); g.lineTo(sz + 1, -1);
  g.moveTo(-1, 1); g.lineTo(1, -1);
  g.moveTo(sz - 1, sz + 1); g.lineTo(sz + 1, sz - 1);
  g.stroke();
  const pat = ctx.createPattern(c, 'repeat');
  if (pat.setTransform) pat.setTransform(new DOMMatrix().scale(1 / d));
  P.pat = pat;
  return pat;
}

function extVariant(name) {
  const i = name.lastIndexOf('.');
  if (i <= 0) return 3;
  let h = 0;
  for (let k = i + 1; k < name.length; k++) h = (h * 31 + name.charCodeAt(k)) | 0;
  return Math.abs(h) % 7;
}

/* ================================================================ 布局 */
function squarify(items, x, y, w, h, place) {
  let total = 0;
  for (const it of items) total += it.s;
  if (total <= 0 || w <= 0 || h <= 0) return;
  const scale = (w * h) / total;
  let i = 0;
  while (i < items.length) {
    const short = Math.min(w, h);
    const sh2 = short * short;
    let j = i, rowSum = 0, worst = Infinity;
    const maxA = items[i].s * scale;
    while (j < items.length) {
      const a = items[j].s * scale;
      const ns = rowSum + a;
      const s2 = ns * ns;
      const r = Math.max((sh2 * maxA) / s2, s2 / (sh2 * a));
      if (j > i && r > worst) break;
      rowSum = ns; worst = r; j++;
    }
    if (j === items.length) {          // 最后一行：铺满剩余空间，避免浮点误差留缝
      rowSum = 0;
      for (let k = i; k < j; k++) rowSum += items[k].s * scale;
    }
    if (w >= h) {
      const cw = Math.min(w, rowSum / h);
      let yy = y;
      for (let k = i; k < j; k++) {
        const nh = (items[k].s * scale) / cw;
        place(items[k], x, yy, cw, k === j - 1 ? y + h - yy : nh);
        yy += nh;
      }
      x += cw; w -= cw;
    } else {
      const rh = Math.min(h, rowSum / w);
      let xx = x;
      for (let k = i; k < j; k++) {
        const nw = (items[k].s * scale) / rh;
        place(items[k], xx, y, k === j - 1 ? x + w - xx : nw, rh);
        xx += nw;
      }
      y += rh; h -= rh;
    }
    i = j;
  }
}

function prep(node, parent, path) {
  node.parent = parent;
  node.path = path;
  if (node.ch) {
    for (const c of node.ch) prep(c, node, c.t === 'a' ? `${path}${SEP}\u0000${c.k}` : joinPath(path, c.n));
  }
}

function layout() {
  S.drawList = [];
  if (S.tree && S.W > 0 && S.H > 0) place(S.tree, 0, 0, S.W, S.H, 0);
}

function place(n, x, y, w, h, depth) {
  n.x = x; n.y = y; n.w = w; n.h = h; n.depth = depth; n.hdr = 0; n.kids = null;
  S.drawList.push(n);
  if (n.t !== 'd' || !n.ch || w < 7 || h < 7) return;
  const hdr = w >= 40 && h >= 36 ? HDR : 0;
  const pad = w >= 16 && h >= 16 ? 2 : 1;
  const top = hdr || pad + 1;
  const ix = x + pad + 1, iy = y + top, iw = w - 2 * pad - 2, ih = h - top - pad - 1;
  if (iw < 2 || ih < 2) return;
  const kids = n.ch.filter((c) => c.s > 0);
  if (!kids.length) return;
  n.hdr = hdr;
  n.kids = kids;
  for (const c of kids) { c.w = 0; c.h = 0; }
  squarify(kids, ix, iy, iw, ih, (c, cx, cy, cw, chh) => {
    if (cw >= 0.75 && chh >= 0.75) place(c, cx, cy, cw, chh, depth + 1);
  });
}

/* ================================================================ 绘制 */
function snap(n) {
  const x0 = Math.round(n.x), y0 = Math.round(n.y);
  return [x0, y0, Math.max(1, Math.round(n.x + n.w) - x0), Math.max(1, Math.round(n.y + n.h) - y0)];
}

const textW = new Map();
function measure(s, font) {
  const key = font + s;
  let w = textW.get(key);
  if (w === undefined) {
    ctx.font = font;
    w = ctx.measureText(s).width;
    if (textW.size > 20000) textW.clear();
    textW.set(key, w);
  }
  return w;
}
function fit(s, font, maxW) {
  if (maxW <= 6) return '';
  if (measure(s, font) <= maxW) return s;
  let lo = 0, hi = s.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (measure(s.slice(0, mid) + '…', font) <= maxW) lo = mid; else hi = mid - 1;
  }
  return lo ? s.slice(0, lo) + '…' : '';
}

function activeHL() { return S.pinned != null ? S.pinned : S.hl; }
function matchesHL(n, hl) {
  if (hl == null) return true;
  if (n.t === 'd') return n.c === hl || (hl === RUNNING && n.r > 0);
  return n.c === hl;
}

function render() {
  const { W, H, dpr } = S;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.globalAlpha = 1;
  ctx.fillStyle = PAL.mapBg;
  ctx.fillRect(0, 0, W, H);
  const hl = activeHL();
  const fName = `600 11.5px ${PAL.font}`;
  const fSize = `400 11px ${PAL.font}`;
  const fFile = `500 11.5px ${PAL.font}`;
  ctx.textBaseline = 'middle';

  for (const n of S.drawList) {
    const P = PAL.cls[n.c] || PAL.cls[6];
    const [x, y, w, h] = snap(n);
    const on = matchesHL(n, hl);
    if (n.t === 'd') {
      ctx.globalAlpha = on ? 1 : 0.4;
      ctx.fillStyle = P.dirBorder;
      ctx.fillRect(x, y, w, h);
      if (w > 2 && h > 2) {
        ctx.fillStyle = n.depth % 2 ? P.dirBg2 : P.dirBg;
        ctx.fillRect(x + 1, y + 1, w - 2, h - 2);
      }
      if (n.hdr) {
        ctx.fillStyle = P.dirHead;
        ctx.fillRect(x + 1, y + 1, w - 2, n.hdr - 1);
        let tx = x + 6;
        const cy = y + 1 + (n.hdr - 1) / 2;
        if (n.r) {
          ctx.fillStyle = PAL.cls[RUNNING].base;
          ctx.beginPath();
          ctx.arc(tx + 3, cy, 3.2, 0, Math.PI * 2);
          ctx.fill();
          tx += 11;
        }
        const avail = x + w - 6 - tx;
        const size = fmtBytes(n.s);
        const sw = measure(size, fSize);
        const name = n.depth === 0 && n.path === S.root ? n.path : n.n;
        const label = fit(name, fName, avail - (avail > sw + 40 ? sw + 8 : 0));
        ctx.fillStyle = P.dirText;
        ctx.font = fName;
        ctx.fillText(label, tx, cy);
        const lw = measure(label, fName);
        if (tx + lw + 8 + sw <= x + w - 6) {
          ctx.fillStyle = P.dirSub;
          ctx.font = fSize;
          ctx.fillText(size, tx + lw + 8, cy);
        }
      }
    } else if (n.t === 'f') {
      ctx.globalAlpha = on ? 1 : 0.16;
      const v = extVariant(n.n);
      ctx.fillStyle = P.file[v];
      ctx.fillRect(x, y, w > 2 ? w - 1 : w, h > 2 ? h - 1 : h);
      if (w >= 44 && h >= 15 && on) {
        ctx.fillStyle = P.fileText[v];
        const twoLines = h >= 32;
        const cy = twoLines ? y + h / 2 - 7 : y + h / 2;
        ctx.font = fFile;
        ctx.fillText(fit(n.n, fFile, w - 9), x + 4, cy);
        if (twoLines) {
          ctx.font = fSize;
          ctx.globalAlpha = 0.8;
          ctx.fillText(fit(fmtBytes(n.s), fSize, w - 9), x + 4, cy + 14);
          ctx.globalAlpha = 1;
        }
      }
    } else { // 合并的小项目
      ctx.globalAlpha = on ? 1 : 0.3;
      ctx.fillStyle = hatchPattern(P);
      ctx.fillRect(x, y, w > 2 ? w - 1 : w, h > 2 ? h - 1 : h);
      if (w >= 70 && h >= 16 && on) {
        ctx.fillStyle = P.dirText;
        ctx.font = fSize;
        ctx.fillText(fit(`${fmtInt(n.k)} 个小项目`, fSize, w - 9), x + 4, y + Math.min(h / 2, 10));
      }
    }
  }
  ctx.globalAlpha = 1;
  drawOverlay();
}

function strokeNode(n, color, width, halo) {
  if (!n || !(n.w > 0)) return;
  const [x, y, w, h] = snap(n);
  if (halo) {
    octx.strokeStyle = halo;
    octx.lineWidth = width + 2;
    octx.strokeRect(x + width / 2, y + width / 2, w - width, h - width);
  }
  octx.strokeStyle = color;
  octx.lineWidth = width;
  octx.strokeRect(x + width / 2, y + width / 2, w - width, h - width);
}

function drawOverlay() {
  const { W, H, dpr } = S;
  octx.setTransform(dpr, 0, 0, dpr, 0, 0);
  octx.clearRect(0, 0, W, H);
  const hv = S.hover;
  if (hv && hv.parent && hv.t !== 'd') strokeNode(hv.parent, PAL.hoverParent, 1);
  if (S.selected) strokeNode(S.selected, PAL.sel, 2, PAL.selHalo);
  if (hv && hv !== S.selected) strokeNode(hv, PAL.hover, 1.5);
  if (S.flash) {
    const t = (performance.now() - S.flash.t0) / 900;
    if (t < 1 && S.flash.node.w > 0) {
      const [x, y, w, h] = snap(S.flash.node);
      const grow = (1 - t) * 10;
      octx.strokeStyle = PAL.sel;
      octx.globalAlpha = 1 - t;
      octx.lineWidth = 2;
      octx.strokeRect(x - grow, y - grow, w + grow * 2, h + grow * 2);
      octx.globalAlpha = 1;
      requestAnimationFrame(drawOverlay);
    } else {
      S.flash = null;
    }
  }
}

function hit(x, y) {
  let n = S.tree;
  const inside = (m) => m.w > 0 && x >= m.x && x < m.x + m.w && y >= m.y && y < m.y + m.h;
  if (!n || !inside(n)) return null;
  outer: while (n.kids) {
    for (const c of n.kids) {
      if (inside(c)) { n = c; continue outer; }
    }
    break;
  }
  return n;
}

function findByPath(path) {
  if (!path) return null;
  for (const n of S.drawList) if (n.path === path) return n;
  return null;
}

/* ================================================================ 尺寸 */
function resize() {
  const r = $('mapwrap').getBoundingClientRect();
  S.dpr = window.devicePixelRatio || 1;
  S.W = Math.max(1, Math.floor(r.width));
  S.H = Math.max(1, Math.floor(r.height));
  for (const c of [map, overlay]) {
    c.width = Math.round(S.W * S.dpr);
    c.height = Math.round(S.H * S.dpr);
  }
  for (const P of PAL.cls) P.pat = null;
  layout();
  render();
}

/* ================================================================ 视图切换 */
function zoomInAnim(n) {
  if (!n || !(n.w > 0)) return Promise.resolve();
  const snapC = document.createElement('canvas');
  snapC.width = map.width; snapC.height = map.height;
  snapC.getContext('2d').drawImage(map, 0, 0);
  octx.setTransform(1, 0, 0, 1, 0, 0);
  octx.clearRect(0, 0, overlay.width, overlay.height);
  const { W, H, dpr } = S;
  const t0 = performance.now(), D = 240;
  return new Promise((res) => {
    const frame = (now) => {
      const t = Math.min(1, (now - t0) / D);
      const e = 1 - (1 - t) ** 3;
      const sx = n.x * e, sy = n.y * e, sw = W + (n.w - W) * e, sh = H + (n.h - H) * e;
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.drawImage(snapC, sx * dpr, sy * dpr, sw * dpr, sh * dpr, 0, 0, map.width, map.height);
      if (t < 1) requestAnimationFrame(frame); else res();
    };
    requestAnimationFrame(frame);
  });
}

function zoomOutAnim(oldSnap, n) {
  if (!oldSnap || !n || !(n.w > 0)) return;
  const newSnap = document.createElement('canvas');
  newSnap.width = map.width; newSnap.height = map.height;
  newSnap.getContext('2d').drawImage(map, 0, 0);
  const { W, H, dpr } = S;
  const t0 = performance.now(), D = 240;
  const frame = (now) => {
    const t = Math.min(1, (now - t0) / D);
    const e = 1 - (1 - t) ** 3;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalAlpha = 1;
    ctx.drawImage(newSnap, 0, 0);
    const x = n.x * e, y = n.y * e, w = W + (n.w - W) * e, h = H + (n.h - H) * e;
    ctx.globalAlpha = 1 - e * 0.85;
    ctx.drawImage(oldSnap, x * dpr, y * dpr, w * dpr, h * dpr);
    ctx.globalAlpha = 1;
    if (t < 1) requestAnimationFrame(frame); else render();
  };
  requestAnimationFrame(frame);
}

let busyTimer = 0;
async function loadView(path, opts = {}) {
  const seq = ++S.fetchSeq;
  const frac = Math.min(0.01, Math.max(1e-6, MIN_PX_AREA / (S.W * S.H)));
  const req = api(`/api/tree?path=${encodeURIComponent(path)}&frac=${frac}`);
  if (!opts.live) {
    clearTimeout(busyTimer);
    busyTimer = setTimeout(() => { $('busy').textContent = '正在加载…'; $('busy').hidden = false; }, 350);
  }
  const anim = opts.zoomFrom ? zoomInAnim(opts.zoomFrom) : null;
  let data;
  try {
    data = await req;
  } catch (e) {
    if (!opts.live) toast(e.message);
    if (anim) { await anim; render(); }
    return false;
  } finally {
    if (!opts.live) { clearTimeout(busyTimer); $('busy').hidden = true; }
  }
  if (anim) await anim;
  if (seq !== S.fetchSeq) return false;

  let oldSnap = null;
  if (opts.zoomOut) {
    oldSnap = document.createElement('canvas');
    oldSnap.width = map.width; oldSnap.height = map.height;
    oldSnap.getContext('2d').drawImage(map, 0, 0);
  }
  applyTree(data);
  if (opts.zoomOut) zoomOutAnim(oldSnap, findByPath(opts.zoomOut));
  return true;
}

function applyTree(data) {
  const viewChanged = data.path !== S.viewPath;
  S.root = data.root;
  S.viewPath = data.path;
  S.totals = data.totals;
  prep(data.tree, null, data.path);
  S.tree = data.tree;
  layout();
  S.selected = findByPath(S.selectedPath) || (S.selected && S.selected.path === S.selectedPath ? S.selected : null);
  S.hover = S.mouse ? hit(S.mouse.x, S.mouse.y) : null;
  render();
  renderCrumbs();
  renderLegend();
  renderTop();
  if (S.tooltipShown && S.hover) showTooltip(S.hover, S.mouse.cx, S.mouse.cy);
  if (viewChanged) {
    const u = new URL(location.href);
    u.searchParams.set('view', data.path);
    history.replaceState(null, '', u);
    reportUiState();
  }
}

function enter(n) {
  if (!n) return;
  if (n.t !== 'd') n = n.parent;
  if (!n || n === S.tree) return;
  loadView(n.path, { zoomFrom: n });
}

function goUp() {
  if (!S.viewPath || !S.root || S.viewPath === S.root) return;
  const from = S.viewPath;
  loadView(parentPath(from), { zoomOut: from });
}

/* ================================================================ 面板 */
function renderCrumbs() {
  const el = $('crumbs');
  el.innerHTML = '';
  if (!S.root || !S.viewPath) return;
  const parts = [{ label: S.root, path: S.root }];
  if (S.viewPath !== S.root) {
    const rel = S.viewPath.slice(S.root.endsWith(SEP) ? S.root.length : S.root.length + 1).split(SEP);
    let p = S.root;
    for (const r of rel) { p = joinPath(p, r); parts.push({ label: r, path: p }); }
  }
  parts.forEach((pt, i) => {
    if (i) {
      const s = document.createElement('span');
      s.className = 'sep';
      s.textContent = '›';
      el.appendChild(s);
    }
    const b = document.createElement('button');
    b.textContent = pt.label;
    b.title = pt.path;
    b.onclick = () => { if (pt.path !== S.viewPath) loadView(pt.path, { zoomOut: childToward(pt.path, S.viewPath) }); };
    el.appendChild(b);
  });
  el.scrollLeft = el.scrollWidth;
  $('upBtn').disabled = S.viewPath === S.root;
  const t = S.tree;
  $('viewSize').innerHTML = t
    ? `<b>${fmtBytes(t.s)}</b> · ${fmtInt(t.nf)} 个文件 · ${fmtInt(t.nd)} 个文件夹`
    : '';
}

function renderLegend() {
  const ul = $('legend');
  const classes = S.meta ? S.meta.classes : [];
  const tot = S.totals || classes.map(() => 0);
  const sum = tot.reduce((a, b) => a + Math.max(0, b), 0);
  const hl = activeHL();
  ul.innerHTML = classes.map((c) => {
    const v = Math.max(0, tot[c.id] || 0);
    const cls = [S.pinned === c.id ? 'active' : '', hl != null && hl !== c.id ? 'dimmed' : ''].join(' ');
    return `<li data-c="${c.id}" class="${cls}" title="${esc(c.desc)}">
      <span class="sw" style="background:${clsVar(c.id)}"></span>
      <span class="lb">${esc(c.label)}</span>
      <span class="sz">${fmtBytes(v)} · ${pct(v, sum)}</span>
      <span class="meter"><i style="width:${sum ? (v / sum) * 100 : 0}%;background:${clsVar(c.id)}"></i></span>
    </li>`;
  }).join('');
}

function renderTop() {
  const files = [];
  const stack = S.tree ? [S.tree] : [];
  while (stack.length) {
    const n = stack.pop();
    if (n.t === 'f') files.push(n);
    else if (n.ch) for (const c of n.ch) stack.push(c);
  }
  files.sort((a, b) => b.s - a.s);
  const top = files.slice(0, 10);
  const ol = $('topFiles');
  if (!top.length) {
    ol.innerHTML = '<li class="empty">暂无</li>';
    return;
  }
  ol.innerHTML = top.map((f, i) => `<li data-i="${i}" title="${esc(f.path)}">
      <span class="dot" style="background:${clsVar(f.c)}"></span>
      <span class="nm">${esc(f.n)}</span><span class="sz">${fmtBytes(f.s)}</span></li>`).join('');
  ol.querySelectorAll('li').forEach((li) => {
    const f = top[+li.dataset.i];
    li.onclick = () => { select(f); S.flash = { node: f, t0: performance.now() }; drawOverlay(); };
  });
}

function clsLabel(id) { return S.meta ? S.meta.classes[id].label : ''; }

const WARN = {
  0: '系统核心文件：删除或修改可能导致系统无法启动。',
  1: '正被程序使用：如需删除，请先关闭相关程序。',
  2: null,   // 见 OS_TEXT.system
  3: '配置文件：删除会丢失相应程序的设置。',
  5: '缓存/临时文件：一般可以清理，程序会按需重新生成。',
};
const warnText = (c) => (c === 2 ? osText().system : WARN[c] || '');

function uninstallHint(pkg) {
  const m = S.status && S.status.pkg && S.status.pkg.manager;
  if (!pkg || !['pacman', 'dpkg', 'rpm'].includes(m)) return '';
  const cmd = m === 'pacman' ? `sudo pacman -Rs ${pkg}` : m === 'dpkg' ? `sudo apt remove ${pkg}` : `sudo dnf remove ${pkg}`;
  return `<br>卸载命令：<code>${esc(cmd)}</code>`;
}

async function select(n) {
  S.selected = n;
  S.selectedPath = n ? n.path : null;
  drawOverlay();
  reportUiState();
  const box = $('detail');
  if (!n) {
    box.innerHTML = '<p class="muted">点击方框查看详情，双击进入文件夹。</p>';
    return;
  }
  if (n.t === 'a') {
    box.innerHTML = `<div class="d-name">${fmtInt(n.k)} 个较小的项目</div>
      <div class="d-path">${esc(n.parent ? n.parent.path : '')}</div>
      <dl><dt>合计大小</dt><dd>${fmtBytes(n.s)}</dd><dt>主要类别</dt><dd>${esc(clsLabel(n.c))}</dd></dl>
      <p class="why">这些项目太小，在当前缩放下合并成一个方框显示。进入所在文件夹即可看到它们。</p>
      ${n.parent && n.parent !== S.tree ? '<div class="actions"><button class="btn small" id="aEnter">进入所在文件夹</button></div>' : ''}`;
    const b = $('aEnter');
    if (b) b.onclick = () => enter(n.parent);
    return;
  }
  const seq = (select.seq = (select.seq || 0) + 1);
  renderDetail(n, null);
  try {
    const info = await api(`/api/info?path=${encodeURIComponent(n.path)}`);
    if (seq === select.seq) renderDetail(n, info);
  } catch (e) { /* 扫描中可能暂时找不到，保留本地信息 */ }
}

function renderDetail(n, info) {
  const c = info ? info.cls : n.c;
  const isDir = n.t === 'd';
  const parentSize = n.parent ? n.parent.s : n.s;
  const rows = [];
  rows.push(['大小', `${fmtBytes(n.s)}${info && info.apparent != null && Math.abs(info.apparent - n.s) > 4096 ? `（文件长度 ${fmtBytes(info.apparent)}）` : ''}`]);
  if (n.parent) rows.push(['占上级', pct(n.s, parentSize)]);
  if (S.tree && n !== S.tree) rows.push(['占当前视图', pct(n.s, S.tree.s)]);
  if (isDir) rows.push(['包含', `${fmtInt(n.nf)} 个文件，${fmtInt(n.nd)} 个文件夹`]);
  if (info && info.mtime) rows.push(['修改时间', info.mtime]);
  if (info && info.mode) rows.push(['权限', info.owner ? `${info.mode} ${esc(info.owner)}:${esc(info.group)}` : info.mode]);
  const pkg = (info && info.pkg) || n.pk;
  if (pkg) rows.push(['软件包', esc(pkg)]);
  if (info && info.mount) rows.push(['分区', `${esc(info.mount.source)}（${esc(info.mount.fstype)}）`]);

  let badges = `<span class="badge" style="color:${clsVar(c)}"><span class="dot" style="background:${clsVar(c)}"></span>${esc(clsLabel(c))}</span>`;
  const sc = info ? info.static_cls : n.sc;
  if (c === RUNNING && sc != null && sc !== RUNNING) {
    badges += ` <span class="badge" style="color:${clsVar(sc)}"><span class="dot" style="background:${clsVar(sc)}"></span>${esc(clsLabel(sc))}</span>`;
  }

  let procs = '';
  if (info && info.procs && info.procs.length) {
    procs = `<div class="procs">${info.procs.map((p) => `<div><span>${esc(p.comm)} <span class="pk">PID ${p.pid || '—'}</span></span><span class="pk">${esc(p.kind)}</span></div>`).join('')}
      ${info.nprocs > info.procs.length ? `<div class="pk">…共 ${info.nprocs} 个进程</div>` : ''}</div>`;
  } else if (!info && n.p) {
    procs = `<div class="procs"><div>${esc(n.p)}</div></div>`;
  }
  let running = '';
  const rc = info ? info.running : n.r;
  if (isDir && rc) running = `<p class="why" style="color:${clsVar(RUNNING)}">● 其中有 ${fmtInt(rc)} 个文件正被运行中的程序使用</p>`;

  const warnCls = c === RUNNING ? RUNNING : (sc != null ? sc : c);
  let warn = warnText(c) || warnText(warnCls);
  if (warn && (c === 2 || sc === 2) && pkg && !isDir) warn += uninstallHint(pkg);
  if (info && info.denied) warn = `没有权限读取此文件夹的内容，大小未计入。${osText().elevate}。`;
  const warnHtml = warn
    ? `<div class="warn" style="background:color-mix(in srgb, ${clsVar(warnCls)} 14%, transparent);border-left:3px solid ${clsVar(warnCls)}">${warn}</div>`
    : '';

  const canEnter = isDir && n !== S.tree;
  $('detail').innerHTML = `
    <div class="d-name">${esc(n.depth === 0 && n.path === S.root ? n.path : n.n)}</div>
    <div class="d-path">${esc(n.path)}</div>
    <div>${badges}</div>
    ${info && info.reason ? `<p class="why">${esc(info.reason)}</p>` : '<p class="why">&nbsp;</p>'}
    <dl>${rows.map(([k, v]) => `<dt>${k}</dt><dd>${v}</dd>`).join('')}</dl>
    ${running}${procs}${warnHtml}
    ${info && info.gone ? '<p class="why">⚠ 该项目在扫描后已被删除或移动。</p>' : ''}
    <div class="actions">
      ${canEnter ? '<button class="btn small" data-act="enter">进入</button>' : ''}
      <button class="btn small" data-act="reveal">在文件管理器中显示</button>
      <button class="btn small" data-act="copy">复制路径</button>
    </div>`;
  $('detail').querySelectorAll('[data-act]').forEach((b) => {
    b.onclick = () => {
      const act = b.dataset.act;
      if (act === 'enter') enter(n);
      else if (act === 'reveal') api('/api/reveal', { path: n.path }).then(() => toast('已在文件管理器中打开')).catch((e) => toast(e.message));
      else if (act === 'copy') copyText(n.path);
    };
  });
}

function copyText(s, msg = '已复制路径') {
  const done = () => toast(msg);
  if (navigator.clipboard && navigator.clipboard.writeText) {
    navigator.clipboard.writeText(s).then(done).catch(() => fallbackCopy(s, done));
  } else fallbackCopy(s, done);
}
function fallbackCopy(s, done) {
  const ta = document.createElement('textarea');
  ta.value = s;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.select();
  try { document.execCommand('copy'); done(); } catch (e) { toast('复制失败'); }
  ta.remove();
}

/* ================================================================ 提示框 */
function showTooltip(n, cx, cy) {
  const tip = $('tooltip');
  if (!n) { tip.hidden = true; S.tooltipShown = false; return; }
  const parentSize = n.parent ? n.parent.s : n.s;
  const lines = [];
  let name, path;
  if (n.t === 'a') {
    name = `${fmtInt(n.k)} 个较小的项目`;
    path = `位于 ${n.parent ? n.parent.path : ''}`;
  } else {
    name = n.depth === 0 && n.path === S.root ? n.path : n.n;
    path = n.path;
  }
  lines.push(`<div class="t-row"><span class="t-size"><b>${fmtBytes(n.s)}</b></span>
    ${n.parent ? `<span class="k">· 占上级 ${pct(n.s, parentSize)}</span>` : ''}
    ${S.tree && n !== S.tree && n.parent !== S.tree ? `<span class="k">· 占视图 ${pct(n.s, S.tree.s)}</span>` : ''}</div>`);
  let tag = `<span class="tag" style="color:${clsVar(n.c)}"><span class="dot" style="background:${clsVar(n.c)}"></span>${esc(clsLabel(n.c))}</span>`;
  if (n.c === RUNNING && n.sc != null) tag += `<span class="k">· 本身属于${esc(clsLabel(n.sc))}</span>`;
  lines.push(`<div class="t-row">${tag}</div>`);
  if (n.p) lines.push(`<div class="t-row"><span class="k">使用者</span><span>${esc(n.p)}</span></div>`);
  if (n.pk) lines.push(`<div class="t-row"><span class="k">软件包</span><span>${esc(n.pk)}</span></div>`);
  if (n.t === 'd') {
    lines.push(`<div class="t-row"><span class="k">包含</span><span>${fmtInt(n.nf)} 个文件 · ${fmtInt(n.nd)} 个文件夹</span></div>`);
    if (n.r) lines.push(`<div class="t-row" style="color:${clsVar(RUNNING)}">● ${fmtInt(n.r)} 个文件正在被使用</div>`);
    if (n.fl & 2) lines.push('<div class="t-row"><span class="k">独立分区的挂载点</span></div>');
  }
  tip.innerHTML = `<div class="t-name">${esc(name)}</div><div class="t-path">${esc(path)}</div>${lines.join('')}`;
  tip.hidden = false;
  S.tooltipShown = true;
  const wrap = $('mapwrap').getBoundingClientRect();
  const tw = tip.offsetWidth, th = tip.offsetHeight;
  let x = cx - wrap.left + 16, y = cy - wrap.top + 18;
  if (x + tw > wrap.width - 6) x = Math.max(6, cx - wrap.left - tw - 14);
  if (y + th > wrap.height - 6) y = Math.max(6, cy - wrap.top - th - 14);
  tip.style.left = `${x}px`;
  tip.style.top = `${y}px`;
}

/* ================================================================ 状态与扫描 */
function renderStatus() {
  const s = S.status;
  const el = $('status');
  if (!s) return;
  const sep = '<span class="sep">·</span>';
  const bits = [];
  if (!s.ready) bits.push('正在读取软件包数据库…');
  if (s.state === 'scanning') {
    bits.push(`正在扫描 <b>${fmtInt(s.files)}</b> 个文件`, `<b>${fmtBytes(s.bytes)}</b>`, `${s.elapsed.toFixed(1)} 秒`);
    if (s.current) bits.push(`<span style="font-family:var(--mono)">${esc(s.current)}</span>`);
  } else if (s.state === 'done' || s.state === 'cancelled') {
    bits.push(`${s.state === 'done' ? '扫描完成' : '已停止'}：<b>${fmtInt(s.files)}</b> 个文件，<b>${fmtInt(s.dirs)}</b> 个文件夹，用时 ${s.elapsed.toFixed(1)} 秒`);
    if (s.fs_used && s.state === 'done') bits.push(`已统计 <b>${fmtBytes(s.bytes)}</b> / 分区已用 ${fmtBytes(s.fs_used)}`);
    if (s.denied) bits.push(`<span title="${esc((s.denied_samples || []).join('\n'))}">${fmtInt(s.denied)} 个文件夹无权读取${s.admin ? '' : `（${osText().elevate}）`}</span>`);
  } else if (s.state === 'error') {
    bits.push(`扫描出错：${esc(s.error || '')}`);
  } else {
    bits.push('准备就绪');
  }
  if (s.pkg && s.pkg.manager) {
    bits.push(s.pkg.manager === 'windows'
      ? `已登记 ${fmtInt(s.pkg.packages)} 个已安装程序`
      : `${esc(s.pkg.label || s.pkg.manager)}：${fmtInt(s.pkg.packages)} 个软件包`);
  }
  if (s.procs && s.procs.refreshing && !s.procs.total) bits.push('正在读取运行中的程序…');
  else if (s.procs && s.procs.total) bits.push(`运行中 ${fmtInt(s.procs.total)} 个进程，${fmtInt(s.procs.files)} 个文件在使用`);
  el.innerHTML = bits.join(sep);
}

function updateScanUI() {
  const scanning = S.status && S.status.state === 'scanning';
  $('progress').hidden = !scanning;
  $('cancelBtn').hidden = !scanning;
  $('scanBtn').textContent = scanning ? '重新扫描' : (S.status && S.status.state !== 'idle' ? '重新扫描' : '扫描');
}

function schedulePoll(ms) {
  clearTimeout(S.pollTimer);
  S.pollTimer = setTimeout(poll, ms);
}

async function poll() {
  try {
    S.status = await api('/api/status');
  } catch (e) {
    $('status').textContent = '与 DiskMap 后台的连接已断开';
    schedulePoll(3000);
    return;
  }
  renderStatus();
  updateScanUI();
  renderAgentBadge();
  const s = S.status;
  if (s.root && s.state !== 'idle' && document.activeElement !== $('pathInput') && $('pathInput').value !== s.root) {
    $('pathInput').value = s.root;   // 扫描可能由 AI 助手发起
  }
  if (s.root && S.root && s.root !== S.root) {
    // 扫描范围变了（例如 AI 助手发起了新的扫描）：回到新扫描的根目录
    S.viewPath = null; S.root = s.root; S.selected = null; S.selectedPath = null;
    $('pathInput').value = s.root;
    select(null);
  }
  if (s.command && s.command.id > (S.cmdId || 0)) {
    S.cmdId = s.command.id;
    runAgentCommand(s.command);
  }
  if (s.state === 'scanning') {
    $('welcome').hidden = true;
    const now = performance.now();
    if (!S.liveBusy && now - S.lastLive > 1200) {
      S.lastLive = now;
      S.liveBusy = true;
      loadView(S.viewPath || s.root, { live: true }).finally(() => { S.liveBusy = false; });
    }
    S.wasScanning = true;
    schedulePoll(500);
  } else {
    if (S.wasScanning) {
      S.wasScanning = false;
      await loadView(S.viewPath || s.root, { live: true });
      if (S.selected) select(S.selected);
    }
    schedulePoll(s.ready ? 2000 : 500);
  }
}

/* ================================================================ AI 助手 */
async function runAgentCommand(cmd) {
  if (cmd.action !== 'show' || !cmd.path) return;
  let info;
  try {
    info = await api(`/api/info?path=${encodeURIComponent(cmd.path)}`);
  } catch (e) { return; }
  $('welcome').hidden = true;
  const path = info.path;
  const isRoot = path === S.root;
  // 在上级文件夹里显示它，这样能看出它和周围项目的大小对比
  await loadView(isRoot ? path : parentPath(path));
  let n = findByPath(path);
  if ((!n || !(n.w > 4 && n.h > 4)) && info.type === 'dir' && !isRoot) {
    await loadView(path);   // 在上级里太小看不清：直接进入它
    n = S.tree;
  }
  if (n) {
    select(n);
    S.flash = { node: n, t0: performance.now() };
    drawOverlay();
  }
  toast(`AI 助手：${cmd.note || `请看 ${path}`}`, 7000, true);
}

function renderAgentBadge() {
  const ago = S.status && S.status.agent_seen_ago;
  const active = ago != null && ago < 90;
  $('aiDot').hidden = !active;
  $('aiBtn').title = active
    ? `AI 助手 ${Math.round(ago)} 秒前读取了分析结果`
    : '把分析结果提供给 Claude 等 AI 助手';
  const st = $('aiStatus');
  if (st) {
    st.textContent = ago == null ? '还没有 AI 助手连接过。'
      : `最近一次 AI 助手访问：${ago < 60 ? `${Math.round(ago)} 秒前` : `${Math.round(ago / 60)} 分钟前`}`;
  }
}

function openAgentDialog() {
  const a = S.meta && S.meta.agent;
  if (!a) return;
  $('aiClaudeCode').textContent = a.claude_code;
  $('aiJson').textContent = a.json;
  $('aiReport').textContent = a.report;
  renderAgentBadge();
  $('aiDialog').showModal();
}

async function startScan(path) {
  S.tree = null; S.viewPath = null; S.selected = null; S.selectedPath = null; S.totals = null;
  S.pinned = null; S.hl = null;
  layout(); render(); select(null); renderLegend(); renderTop();
  try {
    S.status = await api('/api/scan', {
      path,
      apparent: $('sizeMode').value === 'apparent',
      cross_fs: $('crossFs').checked,
    });
  } catch (e) {
    toast(e.message);
    return;
  }
  $('welcome').hidden = true;
  $('pathInput').value = S.status.root;
  S.viewPath = S.status.root;
  S.lastLive = 0;
  renderStatus();
  updateScanUI();
  poll();
}

function renderDisks() {
  const box = $('disks');
  const home = S.meta.home;
  const win = S.meta.os === 'windows';
  const cards = S.meta.disks.map((d) => {
    const letter = d.path.replace(/\\$/, '');
    let title = d.path;
    if (win) title = d.system ? `系统盘 (${letter})` : `本地磁盘 (${letter})`;
    else if (d.system) title = '整个系统';
    else if (S.meta.os === 'macos' && d.path.startsWith('/Volumes/')) title = d.path.slice(9);
    return {
      path: d.path, title,
      sub: win ? d.fstype.toUpperCase() : `${d.source} · ${d.fstype}`,
      used: d.used, total: d.total, free: d.free,
    };
  });
  cards.push({ path: home, title: '我的主目录', sub: home });
  box.innerHTML = cards.map((c, i) => `
    <button class="disk" data-i="${i}">
      <div class="d-top"><span class="d-path">${esc(c.title)}</span>${c.total ? `<span class="d-fs">${esc(c.path)}</span>` : ''}</div>
      <div class="d-sub">${esc(c.sub)}</div>
      ${c.total ? `<div class="d-bar"><i style="width:${(c.used / c.total) * 100}%"></i></div>
      <div class="d-nums">已用 ${fmtBytes(c.used)} / ${fmtBytes(c.total)} · 剩余 ${fmtBytes(c.free)}</div>` : '<div class="d-nums" style="margin-top:10px">只扫描个人文件</div>'}
    </button>`).join('');
  box.querySelectorAll('.disk').forEach((b) => {
    b.onclick = () => {
      const c = cards[+b.dataset.i];
      $('pathInput').value = c.path;
      startScan(c.path);
    };
  });
}

/* ================================================================ 事件 */
function bindEvents() {
  overlay.addEventListener('mousemove', (e) => {
    const r = overlay.getBoundingClientRect();
    S.mouse = { x: e.clientX - r.left, y: e.clientY - r.top, cx: e.clientX, cy: e.clientY };
    const n = hit(S.mouse.x, S.mouse.y);
    if (n !== S.hover) {
      S.hover = n;
      drawOverlay();
      overlay.style.cursor = n && n.t === 'd' && n !== S.tree ? 'zoom-in' : 'default';
    }
    showTooltip(n, e.clientX, e.clientY);
  });
  overlay.addEventListener('mouseleave', () => {
    S.mouse = null;
    S.hover = null;
    drawOverlay();
    showTooltip(null);
  });
  overlay.addEventListener('click', (e) => {
    const n = hit(e.offsetX, e.offsetY);
    if (n) select(n);
  });
  overlay.addEventListener('dblclick', (e) => {
    const n = hit(e.offsetX, e.offsetY);
    if (n) enter(n.t === 'a' ? n.parent : n);
  });
  overlay.addEventListener('contextmenu', (e) => { e.preventDefault(); goUp(); });

  $('upBtn').onclick = goUp;
  $('scanForm').addEventListener('submit', (e) => {
    e.preventDefault();
    const p = $('pathInput').value.trim();
    if (p) startScan(p);
  });
  $('cancelBtn').onclick = () => api('/api/cancel', {}).then(() => poll()).catch((e) => toast(e.message));
  $('procBtn').onclick = async () => {
    try {
      S.status = await api('/api/refresh-procs', {});
      renderStatus();
      if (S.viewPath) await loadView(S.viewPath, { live: true });
      if (S.selected) select(S.selected);
      toast(`已刷新：${S.status.procs.total} 个进程，${S.status.procs.files} 个文件正在使用`);
    } catch (e) { toast(e.message); }
  };

  const legend = $('legend');
  legend.addEventListener('mouseover', (e) => {
    const li = e.target.closest('li');
    if (!li) return;
    const c = +li.dataset.c;
    if (S.hl !== c) { S.hl = c; render(); renderLegendClasses(); }
  });
  legend.addEventListener('mouseleave', () => { S.hl = null; render(); renderLegendClasses(); });
  legend.addEventListener('click', (e) => {
    const li = e.target.closest('li');
    if (!li) return;
    const c = +li.dataset.c;
    S.pinned = S.pinned === c ? null : c;
    render();
    renderLegendClasses();
    reportUiState();
  });

  $('aiBtn').onclick = openAgentDialog;
  $('aiClose').onclick = () => $('aiDialog').close();
  $('aiDialog').addEventListener('click', (e) => { if (e.target === $('aiDialog')) $('aiDialog').close(); });
  document.querySelectorAll('[data-copy]').forEach((b) => {
    b.onclick = () => copyText($(b.dataset.copy).textContent, '已复制');
  });

  document.addEventListener('keydown', (e) => {
    if (e.target.closest('input, select, textarea')) return;
    if (e.key === 'Backspace' || e.key === 'Escape' || (e.altKey && e.key === 'ArrowLeft')) {
      e.preventDefault();
      goUp();
    } else if (e.key === 'Enter' && S.selected) {
      enter(S.selected);
    }
  });

  let rt = 0;
  new ResizeObserver(() => { cancelAnimationFrame(rt); rt = requestAnimationFrame(resize); }).observe($('mapwrap'));
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { buildPalette(); resize(); });
}

function renderLegendClasses() {
  const hl = activeHL();
  $('legend').querySelectorAll('li').forEach((li) => {
    const c = +li.dataset.c;
    li.classList.toggle('active', S.pinned === c);
    li.classList.toggle('dimmed', hl != null && hl !== c);
  });
}

async function init() {
  buildPalette();
  bindEvents();
  resize();
  try {
    S.meta = await api('/api/meta');
  } catch (e) {
    $('status').textContent = `无法连接 DiskMap 后台：${e.message}`;
    return;
  }
  SEP = S.meta.sep || '/';
  document.documentElement.dataset.os = S.meta.os;
  $('welcomeHint').innerHTML = osText().hint;
  $('version').textContent = `v${S.meta.version}`;
  if (!S.status || S.status.state === 'idle') $('pathInput').value = S.meta.system_root;
  renderDisks();
  renderLegend();
  renderTop();
  S.status = await api('/api/status');
  S.cmdId = S.status.command ? S.status.command.id : 0;   // 打开前的旧指令不再执行
  renderStatus();
  updateScanUI();
  renderAgentBadge();
  if (S.status.state !== 'idle') {
    $('welcome').hidden = true;
    $('pathInput').value = S.status.root;
    const want = new URLSearchParams(location.search).get('view');
    const ok = want && await loadView(want);
    if (!ok) await loadView(S.status.root);
    S.wasScanning = S.status.state === 'scanning';
  }
  poll();
}

// 浏览器模式下，页面关闭后后台程序会自动退出
window.addEventListener('pagehide', () => {
  try { navigator.sendBeacon(`/api/bye?t=${encodeURIComponent(TOKEN)}`); } catch (e) { /* 忽略 */ }
});

init();
