// SPDX-License-Identifier: GPL-3.0-or-later
import { initToken } from './api.js';
import { $, h } from './dom.js';
import { S, boot, subscribe, refreshStatus, refreshList, refreshHost, refreshSettings, emit } from './store.js';
import { renderGrid, renderTabs, bindKeyboard } from './preview.js';
import { render as renderInspector } from './inspector.js';
import { renderToolbar } from './sidebar.js';
import { renderDevices } from './devices.js';
import { renderSettings } from './settings.js';
import { startMicPolling, stopMicPolling } from './mic.js';
import { renderCamera, startCameraPolling, stopCameraPolling } from './camera.js';
import { startAutoswitch, stopAutoswitch } from './autoswitch.js';
import { startMixer, stopMixer } from './mixer.js';
import { startObs, stopObs } from './obs.js';
import { openOnboarding } from './onboarding.js';
import { initShell, initRouting, onTab, tab, applyTheme, applyPhoneTheme, renderConn, renderProblems } from './shell.js';

function renderStage() {
  const phone = $('#phone');
  const empty = S.loaded && !S.profile;
  phone.classList.toggle('dark', empty);
  renderTabs();
  renderGrid();
  if (empty) {
    $('#grid').append(S.fault
      ? h('p', { class: 'empty-state' }, 'Offline', h('small', null, 'The agent is not answering. Check that it is running and the URL has its token.'))
      : h('p', { class: 'empty-state' }, 'No profile', h('small', null, 'Create one from the toolbar to start laying out keys.')));
  }
}

// Re-render the toolbar only when what it shows changed, so focus survives the background poll.
let barSig = '';
function refreshBar(force = false) {
  const sig = JSON.stringify([S.loaded, S.fault, S.profiles, S.profile?.id, !!S.profile, S.save]);
  if (!force && sig === barSig) return;
  const el = document.activeElement;
  if ($('#toolbar').contains(el) && el.tagName === 'SELECT' && !force) return;
  barSig = sig;
  renderToolbar();
}

let devSig = '';
function refreshDevices() {
  const sig = JSON.stringify([S.status?.devices, S.host?.adb, !!S.status]);
  if (sig === devSig) return;
  devSig = sig;
  if (tab === 'devices') renderDevices();
}

subscribe((kind) => {
  switch (kind) {
    case 'preview':
      renderTabs();
      renderGrid();
      break;
    case 'sel':
      renderGrid();
      renderInspector();
      break;
    case 'meta':
      refreshBar();
      break;
    default:
      renderStage();
      renderInspector();
      refreshBar(true);
  }
  if (kind === 'preview') refreshBar();
  refreshDevices();
});

onTab((id) => {
  if (id === 'deck') { renderToolbar(); renderStage(); renderInspector(); }
  if (id === 'devices') { devSig = ''; refreshHost().then(() => { devSig = ''; refreshDevices(); }); refreshStatus().then(refreshDevices); renderDevices(); }
  if (id === 'camera') startCameraPolling(); else stopCameraPolling();
  if (id === 'mic') startMicPolling(); else stopMicPolling();
  if (id === 'mixer') startMixer(); else stopMixer();
  if (id === 'obs') startObs(); else stopObs();
  if (id === 'autoswitch') startAutoswitch(); else stopAutoswitch();
  if (id === 'settings') { Promise.all([refreshSettings(), refreshHost(), refreshStatus()]).then(renderSettings); renderSettings(); }
  document.title = `${id[0].toUpperCase()}${id.slice(1)} · Notideck`;
});

initToken();
initShell();
bindKeyboard();
renderConn();
initRouting();
renderToolbar();
boot();
refreshSettings().then(() => {
  if (!S.settings) return;
  applyTheme(S.settings.theme);
  applyPhoneTheme(S.settings.phone_theme);
  if (S.settings.onboarding_done === false) openOnboarding();
});
refreshHost();

// Keep device list and active marker fresh without touching the editor.
setInterval(async () => {
  if (document.hidden) return;
  if (tab === 'devices') return; // the faster poll below handles it
  await refreshStatus();
  if (S.save !== 'writing') {
    try { await refreshList(); emit('meta'); } catch { /* agent restarting */ }
  }
}, 4000);

// Devices tab: poll status every 2 s while visible.
setInterval(async () => {
  if (document.hidden || tab !== 'devices') return;
  await refreshStatus();
  if (S.save !== 'writing') { try { await refreshList(); } catch { /* ignore */ } }
  refreshDevices();
}, 2000);
setInterval(() => { if (!document.hidden) refreshHost().then(refreshDevices); }, 10000);
