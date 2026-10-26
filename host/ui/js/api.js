// SPDX-License-Identifier: GPL-3.0-or-later
// REST client for nd-config. The session token travels in X-ND-Token.

let token = '';
export function initToken() {
  const url = new URL(location.href);
  const fromUrl = url.searchParams.get('token');
  try {
    if (fromUrl) {
      sessionStorage.setItem('nd-token', fromUrl);
      url.searchParams.delete('token');
      history.replaceState(null, '', url.pathname + url.search + url.hash);
    }
    token = fromUrl || sessionStorage.getItem('nd-token') || '';
  } catch {
    token = fromUrl || '';
  }
}

async function req(method, path, body, headers = {}) {
  const init = { method, headers: { 'X-ND-Token': token, ...headers } };
  if (body !== undefined) init.body = body;
  const res = await fetch(`/api${path}`, init);
  if (!res.ok) {
    let msg = `${res.status}`;
    let body = null;
    try { body = await res.json(); msg = body.error || msg; } catch { /* not json */ }
    const e = new Error(msg);
    e.body = body;
    e.status = res.status;
    throw e;
  }
  if (res.status === 204) return null;
  try { return await res.json(); } catch { return null; }
}

const json = (o) => JSON.stringify(o);
const JSON_HDR = { 'Content-Type': 'application/json' };

export const api = {
  profiles: () => req('GET', '/profiles'),
  profile: (id) => req('GET', `/profiles/${encodeURIComponent(id)}`),
  save: (p) => req('PUT', `/profiles/${encodeURIComponent(p.id)}`, json(p), JSON_HDR),
  remove: (id) => req('DELETE', `/profiles/${encodeURIComponent(id)}`),
  activate: (id) => req('POST', `/profiles/${encodeURIComponent(id)}/activate`),
  status: () => req('GET', '/status'),
  settings: () => req('GET', '/settings'),
  saveSettings: (patch) => req('PUT', '/settings', json(patch), JSON_HDR),
  host: () => req('GET', '/host'),
  phoneUsb: () => req('GET', '/phone/usb'),
  phoneInstall: (serial) => req('POST', '/phone/install', json({ serial }), JSON_HDR),
  fix: (id) => req('POST', `/fix/${encodeURIComponent(id)}`),
  open: (which) => req('POST', `/open/${encodeURIComponent(which)}`),
  startPairing: () => req('POST', '/pairing'),
  cancelPairing: () => req('DELETE', '/pairing'),
  paired: () => req('GET', '/devices/paired'),
  unpair: (id) => req('DELETE', `/devices/paired/${encodeURIComponent(id)}`),
  pending: () => req('GET', '/devices/pending'),
  resolvePending: (session, allow) => req('POST', `/devices/pending/${encodeURIComponent(session)}/${allow ? 'approve' : 'deny'}`),
  camera: () => req('GET', '/camera'),
  cameraTest: (on) => req('POST', '/camera/test', json({ on }), JSON_HDR),
  cameraControl: (c) => req('POST', '/camera/control', json(c), JSON_HDR),
  cameraPreviewUrl: () => `/api/camera/preview.png?token=${encodeURIComponent(token)}&t=${Date.now()}`,
  mic: () => req('GET', '/mic'),
  micControl: (c) => req('POST', '/mic', json(c), JSON_HDR),
  autoswitch: () => req('GET', '/autoswitch'),
  saveAutoswitch: (cfg) => req('PUT', '/autoswitch', json(cfg), JSON_HDR),
  autoswitchLock: (locked) => req('POST', '/autoswitch/lock', json({ locked }), JSON_HDR),
  mixer: () => req('GET', '/mixer'),
  saveMixerConfig: (cfg) => req('PUT', '/mixer/config', json(cfg), JSON_HDR),
  mixerApp: (body) => req('POST', '/mixer/app', json(body), JSON_HDR),
  apps: (refresh) => req('GET', `/apps${refresh ? '?refresh=1' : ''}`),
  appIconUrl: (id) => `/api/apps/icon?id=${encodeURIComponent(id)}&token=${encodeURIComponent(token)}`,
  siteIconUrl: (url) => `/api/site-icon?url=${encodeURIComponent(url)}&token=${encodeURIComponent(token)}`,
  obs: () => req('GET', '/obs'),
  obsConfig: (cfg) => req('PUT', '/obs/config', json(cfg), JSON_HDR),
  obsEnableServer: () => req('POST', '/obs/enable_server'),
  obsReconnect: () => req('POST', '/obs/reconnect'),
  rail: () => req('GET', '/rail'),
  saveRail: (keys) => req('PUT', '/rail', json({ keys }), JSON_HDR),
  plugins: () => req('GET', '/plugins'),
  pluginCmd: (id, cmd) => req('POST', `/plugins/${encodeURIComponent(id)}/${cmd}`),
  pluginUninstall: (id) => req('DELETE', `/plugins/${encodeURIComponent(id)}`),
  pluginsReload: () => req('POST', '/plugins/reload'),
  pluginInstall: (zip) => req('POST', '/plugins/install', zip, { 'Content-Type': 'application/zip' }),
  pluginLog: (id) => req('GET', `/plugins/${encodeURIComponent(id)}/log`),
  pluginFileUrl: (id, path) => `/api/plugins/${encodeURIComponent(id)}/file?path=${encodeURIComponent(path)}&token=${encodeURIComponent(token)}`,
  uploadIcon: (file) => req('POST', '/icons', file, { 'Content-Type': file.type || 'application/octet-stream' }),
  iconUrl: (hash) => `/api/icons/${encodeURIComponent(hash)}?token=${encodeURIComponent(token)}`,
};
