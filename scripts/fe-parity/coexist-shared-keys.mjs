// #927 C3 shared-key contract: one snapshot function, used by the browser
// spec via page.evaluate() (the function source is serialized, so it must stay
// fully self-contained: no outer-scope references) and by the offline oracle
// coexist-self-check.test.mjs — the same bytes are tested that the browser runs.
// The old shell keeps the active project name in sessionStorage
// (AppShellV2.tsx:76 reads it, :163-164 writes/removes it); a snapshot that
// only walks localStorage cannot see that key.
export const ACTIVE_PROJECT_KEY = 'pilotdeck-active-project';
export const ACTIVE_PROJECT_ASSERT = '共享契约：活动项目键 pilotdeck-active-project 必须在 sessionStorage 中实际存在（AppShellV2.tsx:163 写入）';
export function sharedStorageSnapshot() {
  const shared = {};
  for (const storage of [localStorage, sessionStorage]) {
    for (const key of Object.keys(storage)) {
      if (key === 'auth-token' || key === 'pilotdeck-active-project' || key.startsWith('draft_input_')) shared[key] = storage.getItem(key);
    }
  }
  return shared;
}
