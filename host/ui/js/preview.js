// SPDX-License-Identifier: GPL-3.0-or-later
// Phone-shaped live preview: renders the grid, handles selection, keyboard and pointer drag.

import { h, svg, $, reducedMotion } from './dom.js';
import { api } from './api.js';
import { S, page, grid, controls, select, moveControl, removeControl, addPage, selectPage, addRailKey, MAX_RAIL_KEYS } from './store.js';
import { icon } from './icons.js';
import { dropTarget, setIconFromFile, installDropGuard } from './imgload.js';
import { iconView, tintInk } from './iconview.js';
import { argbToHex, clamp, sourceLabel, planMove, rectOf, overlaps } from './model.js';

installDropGuard();

const DRAG_THRESHOLD = 5;

export function renderTabs() {
  const nav = $('#tabs');
  nav.replaceChildren();
  if (!S.profile) return;
  S.profile.pages.forEach((p, i) => {
    nav.append(
      h('button', {
        class: 'tab',
        type: 'button',
        'aria-current': i === S.pageIdx ? 'page' : null,
        onclick: () => selectPage(i),
      }, h('span', { class: 'tab-n' }, String(i + 1)), ' ', p.name || 'Untitled'),
    );
  });
  nav.append(h('button', { class: 'tab tab-add', type: 'button', 'aria-label': 'Add page', onclick: addPage }, 'Add page'));
  nav.append(h('button', {
    class: 'tab tab-rail', type: 'button', 'aria-current': S.sel?.railKey ? 'page' : null,
    title: 'Pinned keys on the phone\u2019s side rail',
    onclick: () => (S.rail.length ? select({ railKey: S.rail[0].id }) : addRailKey()),
  }, 'Side rail'));
}

/** The phone's side rail: Home, Back, up to 3 pinned keys, page dots and the status cluster. */
export function renderRail() {
  const root = $('#siderail');
  if (!root) return;
  const focusKey = root.contains(document.activeElement) ? document.activeElement.dataset.rk : null;
  root.replaceChildren();
  root.hidden = !S.profile;
  if (!S.profile) return;
  const static_ = (name, label, dim) => h('div', { class: `sr-btn${dim ? ' dim' : ''}`, role: 'img', 'aria-label': label, title: label }, icon(name));
  root.append(static_('home', 'Home: main menu (long-press also pauses auto-switch)'), static_('back', 'Back (shown on the phone inside a folder or off the start page)', true));
  const keys = h('div', { class: 'sr-keys', role: 'group', 'aria-label': 'Pinned keys' });
  for (let i = 0; i < MAX_RAIL_KEYS; i++) {
    const c = S.rail[i];
    if (!c) {
      keys.append(h('button', {
        class: 'sr-slot', type: 'button', 'aria-label': `Add pinned key ${i + 1}`, dataset: { rk: `slot${i}` },
        onclick: addRailKey,
      }, h('span', { class: 'plus', 'aria-hidden': 'true' }, '+')));
      continue;
    }
    const isSel = S.sel?.railKey === c.id;
    const style = [];
    if (c.bg_argb) style.push(`--ctl-bg:${argbToHex(c.bg_argb)}`);
    if (c.fg_argb) style.push(`--ctl-fg:${argbToHex(c.fg_argb)}`);
    else if (c.bg_argb) style.push(`--ctl-fg:${tintInk(argbToHex(c.bg_argb))}`);
    const rk = h('button', {
      class: `cell ctl ctl-key sr-key${isSel ? ' selected' : ''}${c.bg_argb ? ' custom-bg' : ''}`, type: 'button',
      style: style.join(';'), 'aria-pressed': isSel ? 'true' : 'false', dataset: { rk: c.id },
      'aria-label': `Pinned key ${i + 1}: ${c.title || 'untitled'}`,
      onclick: () => select({ railKey: c.id }),
    }, keyBody(c), c.source ? h('span', { class: 'live', title: `Live: ${sourceLabel(c.source)}` }) : null);
    dropTarget(rk, (f) => setIconFromFile(c, f, true));
    keys.append(rk);
  }
  root.append(keys);
  const n = S.profile.pages.length;
  root.append(h('div', { class: 'sr-dots', role: 'group', 'aria-label': 'Pages' }, n > 1
    ? S.profile.pages.map((pg, i) => h('button', {
      class: `sr-dot${i === S.pageIdx ? ' on' : ''}`, type: 'button', 'aria-label': `Page ${i + 1}: ${pg.name || 'Untitled'}`,
      'aria-current': i === S.pageIdx ? 'page' : null, onclick: () => selectPage(i),
    }))
    : []));
  const now = new Date();
  root.append(h('div', { class: 'sr-status', 'aria-hidden': 'true' }, icon('link', 'ico-svg sm'),
    h('span', null, `${String(now.getHours()).padStart(2, '0')}:${String(now.getMinutes()).padStart(2, '0')}`)));
  if (focusKey) root.querySelector(`[data-rk="${focusKey}"]`)?.focus({ preventScroll: true });
}

export function renderGrid() {
  renderRail();
  const root = $('#grid');
  root.replaceChildren();
  if (!S.profile || !page()) return;
  const { cols, rows } = grid();
  root.style.gridTemplateColumns = `repeat(${cols}, minmax(0, 1fr))`;
  root.style.gridTemplateRows = `repeat(${rows}, minmax(0, 1fr))`;
  root.style.aspectRatio = `${cols} / ${rows}`;
  root.style.maxWidth = `calc(62dvh * ${cols} / ${rows})`;
  root.dataset.cols = cols;

  const occupied = new Set();
  controls().forEach((c) => {
    for (let r = c.row; r < c.row + c.span_h; r++) for (let q = c.col; q < c.col + c.span_w; q++) occupied.add(`${q},${r}`);
  });

  const animate = S.enter && !reducedMotion();
  let i = 0;
  const selCell = S.sel?.cell;
  for (let r = 0; r < rows; r++) {
    for (let q = 0; q < cols; q++) {
      if (occupied.has(`${q},${r}`)) continue;
      const isSel = selCell && selCell.col === q && selCell.row === r;
      const cell = h('button', {
        class: `cell empty${isSel ? ' selected' : ''}${animate ? ' enter' : ''}`,
        type: 'button',
        role: 'gridcell',
        style: `grid-column:${q + 1};grid-row:${r + 1};--i:${i++}`,
        'aria-label': `Empty cell column ${q + 1} row ${r + 1}`,
        'aria-selected': isSel ? 'true' : 'false',
        tabindex: isSel ? 0 : -1,
        dataset: { col: q, row: r },
        onclick: () => select({ cell: { col: q, row: r } }, true),
      }, h('span', { class: 'plus', 'aria-hidden': 'true' }, '+'));
      root.append(cell);
    }
  }

  let firstTab = true;
  for (const c of controls()) {
    const isSel = S.sel?.id === c.id;
    const el = controlEl(c, isSel, animate, i++);
    el.tabIndex = isSel || (!S.sel && firstTab) ? 0 : -1;
    firstTab = false;
    root.append(el);
  }
  S.enter = false;

  if (S.settleIds.length && !reducedMotion()) {
    for (const id of S.settleIds) root.querySelector(`[data-id="${id}"]`)?.classList.add('settle');
  }
  S.settleIds = [];

  if (S.focusSel) {
    S.focusSel = false;
    (root.querySelector('.selected') || root.querySelector('[tabindex="0"]'))?.focus({ preventScroll: true });
  }
}

function controlEl(c, isSel, animate, idx) {
  const style = [
    `grid-column:${c.col + 1} / span ${c.span_w}`,
    `grid-row:${c.row + 1} / span ${c.span_h}`,
    `--i:${idx}`,
  ];
  if (c.bg_argb) style.push(`--ctl-bg:${argbToHex(c.bg_argb)}`);
  if (c.fg_argb) style.push(`--ctl-fg:${argbToHex(c.fg_argb)}`);
  else if (c.bg_argb) style.push(`--ctl-fg:${tintInk(argbToHex(c.bg_argb))}`);
  const el = h('div', {
    class: `cell ctl ctl-${c.kind}${isSel ? ' selected' : ''}${animate ? ' enter' : ''}${c.bg_argb ? ' custom-bg' : ''}`,
    role: 'gridcell',
    style: style.join(';'),
    dataset: { id: c.id },
    'aria-selected': isSel ? 'true' : 'false',
    'aria-label': `${c.kind} ${c.title || 'untitled'}, column ${c.col + 1} row ${c.row + 1}`,
    onpointerdown: (e) => onPointerDown(e, c, el),
    onkeydown: (e) => { if (e.key === 'Enter') { e.preventDefault(); $('#inspector input, #inspector select')?.focus(); } },
  }, body(c), c.source ? h('span', { class: 'live', role: 'img', 'aria-label': `Live data: ${sourceLabel(c.source)}`, title: `Live: ${sourceLabel(c.source)}` }) : null, h('i', { class: 'br tl' }), h('i', { class: 'br tr' }), h('i', { class: 'br bl' }), h('i', { class: 'br bb' }));
  dropTarget(el, (f) => setIconFromFile(c, f));
  return el;
}

function body(c) {
  switch (c.kind) {
    case 'dial': return dialBody(c);
    case 'slider': return sliderBody(c);
    case 'tile': return tileBody(c);
    default: return keyBody(c);
  }
}

/** Title, or the live-source name in a smaller wrapping style so it never truncates. */
function slotPlaceholder(c) {
  const a = c.source?.type === 'app_audio' ? c.source.app : null;
  return a?.by === 'slot' ? `Slot ${(a.index || 0) + 1}` : a?.by === 'media' ? 'Media player' : '';
}

function lblFor(c) {
  if (c.title) return h('span', { class: 'lbl' }, c.title);
  return h('span', { class: 'lbl ph' }, slotPlaceholder(c) || (c.source ? sourceLabel(c.source) : ''));
}

function keyBody(c) {
  return h('div', { class: 'face' },
    c.icon_hash ? iconView(api.iconUrl(c.icon_hash), 'ico') : h('span', { class: 'ico-ph', 'aria-hidden': 'true' }),
    lblFor(c),
  );
}

function dialBody(c) {
  const R = 40, C = 2 * Math.PI * R, arc = C * 0.75, v = clamp(c.value || 0, 0, 1);
  const ring = (len, cls) => svg('circle', {
    class: cls, cx: 50, cy: 50, r: R, fill: 'none', 'stroke-width': 7,
    'stroke-dasharray': `${len} ${C}`, transform: 'rotate(135 50 50)',
  });
  return h('div', { class: 'face' },
    svg('svg', { viewBox: '0 0 100 100', class: 'dial', 'aria-hidden': 'true' }, ring(arc, 'trk'), ring(Math.max(0.001, arc * v), 'val'),
      svg('line', { class: 'tick', x1: 50, y1: 50, x2: 50, y2: 22, 'stroke-width': 3, transform: `rotate(${-135 + 270 * v} 50 50)` })),
    lblFor(c),
    h('span', { class: 'num' }, `${Math.round(v * 100)}`),
  );
}

function sliderBody(c) {
  const v = clamp(c.value || 0, 0, 1);
  return h('div', { class: `face slider ${c.horizontal ? 'hz' : 'vt'}` },
    h('div', { class: 'rail' }, h('div', { class: 'fill', style: c.horizontal ? `width:${v * 100}%` : `height:${v * 100}%` }),
      h('div', { class: 'thumb', style: c.horizontal ? `left:${v * 100}%` : `bottom:${v * 100}%` })),
    c.title || slotPlaceholder(c) ? h('span', { class: `lbl${c.title ? '' : ' ph'}` }, c.title || slotPlaceholder(c)) : null,
    h('span', { class: 'num' }, `${Math.round(v * 100)}`),
  );
}

function tileBody(c) {
  const v = clamp(c.value || 0, 0, 1);
  if (c.face === 'media') {
    return h('div', { class: 'face tile media' },
      h('div', { class: 'art' }, c.source?.art ? icon('music') : null),
      h('div', { class: 'meta' },
        h('span', { class: 'lbl' }, c.title || 'Now playing'),
        h('span', { class: 'sub' }, c.subtitle || ''),
        h('span', { class: 'sub time' }, '1:23 / 3:45'),
        h('div', { class: 'gauge' }, h('div', { style: `width:${v * 100}%` }))));
  }
  if (c.face === 'camera') {
    return h('div', { class: 'face tile camera' },
      h('div', { class: 'cam' }, icon('camera')),
      h('span', { class: 'lbl' }, c.title || 'Camera'));
  }
  if (c.face === 'stream') {
    return h('div', { class: 'face tile stream' },
      h('span', { class: 'pill' }, 'LIVE'),
      h('span', { class: 'lbl' }, c.title || 'Channel'),
      h('span', { class: 'sub' }, c.subtitle || 'Stream title'));
  }
  return h('div', { class: 'face tile' },
    h('span', { class: 'lbl' }, c.title || slotPlaceholder(c) || (c.source ? sourceLabel(c.source) : '')),
    h('span', { class: 'sub' }, c.subtitle || ''),
    h('div', { class: 'gauge' }, h('div', { style: `width:${v * 100}%` })),
  );
}

// ---- pointer drag ----

function onPointerDown(e, c, el) {
  if (e.button !== 0) return;
  e.preventDefault(); // we place focus ourselves; the grid re-renders under the pointer
  const gridEl = $('#grid');
  const gr = gridEl.getBoundingClientRect();
  const { cols, rows } = grid();
  const cw = gr.width / cols, ch = gr.height / rows;
  const cellAt = (x, y) => ({
    col: clamp(Math.floor((x - gr.left) / cw), 0, cols - 1),
    row: clamp(Math.floor((y - gr.top) / ch), 0, rows - 1),
  });
  const p0 = cellAt(e.clientX, e.clientY);
  const grab = { dc: clamp(p0.col - c.col, 0, c.span_w - 1), dr: clamp(p0.row - c.row, 0, c.span_h - 1) };
  const sx = e.clientX, sy = e.clientY;
  let dragging = false, target = null, ok = false;
  const marker = h('div', { class: 'drop', 'aria-hidden': 'true' });

  if (S.sel?.id !== c.id) select({ id: c.id }, true);
  // select() re-rendered the grid; find the live element again.
  const live = () => gridEl.querySelector(`[data-id="${c.id}"]`) || el;
  let node = live();
  node.focus({ preventScroll: true });
  node.setPointerCapture?.(e.pointerId);

  const move = (ev) => {
    const dx = ev.clientX - sx, dy = ev.clientY - sy;
    if (!dragging) {
      if (Math.hypot(dx, dy) < DRAG_THRESHOLD) return;
      dragging = true;
      node = live();
      node.classList.add('lift');
      gridEl.append(marker);
    }
    node.style.transform = `translate(${dx}px, ${dy}px) scale(1.05)`;
    const p = cellAt(ev.clientX, ev.clientY);
    target = {
      col: clamp(p.col - grab.dc, 0, cols - c.span_w),
      row: clamp(p.row - grab.dr, 0, rows - c.span_h),
    };
    ok = planMove(c, target, controls(), cols, rows).ok;
    marker.className = `drop ${ok ? 'ok' : 'bad'}`;
    marker.style.gridColumn = `${target.col + 1} / span ${c.span_w}`;
    marker.style.gridRow = `${target.row + 1} / span ${c.span_h}`;
  };

  const end = (ev) => {
    node.removeEventListener('pointermove', move);
    node.removeEventListener('pointerup', end);
    node.removeEventListener('pointercancel', end);
    marker.remove();
    if (!dragging) return;
    S.focusSel = true;
    if (ev.type === 'pointerup' && ok && target && moveControl(c.id, target)) return;
    S.focusSel = false;
    node.classList.remove('lift');
    node.style.transform = '';
    if (!reducedMotion() && ev.type === 'pointerup') {
      node.animate(
        [{ transform: 'translateX(0)' }, { transform: 'translateX(-6px)' }, { transform: 'translateX(5px)' }, { transform: 'translateX(0)' }],
        { duration: 220, easing: 'cubic-bezier(0.32, 0.72, 0, 1)' },
      );
    }
  };

  node.addEventListener('pointermove', move);
  node.addEventListener('pointerup', end);
  node.addEventListener('pointercancel', end);
}

// ---- keyboard ----

export function bindKeyboard() {
  $('#grid').addEventListener('keydown', (e) => {
    const dirs = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    const { cols, rows } = grid();
    const cur = currentCell();
    if (e.key === 'Delete' || e.key === 'Backspace') {
      if (S.sel?.id && !(e.target instanceof HTMLInputElement)) {
        e.preventDefault();
        S.focusSel = true;
        removeControl(S.sel.id);
      }
      return;
    }
    const d = dirs[e.key];
    if (!d || !cur) return;
    e.preventDefault();
    if (e.altKey && S.sel?.id) {
      const c = controls().find((x) => x.id === S.sel.id);
      S.focusSel = true;
      if (!moveControl(c.id, { col: c.col + d[0], row: c.row + d[1] })) S.focusSel = false;
      return;
    }
    const ncol = clamp(cur.col + d[0], 0, cols - 1);
    const nrow = clamp(cur.row + d[1], 0, rows - 1);
    const hit = controls().find((c) => overlaps({ col: ncol, row: nrow, w: 1, h: 1 }, rectOf(c)));
    select(hit ? { id: hit.id } : { cell: { col: ncol, row: nrow } }, true);
  });
}

function currentCell() {
  if (S.sel?.cell) return S.sel.cell;
  const c = S.sel?.id ? controls().find((x) => x.id === S.sel.id) : null;
  if (c) return { col: c.col, row: c.row };
  return { col: 0, row: 0 };
}
