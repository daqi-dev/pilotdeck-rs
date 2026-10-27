// #927 offline oracle for the router/history boundary used by the coexistence
// deployment. Runs with `node --test scripts/fe-parity/coexist-boundary.test.mjs`
// on the repository's own boundary.js; no listen, no browser.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { installBoundary } from '../../ui-rs/shared/boundary.js';

// Minimal history/location stub: boundary.js only needs the primitives it wraps.
function fakeWindow(pathname, prefixes) {
  const win = {
    location: {
      href: `http://127.0.0.1:28420${pathname}`,
      origin: 'http://127.0.0.1:28420',
      pathname,
      assign: url => { win.location.href = url; },
      replace: url => { win.location.href = url; },
    },
    history: {},
    listeners: {},
    addEventListener: (name, fn) => { (win.listeners[name] ||= []).push(fn); },
    removeEventListener: (name, fn) => { win.listeners[name] = (win.listeners[name] || []).filter(item => item !== fn); },
  };
  const passthrough = [];
  for (const method of ['pushState', 'replaceState']) {
    win.history[method] = function (state, title, url) {
      passthrough.push({ method, url });
      if (url !== undefined && url !== null) {
        const target = new URL(url, win.location.href);
        if (target.origin === win.location.origin) win.location.href = target.href;
      }
    };
  }
  const hard = [];
  const dispose = installBoundary(win, (url, replace) => hard.push({ url, replace }), prefixes);
  return { win, hard, passthrough, dispose, pop: () => {
    // Simulate traversal: the browser updates location, then fires popstate.
    for (const fn of [...win.listeners.popstate || []]) fn();
  } };
}

test('cross-shell pushState hard-navigates instead of SPA routing (old -> new)', () => {
  const b = fakeWindow('/', ['/settings']);
  b.win.history.pushState({}, '', '/settings/models');
  assert.deepEqual(b.hard, [{ url: 'http://127.0.0.1:28420/settings/models', replace: false }], 'cross pushState must call hardNavigate(assign)');
  assert.deepEqual(b.passthrough, [], 'cross pushState must not reach the wrapped router');
});

test('cross-shell replaceState hard-navigates with replace (no duplicate entry)', () => {
  const b = fakeWindow('/settings', ['/settings']);
  b.win.history.replaceState({}, '', '/');
  assert.deepEqual(b.hard, [{ url: 'http://127.0.0.1:28420/', replace: true }],
    'cross replaceState 必须 hardNavigate(replace=true)（不产生重复历史项）');
  assert.deepEqual(b.passthrough, [], 'cross replaceState must not reach the wrapped router');
});

test('same-shell navigation passes through to the wrapped router', () => {
  const b = fakeWindow('/', ['/settings']);
  b.win.history.pushState({}, '', '/session/abc');
  b.win.history.pushState({}, '', '/settings-legacy'); // strict segment boundary: not the migrated prefix
  assert.deepEqual(b.hard, [], 'same-shell URLs must never hard-jump');
  assert.equal(b.passthrough.length, 2);
  assert.equal(b.passthrough[0].method, 'pushState');
});

test('prefix matching is a strict segment boundary', () => {
  const b = fakeWindow('/', ['/settings']);
  b.win.history.pushState({}, '', '/settingsX');
  b.win.history.pushState({}, '', '/settingsx/models');
  b.win.history.pushState({}, '', '/pref/settings');
  assert.deepEqual(b.hard, [], '/settingsX, /settingsx/..., /pref/settings must stay in the old shell');
  b.win.history.pushState({}, '', '/settings');
  assert.deepEqual(b.hard, [{ url: 'http://127.0.0.1:28420/settings', replace: false }], 'exact prefix must cross');
});

test('exact prefix and deeper paths both cross in both directions', () => {
  const oldShell = fakeWindow('/', ['/settings']);
  oldShell.win.history.pushState({}, '', '/settings');
  oldShell.win.history.pushState({}, '', '/settings/models');
  assert.deepEqual(oldShell.hard.map(item => item.url),
    ['http://127.0.0.1:28420/settings', 'http://127.0.0.1:28420/settings/models']);
  const newShell = fakeWindow('/settings/models', ['/settings']);
  newShell.win.history.pushState({}, '', '/');
  newShell.win.history.pushState({}, '', '/session/abc');
  assert.deepEqual(newShell.hard.map(item => item.url),
    ['http://127.0.0.1:28420/', 'http://127.0.0.1:28420/session/abc']);
});

test('popstate traversal across the boundary hard-navigates with replace', () => {
  const b = fakeWindow('/settings', ['/settings']);
  b.win.location.href = 'http://127.0.0.1:28420/'; // traversal landed on an old-shell URL
  b.pop();
  assert.deepEqual(b.hard, [{ url: 'http://127.0.0.1:28420/', replace: true }],
    'traversal must keep the history entry (replace, not assign)');
});

test('cross-origin targets never hard-navigate', () => {
  // Both directions with a pathname that WOULD cross: the origin check must
  // keep foreign origins in router business (and vice versa).
  const oldShell = fakeWindow('/', ['/settings']);
  oldShell.win.history.pushState({}, '', 'http://example.com/settings');
  assert.deepEqual(oldShell.hard, [], 'different origin is router business, not a shell crossing');
  assert.equal(oldShell.passthrough.length, 1);
  const newShell = fakeWindow('/settings', ['/settings']);
  newShell.win.history.pushState({}, '', 'http://example.com/');
  assert.deepEqual(newShell.hard, [], 'different origin is router business, not a shell crossing');
  assert.equal(newShell.passthrough.length, 1);
});

test('dispose restores the wrapped primitives', () => {
  const b = fakeWindow('/', ['/settings']);
  b.dispose();
  b.win.history.pushState({}, '', '/settings');
  assert.deepEqual(b.hard, [], 'disposed boundary must not intercept');
  assert.deepEqual(b.passthrough.map(item => item.url), ['/settings']);
});

test('default prefixes keep the exp007 /probe semantics', () => {
  const b = fakeWindow('/'); // no explicit prefixes argument; old shell at /
  b.win.history.pushState({}, '', '/settings');
  assert.deepEqual(b.passthrough.map(item => item.url), ['/settings'], '/settings stays old shell under default prefixes');
  b.win.history.pushState({}, '', '/probe/leptos');
  assert.deepEqual(b.hard.map(item => item.url), ['http://127.0.0.1:28420/probe/leptos'],
    '默认 prefixes 必须保留 /probe 边界（exp007 语义）');
});
