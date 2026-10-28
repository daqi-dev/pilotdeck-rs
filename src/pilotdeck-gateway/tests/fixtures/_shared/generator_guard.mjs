// Shared prologue for the real-TS snapshot fixture generators (#322-4).
//
// The workspace-snapshot generator historically carried the only copy of the
// fixed clock, the Git provenance binding (explicit absolute Git, pinned HEAD,
// clean tracked tree, every `src` byte against the commit blob) and the
// ownership-checked recursive delete. snapshot-projection copied all three, so
// a hardening fix in one generator did not reach the other. Both now import
// this module; the generators' oracles stay byte-identical.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync, readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { rm } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

/**
 * Pin the process clock at `startMs` (default 2026-01-01T00:00:00Z). A
 * recorder reads `new Date().toISOString()` once per capture; pinning it makes
 * `createdAt` byte-identical across runs, and `tick()` advances the fake clock
 * instead of sleeping so "newest first" stays strictly ordered.
 */
export function installFixedClock(startMs = Date.UTC(2026, 0, 1)) {
  const RealDate = Date;
  let clockMs = startMs;
  class FixedDate extends RealDate {
    constructor(...args) {
      super(...((args.length === 0 ? [clockMs] : args)));
    }
    static now() {
      return clockMs;
    }
  }
  globalThis.Date = FixedDate;
  return {
    now: () => clockMs,
    tick: () => {
      clockMs += 1;
    },
    set: (ms) => {
      clockMs = ms;
    },
  };
}

/**
 * `bindSourceTree` plus the shared Git invocation used by the generators. An
 * inherited `GIT_DIR`/index/object override must not redirect the guard, and
 * the executable must be an absolute, workspace-external Git (the shared guard
 * rejects a workspace-local shim). Returns a `git(...args)` bound to the fixed
 * baseline and the provenance record.
 */
export function bindBaseline({ baseline, commit, gitExecutable }) {
  const executable = realpathSync(
    gitExecutable || process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
  );
  const env = Object.fromEntries(
    Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')),
  );
  const git = (...args) =>
    execFileSync(executable, ['--no-replace-objects', '-C', baseline, ...args], {
      env,
      maxBuffer: 32 * 1024 * 1024,
    });
  const provenance = bindSourceTree(baseline, commit, executable);
  // bindSourceTree itself enforces the tracked-tree status and `ls-files -v`
  // checks (#953), so old and new callers share one implementation instead of
  // drifting copies.
  return { git, gitExecutable: executable, sourceCommit: provenance.ref, provenance };
}

/**
 * Refuse a delete outside the run's own mkdtemp tree before touching the
 * filesystem. The historical fixed-path delete (/tmp/pdrs-r9-*) was a P0; a
 * sentinel that actually sits outside the guarded tree is the only witness
 * that can catch a regression, so every `rm` in these generators goes through
 * `removeOwned`.
 */
export function assertInside(root, path, action) {
  const rel = relative(root, path);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`${action} refused: ${path} is outside the run directory ${root}`);
  }
  return path;
}

function removeOne(value, options, remove) {
  return remove(value, { recursive: !!options.recursive, force: true });
}

export async function removeOwned(root, value, options = {}, remove = rm) {
  let path;
  if (typeof value === 'string') {
    path = assertInside(root, resolve(root, value.split('<dir>').join(root)), 'remove');
  } else {
    // The non-UTF-8 probe path is byte-exact, so compare the run-root prefix as
    // bytes instead of decoding a name that is deliberately not UTF-8.
    const prefix = Buffer.from(root.endsWith(sep) ? root : `${root}${sep}`);
    if (!value.subarray(0, prefix.length).equals(prefix)) {
      throw new Error(`remove refused: ${value.toString('hex')} is outside the run directory ${root}`);
    }
    // The byte-prefix check alone accepts `<root>/../outside`. Path segments
    // are ASCII, so the deliberately non-UTF-8 name can be inspected as latin1.
    const rest = value.subarray(prefix.length).toString('latin1').split(sep);
    if (rest.includes('..')) {
      throw new Error(`remove refused: ${value.toString('hex')} escapes the run directory ${root}`);
    }
    path = value;
  }
  await removeOne(path, options, remove);
}

/** Hash only the requested keys, in their original order, preserving old oracles.
 * The full source tree has already been bound by bindBaseline; re-read each
 * recorded file and compare its SHA to the pinned commit blob, never the index.
 */
export function recordedSourceHashes({ baseline, commit, git }, paths) {
  assert.ok(paths.length > 0, '#280 baseline source list must not be empty');
  const hashes = {};
  for (const path of paths) {
    assert.ok(path.startsWith('src/') && path.split('/').every(part => part !== '..' && part !== ''),
      `#280 expected a baseline-relative source path: ${path}`);
    const committed = git('show', `${commit}:${path}`);
    const hash = bytes => createHash('sha256').update(bytes).digest('hex');
    const expected = hash(committed);
    assert.equal(hash(readFileSync(join(baseline, path))), expected, `#280 ${path} differs from ${commit}`);
    hashes[path] = expected;
  }
  return hashes;
}
