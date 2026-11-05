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
               ``invalid path/hash: <name>`` for ui-server-parity; the
               ``../../`` prefix ui-server reports is normalised away)
  acceptable   a first-registration case that --write settles and an added
               event chain already backs: ``A2 apparatus missing FROZEN hash``,
               ``unregistered/missing file: <path>`` (ui-server-parity) and
               ``hash change lacks exact new event chain``.  These are accepted
               only when the added ``Change:`` rows form the *exact* chain from
               the merge-base bytes to the current bytes (each step exactly one
               matching edge, no leftover edge), the same uniqueness rule
               ``check_frozen`` itself enforces.  A merely-present-but-duplicated
               edge is therefore a blocker, not a silent pass.
  blocking     everything else, in particular a hash change with no matching
               event row, unregistered files, unreadable input
  blocking     anything not recognised (fail-closed: an unknown message is a
               blocker, never a silent pass)

A first registration is not always named in the report: parity only says the
hash list differs and lists the files it skipped in ``unregistered_skipped``;
gateway-parity only says the generated inventory differs.  So the tool also
scans the files added on the branch that the apparatus would actually register
(parity: tracked files under ``scripts/parity/`` plus its five fixed spec files;
gateway-parity: a file under its apparatus directories that its own source
names).  Each one needs the same exact ``Change: - <sha256> <path>`` chain,
otherwise it is a blocker: after ``--write`` the registration check would reject
it.  A new file the apparatus would *not* register is ignored -- the real gate
ignores it too, and a chain for it would itself be an unused event row.

On top of the apparatus checks it inspects the event files added on the branch
(``specs/<kind>/frozen/``, merge-base..HEAD): each must carry at least one
``Change:`` row and the four fields ``Calibration-Evidence``,
``Independent-Reviews``, ``Approved-By``, ``Approval-Basis``.  An
``Independent-Reviews: Pending: ...`` placeholder is reported as
``PENDING-REVIEWS`` and is *not* blocking: the coordinator may ``--write``,
while final registration still waits for real reviews.

All evidence is read from HEAD: an event file that was never committed or a
working-tree edit to its ``Change:`` rows cannot make the branch look ready.
A first registration is decided by the merge-base *hash row* (not by the diff
status): anything ``--write`` would add a row for needs an exact
``Change: - <sha256> <path>`` chain, and a path that had no row at the
merge-base starts there from ``-``.  A dirty tracked working tree is blocking:
the apparatus hashes working-tree bytes, so HEAD could otherwise hide behind
uncommitted changes.

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
    print(f'run with -I: python3 -I {__file__}', file=sys.stderr)
    raise SystemExit(2)
if sys.version_info < (3, 11):  # the frozen entries require CPython 3.11+
    print('CPython 3.11+ required (frozen apparatus contract)', file=sys.stderr)
    raise SystemExit(2)
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
UNREGISTERED = re.compile(r'unregistered/missing file: (.+)')
PENDING_PREFIX = 'Pending:'
PENDING_FAMILY_MSG = ('Independent-Reviews requires two distinct families '
                      'and references separated by ;')
EVENT_FIELDS = ('Calibration-Evidence', 'Independent-Reviews', 'Approved-By', 'Approval-Basis')
EVENT_NAME = re.compile(r'[0-9]{8}-[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*\.md')
EVENT_CHANGE = re.compile(r'Change: ([0-9a-f]{64}|-) ([0-9a-f]{64}|-) ([\w./-]+)')
ADDED_EVENT = re.compile(r'specs/([^/]+)/frozen/([^/]+\.md)$')
BARE_PATH = re.compile(r'(?:\.\./\.\./)?[\w./-]+')
BYTECODE = re.compile(r'(?:^|/)__pycache__(?:/|$)|\.py[co]$')
# Where each apparatus's ``--write`` takes its rows from.  parity walks
# ``scripts/parity/`` recursively and takes five fixed top-level files of
# ``specs/parity/``; gateway-parity keeps an explicit inventory in its own
# source, so a new member there must be named by that source to be registrable.
PARITY_SPEC_FILES = ('README.md', 'acceptance.md', 'normalization.md',
                     'accepted-diffs.md', 'multimodal.md')
GATEWAY_SPEC_FILES = ('acceptance.md', 'normalization.md', 'accepted-diffs.md')
GATEWAY_DIRS = ('scripts/gateway-parity/', 'scripts/a5-19/')
CHECK_TIMEOUT = 300
GIT_ERROR = 125  # git()/git_bytes() return this when the call itself failed


# ---------------------------------------------------------------- git helpers
def git(root, *args):
    """Return (returncode, stdout, stderr); never raises for a git failure.

    ``GIT_ERROR`` marks a call that could not run at all, so callers can tell
    "the answer is no" from "there is no answer" (fail-closed instead of
    treating a transient fork/exec failure as a missing file).
    """
    try:
        proc = subprocess.run(['git', '-C', str(root), *args], capture_output=True,
                              text=True, errors='replace', timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        return GIT_ERROR, '', str(error)
    return proc.returncode, proc.stdout, proc.stderr


def git_bytes(root, *args):
    """Like git(), but keeps stdout as bytes (blob content may be binary)."""
    try:
        proc = subprocess.run(['git', '-C', str(root), *args], capture_output=True,
                              timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        return GIT_ERROR, b'', str(error).encode()
    return proc.returncode, proc.stdout, proc.stderr


def merge_base(root, base, head='HEAD'):
    code, out, err = git(root, 'merge-base', head, base)
    if code or not out.strip():
        return None, f'cannot resolve merge-base({head}, {base}): {err.strip() or out.strip()}'
    return out.strip(), None


def exists_at(root, commit, path):
    """True/False/None -- None when the question cannot be answered.

    ``git cat-file -e`` returns the same status for "path absent" and "commit
    unknown", so the commit is resolved first and an uninterrogable tree is an
    error (None), never a silent "not there".
    """
    code, _, _ = git(root, 'rev-parse', '--verify', '--quiet', f'{commit}^{{commit}}')
    if code != 0:
        return None
    code, _, _ = git(root, 'rev-parse', '--verify', '--quiet', f'{commit}:{path}')
    if code == 0:
        return True
    return None if code == GIT_ERROR else False


def blob_at(root, commit, path):
    """(exists, sha256) for <commit>:<path>; sha256 is None when unreadable."""
    state = exists_at(root, commit, path)
    if state is not True:
        return state, None
    code, data, _ = git_bytes(root, 'cat-file', 'blob', f'{commit}:{path}')
    return True, (hashlib.sha256(data).hexdigest() if code == 0 else None)


def base_digest(root, kind, mb, path):
    """sha256 of the merge-base pin for ``path``, '-' when it had no row there.

    ``check_frozen`` starts every registration chain from
    ``old_hashes.get(name, '-')``, where ``old_hashes`` is built from the
    merge-base **manifest rows** -- not from the blobs that happen to exist.
    A file that was already in the tree at the merge-base but only entered the
    apparatus inventory on this branch therefore starts at '-': anchoring on its
    old blob would reject the correct ``Change: -`` chain (false block) and,
    worse, accept a chain the real gate cannot follow (false READY).
    """
    if not mb:
        return None
    state = registered_at(root, kind, path, mb)
    if state is None:
        return None
    if not state:
        return '-'
    state, value = blob_at(root, mb, path)
    if state is None or (state is True and value is None):
        return None
    # A row whose blob is missing at the merge-base is a broken base: the real
    # gate refuses it ("base manifest does not match base blob"), so nothing this
    # branch can write would be deliverable.
    return value if state else None


# ------------------------------------------------------------ manifest rows
ROW_BARE = re.compile(r'[0-9a-f]{16,64}  (\S+)')
ROW_TABLE = re.compile(r'\| `([^`]+)` \| `[0-9a-f]{16,64}[^`]*`.*')
_ROW_CACHE = {}
_SOURCE_CACHE = {}


def manifest_rows(root, kind, commit):
    """Repo-relative paths that have a hash row in ``specs/<kind>/FROZEN.md``.

    All three apparatuses write their ``--write`` rows into that one file
    (gateway/ui as ``<hash>  ../../<path>``, parity as a table cell), so the row
    list -- not the diff status and not the blob -- says what was registered at
    a given commit.  ``None`` means "cannot tell"; callers fail closed.
    """
    key = (str(root), kind, commit)
    if key in _ROW_CACHE:
        return _ROW_CACHE[key]
    code, data, _ = git_bytes(root, 'cat-file', 'blob', f'{commit}:specs/{kind}/FROZEN.md')
    rows = None
    if code == 0:
        rows = set()
        for line in data.decode('utf-8', 'replace').splitlines():
            match = ROW_TABLE.fullmatch(line) or ROW_BARE.fullmatch(line)
            if not match:
                continue
            name = match.group(1)
            if name.startswith('../../'):
                name = name[6:]
            elif not name.startswith(('scripts/', 'specs/')):
                name = f'specs/{kind}/{name}'
            rows.add(name)
    _ROW_CACHE[key] = rows
    return rows


def registered_at(root, kind, path, commit):
    """True/False/None -- did ``path`` have a hash row at ``commit``?"""
    rows = manifest_rows(root, kind, commit)
    return None if rows is None else path in rows


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
    """Map the three report shapes onto (status, failures, reason, unregistered).

    parity is the only apparatus that publishes files it skipped because no
    FROZEN row covers them (``unregistered_skipped``); dropping that field made
    a first registration without an event chain look READY.
    """
    if not isinstance(report, dict):
        return 'FAIL', ['report is not a JSON object'], None, []
    status = report.get('status') or report.get('verdict')
    failures = report.get('failures')
    if failures is None:
        failures = report.get('errors')
    if failures is None:
        failures = []
    if not isinstance(failures, list) or not all(isinstance(item, str) for item in failures):
        return 'FAIL', ['report failures/errors is not a list of strings'], None, []
    if status is None and 'ok' in report:
        status = 'PASS' if report.get('ok') else 'FAIL'
    if status is None:
        return 'FAIL', ['report has no status/verdict/ok field'], None, []
    reason = report.get('reason')
    unregistered = report.get('unregistered_skipped')
    if not isinstance(unregistered, list) or not all(isinstance(item, str) for item in unregistered):
        unregistered = []
    return str(status), failures, reason if isinstance(reason, str) else None, unregistered


# ------------------------------------------------------------ event evidence
def event_evidence(root, kind, mb):
    """Event rows added on the branch (HEAD), plus Pending: placeholders.

    Returns (edges, pending, issues); ``edges`` are (path, old, new) from event
    files that exist at HEAD and not at the merge-base, exactly the events
    check_frozen.check_registration treats as new.  A working-tree event file
    that was never committed is *not* evidence: the branch is what gets
    received, and a false READY there is what this precheck exists to stop.
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
        if mb:
            state = exists_at(root, mb, relative)
            if state is None:
                issues.append(f'{relative}: cannot tell whether it exists at the merge-base')
                continue
            if state:
                continue  # historical event; append-only, not this delivery's
        head = exists_at(root, 'HEAD', relative)
        if head is None:
            issues.append(f'{relative}: cannot tell whether it is committed')
            continue
        if not head:
            issues.append(f'{relative}: event file is not committed '
                          '(git add/commit required before delivery)')
            continue
        # Content also comes from HEAD: a working-tree edit that adds a Change
        # row must not complete a chain the delivered branch does not contain.
        code, data, _ = git_bytes(root, 'cat-file', 'blob', f'HEAD:{relative}')
        if code:
            issues.append(f'{relative}: cannot read from HEAD')
            continue
        try:
            text = data.decode('utf-8')
        except UnicodeError as error:
            issues.append(f'{relative}: cannot decode as UTF-8: {error}')
            continue
        reviews = re.findall(r'^Independent-Reviews:([^\n]*)$', text, re.M)
        if len(reviews) == 1 and reviews[0].strip().startswith(PENDING_PREFIX):
            pending.add(path.name)
        for line in text.splitlines():
            match = EVENT_CHANGE.fullmatch(line)
            if match:
                edges.append((path.name, match.group(3), match.group(1), match.group(2)))
    return edges, pending, issues


def chain_backed(root, kind, edges, path, mb):
    """True when the added event rows form the *exact* chain check_frozen demands.

    Mirrors ``check_frozen.check_registration``: from the merge-base row, each
    step needs exactly one matching edge (``old == current``), every edge is used
    at most once, no edge may remain unused, and the path must actually differ
    from the merge-base.  A bare "one matching edge exists" test would accept
    duplicate/extra edges that ``--write`` cannot clear (a false PASS).

    Bytes come from HEAD, not the working tree: an uncommitted edit must not be
    able to complete a chain the branch does not contain.
    """
    state, actual = blob_at(root, 'HEAD', path)
    if state is not True or actual is None:
        return False
    if not mb:
        return False
    base = base_digest(root, kind, mb, path)
    if base is None or base == actual:
        return False
    remaining = [(old, new) for _, name, old, new in edges if name == path]
    current = base
    visited = set()
    while current != actual and current not in visited:
        visited.add(current)
        matches = [edge for edge in remaining if edge[0] == current]
        if len(matches) != 1:
            return False
        edge = matches[0]
        remaining.remove(edge)
        current = edge[1]
    return current == actual and not remaining


# ------------------------------------------------------------ classification
def strip_reported(name):
    """Canonicalise a reported path, dropping ui-server's ``../../`` prefix.

    ui-server-parity names every manifest row relative to ``specs/<kind>/`` and
    reports failures as ``../../scripts/...``; gateway-parity reports the same
    repo-relative shape with the prefix already stripped.  Both normalise here so
    the three apparatuses share one resolution rule.
    """
    name = name.strip()
    if name.startswith('../../'):
        name = name[6:]
    if not name or Path(name).is_absolute() or '..' in Path(name).parts:
        return None
    return name


def spec_path(root, kind, name):
    """Resolve a manifest name the way the apparatus reports it, or None."""
    relative = strip_reported(name)
    if relative is None:
        return None
    if relative.startswith(('scripts/', 'specs/')):
        return root / relative
    return root / 'specs' / kind / relative


def head_tracked(root, path):
    """True when ``path`` is a file committed at HEAD (working tree aside)."""
    try:
        relative = Path(path).relative_to(root).as_posix()
    except ValueError:
        return False
    return exists_at(root, 'HEAD', relative) is True


def existing_file(root, kind, name):
    path = spec_path(root, kind, name)
    return (path is not None and path.is_file() and not path.is_symlink()
            and head_tracked(root, path))


def bare_manifest_file(root, kind, name):
    """gateway-parity reports a stale pin as just the manifest row name."""
    if not BARE_PATH.fullmatch(name):
        return False
    relative = name[6:] if name.startswith('../../') else f'specs/{kind}/{name}'
    if Path(relative).is_absolute() or '..' in Path(relative).parts:
        return False
    path = root / relative
    return path.is_file() and not path.is_symlink() and head_tracked(root, path)


def source_text(root, commit, path):
    """Text of ``commit:path``, or None when it cannot be read."""
    key = ('source', str(root), commit, path)
    if key in _SOURCE_CACHE:
        return _SOURCE_CACHE[key]
    code, data, _ = git_bytes(root, 'cat-file', 'blob', f'{commit}:{path}')
    value = data.decode('utf-8', 'replace') if code == 0 else None
    _SOURCE_CACHE[key] = value
    return value


def gateway_inventory(root, commit='HEAD'):
    """Paths gateway-parity's ``--write`` would give a FROZEN row.

    Its inventory is a literal in the apparatus source (``manifest_inventory``
    over ``A2_SCRIPTS`` plus two session fixtures and five a5-19 files), *not*
    every file the source mentions: ``A3_SCRIPTS`` and ``DATA_FILES`` are part of
    the unregistered-file whitelist but get no hash row, so a basename match
    over the sources demands chains for files ``--write`` never registers (a
    dead end).  ``None`` means the shape was not recognised; callers fall back
    to the coarse match, which errs towards blocking.
    """
    key = ('gateway-inventory', str(root), commit)
    if key in _SOURCE_CACHE:
        return _SOURCE_CACHE[key]
    inventory = None
    common = source_text(root, commit, 'scripts/gateway-parity/common.py')
    frozen = source_text(root, commit, 'scripts/gateway-parity/check_frozen.py')
    if common is not None and frozen is not None:
        scripts = re.search(r'^A2_SCRIPTS\s*=\s*\((.*?)\)', common, re.S | re.M)
        body = re.search(r'^def manifest_inventory\(.*?(?=^def |\Z)', frozen, re.S | re.M)
        if scripts and body:
            literals = re.findall(r"'([^']+)'", body.group(0))
            prefixes = {item for item in literals if item.endswith('/')}
            full = {item for item in literals if '/' in item and not item.endswith('/')}
            bare = {item for item in literals if '/' not in item and '.' in item}
            inventory = set(full)
            inventory.update(f'specs/gateway-parity/{name}' for name in bare)
            inventory.update(f'scripts/gateway-parity/{name}'
                             for name in re.findall(r"'([^']+)'", scripts.group(1)))
            inventory.update(prefix + name for prefix in prefixes for name in bare)
    _SOURCE_CACHE[key] = inventory
    return inventory


def registrable(root, kind, path):
    """True when this apparatus's ``--write`` would add a FROZEN row for ``path``.

    Only such a path can be a first registration: accepting a chain for anything
    else blesses a failure ``--write`` cannot clear (parity keeps five fixed
    spec files and ``git ls-files scripts/parity/``; gateway-parity's inventory
    is written out in its own source, so a new member is registrable only when
    that source names it).
    """
    if kind == 'parity':
        if path in {f'specs/parity/{name}' for name in PARITY_SPEC_FILES}:
            return True
        if not path.startswith('scripts/parity/'):
            return False
        return tracked_at_head(root, path)
    if kind == 'gateway-parity':
        if path in {f'specs/gateway-parity/{name}' for name in GATEWAY_SPEC_FILES}:
            return True
        if not path.startswith(GATEWAY_DIRS):
            return False
        inventory = gateway_inventory(root, 'HEAD')
        if inventory is not None:
            return path in inventory
        name = Path(path).name
        for source in ('scripts/gateway-parity/common.py',
                       'scripts/gateway-parity/check_frozen.py'):
            text = source_text(root, 'HEAD', source)
            if text is not None and name in text:
                return True
        return False
    return True  # ui-server-parity names the required files in its own report


def tracked_at_head(root, path):
    """True when ``path`` is tracked in HEAD's tree (one ls-tree per prefix)."""
    prefix = path.split('/', 1)[0] + '/' if '/' in path else path
    key = (str(root), prefix)
    if key not in _SOURCE_CACHE:
        code, out, _ = git(root, 'ls-tree', '-r', '--name-only', 'HEAD', '--', prefix)
        _SOURCE_CACHE[key] = set(out.splitlines()) if code == 0 else None
    listing = _SOURCE_CACHE[key]
    return listing is not None and path in listing


def unregistered_added(root, kind, mb):
    """Paths ``--write`` would add a FROZEN row for, that had no row at the base.

    A first registration is a *row* question, not a diff-status question: a file
    that already existed at the merge-base but only entered the apparatus
    inventory on this branch is not an ``A`` entry (and a file moved into a
    registrable directory is ``R`` under git's default rename detection), yet
    ``--write`` adds a row for both.  Missing them means the failure list holds
    only "the hash list differs", which reads as READY, while ``check_frozen
    --base`` rejects the result right after ``--write``.

    Files the apparatus would not register are ignored: they are inert for the
    real gate, and demanding an event row for them would be a dead end (an
    unused event row is itself a failure).
    """
    if not mb:
        return [], []
    added, issues = [], []
    prefixes = {'parity': ('scripts/parity/',),
                'gateway-parity': GATEWAY_DIRS}.get(kind, ())
    for prefix in prefixes:
        code, out, err = git(root, 'ls-tree', '-r', '--name-only', '-z',
                             'HEAD', '--', prefix)
        if code:
            issues.append(f'cannot list files under {prefix}: {err.strip()}')
            continue
        for name in filter(None, out.split('\0')):
            if BYTECODE.search(name):
                continue
            state = registered_at(root, kind, name, mb)
            if state is None:
                issues.append(f'{name}: cannot tell whether it had a hash row at the base')
                continue
            if state or not registrable(root, kind, name):
                continue
            added.append(name)
    return sorted(set(added)), issues


def classify(ctx, item):
    """'ok' | 'pending' | ('block', why, detail_or_None)."""
    root, kind, mb, edges = ctx['root'], ctx['kind'], ctx['mb'], ctx['edges']
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
        unbacked = []
        for name in match.group(1).split(','):
            if not name.strip():
                continue
            relative = strip_reported(name)
            if not relative or not chain_backed(root, kind, edges, relative, mb):
                unbacked.append(relative or name.strip())
        if not unbacked:
            return 'ok', None, None
        return 'block', 'blocking (no matching Change: - <hash> row)', ', '.join(unbacked)
    # ui-server-parity reports a required-but-unregistered file the same way; a
    # first registration is acceptable only when the file exists and is backed.
    match = UNREGISTERED.fullmatch(item)
    if match:
        relative = strip_reported(match.group(1))
        if relative and chain_backed(root, kind, edges, relative, mb):
            return 'ok', None, None
        return 'block', 'blocking', None
    match = LACKS_CHAIN.match(item)
    if match:
        name = match.group(1)
        if registrable(root, kind, name) and chain_backed(root, kind, edges, name, mb):
            return 'ok', None, None
        return 'block', 'blocking', None
    # A Pending: placeholder can fail the two-family rule; that is expected.
    match = re.fullmatch(r'frozen/([^/:]+): ' + re.escape(PENDING_FAMILY_MSG), item)
    if match and match.group(1) in ctx['pending']:
        return 'pending', None, None
    # Recognised block markers win over a bare "hash list differs" substring so a
    # composite message can never be silently accepted.
    for marker in BLOCK_MARKERS:
        if marker in item:
            return 'block', 'blocking', None
    if ACCEPT_HASH_LIST in item:
        return 'ok', None, None
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
                              capture_output=True, text=True, errors='replace',
                              cwd=str(root), timeout=CHECK_TIMEOUT)
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
    status, failures, reason, unregistered = normalise(report)
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
    if status == 'FAIL' and not failures:
        # A FAIL with no detail would otherwise fall through as READY (fail-open).
        result['blocking'].append(('blocking', 'apparatus reported FAIL with an empty '
                                               'failures/errors list (fail-closed)', None))
    for item in failures:
        verdict, why, detail = classify(ctx, item)
        if verdict == 'ok':
            result['reducible'].append(item)
        elif verdict == 'pending':
            result['pending'].append(item)
        else:
            result['blocking'].append((why, item, detail))
    # First registrations: the apparatus lists them only as "the hash list
    # differs" (or not at all), so check every new file in its --write scope and
    # everything parity skipped for lack of a FROZEN row.  Each needs the exact
    # ``Change: - <sha256>`` chain --write will rely on.
    new_files, new_issues = unregistered_added(root, kind, mb)
    result['blocking'].extend(('blocking', message, None) for message in new_issues)
    reported = set()
    for name in sorted(set(new_files) | set(unregistered)):
        if BYTECODE.search(name) or name in reported:
            continue  # bytecode is already an apparatus error
        if not registrable(root, kind, name):
            continue  # --write ignores it; demanding a chain would be a dead end
        reported.add(name)
        if exists_at(root, 'HEAD', name) is not True:
            result['blocking'].append(('blocking', f'{name}: unregistered file is not '
                                                   'committed (git add/commit required)', None))
        elif not chain_backed(root, kind, edges, name, mb):
            result['blocking'].append(('blocking', f'{name}: first registration without an '
                                                   'exact Change: - <sha256> event chain '
                                                   '(--write would register it and check_frozen '
                                                   '--base would then reject)', None))
    return result


def event_chain_issues(root, base, kinds, mb=None):
    """Validate event files added between merge-base(HEAD, base) and HEAD."""
    issues = []          # (kind_or_None, message)
    added = []
    if mb is None:
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
        # Read the committed bytes, not the working tree: the delivery is HEAD.
        code, data, err = git_bytes(root, 'cat-file', 'blob', f'HEAD:{path}')
        if code:
            issues.append((kind, f'{path}: cannot read from HEAD: {err.decode(errors="replace").strip()}'))
            continue
        try:
            text = data.decode('utf-8')
        except UnicodeError as error:
            issues.append((kind, f'{path}: cannot decode as UTF-8: {error}'))
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

    print(f'check-frozen-delivery: root={root}')

    # Pin the SHA the run was started on and compare it at the end: evidence and
    # target bytes are read from HEAD, and the child apparatus reads HEAD as
    # well, so a commit landing mid-run (``--amend`` in the audited worker tree)
    # would let event rows from one commit back bytes from another -- a false
    # READY that is not fixed by pinning only this process's reads.  The
    # merge-base is computed from this pinned SHA for the same reason.
    code, head_before, head_err = git(root, 'rev-parse', '--verify', 'HEAD')
    head_before = head_before.strip()
    if code or not head_before:
        print('=== head === BLOCKED')
        print('  BLOCK[blocking] cannot resolve HEAD: '
              f'{head_err.strip() or "<empty>"}')
        print('BLOCKED 1 / READY 0')
        return 1

    mb, mb_error = merge_base(root, args.base, head_before)
    print(f'  base={args.base} merge-base={mb or "<unresolved>"}')
    if mb_error:
        # Never fall back to mb=None: chain_backed would degrade the base to '-'
        # and widen acceptance to existing files.
        print('=== base === BLOCKED')
        print(f'  BLOCK[blocking] {mb_error}')
        print('BLOCKED 1 / READY 0')
        return 1

    # Uncommitted tracked changes are blocking, not a note: the apparatus hashes
    # *working-tree* bytes, so a dirty tree can hide a HEAD side change (or the
    # reverse) and yield READY for a branch CI will reject.  Untracked files are
    # invisible to the apparatus, so they stay a note.
    code, dirty, dirty_err = git(root, 'status', '--porcelain', '--untracked-files=no')
    tree_block = None
    if code != 0:
        tree_block = ('cannot check working-tree cleanliness, so HEAD could be hiding '
                      'behind uncommitted bytes: '
                      + (dirty_err.strip() or f'git status rc={code}'))
    elif dirty.strip():
        tree_block = ('working tree has uncommitted tracked changes; the apparatus hashes '
                      'working-tree bytes, so HEAD is not what gets judged: '
                      + '; '.join(sorted(line.strip() for line in dirty.splitlines())[:5]))

    # Pin the resolved SHA: pass it (not the symbolic ref) to every apparatus so
    # a concurrent ``git fetch`` cannot move the base between classification and
    # the child check_frozen runs.  event_chain_issues reuses the same SHA.
    results = [run_apparatus(root, kind, entry, mb, mb)
               for kind, entry in apparatuses]
    issues, added, _ = event_chain_issues(root, mb, kinds, mb=mb)

    if tree_block:
        results.insert(0, {'kind': 'worktree',
                           'blocking': [('blocking', tree_block, None)],
                           'reducible': [], 'pending': [], 'pending_events': [],
                           'note': None})

    code, head_after, _ = git(root, 'rev-parse', '--verify', 'HEAD')
    head_after = head_after.strip() if code == 0 else ''
    if head_after != head_before:
        results.append({'kind': 'snapshot',
                        'blocking': [('blocking',
                                      f'HEAD moved during the run ({head_before[:12]} -> '
                                      f'{head_after[:12] or "<unreadable>"}): event evidence and '
                                      'target bytes would come from different commits -- re-run',
                                      None)],
                        'reducible': [], 'pending': [], 'pending_events': [], 'note': None})

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
