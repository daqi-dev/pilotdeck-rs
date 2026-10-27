// Router/history boundary. Install before either router is initialized.
// prefixes defaults to the exp007 probe prefix; since #927 the coexistence
// deployment also injects it into the deployed old-shell copy (via
// boundary-boot.js) with ['/settings'] so the migrated prefix switches bundles.
// Production wiring is still probe-only: no shipped old-shell code imports it.
export function installBoundary(win, hardNavigate = (url, replace) => replace ? win.location.replace(url) : win.location.assign(url), prefixes = ['/probe']) {
  const isNew = pathname => prefixes.some(prefix => pathname === prefix || pathname.startsWith(prefix + '/'));
  const loadedShell = isNew(win.location.pathname);
  const history = win.history;
  const originalPush = history.pushState;
  const originalReplace = history.replaceState;
  function cross(url) {
    const target = new URL(url ?? win.location.href, win.location.href);
    return target.origin === win.location.origin && isNew(target.pathname) !== loadedShell ? target : null;
  }
  history.pushState = function(state, title, url) {
    const target = cross(url);
    if (target) return hardNavigate(target.href, false);
    return originalPush.call(this, state, title, url);
  };
  history.replaceState = function(state, title, url) {
    const target = cross(url);
    if (target) return hardNavigate(target.href, true);
    return originalReplace.call(this, state, title, url);
  };
  const pop = () => {
    const target = cross(win.location.href);
    // Keep the history entry being traversed; assign() would create an extra entry.
    if (target) hardNavigate(target.href, true);
  };
  win.addEventListener('popstate', pop);
  return () => {
    history.pushState = originalPush;
    history.replaceState = originalReplace;
    win.removeEventListener('popstate', pop);
  };
}
