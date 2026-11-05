#!/usr/bin/env python3
"""Delivery precheck for frozen apparatus changes: is the event chain in place?

Answers one question before a branch goes to the coordinator for
``check_frozen.py --write``: does every byte changed on this branch have the
registration event that explains it?

``check_frozen.py --base origin/main`` *always* fails before ``--write`` (the
old pins no longer match the new bytes), so a bare FAIL says nothing.  This
tool runs each apparatus check, parses the JSON report and classifies every
failure:

  acceptable   ``hash list differs from generated inventory``  (--write fixes it)
  acceptable   a stale pin for an existing file (bare manifest path for
               gateway-parity, ``<name>: hash mismatch`` for parity,
               ``invalid path/hash: <name>`` for ui-server-parity)
  acceptable   a first-registration case that --write settles and an added
               event row already backs: ``A2 apparatus missing FROZEN hash``,
               and ``hash change lacks exact new event chain`` for a file that
               is absent at the merge-base (the current, stale manifest cannot
               list it yet).  Both are only accepted when an event row
               ``Change: - <sha256> <path>`` matches the current bytes.
  acceptable   the same ``lacks exact new event chain`` for an event file that
               still carries a ``Pending:`` review placeholder: check_frozen
               skips such an event before reading its Change rows, so the
               chain is verified here directly from those rows.
  blocking     everything else, in particular a hash change with no matching
               event row, unregistered files, unreadable input
  blocking     anything not recognised (fail-closed: an unknown message is a
               blocker, never a silent pass)

On top of the apparatus checks it inspects the event files added on the branch
(``specs/<kind>/frozen/``, merge-base..HEAD): each must carry at least one
``Change:`` row and the four fields ``Calibration-Evidence``,
``Independent-Reviews``, ``Approved-By``, ``Approval-Basis``.  An
``Independent-Reviews: Pending: ...`` placeholder is reported as
``PENDING-REVIEWS`` and is *not* blocking: the coordinator may ``--write``,
while final registration still waits for real reviews.

Read-only.  It never runs ``--write`` and never edits the tree.

Usage:
    python3 -I scripts/check-frozen-delivery.py [--root PATH] [--base REF]

Exit status: 0 only when nothing is blocking (only --write-reducible items
remain); 1 when any blocker exists; 2 on usage/environment errors.
"""
import sys

# Same isolation rule as the frozen entries this tool calls: keep PYTHONPATH,
# user site and the script directory out of module resolution.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')
if sys.version_info < (3, 11):  # the frozen entries require CPython 3.11+
    raise SystemExit('CPython 3.11+ required (frozen apparatus contract)')
sys.dont_write_bytecode = True

import argparse
import hashlib
import json
import re
import subprocess
from pathlib import Path

ACCEPT_HASH_LIST = 'hash list differs from generated inventory'
BLOCK_MARKERS = ('lacks exact new event chain', 'missing', 'unregistered', 'cannot')
LACKS_CHAIN = re.compile(r'(.+?): hash change lacks exact new event chain')
A2_MISSING = re.compile(r'A2 apparatus missing FROZEN hash: (.+)')
PENDING_PREFIX = 'Pending:'
PENDING_FAMILY_MSG = ('Independent-Reviews requires two distinct families '
                      'and references separated by ;')
EVENT_FIELDS = ('Calibration-Evidence', 'Independent-Reviews', 'Approved-By', 'Approval-Basis')
EVENT_NAME = re.compile(r'[0-9]{8}-[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*\.md')
EVENT_CHANGE = re.compile(r'Change: ([0-9a-f]{64}|-) ([0-9a-f]{64}|-) ([\w./-]+)')
ADDED_EVENT = re.compile(r'specs/([^/]+)/frozen/([^/]+\.md)$')
BARE_PATH = re.compile(r'(?:\.\./\.\./)?[\w./-]+')
CHECK_TIMEOUT = 300


# ---------------------------------------------------------------- git helpers
def git(root, *args):
    """Return (returncode, stdout, stderr); never raises for a git failure."""
    try:
        proc = subprocess.run(['git', '-C', str(root), *args], capture_output=True,
                              text=True, timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        return 1, '', str(error)
    return proc.returncode, proc.stdout, proc.stderr


def git_bytes(root, *args):
    """Like git(), but keeps stdout as bytes (blob content may be binary)."""
    try:
        proc = subprocess.run(['git', '-C', str(root), *args], capture_output=True,
                              timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        return 1, b'', str(error).encode()
    return proc.returncode, proc.stdout, proc.stderr


def merge_base(root, base):
    code, out, err = git(root, 'merge-base', 'HEAD', base)
    if code or not out.strip():
        return None, f'cannot resolve merge-base(HEAD, {base}): {err.strip() or out.strip()}'
    return out.strip(), None


def exists_at(root, commit, path):
    code, _, _ = git(root, 'cat-file', '-e', f'{commit}:{path}')
    return code == 0


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def base_digest(root, mb, path):
    """sha256 of <mb>:<path>, or '-' when the file does not exist there."""
    if not exists_at(root, mb, path):
        return '-'
    code, data, _ = git_bytes(root, 'cat-file', 'blob', f'{mb}:{path}')
    return hashlib.sha256(data).hexdigest() if code == 0 else None


# ------------------------------------------------------------ discovery
def discover(root):
    """Apparatuses are discovered from the tree: specs/<kind>/FROZEN.md.

    Returns [(kind, entry_path_or_None)].  A FROZEN.md without an entry is
    still reported (as blocking) instead of being silently skipped.
    """
    specs = root / 'specs'
    found = []
    if not specs.is_dir():
        return found
    for folder in sorted(specs.iterdir()):
        if not folder.is_dir() or not (folder / 'FROZEN.md').is_file():
            continue
        entry = root / 'scripts' / folder.name / 'check_frozen.py'
        found.append((folder.name, entry if entry.is_file() else None))
    return found


# ------------------------------------------------------------ report parsing
def normalise(report):
    """Map the three report shapes onto (status, failures, reason)."""
    if not isinstance(report, dict):
        return 'FAIL', ['report is not a JSON object'], None
    status = report.get('status') or report.get('verdict')
    failures = report.get('failures')
    if failures is None:
        failures = report.get('errors')
    if failures is None:
        failures = []
    if not isinstance(failures, list) or not all(isinstance(item, str) for item in failures):
        return 'FAIL', ['report failures/errors is not a list of strings'], None
    if status is None and 'ok' in report:
        status = 'PASS' if report.get('ok') else 'FAIL'
    if status is None:
        return 'FAIL', ['report has no status/verdict/ok field'], None
    reason = report.get('reason')
    return str(status), failures, reason if isinstance(reason, str) else None


# ------------------------------------------------------------ event evidence
def event_evidence(root, kind, mb):
    """Event rows added since the merge-base, plus Pending: placeholders.

    Returns (edges, pending, issues); ``edges`` are (path, old, new) from event
    files that do not exist at the merge-base, exactly the events
    check_frozen.check_registration treats as new.
    """
    folder = root / 'specs' / kind / 'frozen'
    edges, pending, issues = [], set(), []
    if folder.is_symlink():
        return edges, pending, [f'specs/{kind}/frozen: directory is a symlink']
    if not folder.is_dir():
        return edges, pending, issues
    for path in sorted(folder.iterdir()):
        if not path.is_file() or path.is_symlink() or not EVENT_NAME.fullmatch(path.name):
            continue
        relative = path.relative_to(root).as_posix()
        if mb and exists_at(root, mb, relative):
            continue  # historical event; append-only, not this delivery's
        try:
            text = path.read_text(encoding='utf-8')
        except (OSError, UnicodeError) as error:
            issues.append(f'{relative}: cannot read: {error}')
            continue
        reviews = re.findall(r'^Independent-Reviews:([^\n]*)$', text, re.M)
        if len(reviews) == 1 and reviews[0].strip().startswith(PENDING_PREFIX):
            pending.add(path.name)
        for line in text.splitlines():
            match = EVENT_CHANGE.fullmatch(line)
            if match:
                edges.append((path.name, match.group(3), match.group(1), match.group(2)))
    return edges, pending, issues


def chain_backed(root, edges, path, mb, pending=frozenset(), *, pending_only=False):
    """True when an added event row bridges the base bytes to the current bytes.

    ``pending_only`` restricts the evidence to event files that still carry a
    ``Pending:`` placeholder.  A valid event is already chain-checked by
    check_frozen itself; only a placeholder (which makes check_frozen skip the
    event before reading its Change rows) needs this independent verification.
    """
    target = root / path
    if not target.is_file() or target.is_symlink():
        return False
    try:
        actual = digest(target)
    except OSError:
        return False
    base = base_digest(root, mb, path) if mb else '-'
    if base is None:
        return False
    for event, name, old, new in edges:
        if name == path and old == base and new == actual and not (
                pending_only and event not in pending):
            return True
    return False


# ------------------------------------------------------------ classification
def spec_path(root, kind, name):
    """Resolve a manifest name the way the apparatus reports it, or None."""
    if not name or Path(name).is_absolute() or '..' in Path(name).parts:
        return None
    if name.startswith(('scripts/', 'specs/')):
        return root / name
    return root / 'specs' / kind / name


def existing_file(root, kind, name):
    path = spec_path(root, kind, name)
    return path is not None and path.is_file() and not path.is_symlink()


def bare_manifest_file(root, kind, name):
    """gateway-parity reports a stale pin as just the manifest row name."""
    if not BARE_PATH.fullmatch(name):
        return False
    relative = name[6:] if name.startswith('../../') else f'specs/{kind}/{name}'
    if Path(relative).is_absolute() or '..' in Path(relative).parts:
        return False
    path = root / relative
    return path.is_file() and not path.is_symlink()


def classify(ctx, item):
    """'ok' | 'pending' | ('block', why, detail_or_None)."""
    root, kind, mb, edges = ctx['root'], ctx['kind'], ctx['mb'], ctx['edges']
    if ACCEPT_HASH_LIST in item:
        return 'ok', None, None
    # parity: "<name>: hash mismatch"; ui-server-parity: "invalid path/hash: <name>".
    for pattern in (r'(.+): hash mismatch', r'invalid path/hash: (.+)'):
        match = re.fullmatch(pattern, item)
        if match and existing_file(root, kind, match.group(1)):
            return 'ok', None, None
    # gateway-parity: the manifest row name itself, unprefixed.
    if ' ' not in item and ':' not in item and bare_manifest_file(root, kind, item):
        return 'ok', None, None
    # First registration: --write adds the row; the added event row must back it.
    match = A2_MISSING.fullmatch(item)
    if match:
        paths = [name.strip()[6:] if name.strip().startswith('../../') else name.strip()
                 for name in match.group(1).split(',') if name.strip()]
        unbacked = [name for name in paths
                    if not chain_backed(root, edges, name, mb, ctx['pending'])]
        if not unbacked:
            return 'ok', None, None
        return 'block', 'blocking (no matching Change: - <hash> row)', ', '.join(unbacked)
    match = LACKS_CHAIN.match(item)
    if match:
        name = match.group(1)
        if mb and not exists_at(root, mb, name) \
                and chain_backed(root, edges, name, mb, ctx['pending']):
            return 'ok', None, None  # new file, correctly backed; --write closes it
        # A Pending: placeholder makes check_frozen skip the event before its
        # Change rows are read, so verify that chain here instead.
        if chain_backed(root, edges, name, mb, ctx['pending'], pending_only=True):
            return 'ok', None, None
        return 'block', 'blocking', None
    # A Pending: placeholder can fail the two-family rule; that is expected.
    match = re.fullmatch(r'frozen/([^/:]+): ' + re.escape(PENDING_FAMILY_MSG), item)
    if match and match.group(1) in ctx['pending']:
        return 'pending', None, None
    for marker in BLOCK_MARKERS:
        if marker in item:
            return 'block', 'blocking', None
    if 'merge-base == HEAD' in item:
        return 'block', 'blocking (branch has no commits of its own vs base)', None
    return 'block', 'blocking (unclassified, fail-closed)', None


# ------------------------------------------------------------ the two checks
def run_apparatus(root, kind, entry, base, mb):
    """Run one check_frozen.py --base and classify its failures."""
    result = {'kind': kind, 'blocking': [], 'reducible': [], 'pending': [],
              'pending_events': [], 'note': None}
    if entry is None:
        result['blocking'].append(('blocking', f'specs/{kind}/FROZEN.md exists but '
                                               f'scripts/{kind}/check_frozen.py is absent', None))
        return result
    try:
        proc = subprocess.run([sys.executable, '-I', str(entry), '--base', base],
                              capture_output=True, text=True, cwd=str(root),
                              timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        result['blocking'].append(('blocking', f'cannot run {entry}: {error}', None))
        return result
    try:
        report = json.loads(proc.stdout)
    except (ValueError, TypeError):
        tail = (proc.stderr or proc.stdout).strip().splitlines()[-3:]
        result['blocking'].append(('blocking', 'check_frozen did not emit parseable JSON '
                                               f'(rc={proc.returncode}): ' + ' | '.join(tail), None))
        return result
    status, failures, reason = normalise(report)
    result['note'] = reason
    edges, pending, issues = event_evidence(root, kind, mb)
    result['pending_events'] = sorted(pending)
    result['blocking'].extend(('blocking', message, None) for message in issues)
    ctx = {'root': root, 'kind': kind, 'mb': mb, 'edges': edges, 'pending': pending}
    if status == 'DRAFT':
        result['blocking'].append(('blocking', 'apparatus is DRAFT (no approved FROZEN.md)', None))
        return result
    if status not in ('PASS', 'FAIL'):
        result['blocking'].append(('blocking', f'unrecognised apparatus status {status!r}', None))
        return result
    for item in failures:
        verdict, why, detail = classify(ctx, item)
        if verdict == 'ok':
            result['reducible'].append(item)
        elif verdict == 'pending':
            result['pending'].append(item)
        else:
            result['blocking'].append((why, item, detail))
    return result


def event_chain_issues(root, base, kinds):
    """Validate event files added between merge-base(HEAD, base) and HEAD."""
    issues = []          # (kind_or_None, message)
    added = []
    mb, error = merge_base(root, base)
    if error:
        return [('event-chain', error)], added, None
    code, out, err = git(root, 'diff', '--name-status', '--no-renames', mb, 'HEAD', '--', 'specs')
    if code:
        return [('event-chain', f'cannot diff specs/: {err.strip()}')], added, mb
    for line in out.splitlines():
        parts = line.split('\t')
        if len(parts) != 2:
            continue  # renames/other shapes are not new event files
        status, path = parts
        match = ADDED_EVENT.fullmatch(path)
        if status != 'A' or not match or not EVENT_NAME.fullmatch(match.group(2)):
            continue
        kind = match.group(1)
        added.append((kind, path))
        try:
            text = (root / path).read_text(encoding='utf-8')
        except (OSError, UnicodeError) as error:
            issues.append((kind, f'{path}: cannot read: {error}'))
            continue
        if not any(line.startswith('Change:') for line in text.splitlines()):
            issues.append((kind, f'{path}: missing Change: row'))
        for field in EVENT_FIELDS:
            values = re.findall(r'^' + field + r':([^\n]*)$', text, re.M)
            if not values:
                issues.append((kind, f'{path}: missing field {field}'))
            elif len(values) > 1:
                issues.append((kind, f'{path}: duplicate field {field}'))
            elif not values[0].strip():
                issues.append((kind, f'{path}: empty field {field}'))
    for kind in sorted({kind for kind, _ in added if kind not in kinds}):
        issues.append((None, f'event file under specs/{kind}/frozen/ but no such apparatus'))
    return issues, added, mb


# ------------------------------------------------------------ output
def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter,
                                     allow_abbrev=False)
    parser.add_argument('--root', help='branch/worktree under audit (default: git top level of cwd)')
    parser.add_argument('--base', default='origin/main', help='freeze base (default: origin/main)')
    parser.add_argument('--show-reducible', action='store_true',
                        help='also list the failures --write will clear')
    args = parser.parse_args(argv)

    if args.root:
        root = Path(args.root).resolve()
    else:
        code, out, _ = git(Path.cwd(), 'rev-parse', '--show-toplevel')
        if code or not out.strip():
            parser.error('not inside a git work tree; pass --root')
        root = Path(out.strip()).resolve()
    if not root.is_dir():
        parser.error(f'--root is not a directory: {root}')

    apparatuses = discover(root)
    if not apparatuses:
        print(f'check-frozen-delivery: no specs/*/FROZEN.md under {root}')
        print('BLOCKED 1 / READY 0')
        return 1
    kinds = {kind for kind, _ in apparatuses}

    mb, mb_error = merge_base(root, args.base)
    print(f'check-frozen-delivery: root={root}')
    print(f'  base={args.base} merge-base={mb or "<unresolved>"}')

    results = [run_apparatus(root, kind, entry, args.base, mb)
               for kind, entry in apparatuses]
    issues, added, _ = event_chain_issues(root, args.base, kinds)

    by_kind = {result['kind']: result for result in results}
    for kind, message in issues:
        target = by_kind.get(kind)
        if target is not None:
            target['blocking'].append(('blocking', message, None))
        else:
            results.append({'kind': 'event-chain', 'blocking': [('blocking', message, None)],
                            'reducible': [], 'pending': [], 'pending_events': [], 'note': None})

    blocked = ready = 0
    for result in results:
        blocked += bool(result['blocking'])
        ready += not result['blocking']
        label = 'BLOCKED' if result['blocking'] else 'READY'
        suffix = (f'  ({len(result["reducible"])} --write-reducible, acceptable pre-write)'
                  if result['reducible'] else '')
        print(f'=== {result["kind"]} === {label}{suffix}')
        if result.get('note'):
            print(f'  note: {result["note"]}')
        for why, item, detail in result['blocking']:
            print(f'  BLOCK[{why}] {item}' + (f'  -> {detail}' if detail else ''))
        for item in result['pending']:
            print(f'  PENDING-REVIEWS {item}')
        for name in result['pending_events']:
            print(f'  PENDING-REVIEWS specs/{result["kind"]}/frozen/{name}: '
                  'Independent-Reviews is still a Pending: placeholder (final registration '
                  'needs real reviews)')
        if args.show_reducible:
            for item in result['reducible']:
                print(f'  reducible  {item}')

    print('=== event files added vs merge-base ===')
    if added:
        for kind, path in added:
            print(f'  A {path}')
    else:
        print('  (none)')

    print(f'BLOCKED {blocked} / READY {ready}')
    return 1 if blocked else 0


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        raise SystemExit(130)
