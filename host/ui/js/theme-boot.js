// SPDX-License-Identifier: GPL-3.0-or-later
// Runs before first paint so the window never flashes the wrong theme.
(function () {
  var pref = 'system';
  try { pref = localStorage.getItem('nd-theme') || 'system'; } catch (e) { /* storage blocked */ }
  var dark = pref === 'dark' || (pref === 'system' && !matchMedia('(prefers-color-scheme: light)').matches);
  document.documentElement.setAttribute('data-theme', dark ? 'dark' : 'light');
})();
