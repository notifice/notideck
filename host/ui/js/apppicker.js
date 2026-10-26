// SPDX-License-Identifier: GPL-3.0-or-later
// Installed-app picker modal for the launch_app action.

import { h } from './dom.js';
import { api } from './api.js';
import { icon } from './icons.js';

let dlg = null;

/** Open the picker. `onPick(app)` runs after the dialog closes with the chosen `{id,name,target,args}`. */
export function openAppPicker(onPick) {
  if (dlg) return;
  let apps = [];
  let shown = [];
  let sel = 0;
  let loading = true;
  let error = '';
  let query = '';

  const list = h('ul', { class: 'app-list', role: 'listbox', 'aria-label': 'Installed apps', tabindex: '-1' });
  const status = h('p', { class: 'muted app-status', role: 'status' });
  const search = h('input', {
    type: 'text', placeholder: 'Search apps', 'aria-label': 'Search apps', autocomplete: 'off', spellcheck: 'false',
    oninput: (e) => { query = e.target.value; sel = 0; render(); },
    onkeydown: (e) => {
      if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
      else if (e.key === 'Enter') { e.preventDefault(); if (shown[sel]) pick(shown[sel]); }
    },
  });

  const close = () => { dlg?.close(); dlg?.remove(); dlg = null; };
  const pick = (app) => { close(); onPick(app); };
  const mark = () => {
    [...list.children].forEach((li, i) => {
      li.setAttribute('aria-selected', i === sel ? 'true' : 'false');
      li.classList.toggle('sel', i === sel);
    });
    list.children[sel]?.scrollIntoView({ block: 'nearest' });
  };
  function move(d) {
    if (!shown.length) return;
    sel = Math.max(0, Math.min(shown.length - 1, sel + d));
    mark();
  }
  function render() {
    const q = query.trim().toLowerCase();
    shown = q ? apps.filter((a) => a.name.toLowerCase().includes(q)) : apps;
    list.replaceChildren(...shown.map((a, i) => h('li', {
      class: 'app-row', role: 'option', 'aria-selected': 'false',
      onclick: () => pick(a), onmousemove: () => { if (sel !== i) { sel = i; mark(); } },
    },
    h('img', { class: 'app-ico', width: 24, height: 24, loading: 'lazy', alt: '', src: api.appIconUrl(a.id), onerror: (e) => { e.target.style.visibility = 'hidden'; } }),
    h('span', { class: 'app-name' }, a.name))));
    if (loading) status.textContent = 'Loading apps…';
    else if (error) status.textContent = `The app list is unavailable: ${error}`;
    else if (!apps.length) status.textContent = 'No apps found.';
    else if (!shown.length) status.textContent = 'No apps match your search.';
    else status.textContent = '';
    status.hidden = !status.textContent;
    mark();
  }
  async function load(refresh) {
    loading = true; error = ''; render();
    try {
      const r = await api.apps(refresh);
      apps = r.apps || [];
      if (r.available === false || r.error) error = r.error || 'not supported on this system';
    } catch (e) {
      apps = [];
      error = e.message;
    }
    loading = false;
    sel = 0;
    render();
  }

  dlg = h('dialog', { class: 'modal app-modal', 'aria-labelledby': 'app-title' },
    h('header', null, h('h2', { id: 'app-title' }, 'Choose an app'),
      h('button', { type: 'button', class: 'btn sm', 'aria-label': 'Close', onclick: close }, icon('close', 'ico-svg sm'))),
    search, status, list,
    h('footer', { class: 'app-foot' },
      h('button', { type: 'button', class: 'link', onclick: () => { load(true); search.focus(); } }, 'Refresh'),
      h('button', { type: 'button', class: 'btn', onclick: close }, 'Cancel')));
  dlg.addEventListener('cancel', (e) => { e.preventDefault(); close(); });
  document.body.append(dlg);
  dlg.showModal();
  search.focus();
  load(false);
}
