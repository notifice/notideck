// SPDX-License-Identifier: GPL-3.0-or-later
// Pure helpers and constants mirroring nd-core::model.

const rid = () => Math.random().toString(36).slice(2, 8);
export const uid = (prefix) => `${prefix}-${rid()}`;

export const MODS = ['ctrl', 'shift', 'alt', 'meta'];

const letters = 'abcdefghijklmnopqrstuvwxyz'.split('');
const digits = '0123456789'.split('');
const fkeys = Array.from({ length: 24 }, (_, i) => `f${i + 1}`);
export const KEY_GROUPS = [
  ['Letters', letters],
  ['Digits', digits],
  ['Function', fkeys],
  ['Navigation', ['enter', 'escape', 'backspace', 'tab', 'space', 'delete', 'insert', 'home', 'end', 'page_up', 'page_down', 'up', 'down', 'left', 'right', 'print_screen', 'caps_lock']],
  ['Punctuation', ['minus', 'equal', 'left_bracket', 'right_bracket', 'backslash', 'semicolon', 'apostrophe', 'grave', 'comma', 'period', 'slash']],
  ['Media', ['media_play_pause', 'media_next', 'media_prev', 'media_stop', 'volume_up', 'volume_down', 'volume_mute']],
];

export const KINDS = ['key', 'dial', 'slider', 'tile'];

export const TRIGGERS_BY_KIND = {
  key: ['key_down', 'key_up', 'long_press', 'double_tap'],
  dial: ['dial_rotate', 'dial_rotate_cw', 'dial_rotate_ccw', 'dial_press', 'dial_release'],
  slider: ['slider_value', 'touch_start', 'touch_end', 'key_down'], // key_down: tap on the slider's app badge (round 13)
  tile: ['key_down', 'long_press', 'slider_value'], // slider_value: the media face's progress bar (round 17)
};

export const TRIGGER_LABELS = {
  key_down: 'Press', key_up: 'Release', long_press: 'Long press', double_tap: 'Double tap',
  dial_rotate: 'Turn (any direction)', dial_rotate_cw: 'Turn right', dial_rotate_ccw: 'Turn left',
  dial_press: 'Press', dial_release: 'Release',
  slider_value: 'Value changes', touch_start: 'Touch start', touch_end: 'Touch end',
};

export const triggerLabel = (t) => TRIGGER_LABELS[t] || t.replace(/_/g, ' ');
export const triggerValid = (kind, t) => (TRIGGERS_BY_KIND[kind] || []).includes(t);

export const FACES = [['', 'Default'], ['media', 'Media (now playing)'], ['stream', 'Stream (live channel)'], ['camera', 'Camera (phone preview)']];

export const AUDIO_TARGETS = [['output', 'Speakers'], ['input', 'Microphone']];

export const SOURCES = [
  ['', 'None'], ['clock', 'Clock'], ['cpu', 'CPU'], ['memory', 'Memory'], ['network', 'Network'],
  ['volume', 'Volume'], ['mute', 'Mute'], ['now_playing', 'Now playing'], ['media_playing', 'Media playing'], ['media_shuffle', 'Media: shuffle'], ['media_repeat', 'Media: repeat'],
  ['camera_state', 'Webcam on'], ['camera_torch', 'Webcam torch'], ['camera_zoom', 'Webcam zoom'],
  ['mic_state', 'Phone mic on'], ['mic_muted', 'Phone mic muted'],
  ['auto_switch_paused', 'Auto-switch paused'],
  ['profile_destination', 'Profile switch target'], ['current_profile', 'Current profile'],
  ['app_audio', 'App audio (mixer)'],
  ['obs_scene', 'OBS: scene'], ['obs_stream', 'OBS: streaming'], ['obs_record', 'OBS: recording'],
  ['obs_replay_buffer', 'OBS: replay buffer'], ['obs_virtual_cam', 'OBS: virtual camera'], ['obs_studio_mode', 'OBS: studio mode'],
  ['obs_source_visibility', 'OBS: source shown'], ['obs_input', 'OBS: audio input'], ['obs_status', 'OBS: connection'],
];

export const SOURCE_HINTS = {
  clock: 'Title shows HH:MM, subtitle shows weekday and date.',
  cpu: 'Value follows CPU usage; title shows the percentage.',
  memory: 'Value follows memory usage; title shows GB used.',
  network: 'Title shows download rate, subtitle shows upload rate.',
  volume: 'Value follows the device volume; subtitle shows the percentage.',
  mute: 'Shows state 2 of the control while the device is muted.',
  camera_state: 'Shows state 2 of the control while the webcam is on.',
  camera_torch: 'Shows state 2 of the control while the phone torch is on.',
  mic_state: 'Shows state 2 of the control while the phone microphone is on.',
  mic_muted: 'Shows state 2 of the control while the phone microphone is muted.',
  camera_zoom: 'Value follows the webcam zoom (1x to 8x); subtitle shows the ratio.',
  auto_switch_paused: 'Shows state 2 of the control while auto-switch is paused (the Auto-switch pause key, or the lock in the PC app).',
  profile_destination: 'Subtitle shows the profile this key switches to. Lit (state 2) while that profile is on screen; dimmed when there is nowhere to go (for example Back with no history).',
  current_profile: 'Title shows the name of the profile on screen.',
  app_audio: 'Value follows the chosen app\'s volume and shows state 2 while it is muted. Slot controls fill themselves with the app\'s name and icon.',
  obs_scene: 'State 2 while it is the live scene, state 3 while it is the preview scene (studio mode). Scene slots also show the scene\'s name as the title.',
  obs_stream: 'State 2 while streaming; the subtitle shows how long.',
  obs_record: 'State 2 while recording, state 3 while paused; the subtitle shows how long.',
  obs_replay_buffer: 'State 2 while the replay buffer is running.',
  obs_virtual_cam: 'State 2 while OBS\'s virtual camera is on.',
  obs_studio_mode: 'State 2 while studio mode is on.',
  obs_source_visibility: 'State 2 while the source is visible in the scene.',
  obs_input: 'Value follows the input\'s fader, the subtitle shows dB, state 2 while muted.',
  obs_status: 'State 2 while connected to OBS; otherwise the subtitle says why not (offline, wrong password, ...).',
  now_playing: 'Title is the track, subtitle the artist, value the progress, detail the times (1:23 / 3:45). State 2 while playing. Dimmed when no player is active. With album art on, the icon is the cover.',
  media_shuffle: 'State 2 while the player reports shuffle on. Dimmed when no player is active or it does not report shuffle.',
  media_repeat: 'State 1 = off, 2 = repeat all, 3 = repeat one. Dimmed when no player is active or it does not report repeat.',
  media_playing: 'State 2 while media is playing, dimmed when no player is active. Title and icon come from the control\'s states, so it suits a play / pause key.',
};

export const sourceLabel = (src) => {
  if (!src) return '';
  const l = (SOURCES.find(([v]) => v === src.type) || [0, src.type])[1];
  if (src.type === 'profile_destination') return src.to ? `${l} ${profileTargetLabel(src.to)}` : l;
  if (src.type === 'app_audio') return src.app ? `${l} ${appTargetLabel(src.app)}` : l;
  return src.target ? `${l} ${src.target === 'input' ? 'MIC' : 'OUT'}` : l;
};

export const ACTION_TYPES = [
  ['hotkey', 'Hotkey'],
  ['type_text', 'Type text'],
  ['open_url', 'Open URL'],
  ['launch_app', 'Launch app'],
  ['go_to_page', 'Go to page'],
  ['next_page', 'Next page'],
  ['prev_page', 'Previous page'],
  ['open_folder', 'Open folder'],
  ['go_back', 'Go back'],
  ['toggle_state', 'Toggle state'],
  ['adjust_volume', 'Adjust volume'],
  ['set_volume', 'Set volume'],
  ['toggle_mute', 'Toggle mute'],
  ['adjust_app_volume', 'App volume: adjust'],
  ['set_app_volume', 'App volume: set'],
  ['toggle_app_mute', 'App volume: toggle mute'],
  ['media_transport', 'Media: play / pause / next... (session)'],
  ['media_seek', 'Media: seek to position'],
  ['media_seek_by', 'Media: skip forward / back'],
  ['media_shuffle', 'Media: shuffle'],
  ['media_repeat', 'Media: repeat'],
  ['camera_toggle', 'Webcam on / off'],
  ['camera_switch_lens', 'Webcam: switch lens'],
  ['camera_torch', 'Webcam: torch'],
  ['camera_zoom', 'Webcam: zoom'],
  ['mic_toggle', 'Phone mic on / off'],
  ['mic_mute', 'Phone mic mute'],
  ['go_home', 'Main menu'],
  ['switch_profile', 'Switch profile'],
  ['auto_switch_pause', 'Auto-switch pause'],
  ['obs_scene', 'OBS: switch scene'],
  ['obs_source_visibility', 'OBS: show / hide source'],
  ['obs_stream', 'OBS: stream'],
  ['obs_record', 'OBS: record'],
  ['obs_record_pause', 'OBS: pause recording'],
  ['obs_replay_buffer', 'OBS: replay buffer'],
  ['obs_save_replay', 'OBS: save replay'],
  ['obs_virtual_cam', 'OBS: virtual camera'],
  ['obs_studio_mode', 'OBS: studio mode'],
  ['obs_studio_transition', 'OBS: studio transition'],
  ['obs_input_mute', 'OBS: mute input'],
  ['obs_set_input_volume', 'OBS: set input volume'],
  ['obs_adjust_input_volume', 'OBS: adjust input volume'],
  ['multi', 'Multi action'],
];

/** The 13 OBS action types and their toggle-mode capable subset. */
export const OBS_ACTION_TYPES = ACTION_TYPES.map(([t]) => t).filter((t) => t.startsWith('obs_'));
/** Round 17 media session control. */
export const MEDIA_COMMANDS = [['play_pause', 'Play / pause'], ['play', 'Play'], ['pause', 'Pause'], ['next', 'Next track'], ['prev', 'Previous track'], ['stop', 'Stop']];
export const MEDIA_REPEAT_MODES = [['cycle', 'Cycle (off, all, one)'], ['off', 'Off'], ['list', 'Repeat all'], ['track', 'Repeat one']];
export const OBS_MODES = [['toggle', 'Toggle'], ['on', 'Turn on'], ['off', 'Turn off']];
export const OBS_MODE_ACTIONS = ['obs_source_visibility', 'obs_stream', 'obs_record', 'obs_record_pause', 'obs_replay_buffer', 'obs_virtual_cam', 'obs_studio_mode', 'obs_input_mute'];
export const OBS_SPECIALS = [
  ['mic1', 'Mic (OBS default)'], ['desktop1', 'Desktop audio (OBS default)'], ['desktop2', 'Desktop audio 2 (OBS)'],
  ['mic2', 'Mic 2 (OBS)'], ['mic3', 'Mic 3 (OBS)'], ['mic4', 'Mic 4 (OBS)'],
];
export const defaultObsScene = () => ({ by: 'slot', index: 0 });
export const defaultObsInput = () => ({ by: 'special', which: 'mic1' });

/** Default `source` object for a data source type (OBS sources need a target). */
export function defaultObsSource(type) {
  switch (type) {
    case 'obs_scene': return { type, scene: defaultObsScene() };
    case 'obs_input': return { type, input: defaultObsInput() };
    case 'obs_source_visibility': return { type, source: '' };
    default: return { type };
  }
}

// ---- profile switching (round 22) ----

/** Profile list entries are `{ id, name }` in list order (the order Next / Previous follow). */
export const defaultProfileTarget = (profiles = []) => (profiles[0] ? { by: 'profile', id: profiles[0].id } : { by: 'next' });
export const PROFILE_SPECIAL_TARGETS = [['next', 'Next profile'], ['previous', 'Previous profile'], ['back', 'Back to previous profile']];
export const profileTargetLabel = (t, profiles = []) => {
  if (!t) return '';
  if (t.by === 'profile') return (profiles.find((p) => p.id === t.id)?.name || t.id || '?').toUpperCase();
  return { next: 'NEXT', previous: 'PREVIOUS', back: 'BACK' }[t.by] || String(t.by).toUpperCase();
};

export const APP_TARGET_KINDS = [['slot', 'Mixer slot'], ['focused', 'Focused app'], ['media', 'Media player'], ['name', 'Specific app']];
export const defaultAppTarget = () => ({ by: 'slot', index: 0 });
export const appTargetLabel = (t) => (!t ? '' : t.by === 'slot' ? `SLOT ${(t.index || 0) + 1}` : t.by === 'focused' ? 'FOCUSED' : t.by === 'media' ? 'MEDIA PLAYER' : (t.name || 'APP').toUpperCase());

/** Action types allowed inside a multi step (nested multi is rejected by the host). */
export const STEP_ACTION_TYPES = ACTION_TYPES.filter(([t]) => t !== 'multi');

/** Mirrors nd-core router::MAX_MULTI_DELAY_MS. */
export const MAX_MULTI_DELAY_MS = 5000;

export const multiTotal = (a) => (a.steps || []).reduce((n, s) => n + (s.delay_ms || 0), 0);

export function defaultAction(type, profile, profiles = []) {
  const firstPage = profile?.pages?.[0]?.id ?? '';
  switch (type) {
    case 'hotkey': return { type, combo: { modifiers: [], key: 'f13' } };
    case 'type_text': return { type, text: '' };
    case 'open_url': return { type, url: 'https://' };
    case 'launch_app': return { type, path: '', args: [], confirmed: false };
    case 'go_to_page': return { type, page_id: firstPage };
    case 'open_folder': return { type, folder_page_id: firstPage };
    case 'adjust_volume': return { type, target: 'output', step: 0.02 };
    case 'set_volume': return { type, target: 'output' };
    case 'toggle_mute': return { type, target: 'output' };
    case 'adjust_app_volume': return { type, app: defaultAppTarget(), step: 0.05 };
    case 'set_app_volume': return { type, app: defaultAppTarget() };
    case 'toggle_app_mute': return { type, app: defaultAppTarget() };
    case 'camera_zoom': return { type, step: 0.25 };
    case 'media_transport': return { type, command: 'play_pause' };
    case 'media_seek': return { type };
    case 'media_seek_by': return { type, seconds: 10 };
    case 'media_shuffle': return { type, mode: 'toggle' };
    case 'media_repeat': return { type, mode: 'cycle' };
    case 'obs_scene': return { type, scene: defaultObsScene() };
    case 'obs_source_visibility': return { type, source: '', mode: 'toggle' };
    case 'obs_stream': case 'obs_record': case 'obs_record_pause': case 'obs_replay_buffer':
    case 'obs_virtual_cam': case 'obs_studio_mode': return { type, mode: 'toggle' };
    case 'obs_input_mute': return { type, input: defaultObsInput(), mode: 'toggle' };
    case 'obs_set_input_volume': return { type, input: defaultObsInput() };
    case 'obs_adjust_input_volume': return { type, input: defaultObsInput(), step: 0.05 };
    case 'go_home': return { type, pause_auto_switch: false };
    case 'switch_profile': return { type, to: defaultProfileTarget(profiles) };
    case 'auto_switch_pause': return { type, mode: 'toggle' };
    case 'multi': return { type, steps: [{ action: defaultAction('hotkey', profile), delay_ms: 0 }] };
    default: return { type };
  }
}

/** Trigger that suits a freshly picked action on a control of this kind (null = keep the current one). */
export function triggerForAction(kind, type) {
  if (type === 'set_app_volume') return kind === 'slider' ? 'slider_value' : kind === 'dial' ? 'dial_rotate' : null;
  if (type === 'media_seek') return kind === 'slider' ? 'touch_end' : null;
  if (type === 'media_seek_by') return kind === 'dial' ? 'dial_rotate' : kind === 'key' || kind === 'tile' ? 'key_down' : null;
  if (type === 'media_transport' || type === 'media_shuffle' || type === 'media_repeat') return kind === 'dial' ? 'dial_press' : kind === 'key' || kind === 'tile' ? 'key_down' : null;
  if (type === 'adjust_app_volume') return kind === 'dial' ? 'dial_rotate' : null;
  if (type === 'obs_set_input_volume') return kind === 'slider' ? 'slider_value' : kind === 'dial' ? 'dial_rotate' : null;
  if (type === 'obs_adjust_input_volume') return kind === 'dial' ? 'dial_rotate' : null;
  if (type === 'obs_input_mute') return kind === 'dial' ? 'dial_press' : kind === 'key' || kind === 'tile' ? 'key_down' : null;
  if (type === 'switch_profile') return kind === 'dial' ? 'dial_press' : kind === 'key' || kind === 'tile' ? 'key_down' : null;
  if (type === 'toggle_app_mute') return kind === 'dial' ? 'dial_press' : kind === 'key' || kind === 'tile' ? 'key_down' : null;
  return null;
}

export function defaultControl(kind, col, row, profile) {
  const c = { id: uid('c'), kind, col, row, span_w: 1, span_h: 1 };
  if (kind === 'key') {
    c.title = 'Key';
    c.actions = [{ action: defaultAction('hotkey', profile), trigger: 'key_down' }];
  } else if (kind === 'dial') {
    c.title = 'Dial';
    c.value = 0.5;
    c.actions = [{ action: { type: 'hotkey', combo: { modifiers: [], key: 'volume_up' } }, trigger: 'dial_rotate' }];
  } else if (kind === 'slider') {
    c.value = 0.5;
    c.horizontal = true;
    c.span_w = 2;
    c.actions = [];
  } else {
    c.title = 'Tile';
    c.subtitle = 'status';
    c.value = 0.4;
    c.actions = [];
  }
  return c;
}

export function gridOf(profile, page) {
  return { cols: page?.cols ?? profile.cols, rows: page?.rows ?? profile.rows };
}

export const rectOf = (c) => ({ col: c.col, row: c.row, w: c.span_w || 1, h: c.span_h || 1 });

export function overlaps(a, b) {
  return a.col < b.col + b.w && b.col < a.col + a.w && a.row < b.row + b.h && b.row < a.row + a.h;
}

export const inBounds = (r, cols, rows) => r.col >= 0 && r.row >= 0 && r.col + r.w <= cols && r.row + r.h <= rows;

/** Plan moving control `c` so its top-left lands on `target`. Returns {ok, moves}. */
export function planMove(c, target, controls, cols, rows) {
  const next = { col: target.col, row: target.row, w: c.span_w, h: c.span_h };
  if (!inBounds(next, cols, rows)) return { ok: false, moves: [] };
  const others = controls.filter((o) => o.id !== c.id);
  const hit = others.filter((o) => overlaps(next, rectOf(o)));
  if (hit.length === 0) return { ok: true, moves: [{ id: c.id, col: next.col, row: next.row }] };
  if (hit.length > 1) return { ok: false, moves: [] };
  const o = hit[0];
  const oNew = { col: c.col, row: c.row, w: o.span_w, h: o.span_h };
  const rest = others.filter((x) => x.id !== o.id);
  const fits = inBounds(oNew, cols, rows) && !overlaps(oNew, next) && !rest.some((x) => overlaps(oNew, rectOf(x)));
  if (!fits) return { ok: false, moves: [] };
  return { ok: true, moves: [{ id: c.id, col: next.col, row: next.row }, { id: o.id, col: oNew.col, row: oNew.row }] };
}

export function canResize(c, w, h, controls, cols, rows) {
  const r = { col: c.col, row: c.row, w, h };
  return inBounds(r, cols, rows) && !controls.some((o) => o.id !== c.id && overlaps(r, rectOf(o)));
}

export function firstFree(controls, cols, rows) {
  for (let row = 0; row < rows; row++) {
    for (let col = 0; col < cols; col++) {
      const r = { col, row, w: 1, h: 1 };
      if (!controls.some((o) => overlaps(r, rectOf(o)))) return { col, row };
    }
  }
  return null;
}

export const argbToHex = (n) => '#' + ((n >>> 0) & 0xffffff).toString(16).padStart(6, '0');
export const hexToArgb = (hex) => ((0xff000000 | parseInt(hex.slice(1), 16)) >>> 0);
export const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));

/** Round 19: plugin action labels come from plugins.js (registered there to avoid an import cycle). */
let pluginLabel = (a) => `${a.plugin}: ${a.action}`;
export function setPluginLabeler(fn) { pluginLabel = fn; }

export function describeAction(def) {
  const a = def.action;
  switch (a.type) {
    case 'plugin': return pluginLabel(a);
    case 'hotkey': return [...a.combo.modifiers, a.combo.key].join('+').toUpperCase();
    case 'type_text': return `TEXT ${a.text.length}C`;
    case 'open_url': return a.url.replace(/^https?:\/\//, '').slice(0, 24).toUpperCase();
    case 'launch_app': return a.path.split(/[\\/]/).pop().toUpperCase() || 'App';
    case 'adjust_volume': return `VOL ${a.step > 0 ? '+' : ''}${Math.round((a.step || 0) * 100)}% ${a.target === 'input' ? 'MIC' : 'OUT'}`;
    case 'set_volume': return `VOL ${a.level == null ? 'Dial' : Math.round(a.level * 100) + '%'} ${a.target === 'input' ? 'MIC' : 'OUT'}`;
    case 'adjust_app_volume': return `${appTargetLabel(a.app)} ${a.step > 0 ? '+' : ''}${Math.round((a.step || 0) * 100)}%`;
    case 'set_app_volume': return `${appTargetLabel(a.app)} ${a.level == null ? 'Dial' : Math.round(a.level * 100) + '%'}`;
    case 'toggle_app_mute': return `MUTE ${appTargetLabel(a.app)}`;
    case 'media_transport': return `MEDIA ${(a.command || 'play_pause').replace(/_/g, '/').toUpperCase()}`;
    case 'media_seek': return a.position == null ? 'SEEK' : `SEEK ${Math.round(a.position * 100)}%`;
    case 'media_seek_by': return `SKIP ${a.seconds > 0 ? '+' : ''}${a.seconds ?? 0}S`;
    case 'media_shuffle': return `SHUFFLE${a.mode && a.mode !== 'toggle' ? ' ' + a.mode.toUpperCase() : ''}`;
    case 'media_repeat': return `REPEAT${a.mode && a.mode !== 'cycle' ? ' ' + a.mode.toUpperCase() : ''}`;
    case 'mic_toggle': return 'PHONE MIC';
    case 'mic_mute': return 'MIC MUTE';
    case 'camera_toggle': return 'WEBCAM';
    case 'camera_switch_lens': return 'FLIP LENS';
    case 'camera_torch': return 'TORCH';
    case 'camera_zoom': return `ZOOM ${a.step > 0 ? '+' : ''}${a.step ?? 0.25}x`;
    case 'switch_profile': return `PROFILE ${profileTargetLabel(a.to)}`.slice(0, 40);
    case 'go_home': return a.pause_auto_switch ? 'MAIN + PAUSE' : 'MAIN MENU';
    case 'auto_switch_pause': return `AUTO-SWITCH ${(a.mode || 'toggle').toUpperCase()}`;
    case 'toggle_mute': return `MUTE ${a.target === 'input' ? 'MIC' : 'OUT'}`;
    case 'multi': return `MULTI ${(a.steps || []).length} STEPS`;
    default:
      if (a.type.startsWith('obs_')) return describeObs(a);
      return a.type.replace(/_/g, ' ').toUpperCase();
  }
}

export const obsSceneLabel = (s) => (!s ? 'CURRENT' : s.by === 'slot' ? `SCENE SLOT ${(s.index || 0) + 1}` : `SCENE ${(s.name || '?').toUpperCase()}`);
export const obsInputLabel = (i) => {
  if (!i) return '';
  if (i.by === 'special') return i.which.startsWith('mic') ? (i.which === 'mic1' ? 'MIC' : `MIC ${i.which.slice(3)}`) : (i.which === 'desktop1' ? 'DESKTOP' : `DESKTOP ${i.which.slice(7)}`);
  return (i.name || '?').toUpperCase();
};

function describeObs(a) {
  const mode = a.mode && a.mode !== 'toggle' ? ` ${a.mode.toUpperCase()}` : '';
  const pct = (v) => `${v > 0 ? '+' : ''}${Math.round((v || 0) * 100)}%`;
  switch (a.type) {
    case 'obs_scene': return `OBS ${obsSceneLabel(a.scene)}${a.preview ? ' PREVIEW' : ''}`;
    case 'obs_source_visibility': return `OBS ${a.mode === 'on' ? 'SHOW' : a.mode === 'off' ? 'HIDE' : 'TOGGLE'} ${(a.source || '?').toUpperCase()}`.slice(0, 40);
    case 'obs_stream': return `OBS STREAM${mode}`;
    case 'obs_record': return `OBS RECORD${mode}`;
    case 'obs_record_pause': return `OBS PAUSE${mode}`;
    case 'obs_replay_buffer': return `OBS REPLAY BUFFER${mode}`;
    case 'obs_save_replay': return 'OBS SAVE REPLAY';
    case 'obs_virtual_cam': return `OBS VIRTUAL CAM${mode}`;
    case 'obs_studio_mode': return `OBS STUDIO MODE${mode}`;
    case 'obs_studio_transition': return 'OBS TRANSITION';
    case 'obs_input_mute': return `OBS ${a.mode === 'on' ? 'MUTE' : a.mode === 'off' ? 'UNMUTE' : 'TOGGLE MUTE'} ${obsInputLabel(a.input)}`.replace('TOGGLE MUTE', 'MUTE');
    case 'obs_set_input_volume': return `OBS VOL ${a.level == null ? 'Dial' : Math.round(a.level * 100) + '%'} ${obsInputLabel(a.input)}`;
    case 'obs_adjust_input_volume': return `OBS VOL ${pct(a.step)} ${obsInputLabel(a.input)}`;
    default: return a.type.replace(/_/g, ' ').toUpperCase();
  }
}

export function slug(name) {
  const s = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 32);
  return `${s || 'profile'}-${rid()}`;
}

/** Build the controls of a generated mixer page: one column per slot. mode is 'sliders' or 'dials'. */
export function mixerPageControls(mode, slots, rows) {
  const out = [];
  const muteKey = (i) => ({
    id: uid('c'), kind: 'key', col: i, row: rows - 1, span_w: 1, span_h: 1, title: 'Mute',
    source: { type: 'app_audio', app: { by: 'slot', index: i } },
    states: [{ title: 'Mute' }, { title: 'Muted', bg_argb: 0xFFB3261E }],
    actions: [{ action: { type: 'toggle_app_mute', app: { by: 'slot', index: i } }, trigger: 'key_down' }],
  });
  for (let i = 0; i < slots; i++) {
    const app = () => ({ by: 'slot', index: i });
    const keyBelow = rows >= 2 && mode === 'dials';
    if (mode === 'dials') {
      out.push({
        id: uid('c'), kind: 'dial', col: i, row: 0, span_w: 1, span_h: 1, value: 0.5,
        source: { type: 'app_audio', app: app() },
        actions: [
          { action: { type: 'adjust_app_volume', app: app(), step: 0.05 }, trigger: 'dial_rotate' },
          { action: { type: 'toggle_app_mute', app: app() }, trigger: 'dial_press' },
        ],
      });
    } else {
      out.push({
        id: uid('c'), kind: 'slider', col: i, row: 0, span_w: 1, span_h: Math.max(1, rows), horizontal: false, value: 0.5,
        source: { type: 'app_audio', app: app() },
        actions: [
          { action: { type: 'set_app_volume', app: app() }, trigger: 'slider_value' },
          { action: { type: 'toggle_app_mute', app: app() }, trigger: 'key_down' },
        ],
      });
    }
    if (keyBelow) out.push(muteKey(i));
  }
  return out;
}

// ---- OBS templates (round 14, M8) ----

export const OBS_COLORS = { sage: 0xFF5E8C6A, amber: 0xFFB08A3E, red: 0xFFB3261E };

/** Glyph names the OBS templates use (rasterised and uploaded once per apply). */
export const OBS_GLYPHS = ['scene', 'record', 'broadcast', 'mic', 'mic-off', 'speaker', 'speaker-off', 'replay', 'transition', 'studio', 'camera', 'plug', 'back', 'mixer'];

/**
 * Controls of the OBS page for a cols x rows grid. Row 0 (and row 1 when rows >= 5) are scene slots, the rest is
 * filled row-major with the actions in priority order. `icons` maps glyph name -> icon_hash (missing = no icon).
 * `audioPageId` adds an "Audio" key that opens the OBS audio page when there is a cell left.
 */
export function obsPageControls(cols, rows, icons = {}, audioPageId = null) {
  const ic = (n) => icons[n] || undefined;
  const st = (title, glyph, extra = {}) => ({ title, icon_hash: ic(glyph), ...extra });
  const key = (title, glyph, extra) => ({ id: uid('c'), kind: 'key', span_w: 1, span_h: 1, title, icon_hash: ic(glyph), ...extra });
  const act = (action, trigger = 'key_down') => ({ action, trigger });
  const clean = (c) => { for (const k of Object.keys(c)) if (c[k] === undefined) delete c[k]; if (c.states) c.states = c.states.map((s) => { const o = { ...s }; for (const k of Object.keys(o)) if (o[k] === undefined) delete o[k]; return o; }); return c; };
  const sage = { bg_argb: OBS_COLORS.sage };
  const amber = { bg_argb: OBS_COLORS.amber };
  const red = { bg_argb: OBS_COLORS.red };

  const sceneRows = rows >= 5 ? 2 : 1;
  const nScenes = Math.min(cols * sceneRows, 32);
  const cells = [];
  for (let r = 0; r < rows; r++) for (let c = 0; c < cols; c++) cells.push({ col: c, row: r });
  const out = [];
  for (let i = 0; i < Math.min(nScenes, cells.length); i++) {
    const scene = () => ({ by: 'slot', index: i });
    out.push(clean({
      ...key(`Scene ${i + 1}`, 'scene', { ...cells[i] }),
      source: { type: 'obs_scene', scene: scene() },
      states: [{}, { ...sage }, { ...amber }],
      actions: [act({ type: 'obs_scene', scene: scene() })],
    }));
  }
  const free = cells.slice(out.length);
  const mic = (which, label, offLabel, glyph, offGlyph) => ({
    ...key(label, glyph),
    source: { type: 'obs_input', input: { by: 'special', which } },
    states: [st(label, glyph), st(offLabel, offGlyph, red)],
    actions: [act({ type: 'obs_input_mute', input: { by: 'special', which }, mode: 'toggle' })],
  });
  const items = [
    () => ({
      ...key('Record', 'record'), subtitle: undefined,
      source: { type: 'obs_record' },
      states: [st('Record', 'record'), st('REC', 'record', red), st('Paused', 'record', amber)],
      actions: [act({ type: 'obs_record', mode: 'toggle' }), act({ type: 'obs_record_pause', mode: 'toggle' }, 'long_press')],
    }),
    () => ({
      ...key('Stream', 'broadcast'), subtitle: 'hold',
      source: { type: 'obs_stream' },
      states: [st('Stream', 'broadcast', { subtitle: 'hold' }), st('LIVE', 'broadcast', red)],
      actions: [act({ type: 'obs_stream', mode: 'toggle' }, 'long_press')],
    }),
    () => mic('mic1', 'Mic', 'Mic off', 'mic', 'mic-off'),
    () => mic('desktop1', 'Desktop', 'Muted', 'speaker', 'speaker-off'),
    () => ({
      ...key('Replay', 'replay'),
      source: { type: 'obs_replay_buffer' },
      states: [st('Replay', 'replay'), st('Save replay', 'replay', sage)],
      actions: [act({ type: 'obs_save_replay' }), act({ type: 'obs_replay_buffer', mode: 'toggle' }, 'long_press')],
    }),
    () => ({
      ...key('OBS', 'plug'),
      source: { type: 'obs_status' },
      states: [st('OBS', 'plug'), st('OBS', 'plug', sage)],
      actions: [],
    }),
    () => ({
      ...key('Transition', 'transition'),
      source: { type: 'obs_studio_mode' },
      states: [st('Transition', 'transition'), st('Transition', 'transition', sage)],
      actions: [act({ type: 'obs_studio_transition' })],
    }),
    () => ({
      ...key('Studio', 'studio'),
      source: { type: 'obs_studio_mode' },
      states: [st('Studio', 'studio'), st('Studio', 'studio', sage)],
      actions: [act({ type: 'obs_studio_mode', mode: 'toggle' })],
    }),
    () => ({
      ...key('Virtual cam', 'camera'),
      source: { type: 'obs_virtual_cam' },
      states: [st('Virtual cam', 'camera'), st('Virtual cam', 'camera', sage)],
      actions: [act({ type: 'obs_virtual_cam', mode: 'toggle' })],
    }),
  ];
  if (audioPageId) {
    items.push(() => ({
      ...key('Audio', 'mixer'),
      actions: [act({ type: 'open_folder', folder_page_id: audioPageId })],
    }));
  }
  for (let i = 0; i < Math.min(items.length, free.length); i++) out.push(clean({ ...items[i](), ...free[i] }));
  return out;
}

/** Controls of the OBS audio page: full-height faders for the default mic and desktop audio, then a Back key. */
export function obsAudioPageControls(cols, rows, icons = {}) {
  const ic = (n) => icons[n] || undefined;
  const out = [];
  const specials = [['mic1', 'Mic', 'Mic off', 'mic', 'mic-off'], ['desktop1', 'Desktop', 'Muted', 'speaker', 'speaker-off']].slice(0, Math.max(1, cols - 1));
  specials.forEach(([which, label, off, glyph, offGlyph], i) => {
    const input = () => ({ by: 'special', which });
    const c = {
      id: uid('c'), kind: 'slider', col: i, row: 0, span_w: 1, span_h: Math.max(1, rows), horizontal: false, value: 0.5,
      title: label, icon_hash: ic(glyph),
      source: { type: 'obs_input', input: input() },
      states: [{ title: label, icon_hash: ic(glyph) }, { title: off, icon_hash: ic(offGlyph), bg_argb: OBS_COLORS.red }],
      actions: [
        { action: { type: 'obs_set_input_volume', input: input() }, trigger: 'slider_value' },
        { action: { type: 'obs_input_mute', input: input(), mode: 'toggle' }, trigger: 'key_down' },
      ],
    };
    for (const k of Object.keys(c)) if (c[k] === undefined) delete c[k];
    c.states = c.states.map((s) => { const o = { ...s }; for (const k of Object.keys(o)) if (o[k] === undefined) delete o[k]; return o; });
    out.push(c);
  });
  const back = { id: uid('c'), kind: 'key', col: specials.length, row: 0, span_w: 1, span_h: 1, title: 'Back', actions: [{ action: { type: 'go_back' }, trigger: 'key_down' }] };
  if (ic('back')) back.icon_hash = ic('back');
  out.push(back);
  return out;
}

/** Auto-switch rule for the OBS profile. Process rules ignore a trailing .exe, so the same rule fits both systems. */
export function obsAutoRule(profileId, os) {
  return { id: uid('r'), enabled: true, kind: 'process', pattern: os === 'linux' ? 'obs' : 'obs64.exe', profile_id: profileId };
}

// ---- Media template (round 16) ----

export const MEDIA_GLYPHS = ['play', 'pause', 'prev', 'next', 'stop', 'music', 'speaker', 'speaker-off', 'mic', 'mic-off', 'shuffle', 'repeat', 'repeat-one', 'rewind-10', 'forward-10'];

/**
 * Controls of the media page for a cols x rows grid. Default 5x3 (N = now playing tile, M = mute, m = mic mute,
 * V = volume slider, then Prev / Play-Pause / Next / Stop):
 *
 *   N N N M V
 *   N N N m V
 *   Pv Pl Nx St V
 *
 * Volume takes the rightmost column when cols >= 4 and rows >= 2. The transport row is the bottom row (Prev, Play,
 * Next, Stop as far as the width W left of the slider allows). The tile fills the rows above it; when W >= 4 the
 * last of those columns holds the mute keys. `icons` maps glyph name -> icon_hash (missing = no icon).
 */
export function mediaPageControls(cols, rows, icons = {}) {
  const ic = (n) => icons[n] || undefined;
  const red = { bg_argb: OBS_COLORS.red };
  const clean = (o) => { for (const k of Object.keys(o)) if (o[k] === undefined) delete o[k]; return o; };
  const st = (title, glyph, extra = {}) => clean({ title, icon_hash: ic(glyph), ...extra });
  const key = (col, row, title, glyph, extra) => clean({
    id: uid('c'), kind: 'key', col, row, span_w: 1, span_h: 1, title, icon_hash: ic(glyph), ...extra,
  });
  const media = (command) => ({ action: { type: 'media_transport', command }, trigger: 'key_down' });
  const out = [];
  const hasVol = rows >= 2 && cols >= 4;
  const W = cols - (hasVol ? 1 : 0);
  const tr = rows - 1; // transport row
  const topRows = rows - 1;
  const sideCol = topRows >= 1 && W >= 4;
  const tileW = sideCol ? W - 1 : W;

  if (topRows >= 1 && tileW >= 2) {
    out.push(clean({
      id: uid('c'), kind: 'tile', col: 0, row: 0, span_w: tileW, span_h: topRows, face: 'media',
      title: 'Now playing', subtitle: 'Nothing playing', value: 0.4,
      source: { type: 'now_playing', art: true },
      actions: [media('play_pause'), { action: { type: 'media_seek' }, trigger: 'slider_value' }],
    }));
  }
  if (sideCol) {
    const audio = (row, target, label, offLabel, glyph, offGlyph) => key(W - 1, row, label, glyph, {
      source: { type: 'mute', target },
      states: [st(label, glyph), st(offLabel, offGlyph, red)],
      actions: [{ action: { type: 'toggle_mute', target }, trigger: 'key_down' }],
    });
    out.push(audio(0, 'output', 'Mute', 'Muted', 'speaker', 'speaker-off'));
    if (topRows >= 2) out.push(audio(1, 'input', 'Mic', 'Mic off', 'mic', 'mic-off'));
  }
  if (hasVol) {
    out.push(clean({
      id: uid('c'), kind: 'slider', col: cols - 1, row: 0, span_w: 1, span_h: rows, horizontal: false, value: 0.5,
      title: 'Volume', icon_hash: ic('speaker'),
      source: { type: 'volume', target: 'output' },
      actions: [{ action: { type: 'set_volume', target: 'output' }, trigger: 'slider_value' }],
    }));
  }
  const play = (col) => key(col, tr, 'Play', 'play', {
    source: { type: 'media_playing' },
    states: [st('Play', 'play'), st('Pause', 'pause')],
    actions: [media('play_pause')],
  });
  const prev = (col) => key(col, tr, 'Prev', 'prev', { actions: [media('prev')] });
  const next = (col) => key(col, tr, 'Next', 'next', { actions: [media('next')] });
  const stop = (col) => key(col, tr, 'Stop', 'stop', { actions: [media('stop')] });
  const fns = W >= 3 ? [prev, play, next, stop] : W === 2 ? (rows >= 2 ? [prev, next] : [prev, play]) : [play];
  fns.slice(0, W).forEach((f, i) => out.push(f(i)));
  return out;
}

/**
 * Controls of the second media page ("Media 2") for a cols x rows grid. Default 5x3 (Sh shuffle, Rp repeat,
 * PV player volume (`{by: media}`), V master volume, Seek = progress slider):
 *
 *   Sh Rp .  PV V
 *   Seek     PV V
 *   -10 Pl +10 PV V
 *
 * Degrading: the sliders are full height and need rows >= 2; PV takes the rightmost column when cols >= 4, V joins
 * it (PV moves one left) when cols >= 5, so V is dropped first. W = the columns left of the sliders. The transport
 * row is the bottom row: Back 10 s, Play/Pause, Forward 10 s when W >= 3, Play + Forward when W = 2, Play alone
 * when W = 1. Shuffle and Repeat sit in row 0 (Repeat needs W >= 2) when rows >= 2. The seek slider takes row 1
 * across W columns when rows >= 3 and W >= 2. With a single row only the transport keys remain. Nothing overlaps.
 */
export function mediaExtraPageControls(cols, rows, icons = {}) {
  const ic = (n) => icons[n] || undefined;
  const sage = { bg_argb: OBS_COLORS.sage };
  const clean = (o) => { for (const k of Object.keys(o)) if (o[k] === undefined) delete o[k]; return o; };
  const st = (title, glyph, extra = {}) => clean({ title, icon_hash: ic(glyph), ...extra });
  const key = (col, row, title, glyph, extra) => clean({
    id: uid('c'), kind: 'key', col, row, span_w: 1, span_h: 1, title, icon_hash: ic(glyph), ...extra,
  });
  const act = (action, trigger = 'key_down') => ({ action, trigger });
  const out = [];
  const tall = rows >= 2;
  const hasPlayer = tall && cols >= 4;
  const hasVol = tall && cols >= 5;
  const W = cols - (hasPlayer ? 1 : 0) - (hasVol ? 1 : 0);
  const tr = rows - 1;

  if (hasPlayer) {
    const app = () => ({ by: 'media' });
    out.push(clean({
      id: uid('c'), kind: 'slider', col: hasVol ? cols - 2 : cols - 1, row: 0, span_w: 1, span_h: rows, horizontal: false, value: 0.5,
      title: 'Player', icon_hash: ic('music'),
      source: { type: 'app_audio', app: app() },
      actions: [act({ type: 'set_app_volume', app: app() }, 'slider_value'), act({ type: 'toggle_app_mute', app: app() })],
    }));
  }
  if (hasVol) {
    out.push(clean({
      id: uid('c'), kind: 'slider', col: cols - 1, row: 0, span_w: 1, span_h: rows, horizontal: false, value: 0.5,
      title: 'Volume', icon_hash: ic('speaker'),
      source: { type: 'volume', target: 'output' },
      actions: [act({ type: 'set_volume', target: 'output' }, 'slider_value')],
    }));
  }
  if (tall) {
    out.push(key(0, 0, 'Shuffle', 'shuffle', {
      source: { type: 'media_shuffle' },
      states: [st('Shuffle', 'shuffle'), st('Shuffle on', 'shuffle', sage)],
      actions: [act({ type: 'media_shuffle', mode: 'toggle' })],
    }));
    if (W >= 2) {
      out.push(key(1, 0, 'Repeat', 'repeat', {
        source: { type: 'media_repeat' },
        states: [st('Repeat off', 'repeat'), st('Repeat all', 'repeat', sage), st('Repeat one', 'repeat-one', sage)],
        actions: [act({ type: 'media_repeat', mode: 'cycle' })],
      }));
    }
  }
  if (rows >= 3 && W >= 2) {
    out.push(clean({
      id: uid('c'), kind: 'slider', col: 0, row: 1, span_w: W, span_h: 1, horizontal: true, value: 0, title: 'Seek',
      source: { type: 'now_playing' },
      actions: [act({ type: 'media_seek' }, 'touch_end')],
    }));
  }
  const back = (col) => key(col, tr, 'Back 10 s', 'rewind-10', { actions: [act({ type: 'media_seek_by', seconds: -10 })] });
  const play = (col) => key(col, tr, 'Play', 'play', {
    source: { type: 'media_playing' },
    states: [st('Play', 'play'), st('Pause', 'pause')],
    actions: [act({ type: 'media_transport', command: 'play_pause' })],
  });
  const fwd = (col) => key(col, tr, 'Forward 10 s', 'forward-10', { actions: [act({ type: 'media_seek_by', seconds: 10 })] });
  const fns = W >= 3 ? [back, play, fwd] : W === 2 ? [play, fwd] : [play];
  fns.slice(0, W).forEach((f, i) => out.push(f(i)));
  return out;
}

// ---- App templates (round 19) ----

/**
 * Ready-made profiles for desktop apps. Each page lists keys row-major (5 per row on the default 5x3 grid) as
 * `[title, glyph, combo, opts?]`; `combo` is `mod+mod+key` with the `Key` names of nd-core (`ctrl+shift+p`,
 * `ctrl+grave`). Shortcuts are the apps' Windows defaults. `processes` are the auto-switch rule patterns (a process
 * rule ignores `.exe` and case, so `Code` fits `Code.exe` and `code`).
 */
export const APP_TEMPLATES = {
  vscode: {
    label: 'VS Code', name: 'VS Code', app: 'VS Code', processes: ['Code'],
    info: 'One page of VS Code shortcuts (Windows defaults): command palette, quick open, search, sidebar, terminal, debugging, go to definition, rename, comment, format, split editor, quick fix and close tab. Format document is Shift+Alt+F on Windows (Ctrl+Shift+I on Linux).',
    pages: [{ name: 'VS Code', keys: [
      ['Palette', 'bolt', 'ctrl+shift+p'], ['Open file', 'folder', 'ctrl+p'], ['Search', 'search', 'ctrl+shift+f'], ['Sidebar', 'sidebar', 'ctrl+b'], ['Terminal', 'terminal', 'ctrl+grave'],
      ['Debug', 'debug', 'f5'], ['Step over', 'next', 'f10'], ['Step into', 'down', 'f11'], ['Definition', 'link', 'f12'], ['Rename', 'edit', 'f2'],
      ['Comment', 'comment', 'ctrl+slash'], ['Format', 'format', 'shift+alt+f'], ['Split', 'studio', 'ctrl+backslash'], ['Quick fix', 'wand', 'ctrl+period'], ['Close tab', 'close', 'ctrl+w'],
    ] }],
  },
  discord: {
    label: 'Discord', name: 'Discord', app: 'Discord', processes: ['Discord'],
    info: 'One page of Discord\'s in-app shortcuts: mute and deafen, quick switcher, upload, emoji, channel and server navigation, mark as read, GIF picker, inbox and the shortcut list. These work while Discord is the focused window.',
    pages: [{ name: 'Discord', keys: [
      ['Mute', 'mic-off', 'ctrl+shift+m'], ['Deafen', 'headset', 'ctrl+shift+d'], ['Switcher', 'search', 'ctrl+k'], ['Upload', 'export', 'ctrl+shift+u'], ['Emoji', 'emoji', 'ctrl+e'],
      ['Prev chan', 'up', 'alt+up'], ['Next chan', 'down', 'alt+down'], ['Unread up', 'up', 'alt+shift+up'], ['Unread down', 'down', 'alt+shift+down'], ['Mark read', 'check', 'escape'],
      ['Prev server', 'prev', 'ctrl+alt+up'], ['Next server', 'next', 'ctrl+alt+down'], ['GIF', 'scene', 'ctrl+g'], ['Inbox', 'inbox', 'ctrl+i'], ['Shortcuts', 'settings', 'ctrl+slash'],
    ] }],
  },
  teamspeak: {
    label: 'TeamSpeak', name: 'TeamSpeak', app: 'TeamSpeak', processes: ['ts3client_win64', 'ts3client_win32', 'ts3client_linux_amd64', 'ts3client_linux', 'TeamSpeak'],
    info: 'TeamSpeak has no default hotkeys, so these keys send Ctrl+Alt+Shift+F13 to F20. In TeamSpeak open Tools > Options > Hotkeys and add the same combo for each action: F13 mute microphone, F14 mute speakers, F15 toggle away, F16 toggle push-to-talk, F17 toggle voice activation, F18 previous server tab, F19 next server tab, F20 disconnect.',
    pages: [{ name: 'TeamSpeak', keys: [
      ['Mic', 'mic-off', 'ctrl+alt+shift+f13', { sub: 'F13' }], ['Speakers', 'speaker-off', 'ctrl+alt+shift+f14', { sub: 'F14' }], ['Away', 'away', 'ctrl+alt+shift+f15', { sub: 'F15' }], ['PTT', 'mic', 'ctrl+alt+shift+f16', { sub: 'F16' }], ['Voice act.', 'broadcast', 'ctrl+alt+shift+f17', { sub: 'F17' }],
      ['Prev tab', 'prev', 'ctrl+alt+shift+f18', { sub: 'F18' }], ['Next tab', 'next', 'ctrl+alt+shift+f19', { sub: 'F19' }], ['Disconnect', 'plug', 'ctrl+alt+shift+f20', { sub: 'F20' }],
    ] }],
  },
  photoshop: {
    label: 'Photoshop', name: 'Photoshop', app: 'Photoshop', processes: ['Photoshop'],
    info: 'One page of Photoshop shortcuts (defaults): move, brush, eraser, crop, swap colours, brush size, zoom, fit on screen, undo, redo, new layer, save and Export As.',
    pages: [{ name: 'Photoshop', keys: [
      ['Move', 'move', 'v'], ['Brush', 'brush', 'b'], ['Eraser', 'eraser', 'e'], ['Crop', 'crop', 'c'], ['Swap', 'autoswitch', 'x'],
      ['Smaller', 'minus', 'left_bracket'], ['Bigger', 'plus', 'right_bracket'], ['Zoom in', 'zoom-in', 'ctrl+equal'], ['Zoom out', 'zoom-out', 'ctrl+minus'], ['Fit', 'fit', 'ctrl+0'],
      ['Undo', 'undo', 'ctrl+z'], ['Redo', 'redo', 'ctrl+shift+z'], ['New layer', 'layers', 'ctrl+shift+n'], ['Save', 'save', 'ctrl+s'], ['Export', 'export', 'ctrl+alt+shift+w'],
    ] }],
  },
  gimp: {
    label: 'GIMP', name: 'GIMP', app: 'GIMP', processes: ['gimp', 'gimp-2.10', 'gimp-2.99', 'gimp-3.0', 'gimp-3'],
    info: 'One page of GIMP shortcuts (defaults): move, paintbrush, eraser, crop, swap colours, brush size, zoom, fit image in window, undo, redo, new layer, save and Export As.',
    pages: [{ name: 'GIMP', keys: [
      ['Move', 'move', 'm'], ['Brush', 'brush', 'p'], ['Eraser', 'eraser', 'shift+e'], ['Crop', 'crop', 'shift+c'], ['Swap', 'autoswitch', 'x'],
      ['Smaller', 'minus', 'left_bracket'], ['Bigger', 'plus', 'right_bracket'], ['Zoom in', 'zoom-in', 'equal'], ['Zoom out', 'zoom-out', 'minus'], ['Fit', 'fit', 'ctrl+shift+j'],
      ['Undo', 'undo', 'ctrl+z'], ['Redo', 'redo', 'ctrl+y'], ['New layer', 'layers', 'ctrl+shift+n'], ['Save', 'save', 'ctrl+s'], ['Export as', 'export', 'ctrl+shift+e'],
    ] }],
  },
  premiere: {
    label: 'Premiere Pro', name: 'Premiere Pro', app: 'Premiere Pro', processes: ['Adobe Premiere Pro'],
    info: 'One page of Premiere Pro shortcuts (defaults): selection, razor, add edit, ripple delete, marker, play/pause, frame step, mark in and out, timeline zoom, undo, save and Export Media.',
    pages: [{ name: 'Premiere', keys: [
      ['Select', 'cursor', 'v'], ['Razor', 'cut', 'c'], ['Split', 'cut', 'ctrl+k'], ['Ripple del', 'trash', 'shift+delete'], ['Marker', 'flag', 'm'],
      ['Play', 'play', 'space'], ['Frame -1', 'prev', 'left'], ['Frame +1', 'next', 'right'], ['Mark in', 'mark-in', 'i'], ['Mark out', 'mark-out', 'o'],
      ['Zoom in', 'zoom-in', 'equal'], ['Zoom out', 'zoom-out', 'minus'], ['Undo', 'undo', 'ctrl+z'], ['Save', 'save', 'ctrl+s'], ['Export', 'export', 'ctrl+m'],
    ] }],
  },
  resolve: {
    label: 'DaVinci Resolve', name: 'Resolve', app: 'DaVinci Resolve', processes: ['Resolve'],
    info: 'One page of DaVinci Resolve shortcuts (Windows defaults): selection and blade modes, split clip, delete, marker, play/pause, frame step, mark in and out, timeline zoom, zoom to fit, undo and the Deliver page.',
    pages: [{ name: 'Resolve', keys: [
      ['Select', 'cursor', 'a'], ['Blade', 'cut', 'b'], ['Split', 'cut', 'ctrl+b'], ['Delete', 'trash', 'delete'], ['Marker', 'flag', 'm'],
      ['Play', 'play', 'space'], ['Frame -1', 'prev', 'left'], ['Frame +1', 'next', 'right'], ['Mark in', 'mark-in', 'i'], ['Mark out', 'mark-out', 'o'],
      ['Zoom in', 'zoom-in', 'ctrl+equal'], ['Zoom out', 'zoom-out', 'ctrl+minus'], ['Fit', 'fit', 'shift+z'], ['Undo', 'undo', 'ctrl+z'], ['Deliver', 'export', 'shift+8'],
    ] }],
  },
  spotify: {
    label: 'Spotify', name: 'Spotify', app: 'Spotify', processes: ['Spotify'],
    info: 'Two pages: the media page (now-playing tile, transport, mute keys, volume) and one page of Spotify desktop shortcuts: like, search, quick search, back and forward, shuffle, repeat, volume, mute, home, library, new playlist, preferences and full screen.',
    pages: [{ name: 'Media', media: true }, { name: 'Spotify', keys: [
      ['Like', 'heart', 'alt+shift+b'], ['Search', 'search', 'ctrl+l'], ['Quick find', 'bolt', 'ctrl+k'], ['Back', 'back', 'alt+left'], ['Forward', 'next', 'alt+right'],
      ['Shuffle', 'shuffle', 'ctrl+s'], ['Repeat', 'repeat', 'ctrl+r'], ['Vol -', 'speaker', 'ctrl+down'], ['Vol +', 'speaker', 'ctrl+up'], ['Mute', 'speaker-off', 'ctrl+shift+down'],
      ['Home', 'home', 'alt+shift+h'], ['Library', 'music', 'alt+shift+l'], ['New list', 'plus', 'ctrl+n'], ['Prefs', 'settings', 'ctrl+p'], ['Full screen', 'fit', 'f11'],
    ] }],
  },
};

/** Glyph names a template's pages need (rasterised and uploaded once per apply). */
export function appTemplateGlyphs(id) {
  const t = APP_TEMPLATES[id];
  const set = new Set();
  for (const p of t.pages) {
    if (p.media) MEDIA_GLYPHS.forEach((g) => set.add(g));
    else p.keys.forEach((k) => set.add(k[1]));
  }
  return [...set];
}

/** `ctrl+shift+p` -> `{ modifiers: ['ctrl', 'shift'], key: 'p' }`. */
export function parseCombo(s) {
  const parts = s.split('+');
  const key = parts.pop();
  return { modifiers: parts, key };
}

/** Controls for a key list, filled row-major from the top-left; keys that do not fit the grid are dropped. */
export function appKeyControls(keys, cols, rows, icons = {}) {
  return keys.slice(0, cols * rows).map(([title, glyph, combo, opts = {}], i) => {
    const c = {
      id: uid('c'), kind: 'key', col: i % cols, row: Math.floor(i / cols), span_w: 1, span_h: 1, title,
      actions: [{ action: { type: 'hotkey', combo: parseCombo(combo) }, trigger: 'key_down' }],
    };
    if (icons[glyph]) c.icon_hash = icons[glyph];
    if (opts.sub) c.subtitle = opts.sub;
    return c;
  });
}

/** Pages of an app template for a cols x rows grid. */
export function appTemplatePages(id, cols, rows, icons = {}) {
  return APP_TEMPLATES[id].pages.map((p) => ({
    id: uid('p'), name: p.name,
    controls: p.media ? mediaPageControls(cols, rows, icons) : appKeyControls(p.keys, cols, rows, icons),
  }));
}

/** Auto-switch rules (one per process name) that show `profileId` while the app is focused. */
export function appAutoRules(id, profileId) {
  return APP_TEMPLATES[id].processes.map((pattern) => ({ id: uid('r'), enabled: true, kind: 'process', pattern, profile_id: profileId }));
}

/** Every manifest default of a plugin action, as the inspector stores them in a binding's settings. */
export const pluginDefaults = (act) => Object.fromEntries((act?.settings || []).filter((f) => f.default !== undefined).map((f) => [f.key, f.default]));

// ---- Profile switching templates (round 22) ----

export const PROFILE_GLYPHS = ['back', 'forward', 'layers'];
export const PROFILE_COLORS = { accent: 0xFF5E8C6A };

/** Free 1x1 cells of a page, row-major. */
export function freeCells(controls, cols, rows) {
  const out = [];
  for (let row = 0; row < rows; row++) for (let col = 0; col < cols; col++) {
    const r = { col, row, w: 1, h: 1 };
    if (!controls.some((o) => overlaps(r, rectOf(o)))) out.push({ col, row });
  }
  return out;
}

const profileKeyBase = (cell, title, glyph, icons, extra) => {
  const c = { id: uid('c'), kind: 'key', col: cell.col, row: cell.row, span_w: 1, span_h: 1, title, ...extra };
  if (icons[glyph]) c.icon_hash = icons[glyph];
  return c;
};
const backKey = (cell, icons) => profileKeyBase(cell, 'Back', 'back', icons, {
  source: { type: 'profile_destination', to: { by: 'back' } },
  actions: [{ action: { type: 'switch_profile', to: { by: 'back' } }, trigger: 'key_down' }],
});
const nextKey = (cell, icons) => profileKeyBase(cell, 'Next', 'forward', icons, {
  source: { type: 'profile_destination', to: { by: 'next' } },
  actions: [{ action: { type: 'switch_profile', to: { by: 'next' } }, trigger: 'key_down' }],
});

/**
 * Controls of the "Profiles" page for a cols x rows grid: one key per profile in list order (switch_profile to it,
 * `profile_destination` source, lit while it is on screen), then Back and Next when cells remain. Returns
 * `{ controls, placed, total }`; `placed < total` means the grid was too small for every profile.
 */
export function profilesPageControls(cols, rows, profiles, icons = {}) {
  const cells = freeCells([], cols, rows);
  const out = [];
  const n = Math.min(profiles.length, cells.length);
  for (let i = 0; i < n; i++) {
    const to = () => ({ by: 'profile', id: profiles[i].id });
    out.push(profileKeyBase(cells[i], profiles[i].name || profiles[i].id, 'layers', icons, {
      source: { type: 'profile_destination', to: to() },
      states: [{}, { bg_argb: PROFILE_COLORS.accent }],
      actions: [{ action: { type: 'switch_profile', to: to() }, trigger: 'key_down' }],
    }));
  }
  const rest = cells.slice(n);
  if (rest[0]) out.push(backKey(rest[0], icons));
  if (rest[1]) out.push(nextKey(rest[1], icons));
  return { controls: out, placed: n, total: profiles.length };
}

/**
 * Back / current profile / Next keys for the free cells of an existing page (controls = its current controls).
 * Prefers three free cells side by side in a row, then in a column, else the first three free cells. Returns the new controls, or
 * null when fewer than three cells are free.
 */
export function profileBarControls(controls, cols, rows, icons = {}) {
  const free = freeCells(controls, cols, rows);
  if (free.length < 3) return null;
  const has = (c, r) => free.some((f) => f.col === c && f.row === r);
  let cells = null;
  for (const f of free) if (has(f.col + 1, f.row) && has(f.col + 2, f.row)) { cells = [f, { col: f.col + 1, row: f.row }, { col: f.col + 2, row: f.row }]; break; }
  if (!cells) for (const f of free) if (has(f.col, f.row + 1) && has(f.col, f.row + 2)) { cells = [f, { col: f.col, row: f.row + 1 }, { col: f.col, row: f.row + 2 }]; break; }
  cells = cells || free.slice(0, 3);
  const cur = profileKeyBase(cells[1], 'Profile', 'layers', icons, {
    source: { type: 'current_profile' },
    actions: [{ action: { type: 'switch_profile', to: { by: 'next' } }, trigger: 'key_down' }],
  });
  return [backKey(cells[0], icons), cur, nextKey(cells[2], icons)];
}
