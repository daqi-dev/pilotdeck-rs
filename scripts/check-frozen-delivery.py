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
(both apparatuses: every tracked file under ``scripts/<kind>/`` and
``specs/<kind>/`` that the apparatus's own source says it would register --
parity's ``manifest_inventory.names``, gateway's ``manifest_inventory``
literals).  A manifest name that points outside those directories is scanned
too, because ``--write`` registers it exactly as written.  Each one needs the
same exact ``Change: - <sha256> <path>`` chain,
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

Read-only with a verifiable write scope.  It never runs ``--write`` and never
writes into the audited worktree: every child apparatus runs in a throwaway
checkout of the pinned commit (``git worktree add --detach``), so neither a
moving ``HEAD`` nor a file edit in the audited tree can reach it.  The scope is
*observed* on two channels, not asserted in general: the audited tree's ``git
status`` (tracked and untracked) is compared before and after the run, and the
git worktree bookkeeping is sampled before and after each child.  A changed
audited tree, an unreadable bookkeeping sample, or this run's own snapshot
record surviving is blocking.  A *new* worktree entry that appears while a
child runs is reported as a diagnostic note, not a blocker: git records no
creator, so a concurrent sibling coordinator's ``git worktree add`` and a child
reaching out of its snapshot are indistinguishable, and a false BLOCK for the
former is the failure this channel must avoid.  Writes a child makes elsewhere
(``$HOME``, some other ``$TMPDIR`` path) are outside what a wrapper can see;
the promise is about the audited tree and this repo's worktree records.  The
only writes outside temporary directories are git's own worktree bookkeeping
under ``.git/worktrees``, which is removed again before the tool exits; if that
removal cannot be verified the run is blocking, never silently READY.

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
import ast
import atexit
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import tempfile
import time
from collections import Counter
from pathlib import Path, PurePosixPath

# The only hash-list message the three apparatuses emit (verbatim).  A substring
# test accepted a composite message that merely *contained* the known phrase --
# e.g. "hash list differs from generated inventory; permission denied" -- as
# reducible, i.e. a false READY for a report the real gate would refuse.
HASH_LIST_MSG = 'hash list differs from generated inventory; run check_frozen.py --write'
BLOCK_MARKERS = ('lacks exact new event chain', 'missing', 'unregistered', 'cannot')
# Full message, suffix included: the apparatus always appends the parenthetical,
# so an unknown tail is a different failure, not this one.
LACKS_CHAIN = re.compile(r'(.+?): hash change lacks exact new event chain '
                         r'\(or has stale/duplicate event\)')
# A registered file whose bytes the apparatus could not read because the branch
# deletes it (parity/gateway ``check()``).  ``Errno 2`` is ENOENT everywhere; the
# human-readable part after it is locale-dependent, so only the errno is matched.
MISSING_FILE = re.compile(r'(.+?): \[Errno 2\] .+')
# ``check_registration`` aborts this way when ``new_hashes`` cannot read a row
# the current manifest still lists -- exactly the pre-``--write`` deletion case.
ABORTED_REGISTRATION = re.compile(r'cannot verify registration base \S+: (.+)')
NONREGULAR_FILE = re.compile(r'nonregular frozen file: (.+)')
A2_MISSING = re.compile(r'A2 apparatus missing FROZEN hash: (.+)')
UNREGISTERED = re.compile(r'unregistered/missing file: (.+)')
UNEXPECTED_REGISTERED = re.compile(r'unexpected registered file: (.+)')
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
# ``scripts/parity/`` recursively and registers the names in its own
# ``manifest_inventory`` (under ``specs/parity/`` unless already prefixed);
# gateway-parity keeps an explicit inventory in its own source, so a new member
# there must be named by that source to be registrable.  Either way the list
# itself lives in the apparatus -- this tool keeps no second copy.
# parity's auto-registered directory: every tracked file under it gets a row
# even when ``manifest_inventory`` never names it.  Its spec documents live in
# the second directory but are rows only when the manifest names them.
PARITY_AUTO_DIR = 'scripts/parity/'
PARITY_DIRS = (PARITY_AUTO_DIR, 'specs/parity/')
CHECK_TIMEOUT = 300
GIT_ERROR = 125  # git()/git_bytes() return this when the call itself failed


# ---------------------------------------------------------------- git helpers
# #2398 fix-r1: same git isolation as the frozen apparatus checkers — never
# inherit GIT_* from the caller, never read a global gitconfig, never enable
# optional locks / fsmonitor / hooks. safe.directory=* keeps cross-uid
# checkouts usable: pinning GIT_CONFIG_GLOBAL drops the caller's
# safe.directory, and without it a dubious-ownership repo would turn this
# delivery precheck into a wrong READY/FAIL.
GIT_ISOLATION = ['git', '--no-optional-locks', '-c', 'core.fsmonitor=false',
                 '-c', 'core.hooksPath=/dev/null', '-c', 'safe.directory=*']


def isolated_git_env():
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env['GIT_CONFIG_GLOBAL'] = os.devnull
    return env


def git(root, *args):
    """Return (returncode, stdout, stderr); never raises for a git failure.

    ``GIT_ERROR`` marks a call that could not run at all, so callers can tell
    "the answer is no" from "there is no answer" (fail-closed instead of
    treating a transient fork/exec failure as a missing file).
    """
    try:
        proc = subprocess.run([*GIT_ISOLATION, '-C', str(root), *args], capture_output=True,
                              text=True, errors='replace', env=isolated_git_env(),
                              timeout=CHECK_TIMEOUT)
    except (OSError, subprocess.SubprocessError) as error:
        return GIT_ERROR, '', str(error)
    return proc.returncode, proc.stdout, proc.stderr


def git_bytes(root, *args):
    """Like git(), but keeps stdout as bytes (blob content may be binary)."""
    try:
        proc = subprocess.run([*GIT_ISOLATION, '-C', str(root), *args], capture_output=True,
                              env=isolated_git_env(), timeout=CHECK_TIMEOUT)
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

    Two ``rev-parse --verify --quiet`` queries: the commit is resolved first,
    so a bad SHA is an error (None) instead of a silent "not there"; then the
    path query answers 0 (present), 1 ("no such path in that tree", measured
    with git 2.54) or anything else (the query failed -> None).
    """
    code, _, _ = git(root, 'rev-parse', '--verify', '--quiet', f'{commit}^{{commit}}')
    if code != 0:
        return None
    code, _, _ = git(root, 'rev-parse', '--verify', '--quiet', f'{commit}:{path}')
    if code == 0:
        return True
    # 1 is git's "no such path in that tree" (measured: a missing path and an
    # unknown commit both exit 1, and the commit was verified above).  Any
    # other non-zero code is the query itself failing, and answering False
    # there would let a transient git error look like "the runner is gone".
    return False if code == 1 else None


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
# The three apparatuses' row grammars, mirrored rather than widened: a line the
# real manifest reader treats as prose must not become a "registered" row here,
# or a first registration loses its ``Change: -`` requirement (false READY).
# gateway/ui rows are exactly 64 hex + two spaces + a path (``manifest_row``);
# parity's table is the one place 16-64 hex is legal, and only with its
# ``…``/``...`` ellipsis and the trailing cells the real fullmatch demands.
# ``re.ASCII`` on ROW_BARE: the real ``manifest_row`` readers compile on *bytes*,
# where ``\w`` is ASCII-only, so a CJK row name must read as prose here too.
ROW_BARE = re.compile(r'[0-9a-f]{64}  ([\w./-]+)', re.ASCII)
ROW_TABLE = re.compile(r'\| `([^`]+)` \| `[0-9a-f]{16,64}(?:…|\.\.\.)?` \|[^\r\n]*\|')
_ROW_CACHE = {}
_SOURCE_CACHE = {}
# Source text of every tree this run parsed, keyed by ``id(tree)`` and holding
# the tree itself: the bootstrap model compares an entry's own statements, and a
# tree whose text is gone (or whose id was recycled) must read "cannot tell".
_TREE_SOURCE = {}
# ``_bindings_confined`` counts per canonical block, computed once per block.
_BLOCK_COUNTS = {}
# The delivery commit, pinned to a SHA before anything else runs.  Reading the
# symbolic ``HEAD`` throughout would let a HEAD A -> C -> A wobble during the
# classification window (the child apparatus has already exited) feed every
# evidence lookup from C while the final snapshot still matches A -- a false
# READY for a tree that fails the real gate.
_PINNED_HEAD = {'sha': None}


def pinned_head():
    """The SHA pinned at start-up, or the symbolic ref before it is known."""
    return _PINNED_HEAD['sha'] or 'HEAD'


def manifest_normalise(kind, name):
    """The one rule that turns a FROZEN.md row name into a repo-relative path.

    ``manifest_rows`` builds the drop set with it, and a reported failure name
    has to resolve through the *same* rule: parity reports a spec document by its
    table key (``multimodal.md``) while the drop set holds
    ``specs/parity/multimodal.md``, so a bare name two lines apart used to miss
    the set and turn a legitimate deletion into a false BLOCK.
    """
    name = name.strip()
    if name.startswith('../../'):
        return name[6:]
    if not name.startswith(('scripts/', 'specs/')):
        return f'specs/{kind}/{name}'
    return name


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
            rows.add(manifest_normalise(kind, match.group(1)))
    _ROW_CACHE[key] = rows
    return rows


def registered_at(root, kind, path, commit):
    """True/False/None -- did ``path`` have a hash row at ``commit``?"""
    rows = manifest_rows(root, kind, commit)
    return None if rows is None else path in rows


# ------------------------------------------------------------ discovery
def discover(root, commit):
    """Apparatuses are discovered from the tree at ``commit``: specs/<kind>/FROZEN.md.

    ``commit`` is the SHA pinned at start-up and is **required**: reading
    ``specs/`` from the *working tree* let a checkout A -> B -> A inside the
    discovery window shrink the set -- everything after this point is pinned to
    A, so an apparatus missing from B would never run and the run could report
    READY for a tree A the real gate refuses.  There is deliberately no
    filesystem fallback: a caller with no SHA to pin to must fail closed, not
    silently revive the pre-fix semantics.

    Returns ``(found, invalid)``.  ``found`` is ``[(kind, entry_path_or_None)]``;
    ``invalid`` is ``[(name, mode, type)]`` for each ``specs/*`` entry whose
    object is directory-like but not a tree -- a symlink (``120000 blob``) or a
    submodule (``160000 commit``).  Both are empty-able lists, or the whole
    result is ``None`` when the tree cannot be listed at ``commit`` (callers
    fail closed).  A FROZEN.md without an entry is still reported (as blocking)
    instead of being silently skipped, and a directory-like non-tree entry is
    returned rather than skipped: it can stand in for a whole apparatus
    directory, so treating it as absent let the run print READY while a sibling
    apparatus for the same alias failed (two non-author reviews caught exactly
    that).  A plain file directly under ``specs/`` (``specs/README.md``) cannot
    hold a ``FROZEN.md`` and stays a non-apparatus -- unless
    ``scripts/<alias>/check_frozen.py`` is still in the tree, which means the
    file replaced an apparatus directory and that apparatus would otherwise be
    silently dropped from the run.
    """
    # ``specs/`` (not ``specs``): the bare pathspec lists the ``specs`` tree
    # entry itself and hides everything one level down.
    #
    # Parsed as *bytes*: a path git can store but UTF-8 cannot represent (or a
    # C locale turning every non-ASCII name into U+FFFD under ``text=True``)
    # would make ``exists_at`` ask for a name that never existed, and the
    # apparatus would be silently skipped -- the real gate, which discovers via
    # the filesystem, would still judge it.  ``os.fsdecode`` keeps the original
    # bytes through the surrogateescape round trip.
    code, out, err = git_bytes(root, 'ls-tree', '-z', commit, '--', 'specs/')
    if code != 0:
        return None
    found = []
    invalid = []
    for entry in filter(None, out.split(b'\0')):
        meta, sep, name = entry.partition(b'\t')
        parts = meta.split()
        if not sep or len(parts) != 3:
            continue
        if not name.startswith(b'specs/') or name.count(b'/') != 1:
            continue
        if parts[1] != b'tree':
            # Fail closed for anything directory-like: a symlink (``120000``)
            # or submodule (``160000``) named ``specs/<alias>`` can stand in
            # for an apparatus directory the tool cannot enumerate, so the true
            # apparatus set is unknown.  Skipping it silently made the parent
            # report READY while the child apparatus for that alias failed.
            if parts[0] not in (b'120000', b'160000'):
                # A plain file cannot hold a ``FROZEN.md`` -- but if
                # ``scripts/<alias>/check_frozen.py`` is still in the tree,
                # the file is what an apparatus directory was replaced with
                # (a bad merge), and skipping it drops that apparatus from the
                # run exactly like the symlink case.  A real index file
                # (``specs/README.md``) has no runner and stays a non-apparatus.
                alias = os.fsdecode(name.split(b'/', 1)[1])
                runner = exists_at(root, commit,
                                   f'scripts/{alias}/check_frozen.py')
                if runner is None:
                    return None
                if not runner:
                    continue
            invalid.append((os.fsdecode(name), os.fsdecode(parts[0]),
                            os.fsdecode(parts[1])))
            continue
        kind = os.fsdecode(name.split(b'/', 1)[1])
        frozen = exists_at(root, commit, f'specs/{kind}/FROZEN.md')
        if frozen is None:
            return None
        if not frozen:
            continue
        present = exists_at(root, commit, f'scripts/{kind}/check_frozen.py')
        if present is None:
            return None
        found.append((kind, root / 'scripts' / kind / 'check_frozen.py'
                      if present else None))
    return sorted(found), invalid


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
        head = exists_at(root, pinned_head(), relative)
        if head is None:
            issues.append(f'{relative}: cannot tell whether it is committed')
            continue
        if not head:
            issues.append(f'{relative}: event file is not committed '
                          '(git add/commit required before delivery)')
            continue
        # Content also comes from HEAD: a working-tree edit that adds a Change
        # row must not complete a chain the delivered branch does not contain.
        code, data, _ = git_bytes(root, 'cat-file', 'blob', f'{pinned_head()}:{relative}')
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
        # Validate *every* Change: row here, not only the ones that happen to
        # parse.  A Pending: review makes the real apparatus raise on the review
        # field before it ever reaches the row loop, so a malformed row, an
        # equal-hash row or a duplicate would stay invisible until the reviews
        # are backfilled -- then the gate fails on a delivery this tool called
        # READY.  The row rules mirror the apparatus's ``read_events`` exactly.
        changes = []
        malformed = False
        for line in text.splitlines():
            if not line.startswith('Change:'):
                continue
            match = EVENT_CHANGE.fullmatch(line)
            if not match:
                issues.append(f'{relative}: invalid Change row')
                malformed = True
                continue
            name = match.group(3)
            if (Path(name).is_absolute() or PurePosixPath(name).as_posix() != name
                    or '..' in PurePosixPath(name).parts or match.group(1) == match.group(2)):
                issues.append(f'{relative}: invalid Change path or equal hashes')
                malformed = True
                continue
            changes.append((name, match.group(1), match.group(2)))
        if not changes and not malformed:
            issues.append(f'{relative}: event file has no Change row '
                          '(the real gate reports missing/duplicate Change rows)')
        if len(changes) != len(set(changes)):
            issues.append(f'{relative}: duplicate Change row')
        edges.extend((path.name, name, old, new) for name, old, new in changes)
    # No event row may sit outside an exact chain: the real gate reports that as
    # a stale/duplicate event, and before this check the same rows were invisible
    # whenever the file (or a sibling event) was still Pending.
    for name in sorted({name for _, name, _, _ in edges}):
        if not chain_backed(root, kind, edges, name, mb):
            issues.append(f'{name}: event row is not part of an exact chain from the '
                          'merge-base row to the bytes --write will pin '
                          '(the real gate reports a stale/duplicate event)')
    return edges, pending, issues


def row_targets(root, kind, path, mb):
    """(merge-base pin, target pin) for ``path``, or None when either is unknown.

    The target is "what the new manifest will pin": the HEAD bytes when --write
    would register the path, '-' when it drops or never had the row (a deleted
    file *and* a member removed from the inventory while the file stays tracked
    both end at '-', exactly like ``new_hashes.get(name, '-')``).
    """
    if not mb:
        return None
    registration = registrable(root, kind, path)
    if registration is None:
        return None  # cannot tell -> nothing is provable (fail closed)
    if registration is False:
        target = '-'
    else:
        state, actual = blob_at(root, pinned_head(), path)
        if state is not True or actual is None:
            return None
        target = actual
    base = base_digest(root, kind, mb, path)
    if base is None:
        return None
    return base, target


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
    targets = row_targets(root, kind, path, mb)
    if targets is None:
        return False
    base, target = targets
    if base == target:
        return False
    remaining = [(old, new) for _, name, old, new in edges if name == path]
    current = base
    visited = set()
    while current != target and current not in visited:
        visited.add(current)
        matches = [edge for edge in remaining if edge[0] == current]
        if len(matches) != 1:
            return False
        edge = matches[0]
        remaining.remove(edge)
        current = edge[1]
    return current == target and not remaining


def registration_ok(root, kind, edges, path, mb):
    """True when this path needs no event chain, or its exact chain is present."""
    targets = row_targets(root, kind, path, mb)
    if targets is None:
        return False
    base, target = targets
    return True if base == target else chain_backed(root, kind, edges, path, mb)


def base_event_issues(root, kind, mb):
    """Event files that changed since the merge-base (the gate's append-only half).

    ``check_registration`` verifies this *after* the loop that can abort on a
    deleted row, so the precheck has to re-prove it before it may accept that
    abort: a rewritten historical event would otherwise ride along unseen.
    """
    if not mb:
        return []
    prefix = f'specs/{kind}/frozen/'
    code, out, err = git(root, 'ls-tree', '-r', '-z', mb, '--', prefix)
    if code:
        return [f'cannot list {prefix} at the merge-base: {err.strip()}']
    # Resolve the pinned rev once: with a symbolic ``HEAD`` a broken ref would
    # make every per-entry query exit 1 and be reported as "the event was
    # rewritten" (the query failed; it did not answer).
    code, _, err = git(root, 'rev-parse', '--verify', '--quiet',
                       f'{pinned_head()}^{{commit}}')
    if code:
        return [f'cannot resolve {pinned_head()} to a commit: {err.strip()}']
    issues = []
    for entry in filter(None, out.split('\0')):
        meta, raw_path = entry.split('\t', 1)
        mode, kind_name, oid = meta.split()
        name = raw_path
        if mode != '100644' or kind_name != 'blob':
            issues.append(f'{name}: base event is not a regular file (append-only)')
            continue
        head_code, head_out, head_err = git(root, 'rev-parse', '--verify',
                                            '--quiet', f'{pinned_head()}:{name}')
        if head_code and head_code != 1:
            # Same rule as ``exists_at``: only exit 1 means "no such path in
            # that tree" (measured with git 2.54).  Anything else is the query
            # itself failing, and folding it into "the event was rewritten"
            # blocks with a message that blames the append-only rule.
            issues.append(
                f'{name}: cannot tell whether the base event survives at HEAD '
                f'({head_err.strip() or f"git exit {head_code}"}) (append-only)')
            continue
        if head_code or head_out.strip() != oid:
            issues.append(f'{name}: base event changed/deleted/renamed (append-only)')
            continue
        path = root / name
        if path.is_symlink() or not path.is_file() or path.stat().st_mode & 0o111:
            issues.append(f'{name}: base event changed/deleted/renamed (append-only)')
    return issues


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


def repo_relative(root, name):
    """Reported path -> repo-relative; absolute paths must be inside the tree."""
    name = name.strip()
    candidate = Path(name)
    if not candidate.is_absolute():
        return strip_reported(name)
    try:
        return candidate.resolve().relative_to(root.resolve()).as_posix()
    except (ValueError, OSError):
        return None


def repo_relative_any(ctx, name):
    """Reported path -> repo-relative, resolving against either root in play.

    The child apparatus runs in the pinned snapshot, so an absolute path it
    prints is rooted *there* (``/tmp/cfd-snapshot-*/tree/rust-toolchain.toml``)
    even though every decision is made in repo-relative terms.  Resolving only
    against the audited worktree returned ``None`` and turned ui-server's
    legitimate "unexpected registered file" into a false BLOCK.

    When one root is nested inside the other, the *deepest* prefix wins: taking
    the first match would make a snapshot path inside the audited tree resolve
    to ``tmp/cfd-snapshot-.../tree/...`` instead of the repo-relative path the
    apparatus names.
    """
    if not Path(name).is_absolute():
        return strip_reported(name)
    best, best_depth = None, -1
    for root in (ctx['root'], ctx.get('snapshot')):
        if root is None:
            continue
        relative = repo_relative(root, name)
        if relative is None:
            continue
        try:
            depth = len(Path(root).resolve().parts)
        except OSError:
            continue
        if depth > best_depth:
            best, best_depth = relative, depth
    return best


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
    return exists_at(root, pinned_head(), relative) is True


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


def _module_tuple(tree, name):
    """Value of the one module-level ``name = (...tuple...)``, or None.

    Parsed with ``ast`` rather than a regex: a commented-out member
    (``# 'observer.py',``) must not count, and a ``)`` inside a comment must not
    truncate the tuple.

    The binding must be unique: a later reassignment or an augmented assignment
    means the value the interpreter sees is not the literal parsed here, so the
    shape is "cannot tell" (None).  Returning the first literal would keep
    reporting a member as registered after the branch removed it -- the same
    false-READY class a comment used to cause.
    """
    if _unmodelled_binding(tree, name):
        return None
    stores = [node for node in _module_scope_nodes(tree)
              if isinstance(node, ast.Name) and node.id == name
              and isinstance(node.ctx, ast.Store)]
    augments = [node for node in _module_scope_nodes(tree)
                if isinstance(node, ast.AugAssign) and isinstance(node.target, ast.Name)
                and node.target.id == name]
    if len(stores) != 1 or augments:
        return None
    for node in tree.body:
        targets, value = [], None
        if isinstance(node, ast.Assign):
            targets, value = node.targets, node.value
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            targets, value = [node.target], node.value
        for target in targets:
            if isinstance(target, ast.Name) and target.id == name:
                try:
                    parsed = ast.literal_eval(value)
                except (ValueError, SyntaxError):
                    return None
                return parsed if isinstance(parsed, tuple) else None
    return None


def _module_path(tree, name):
    """Value of ``name = ROOT / '<literal>'``, or None."""
    if _unmodelled_binding(tree, name):
        return None
    stores = [node for node in _module_scope_nodes(tree)
              if isinstance(node, ast.Name) and node.id == name
              and isinstance(node.ctx, ast.Store)]
    if len(stores) != 1:
        return None
    for node in tree.body:
        if not isinstance(node, ast.Assign):
            continue
        for target in node.targets:
            if not (isinstance(target, ast.Name) and target.id == name):
                continue
            value = node.value
            if (isinstance(value, ast.BinOp) and isinstance(value.op, ast.Div)
                    and isinstance(value.right, ast.Constant)
                    and isinstance(value.right.value, str)):
                return value.right.value
            return None
    return None


def _imported_from(tree, name):
    """Module ``name`` is bound from, when the only binding is ``from X import name``.

    ``required_files`` registers names that come straight out of ``apparatus.py``
    (``SPEC_FILES``, ``EXP008_FILES``, ``toolchain_files``) and calls ``scan``,
    so this tool models them from that file.  Only a single ``from apparatus
    import name`` keeps the two identical: a second import, a local assignment,
    ``import apparatus as name``, ``from other import y as name`` or ``from
    other import *`` all put a different object behind the name -- "cannot tell"
    (None), never the literal this tool read.
    """
    if _unmodelled_binding(tree, name, imports=False):
        return None
    # Module scope only: a ``from apparatus import name`` inside a function body
    # binds nothing at import time, so counting it would report a module-level
    # name as imported when the interpreter raises NameError instead.
    for node in _module_scope_nodes(tree):
        if isinstance(node, ast.Name) and node.id == name \
                and isinstance(node.ctx, ast.Store):
            return None
    module, seen = None, 0
    for node in _module_scope_nodes(tree):
        # ``import apparatus as name`` / ``import name`` bind a module object and
        # leave no ``Name(Store)`` behind -- the pre-r17 code rejected them
        # explicitly, and dropping that branch made a file with both a plain
        # ``from apparatus import name`` and one of these look trustworthy.
        if isinstance(node, ast.Import):
            if any((alias.asname or alias.name).split('.')[0] == name
                   for alias in node.names):
                return None
            continue
        if not isinstance(node, ast.ImportFrom):
            continue
        # ``from .apparatus import name`` is not the module this tool read, and a
        # star import hides an arbitrary set of names behind the module.
        if any(alias.name == '*' for alias in node.names) or node.level:
            return None
        for alias in node.names:
            # Any alias that *binds* this name counts as a binding, not just the
            # identical spelling: ``from other import y as name`` puts a
            # different object behind the name while looking like one import.
            if (alias.asname or alias.name) != name:
                continue
            if alias.name != name:
                return None
            seen += 1
            module = node.module
    return module if seen == 1 else None


def _function_node(tree, name):
    """The single ``def name``, or None when absent or redefined.

    A second definition means the interpreter runs the later body while a walk
    that takes the first would model the wrong inventory -- so a redefinition is
    "cannot tell", not "use the first one".  The same holds for a body in a
    nested scope or a class: it does not shadow the module-level function, but
    it can be the one the entry calls (``App().required_files()``), and two
    bodies behind one name is a shape this model cannot tell apart.
    """
    found = [node for node in _module_scope_nodes(tree)
             if isinstance(node, ast.FunctionDef) and node.name == name]
    if len(found) != 1:
        return None
    bodies = [node for node in ast.walk(tree)
              if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
              and node.name == name]
    return found[0] if len(bodies) == 1 else None


def _parse(source):
    """``ast.parse`` that treats every unparsable shape as "cannot tell".

    The text is kept next to the tree: the bootstrap model compares the entry's
    own statements, so a tree without its source is "cannot tell" rather than a
    guessed match.  Holding the tree keeps ``id(tree)`` from being reused.
    """
    try:
        tree = ast.parse(source)
    except (SyntaxError, ValueError, RecursionError, MemoryError):
        return None
    _TREE_SOURCE[id(tree)] = (tree, source)
    return tree


def _parents(tree):
    parents = {}
    for node in ast.walk(tree):
        for child in ast.iter_child_nodes(node):
            parents[id(child)] = node
    return parents


def _unmodelled_binding(node, name, imports=True):
    """True when ``name`` is bound in a form that is not ``name = <literal>``.

    Parameters (defaults included), ``import ... as``, ``except ... as``,
    ``match`` captures, a same-named ``def``/``class``, ``import *`` and
    ``global``/``nonlocal`` all rebind the name, and none of them produces the
    plain ``Name(Store)`` the literal readers count -- so a value read from
    elsewhere is not the value the interpreter uses.  Nested function *bodies*
    are included (a ``global`` assignment there rebinds the module name), but
    nested parameter lists are not: a local parameter of the same name shadows
    only inside that call.

    ``imports=False`` skips the two import branches, for callers that model the
    one legitimate ``from X import name`` themselves (``_imported_from``).
    """
    arguments = getattr(node, 'args', None)
    if isinstance(arguments, ast.arguments):
        every = (list(arguments.posonlyargs) + list(arguments.args)
                 + list(arguments.kwonlyargs)
                 + [arg for arg in (arguments.vararg, arguments.kwarg) if arg is not None])
        if any(arg.arg == name for arg in every):
            return True
    for child in ast.walk(node):
        if child is node:
            continue
        if imports and isinstance(child, ast.alias) \
                and (child.asname or child.name).split('.')[0] == name:
            return True
        if isinstance(child, ast.ExceptHandler) and child.name == name:
            return True
        if isinstance(child, (ast.MatchAs, ast.MatchStar)) and child.name == name:
            return True
        if isinstance(child, ast.MatchMapping) and child.rest == name:
            return True
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) \
                and child.name == name:
            return True
        if imports and isinstance(child, ast.ImportFrom) \
                and any(a.name == '*' for a in child.names):
            return True
        if isinstance(child, (ast.Global, ast.Nonlocal)) and name in child.names:
            return True
    return False


def _header_nodes(node):
    """Children a ``def``/``class`` evaluates in the *enclosing* scope.

    Decorators, default values, annotations and base expressions run when the
    statement executes; only the body waits for a call.  A walrus in a default
    (``def f(a=(x := 2))``) therefore binds ``x`` at module scope, and skipping
    the whole statement would miss it.  A lambda's ``body`` is call-scoped for
    the same reason its defaults are not.
    """
    # ``Lambda.body`` is one expression, not a statement list.
    body = node.body if isinstance(node.body, list) else [node.body]
    body_ids = {id(item) for item in body}
    for child in ast.iter_child_nodes(node):
        if id(child) in body_ids:
            continue
        yield child


def _scope_nodes(body):
    """Nodes evaluated in one scope: ``body`` and its children, nested scopes out."""
    stack = list(body)
    while stack:
        node = stack.pop()
        yield node
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda,
                             ast.ClassDef)):
            stack.extend(_header_nodes(node))
            continue
        stack.extend(ast.iter_child_nodes(node))


def _module_scope_nodes(tree):
    """Module-level statements and their children, function/class bodies excluded."""
    return _scope_nodes(tree.body)


def _nested_scopes(function):
    """``def``/``class`` statements nested inside a function this tool models.

    ``ast.walk`` over the body counts their literals as members; ``_scope_nodes``
    skips them.  Both readings are wrong: a nested ``def`` contributes when it
    is *called*, so its members are neither always there nor always absent -- a
    ``def _extra(): paths.append('scripts/gateway-parity/evil.md')`` that the
    modelled function calls registers a row the model never sees.  A function
    containing one is therefore "cannot tell".  Sibling helpers that happen to
    be nested are rare enough that blocking costs a rerun, while guessing costs
    a silently short inventory.
    """
    return [node for node in ast.walk(function)
            if node is not function
            and isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef,
                                  ast.Lambda, ast.ClassDef))]


def _sole_binding(tree, name):
    """The single module-scope ``name = <expr>``, or None when the shape is unclear.

    Exactly one ``Name(Store)`` for ``name`` outside function/class bodies -- a
    walrus, a ``for``/``with ... as`` target, an unpacking target or a second
    assignment leaves the interpreter with a value this parser never saw -- and
    that one store must be the target of a plain ``name = <expr>``.  Bindings in
    any other form (``import ... as``, ``except ... as``, a same-named
    ``def``/``class``, ``import *``, ``global``/``nonlocal``) disqualify the
    module through ``_unmodelled_binding``.
    """
    if _unmodelled_binding(tree, name):
        return None
    stores = [node for node in _module_scope_nodes(tree)
              if isinstance(node, ast.Name) and node.id == name
              and isinstance(node.ctx, ast.Store)]
    if len(stores) != 1:
        return None
    for node in _module_scope_nodes(tree):
        if isinstance(node, ast.Assign) and len(node.targets) == 1:
            target, value = node.targets[0], node.value
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            target, value = node.target, node.value
        else:
            continue
        if isinstance(target, ast.Name) and target.id == name:
            return value
    return None


# The reviewed apparatus bootstrap, statement by statement (fix-r19).
# ``_bootstrap_ok`` compares the entry's own lines against these tuples, so
# the model cannot be steered by a shape it recognises but does not pin: a
# swapped ``_spec`` path, exec source, ``module_from_spec`` receiver or
# helper import changes a line and the answer becomes "cannot tell".
# ``test-check-frozen-delivery`` pins both tuples to the real files, so any
# apparatus change that is not reflected here fails a test instead of
# silently relaxing the precheck (or blocking every delivery).
_GATEWAY_BOOTSTRAP_ANCHORS = ('# >>> apparatus bootstrap', '# <<< apparatus bootstrap')
_UI_BOOTSTRAP_ANCHORS = ('import sys as _boot_sys',
                         "_boot_namespace['preflight'](_boot_dir)")

_GATEWAY_BOOTSTRAP = (
    'import sys',
    'if not sys.flags.isolated:',
    "    raise SystemExit(f'run with -I: python3 -I {__file__}')",
    "if sys.version_info < (3, 11) or 'os' not in sys.modules:",
    "    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')",
    'import os',
    '_boot_dir = os.path.dirname(os.path.realpath(__file__))',
    "_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir",
    'if any(',
    '        os.path.isdir(entry or os.curdir)',
    '        and any(os.path.isdir(root)',
    '                and os.path.samefile(entry or os.curdir, root)',
    "                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))",
    '        for entry in sys.path):',
    "    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')",
    'import importlib.util',
    'from pathlib import Path',
    'import tempfile',
    "sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-gateway-pyc-')",
    'sys.dont_write_bytecode = True',
    '_apparatus_code = sys._getframe().f_code',
    '_here = Path(__file__).resolve().parent',
    "_apparatus = _here.parent if _here.name == 'controls' else _here",
    "_guard_path = _apparatus / '_guard.py'",
    "_guard = sys.modules.get('_guard')",
    "if not (getattr(_guard, '__file__', None)",
    '        and Path(_guard.__file__).resolve() == _guard_path',
    "        and isinstance(getattr(_guard, '_apparatus_source', None), bytes)):",
    "    _spec = importlib.util.spec_from_file_location('_guard', _guard_path)",
    '    _guard = importlib.util.module_from_spec(_spec)',
    "    sys.modules['_guard'] = _guard",
    '    _guard_source = _guard_path.read_bytes()',
    '    try:',
    "        exec(compile(_guard_source, str(_guard_path), 'exec'), _guard.__dict__)",
    '    except BaseException:',
    "        sys.modules.pop('_guard', None)",
    '        raise',
    '    _guard._apparatus_source = _guard_source',
    'if Path(_guard.__file__).resolve() != _guard_path:',
    "    raise RuntimeError(f'_guard: loaded {_guard.__file__}, expected {_guard_path}')",
    '_load = _guard.load',
    '_has_git_checkout = _guard._has_git_checkout',
    '_verify_cli_imports = _guard.verify_cli_imports',
    'run_entry = _guard.run_entry',
    "common = _load('common', _apparatus / 'common.py')",
)

_UI_BOOTSTRAP = (
    'import sys as _boot_sys',
    'if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment',
    '        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):',
    '    print(\'{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}\')',
    '    raise SystemExit(1)',
    "if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:",
    '    print(\'{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}\')',
    '    raise SystemExit(1)',
    'import os as _boot_os',
    '_boot_sys.dont_write_bytecode = True',
    '_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))',
    'if any(',
    '        _boot_os.path.isdir(entry or _boot_os.curdir)',
    '        and any(_boot_os.path.isdir(root)',
    '                and _boot_os.path.samefile(entry or _boot_os.curdir, root)',
    "                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))",
    '        for entry in _boot_sys.path):',
    '    print(\'{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}\')',
    '    raise SystemExit(1)',
    "_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')",
    'if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):',
    '    print(\'{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}\')',
    '    raise SystemExit(1)',
    'import hashlib as _boot_hashlib',
    'import subprocess as _boot_subprocess',
    '_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))',
    "_boot_write = __name__ == '__main__' and '--write' in _boot_sys.argv[1:]",
    'if _boot_write:',
    '    _boot_head = _boot_subprocess.check_output(',
    "        ['git', '-C', _boot_root, 'rev-parse', '--verify', 'HEAD']).decode().strip()",
    '    _boot_checker = _boot_subprocess.check_output(',
    "        ['git', '-C', _boot_root, 'show', f'{_boot_head}:scripts/ui-server-parity/check_frozen.py'])",
    '    if _boot_sys._getframe().f_code != compile(',
    "            _boot_checker, __file__, 'exec', dont_inherit=True, optimize=_boot_sys.flags.optimize):",
    "        raise SystemExit('#1089 --write executing checker differs from HEAD')",
    "_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')",
    "with open(_boot_source, 'rb') as _boot_file:",
    '    _boot_bytes = _boot_file.read()',
    'try:',
    '    if _boot_write:',
    '        _boot_trusted = _boot_subprocess.check_output(',
    "            ['git', '-C', _boot_root, 'show', f'{_boot_head}:scripts/ui-server-parity/apparatus.py'])",
    '        if _boot_bytes != _boot_trusted:',
    "            raise ValueError('--write apparatus source differs from HEAD')",
    '    elif _boot_os.path.isfile(_boot_manifest):',
    '        with open(_boot_manifest) as _boot_file:',
    '            _boot_pins = [line.split()[0] for line in _boot_file',
    "                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]",
    '        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:',
    "            raise ValueError('apparatus bootstrap hash mismatch')",
    '    else:',
    '        _boot_trusted = _boot_subprocess.check_output(',
    "            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])",
    '        if _boot_bytes != _boot_trusted:',
    "            raise ValueError('apparatus bootstrap differs from HEAD without manifest')",
    'except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:',
    '    print(str(_boot_error), file=_boot_sys.stderr)',
    '    print(\'{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}\')',
    '    raise SystemExit(1)',
    "_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}",
    "exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)",
    'if _boot_write:',
    '    import types as _boot_types',
    "    _boot_module = _boot_types.ModuleType('apparatus')",
    '    _boot_module.__dict__.update(_boot_namespace)',
    "    _boot_sys.modules['apparatus'] = _boot_module",
    'else:',
    "    _boot_namespace['preflight'](_boot_dir)",
)


def _module_stores(tree):
    """How many times each name is bound at module scope (function bodies out).

    ``def``/``class`` bind their name too: a ``def common(...)`` two lines after
    the bootstrap shadows the loader result exactly like ``common = other``.
    """
    counts = Counter()
    for node in _module_scope_nodes(tree):
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store):
            counts[node.id] += 1
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            counts[node.name] += 1
    return counts


def _import_names(tree):
    """Names bound by module-scope ``import``/``from ... import`` statements."""
    names = set()
    for node in _module_scope_nodes(tree):
        if isinstance(node, ast.Import):
            names.update((alias.asname or alias.name).split('.')[0]
                         for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            names.update(alias.asname or alias.name for alias in node.names)
    return names


def _import_origin(tree, name, module):
    """True when every module-scope binding of ``name`` imports it from ``module``.

    The bootstrap trusts its helpers by spelling: ``from evil import Path`` (or
    ``import evil as sys``) leaves every shape below untouched while the
    directory it derives comes from somewhere else.  A plain store of the name,
    a star import, a relative import or an import of another module therefore
    makes the answer "cannot tell".
    """
    seen = 0
    for node in _module_scope_nodes(tree):
        if isinstance(node, ast.Name) and node.id == name \
                and isinstance(node.ctx, ast.Store):
            return False
        if isinstance(node, ast.Import):
            for alias in node.names:
                if (alias.asname or alias.name).split('.')[0] != name:
                    continue
                if alias.name != module:
                    return False
                seen += 1
        elif isinstance(node, ast.ImportFrom):
            if any(alias.name == '*' for alias in node.names):
                return False  # ``from evil import *``: what ``name`` means is unknown
            for alias in node.names:
                if (alias.asname or alias.name) != name:
                    continue
                if alias.name != name or node.level or node.module != module:
                    return False
                seen += 1
    return seen > 0


_LINE_BREAK = re.compile(r'\r\n|\r|\n')


def _source_lines(source):
    """The file's lines split the way ``ast`` counts them: universal newlines.

    ``str.splitlines`` also breaks on ``\\f``, ``\\x1c``-``\\x1e``, ``\\x85``,
    ``\\u2028`` and ``\\u2029``, none of which starts a new line for the
    tokenizer; a form feed inside the block would shift every index after it, so
    the text comparison and the ``lineno`` window would disagree.  Splitting on
    ``\\n`` alone is wrong the other way: the tokenizer also ends a line at a
    lone ``\\r``.  Both directions are a wrong answer -- false PASS or a delivery
    blocked for nothing -- so match the tokenizer exactly.
    """
    return _LINE_BREAK.split(source)


def _bootstrap_span(source, anchors):
    """Line indexes (first, last) of the reviewed block, or None when ambiguous.

    One occurrence of each anchor, end after start: anything else is "cannot
    tell" -- a second block, a commented-out anchor or a reordered file.
    """
    start, end = anchors
    lines = _source_lines(source)
    starts = [i for i, line in enumerate(lines) if line.strip().startswith(start)]
    ends = [i for i, line in enumerate(lines) if line.strip().startswith(end)]
    if len(starts) != 1 or len(ends) != 1 or ends[0] <= starts[0]:
        return None
    return starts[0], ends[0]


def _bootstrap_lines(source, anchors):
    """The entry's bootstrap statements between the two unique anchor lines.

    Comments and blank lines are dropped (prose can change); every statement
    line, indentation included, must match the model verbatim.
    """
    span = _bootstrap_span(source, anchors)
    if span is None:
        return None
    block = []
    for line in _source_lines(source)[span[0]:span[1] + 1]:
        text = line.strip()
        if text and not text.startswith('#'):
            block.append(line.rstrip())
    return block


def _block_is_code(tree, span, anchors, canonical):
    """True when the reviewed lines are statements, not text that looks like them.

    Line equality alone also holds for the block pasted into a triple-quoted
    string or a string-valued constant: the entry then runs the reviewed
    *text* and none of the reviewed behaviour.  Two shapes are refused -- a
    string spanning the anchor lines, and a statement count that does not
    match parsing the canonical block with its anchors -- so the answer is
    "cannot tell" instead of a read of an apparatus that never ran.
    """
    low, high = span[0] + 1, span[1] + 1
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) \
                and (node.end_lineno or node.lineno) > node.lineno \
                and node.lineno <= high and (node.end_lineno or node.lineno) >= low:
            return False
    # The ui block's anchors are its first and last *statements* (the gateway's
    # are comments), so only add the ones the canonical tuple does not already
    # start/end with.
    text = list(canonical)
    if not text or text[0].strip() != anchors[0]:
        text.insert(0, anchors[0])
    if not text or text[-1].strip() != anchors[1]:
        text.append(anchors[1])
    model = _parse('\n'.join(text))
    if model is None:
        return False

    def located(nodes):
        # Context/operator nodes (``Load``, ``Store``, ``Div`` …) carry no line,
        # so counting them would compare a filter against a non-filter.
        return [node for node in nodes
                if isinstance(getattr(node, 'lineno', None), int)]

    inside = located(node for node in _module_scope_nodes(tree)
                     if isinstance(getattr(node, 'lineno', None), int)
                     and low <= node.lineno <= high)
    return len(inside) == len(located(_module_scope_nodes(model)))


# Every helper the bootstrap imports, with the module it must come from.  The
# unit suite pins this list against the block's own imports: adding one to the
# block without adding it here (or dropping one here to relax the check) is a
# red test, not a silent hole.
_BOOTSTRAP_HELPERS = (('sys', 'sys'), ('os', 'os'), ('importlib', 'importlib.util'),
                      ('Path', 'pathlib'), ('tempfile', 'tempfile'))
# The same rule for the ui-server-parity block, whose five helpers are all
# aliased: ``import types as _boot_types`` two lines after the block would swap
# the module object ``_boot_module`` is built from.  ``Path`` is imported
# outside the block (``ROOT``), so it is checked separately in ``ui_inventory``.
_UI_BOOTSTRAP_HELPERS = (('_boot_sys', 'sys'), ('_boot_os', 'os'),
                         ('_boot_hashlib', 'hashlib'),
                         ('_boot_subprocess', 'subprocess'),
                         ('_boot_types', 'types'))

# Namespace lookups by name: outside the reviewed block they can replace what
# the block bound without leaving a ``Name(Store)`` behind.  ``setitem`` /
# ``delitem`` are the ``operator`` spellings of ``d[k] = v``; they are listed
# here as well as in ``_DUNDER_MUTATORS`` because ``from operator import
# setitem; setitem(_m, ...)`` binds a plain name while the call is the same
# write as ``_m[...] = ...``.
_REFLECTIVE_CALLS = frozenset({'globals', 'locals', 'vars', 'setattr', 'delattr',
                               'setitem', 'delitem', 'exec', 'eval', '__import__'})
# ``sys.modules['apparatus'] = evil`` swaps the module a later ``from apparatus
# import ...`` picks up; both spellings of the module object stay watched, and
# ``_sys_roots`` adds whatever else the entry imports ``sys`` (or ``sys.modules``
# /``sys.path``) as.
_MODULE_TABLE_ROOTS = frozenset({'sys', '_boot_sys'})
# Attributes that reach the namespace behind a name.  ``common.__dict__.update()``
# or ``_d = common.__dict__; _d['A2_SCRIPTS'] = ...`` rebind what the block bound
# while every ``Name(Store)`` count stays put.  Counted only when the attribute
# hangs from a name that still refers to a binding the block made -- a plain
# ``x.__class__`` read two hundred lines down is not a write.
_NAMESPACE_ATTRS = frozenset({'__dict__', '__class__', '__globals__', '__builtins__'})
# ``object.__setattr__(common, ...)`` has no watched root at all, so these are
# matched by attribute name alone; ``operator.setitem(d, k, v)`` is the same
# write as ``d[k] = v`` under another spelling.
_DUNDER_MUTATORS = frozenset({'__setattr__', '__delattr__', '__setitem__',
                              '__delitem__', '__getattribute__',
                              'setitem', 'delitem'})
# ``sys.modules`` / ``sys.path`` are the process-global tables a later
# ``from apparatus import ...`` reads; mutating them is the injection this
# check exists for.  ``sys.meta_path`` / ``sys.path_hooks`` decide the same
# lookup one layer down -- a finder appended there resolves ``apparatus`` from
# anywhere -- so they count as tables too.  ``sys.argv.append(...)`` does not.
_SYS_TABLES = frozenset({'modules', 'path', 'meta_path', 'path_hooks'})
# ``getattr(obj, '<attr>')`` is the same write with the attribute name in a
# string.  Only the name-independent write paths are listed: ``'__dict__'``,
# ``'__setattr__'``, ``'modules'`` -- not every mutator method name, which is
# what ``_MUTATOR_ATTRS`` is for and which would block harmless lookups.
_REFLECTIVE_ATTRS = _NAMESPACE_ATTRS | _DUNDER_MUTATORS | _SYS_TABLES
# Calls that mutate the object they hang from.  Only counted when the receiver
# is a watched name -- ``failures.append(...)`` two hundred lines down is the
# apparatus doing its job, not a write to the bootstrap.  The three
# ``*_update`` set methods edit a registered name list in place just as
# ``update`` does, and ``sort``/``reverse`` do it to a list: ``sys.path.sort()``
# reorders import resolution just as ``sys.path.insert`` does.
_MUTATOR_ATTRS = frozenset({'update', 'setdefault', 'pop', 'popitem', 'clear',
                            'insert', 'append', 'extend', 'add', 'discard',
                            'remove', 'difference_update', 'intersection_update',
                            'symmetric_difference_update', 'sort', 'reverse',
                            '__setitem__', '__delitem__'})
# A ``lambda`` that reaches its parameters without ever being bound to a name
# (``sorted(xs, key=lambda o: o.update(...))``) is decided at the call site that
# receives it; ``_mutating_functions`` returns its id separately for that.


def _root_name(node):
    while isinstance(node, (ast.Attribute, ast.Subscript)):
        node = node.value
    return node.id if isinstance(node, ast.Name) else None


def _in_block(node, span):
    """True when ``node`` sits inside the reviewed block's line span.

    ``ast`` numbers lines from 1 while the block span is a pair of 0-based
    indices from ``_source_lines``, so the block covers ``low + 1 ..
    high + 1``.  One definition on purpose: r21 fixed a line-index mismatch
    between ``str.splitlines`` and the tokenizer, and a second copy of this
    arithmetic could drift the same way.
    """
    line = getattr(node, 'lineno', None)
    return (span is not None and line is not None
            and span[0] + 1 <= line <= span[1] + 1)


def _target_names(target):
    """Every plain name a binding target can bind, ``*rest`` included."""
    return {node.id for node in ast.walk(target)
            if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Store)}


def _literal_values(value):
    """``(elements, exact)`` for a literal whose values the file spells out.

    ``elements`` lists the possible values in the order the file fixes it, or
    ``None`` when ``value`` is not a literal this model can enumerate.  ``exact``
    is False when a ``*`` / ``**`` part hides values: the file does not say what
    they are, so a caller that reads the visible elements as the whole story
    would follow one value too few and let a renamed write through.
    """
    if isinstance(value, ast.Dict):
        # A dict literal iterates (and unpacks to) its keys, and a literal's key
        # order is fixed by the file.
        return ([key for key in value.keys if key is not None],
                all(key is not None for key in value.keys))
    if isinstance(value, (ast.Tuple, ast.List, ast.Set)):
        elements = list(value.elts)
        return elements, not any(isinstance(item, ast.Starred) for item in elements)
    return None, True


def _bound_pairs(node):
    """``(target, value)`` pairs this model can follow for an alias binding.

    Plain and chained assignment, annotated assignment, walrus and
    ``with ... as``: each can put a seeded object behind another name.  A
    ``for`` loop is followed only over a literal sequence written out in the
    file -- ``for _c in (common,)`` renames the object, while
    ``for name in REQUIRED`` binds an *element* of the set and must not turn
    every loop variable into a watched root.  A loop variable takes every
    element of a literal sequence in turn, so each element is paired with the
    target -- one pair per element, whether the target is a single name or a
    tuple that unpacks the element.  ``_alias_roots`` decides how far each pair
    reaches and treats a ``*rest`` target or a shape that does not line up as
    reaching every name on the left.
    """
    if isinstance(node, ast.Assign):
        for target in node.targets:
            yield target, node.value
    elif isinstance(node, ast.AnnAssign) and node.value is not None:
        yield node.target, node.value
    elif isinstance(node, ast.NamedExpr):
        yield node.target, node.value
    elif isinstance(node, (ast.For, ast.AsyncFor)):
        elements, exact = _literal_values(node.iter)
        if elements is not None:
            # The loop variable takes each element of the literal in turn, so
            # every element is a possible value for the target -- including when
            # the literal is a ``set`` or a ``dict``, whose iteration order the
            # file does not fix but which changes nothing once each element is
            # considered on its own.
            for element in elements:
                yield node.target, element
            if not exact:
                # ``for _k in [*_xs]`` / ``for _k in {**m}``: the expansion
                # contributes values the file never spells out, so the target
                # may stand for anything.  ``None`` says so to every reader.
                yield node.target, None
    elif isinstance(node, (ast.With, ast.AsyncWith)):
        for item in node.items:
            if item.optional_vars is not None:
                yield item.optional_vars, item.context_expr


def _sys_roots(tree):
    """``(module names, table names)`` that still refer to ``sys`` or its tables.

    ``import sys as _s; _s.modules['apparatus'] = ...`` and
    ``from sys import modules as _m; _m['apparatus'] = ...`` put another module
    behind a later ``from apparatus import ...`` without touching ``sys``.
    ``sys.modules`` and ``sys.path`` live for the whole process, so an alias
    bound inside a function body reaches them exactly like one bound at module
    scope; ``from sys import *`` puts both tables in reach by their bare names.

    The two kinds are kept apart because they answer different questions:
    ``X.modules`` needs ``X`` to be the *module*, while ``_p.append(...)`` only
    needs ``_p`` to be a *table*.  ``from sys import path as _p`` puts ``_p``
    among the tables but not among the modules, and a single flat set let that
    spelling through the ``root not in sys_roots`` guard.
    """
    modules = set(_MODULE_TABLE_ROOTS)
    tables = set(_MODULE_TABLE_ROOTS)
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name == 'sys':
                    modules.add(alias.asname or 'sys')
                    tables.add(alias.asname or 'sys')
        elif isinstance(node, ast.ImportFrom) and node.module == 'sys' and not node.level:
            for alias in node.names:
                if alias.name == '*':
                    tables.update(_SYS_TABLES)
                elif alias.name in _SYS_TABLES:
                    tables.add(alias.asname or alias.name)
    return modules, tables


def _sys_aliases(tree, span=None):
    """``(module names, table names, table-bound names)`` for ``sys``.

    Follows ``_s = sys``, ``_m = sys.modules``, ``_p = _m``, ``for t in _ml``
    over a tracked container, and unpacking to a fixed point over every scope.
    A name is a *table* when it was bound to ``<module>.<table>``, and a *module*
    only when it was bound to the module itself: ``_alias_roots`` strips
    attributes, so ``_p = sys.path`` lands in both sets there and the two
    questions ("is this the module?" for ``X.modules``, "is this a table?" for
    ``_p.append``) would collapse again.

    The third set answers a third question the first two cannot: *was this name
    ever bound to a table by something other than being the module?*  A bare
    module name is in both sets (``import sys`` puts ``sys`` there), and
    ``sys.argv.append(...)`` is not a write to the import table, so
    ``_foreign_writes`` has to leave plain module names out of the receiver
    check.  A name that was *also* bound to a table -- ``both = sys`` then
    ``both = sys.path``, ``both = [sys.path]``, ``both, _p = (*pair,)`` -- must
    stay in: subtracting it there is what let those spellings write the table.
    """
    modules, tables = (set(part) for part in _sys_roots(tree))
    bound = set()
    while True:
        added_modules, added_tables, added_bound = set(), set(), set()
        for node in ast.walk(tree):
            if _in_block(node, span):
                continue
            for target, value in _bound_pairs(node):
                if value is None:
                    # An expansion the file does not spell out can contribute
                    # any value, so every name on the left may carry ``sys`` or
                    # one of its tables.
                    names = _target_names(target)
                    added_modules |= names
                    added_tables |= names
                    added_bound |= names
                    continue
                if isinstance(target, ast.Name):
                    if isinstance(value, ast.Name):
                        if value.id in modules:
                            added_modules.add(target.id)
                        if value.id in tables:
                            added_tables.add(target.id)
                            if value.id in bound:
                                added_bound.add(target.id)
                    elif _sys_table(value, modules):
                        added_tables.add(target.id)
                        added_bound.add(target.id)
                    else:
                        # ``rows = [sys.modules]`` / ``_m = [sys.path][0]``: a
                        # literal container, or an element of one, hands the
                        # table on without spelling it.  The two kinds stay
                        # apart -- calling a table a module would make the
                        # ``X.modules`` question answer for the wrong object.
                        is_module, is_table = _container_sys_kinds(
                            value, modules, tables)
                        if is_module:
                            added_modules.add(target.id)
                        if is_table:
                            added_tables.add(target.id)
                            added_bound.add(target.id)
                    continue
                if not isinstance(target, (ast.Tuple, ast.List)):
                    continue
                # An unpacking target this model cannot line up: any table on
                # the right makes every name on the left a possible table.
                values, exact = _literal_values(value)
                if values is None:
                    values, exact = [value], True
                # The two questions stay apart -- "is this the module?" (for
                # ``X.modules``) and "is this a table?" (for ``_p.append``) --
                # but an unpacking hands out both: ``import sys as _s`` puts
                # ``_s`` in both sets, and ``_p, _q = _s, 0`` must then keep
                # ``_p`` in both as well.  Dropping a module name from the
                # table side here would lose ``_p.modules[...] = ...``.
                names = _target_names(target)
                if not exact:
                    # An expansion: any name on the left may be either, and may
                    # be the table itself rather than the module.
                    added_modules |= names
                    added_tables |= names
                    added_bound |= names
                    continue
                for item in values:
                    if _sys_table(item, modules):
                        added_tables |= names
                        added_bound |= names
                    elif isinstance(item, ast.Name):
                        if item.id in modules:
                            added_modules |= names
                        if item.id in tables:
                            added_tables |= names
                            if item.id in bound:
                                added_bound |= names
                    else:
                        # ``_a, _b = [sys.modules], 0``: the unpacking hands out
                        # what the container holds, exactly as it would for a
                        # single name target.
                        is_module, is_table = _container_sys_kinds(
                            item, modules, tables)
                        if is_module:
                            added_modules |= names
                        if is_table:
                            added_tables |= names
                            added_bound |= names
            if isinstance(node, (ast.For, ast.AsyncFor)):
                # ``for t in _ml`` hands out what the tracked container holds;
                # which kind the elements are is decided conservatively -- a
                # module on the left of the loop is as bad as a table.
                names = _target_names(node.target)
                if _contains_root(node.iter, modules):
                    added_modules |= names
                if _contains_root(node.iter, tables):
                    added_tables |= names
                    if _contains_root(node.iter, bound):
                        added_bound |= names
        if added_modules <= modules and added_tables <= tables \
                and added_bound <= bound:
            return modules, tables, bound
        modules |= added_modules
        tables |= added_tables
        bound |= added_bound


def _iterates_holder(node, holders, roots):
    """True when a ``for`` over ``node`` hands out elements of a held container.

    ``for e in rows`` (``rows = [common]``) and ``for e in list(rows)`` are one
    hop from the object, the same hop an index or a star-unpack takes.  The test
    is deliberately narrower than ``_container_value``: that one asks "does this
    expression mention a seed anywhere", which is right for a *binding* but
    drags in every loop over a value merely derived from one -- the real
    apparatus walks ``sorted(manifest_inventory(root).items())``, and following
    that loop would make the pristine file unreadable to the model.
    """
    if isinstance(node, ast.Name):
        return node.id in holders
    if isinstance(node, (ast.List, ast.Tuple, ast.Set)):
        return any(_contains_root(element, roots) for element in node.elts)
    if isinstance(node, ast.Call) \
            and _call_name(node.func) in _CONTAINER_CALLS:
        return any(isinstance(argument, ast.Name) and argument.id in holders
                   for argument in node.args)
    return False


def _alias_roots(tree, seeds, span=None):
    """Names that can still reach one of ``seeds``, followed to a fixed point.

    ``_c = common`` then ``_c.A2_SCRIPTS = (...)`` reaches the block's module
    object without ever naming it, and ``_m = sys.modules`` does the same for
    the table a later ``from apparatus import ...`` reads.  Every binding form
    that can put a seeded value behind another name is followed -- plain and
    chained assignment, annotated assignment, walrus, tuple unpacking,
    ``for``/``with`` targets and renamed imports -- and in *any* scope: a
    function body reaches the same objects the module holds, so
    ``def _swap(): _c = common; _c.A2_SCRIPTS = (...)`` is the same write one
    call deep.  ``_d = common.__dict__`` and ``_p = _m`` resolve too.

    Values are matched one-for-one (the expression's own root, or a tuple's
    element roots), never by "mentions a seed somewhere": a comprehension such
    as ``[f'...{name}' for name in A2_SCRIPTS]`` reads the seeded name without
    keeping the object, and following it would drag every derived local into
    the watched set.  Following one hop too many only costs a legitimate
    delivery a rerun; following one too few lets a renamed write through.
    """
    roots = set(seeds)
    holders = set()
    while True:
        added, new_holders = set(), set()
        for node in ast.walk(tree):
            if _in_block(node, span):
                continue
            for target, value in _bound_pairs(node):
                if value is None:
                    # An expansion the file does not spell out can contribute
                    # any value, so every name on the left may reach a seed.
                    added |= _target_names(target)
                    continue
                values, exact = _literal_values(value)
                if values is None:
                    values, exact = [value], True
                if isinstance(target, (ast.Tuple, ast.List)):
                    names = _target_names(target)
                    if not exact:
                        # ``a, b = (*xs,)``: which element lands in which name
                        # is exactly what the file does not say.
                        added |= names
                        continue
                    # A ``set`` literal has no order the file fixes, so the
                    # one-for-one pairing below only applies to sequences.
                    if len(values) == len(target.elts) \
                            and not isinstance(value, ast.Set) \
                            and not any(isinstance(item, ast.Starred)
                                        for item in target.elts):
                        for name, item in zip(target.elts, values):
                            if _root_name(item) in roots \
                                    or _container_value(item, roots):
                                added.update(_target_names(name))
                    elif any(_root_name(item) in roots
                             or _container_value(item, roots)
                             for item in values):
                        added.update(names)
                    continue
                if isinstance(target, ast.Name) and (
                        _root_name(value) in roots
                        or _container_value(value, roots)):
                    # ``rows = [common]`` is the same object one hop away:
                    # ``_swap(*rows)`` / ``**[...]`` hands it on without the
                    # name, and ``_p = [common][0]`` indexes it back out.
                    added.add(target.id)
                if value is not None and _container_value(value, roots) \
                        and not _root_name(value) in roots:
                    # ``rows = [common]`` holds the object without being it;
                    # ``for e in rows`` hands the elements out one at a time.
                    # Tracked apart from ``roots`` so that a loop whose
                    # iterable merely *mentions* a seeded name -- ``for line in
                    # sys.argv`` -- does not drag its target in.
                    new_holders |= _target_names(target)
            if isinstance(node, (ast.For, ast.AsyncFor)) \
                    and _iterates_holder(node.iter, holders, roots):
                # ``for e in x`` over a container this model tracks hands the
                # elements out under the loop target, exactly as indexing the
                # container would; ``_bound_pairs`` only follows a literal.
                added |= _target_names(node.target)
            if isinstance(node, (ast.Import, ast.ImportFrom)):
                for alias in node.names:
                    bound = alias.asname or alias.name.split('.')[0]
                    # ``from apparatus import common as _c``: the seeded name
                    # itself is bound under another spelling.
                    if alias.asname and bound not in roots \
                            and alias.name.split('.')[0] in roots:
                        added.add(bound)
        if not added - roots and not new_holders - holders:
            return roots
        roots |= added
        holders |= new_holders


def _parameters(tree, span):
    """Parameter names of every function defined outside the reviewed block.

    ``def _swap(x): x.A2_SCRIPTS = (...)`` writes through whatever object the
    caller hands it; ``_swap(common)`` needs no name the checks above look for.
    """
    names = set()
    for node in ast.walk(tree):
        if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            continue
        if _in_block(node, span):
            continue
        args = node.args
        for group in (args.posonlyargs, args.args, args.kwonlyargs):
            names.update(arg.arg for arg in group)
        for star in (args.vararg, args.kwarg):
            if star is not None:
                names.add(star.arg)
    return names


def _contains_root(node, roots):
    """True when ``node`` reads any of ``roots`` anywhere inside it.

    The precise question -- does this argument *become* the object -- is only
    answerable for a bare name.  Everything else (``*[common]``, ``**{'x': c}``,
    ``[common]``) is matched by containment so an expansion the model cannot
    follow fails closed instead of quietly passing.
    """
    return any(isinstance(child, ast.Name) and isinstance(child.ctx, ast.Load)
               and child.id in roots for child in ast.walk(node))


# Constructors that hand back a container holding what they were given, so
# ``list([common])`` is ``[common]`` under another spelling.  ``enumerate`` /
# ``zip`` / ``filter`` / ``map`` hand back iterators over the same elements
# (``zip`` interleaves two of its arguments), so a name bound to one of them
# reaches whatever the arguments hold.
_CONTAINER_CALLS = frozenset({'list', 'tuple', 'set', 'frozenset', 'dict',
                              'sorted', 'reversed', 'iter',
                              'enumerate', 'zip', 'filter', 'map'})
# Methods that hand back what the container already holds, so a call on a
# container the model tracks is one more hop to the same objects.  ``values`` /
# ``keys`` / ``items`` are the dict views: ``list(d.values())[0]`` is a value
# of ``d`` one call later, exactly like ``copy()``.
_CONTAINER_METHODS = frozenset({'copy', 'get', 'pop', 'popitem', 'setdefault',
                                'values', 'keys', 'items'})


def _container_elements(value, roots=None):
    """The elements a container expression spells out, or ``None``.

    ``[common]``, ``{'x': common}``, ``[common][0]``, ``[common] + [None]`` and
    ``list([common])`` all keep the same object one hop behind the bound name,
    and the two sides of the check (the watched roots and the ``sys`` tables)
    must agree on what counts -- they drifted apart once, one reading a
    container to any depth and the other only its top level.

    A comprehension contributes its *element* expression: ``[common for _ in
    (0,)]`` holds ``common``, while ``[f'{n}' for n in A2_SCRIPTS]`` holds
    strings over an iterable that merely names the seeded object -- following
    every iterable would drag each derived local into the watched set.  The one
    iterable worth following is a container this model already tracks, so
    ``[e for e in x]`` over ``x = [common]`` keeps ``common`` in view while
    ``[e['k'] for e in x]`` (a derived value) does not.  A method read off a
    literal container (``{'k': common}.get('k')``, ``[common].pop()``) hands
    out what the container holds, so it carries the container's elements.

    ``roots`` is optional: the ``sys`` side passes its own name sets so the
    same "already tracked container" follow applies there, and ``None`` keeps
    the purely literal reading.
    """
    if isinstance(value, ast.Subscript):
        return _container_elements(value.value, roots)
    if isinstance(value, ast.Dict):
        return ([key for key in value.keys if key is not None]
                + [item for item in value.values if item is not None])
    if isinstance(value, (ast.Tuple, ast.List, ast.Set)):
        return list(value.elts)
    if isinstance(value, ast.BinOp) \
            and isinstance(value.op, (ast.Add, ast.BitOr, ast.Mult,
                                      ast.Sub, ast.BitAnd, ast.BitXor)):
        # ``[common] + [None]`` / ``{'k': common} | _d`` / ``[common] * 3``: one
        # half is written out, so the result holds what that half holds even
        # when the other half is an expression this model cannot read.  Dropping
        # the known half would be the fail-open direction.  The set operators
        # are here for the same reason: ``{common} - _x`` may still hold the
        # object, and the model does not evaluate the operands.
        halves = []
        for half in (value.left, value.right):
            part = _container_elements(half, roots)
            if part is None and roots is not None and _contains_root(half, roots):
                part = [half]  # ``x + [None]`` over a container already tracked
            if part is not None:
                halves.extend(part)
        return halves or None
    if isinstance(value, (ast.ListComp, ast.SetComp, ast.GeneratorExp)):
        if roots is not None and isinstance(value.elt, ast.Name) and any(
                _contains_root(generator.iter, roots)
                for generator in value.generators):
            return [generator.iter for generator in value.generators]
        return [value.elt]
    if isinstance(value, ast.DictComp):
        return [value.key, value.value]
    if isinstance(value, ast.Call):
        if isinstance(value.func, ast.Attribute):
            part = _container_elements(value.func.value, roots)
            if part is not None:
                return part
            if roots is not None and value.func.attr in _CONTAINER_METHODS \
                    and _contains_root(value.func.value, roots):
                return [value.func.value]  # x.copy() / x.pop() on a tracked container
            return None
        if _call_name(value.func) in _CONTAINER_CALLS:
            elements = []
            for argument in _call_values(value):
                part = _container_elements(argument, roots)
                if part is not None:
                    elements.extend(part)
                elif roots is not None and _contains_root(argument, roots):
                    elements.append(argument)  # list(x) / sorted(x) on a tracked container
            return elements or None
    return None


def _container_value(value, roots):
    """True when binding ``value`` keeps one of ``roots`` one step away.

    ``rows = [common]`` / ``kw = {'x': common}`` / ``_p = [common][0]``: the
    bound name is not the object itself, so ``_root_name`` (which strips
    attributes and subscripts but stops at any other node) answers ``None`` --
    yet ``_swap(*rows)``, ``_swap(**kw)`` and ``_p.update(...)`` all reach the
    object without ever spelling its name.  ``_container_elements`` fixes what
    counts as such a container; the root has to sit in an element, never in a
    comprehension's iterable.  A dict contributes its keys and its values --
    ``*d`` hands over the keys, ``**d`` the values.
    """
    elements = _container_elements(value, roots)
    return elements is not None and any(
        _contains_root(element, roots) for element in elements)


def _argument_pairs(call, order, stars):
    """``(parameter name or None, argument)`` pairs for one call site.

    Positional arguments are matched against the callee's positional parameter
    order and keywords by name.  ``None`` means the argument cannot be pinned to
    one parameter -- ``*args``/``**kwargs`` expansion, an argument past the end,
    or a keyword the signature does not name -- and the caller then fails closed.
    """
    pairs = []
    for index, argument in enumerate(call.args):
        if isinstance(argument, ast.Starred):
            pairs.append((None, argument.value))
            continue
        if order is None or index >= len(order):
            name = stars[0] if stars else None
        else:
            name = order[index]
        pairs.append((name, argument))
    for keyword in call.keywords:
        if keyword.arg is None:
            pairs.append((None, keyword.value))
        elif order is not None and keyword.arg in order:
            pairs.append((keyword.arg, keyword.value))
        elif len(stars) > 1:
            pairs.append((stars[1], keyword.value))
        else:
            pairs.append((None, keyword.value))
    return pairs


def _local_aliases(function, names):
    """``{local name: parameters it may stand for}`` inside one function body.

    ``def _swap(x): y = x; y.update(...)`` writes through ``x`` just like the
    one-line spelling, and so does the caller that renames before calling
    (``z = y; _swap(z)``).  The map only ever grows -- a name may stand for
    more than one parameter -- so the fixed point terminates even when
    parameters are assigned to each other (``a = b; b = a`` would otherwise
    flip two single-valued entries back and forth for ever).  A name that is
    never connected to a parameter simply is not in the map.
    """
    local = {name: {name} for name in names}
    while True:
        added = {}
        for child in ast.walk(function):
            for target, value in _bound_pairs(child):
                if not isinstance(target, ast.Name):
                    if isinstance(target, (ast.Tuple, ast.List)):
                        # ``_a, _b = x, None`` renames a parameter through an
                        # unpacking target.  Only a literal the file spells out
                        # says which element lands in which name; a call's
                        # result (``events, failures = read_events(root)``) says
                        # nothing, and reading its arguments as elements would
                        # turn every result name into an alias of the argument.
                        values, exact = _literal_values(value)
                        if values is None:
                            continue
                        ordered = exact and not isinstance(value, ast.Set) \
                            and len(values) == len(target.elts) \
                            and not any(isinstance(item, ast.Starred)
                                        for item in target.elts)
                        if ordered:
                            for name, item in zip(target.elts, values):
                                if not isinstance(name, ast.Name):
                                    continue
                                reached = set()
                                for read in _names_read(item, local):
                                    reached |= local[read]
                                if reached and not reached <= local.get(name.id, set()):
                                    added[name.id] = local.get(name.id, set()) | reached
                            continue
                        # ``a, *b = x, 0, 0`` / ``a, *b = {x, 0}``: the file does
                        # not fix which name takes which value, so every name on
                        # the left may stand for whatever the right reads.
                        reached = set()
                        for item in values:
                            for read in _names_read(item, local):
                                reached |= local[read]
                        for name in _target_names(target):
                            if reached and not reached <= local.get(name, set()):
                                added[name] = local.get(name, set()) | reached
                    continue
                root = _root_name(value)
                reached = set()
                if root in local:
                    reached = local[root]
                elif _container_value(value, local):
                    # ``rows = [x]`` then ``_swap2(*rows)``: the parameter is
                    # one literal hop away, exactly as at module scope.
                    for read in _names_read(value, local):
                        reached |= local[read]
                if not reached:
                    continue
                reached |= local.get(target.id, set())
                if not reached <= local.get(target.id, set()):
                    added[target.id] = reached
        if not added:
            return local
        for name, reached in added.items():
            local[name] = local.get(name, set()) | reached


def _defaults(arguments):
    """``{parameter name: [default expressions]}`` for one ``ast.arguments``.

    A list, not one expression: a name defined more than once (``def _swap(x=
    common): ...`` above, ``def _swap(x=0): ...`` below) is callable through
    either definition, and a call written between the two runs the first one.
    Keeping only the last default parsed let the watched value disappear; the
    merge in ``_mutating_functions`` appends instead of replacing.
    """
    defaults = {}
    positional = arguments.posonlyargs + arguments.args
    for arg, default in zip(positional[len(positional) - len(arguments.defaults):],
                            arguments.defaults):
        defaults.setdefault(arg.arg, []).append(default)
    for arg, default in zip(arguments.kwonlyargs, arguments.kw_defaults):
        if default is not None:
            defaults.setdefault(arg.arg, []).append(default)
    return defaults


def _mutating_functions(tree, span):
    """Functions defined outside the block that reach a write through a parameter.

    ``def _swap(x): x.update(...)`` reaches the block's objects through a call
    that hands it one, under whatever name the callee uses for it
    (``y = x; y.update(...)`` is the same parameter).  Which parameter matters:
    the apparatus's own ``def check_append_only(root, base, notes)`` appends to
    ``notes``, so a call that hands it only ``ROOT`` cannot write anything the
    block bound.  A parameter that is only *passed on* counts too
    (``def _a(y): _swap(y)``), followed to a fixed point, and so does a
    parameter whose default is a watched object (``def _swap(x=common)``).
    Records ``{name: (positional names, star names, mutated names, defaults)}``;
    the positional list is ``None`` when the name is defined more than once, and
    then any watched argument counts.  ``lambda`` bodies are recorded under every
    name they are bound to (chained assignment and walrus included); one that is
    handed straight to a call has no name, so its id is returned separately and
    the call site decides whether it can receive a watched object.  Attribute
    and subscript targets on a parameter are already caught by
    ``_foreign_writes``' own store rule.
    """
    records = []
    unbound = set()
    lambda_owners = {}
    for node in ast.walk(tree):
        if _in_block(node, span):
            continue
        for target, value in _bound_pairs(node):
            if isinstance(value, ast.Lambda):
                lambda_owners.setdefault(id(value), set()).update(_target_names(target))
    for node in ast.walk(tree):
        if _in_block(node, span):
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            arguments, body, owners = node.args, node, [node.name]
        elif isinstance(node, ast.Lambda):
            arguments, body = node.args, node.body
            # A lambda reaches its parameters only through a name it is bound
            # to, and every binding form ``_bound_pairs`` reads counts.  One
            # handed straight to a call cannot be named at all: collect its id
            # so the call site decides whether it can be handed a watched
            # object (``sorted(rows, key=lambda r: r.setdefault(...))`` cannot).
            owners = sorted(lambda_owners.get(id(node), ()))
        else:
            continue
        order = [arg.arg for arg in arguments.posonlyargs + arguments.args]
        order += [arg.arg for arg in arguments.kwonlyargs]
        stars = [star.arg for star in (arguments.vararg, arguments.kwarg)
                 if star is not None]
        names = set(order) | set(stars)
        if not names:
            continue
        local = _local_aliases(body, names)
        mutated = set()
        for child in ast.walk(body):
            # The receiver is matched by containment, not by its bare root name:
            # ``[x][0].update(...)`` is the same write as ``x.update(...)``, and
            # ``_root_name`` stops at the list literal.
            if isinstance(child, ast.Call) and isinstance(child.func, ast.Attribute) \
                    and child.func.attr in _MUTATOR_ATTRS:
                for read in _names_read(child.func, local):
                    mutated |= local[read]
            elif isinstance(child, (ast.Attribute, ast.Subscript)) \
                    and isinstance(child.ctx, (ast.Store, ast.Del)):
                # ``def _swap(x): _a, _b = x, None; _a.A2_SCRIPTS = ()``: a
                # store through a renamed parameter edits the object the
                # caller handed over, exactly like ``x.update(...)``.  ``del
                # x['k']`` edits in place just the same.
                for read in _names_read(child, local):
                    mutated |= local[read]
        defaults = _defaults(arguments)
        if not owners:
            # Nobody can call this lambda by name; it only matters when the call
            # it sits in can hand its parameter a watched object.  That is the
            # call site's question, so remember the node instead of failing the
            # whole file for a ``lambda r: r.setdefault(...)`` over plain rows.
            if mutated:
                unbound.add(id(node))
            continue
        for name in owners:
            records.append({'name': name, 'order': order, 'stars': stars,
                            'mutated': set(mutated), 'defaults': defaults,
                            'local': local, 'body': body})
    while True:
        changed = False
        known = {}
        for record in records:
            known.setdefault(record['name'], []).append(record)
        for record in records:
            for child in ast.walk(record['body']):
                if not isinstance(child, ast.Call):
                    continue
                callee = _call_name(child.func)
                if callee is None:
                    continue
                for target_record in known.get(callee, ()):
                    for target, argument in _argument_pairs(
                            child, target_record['order'], target_record['stars']):
                        # A bound method's instance offset is not visible here,
                        # and an expanded argument cannot be pinned either: then
                        # any parameter the argument reads may be the written
                        # one, so the write counts against all of them.
                        if not target_record['mutated']:
                            continue  # the callee writes nothing to pass on
                        if target is not None \
                                and not isinstance(child.func, ast.Attribute) \
                                and target not in target_record['mutated']:
                            continue
                        # only passed on: the same write, one call deeper
                        for name in _names_read(argument, record['local']):
                            for parameter in record['local'][name]:
                                if parameter not in record['mutated']:
                                    record['mutated'].add(parameter)
                                    changed = True
        if not changed:
            break
    found = {}
    for record in records:
        if not record['mutated']:
            continue
        name = record['name']
        if name in found:
            previous = found[name]
            # Every definition's defaults stay in reach: which one a call runs
            # depends on where the call sits, which this model does not track,
            # so replacing the older default let ``def _swap(x=0)`` below hide
            # the ``x=common`` above.
            merged = {key: list(value) for key, value in previous[3].items()}
            for key, value in record['defaults'].items():
                merged.setdefault(key, []).extend(value)
            found[name] = (None, (), record['mutated'] | previous[2], merged)
        else:
            found[name] = (record['order'], record['stars'],
                           set(record['mutated']),
                           {key: list(value)
                            for key, value in record['defaults'].items()})
    return found, unbound


def _names_read(node, local):
    """Local names in ``local`` that ``node`` reads."""
    return {child.id for child in ast.walk(node)
            if isinstance(child, ast.Name) and isinstance(child.ctx, ast.Load)
            and child.id in local}


def _watched_argument(call, entry, roots):
    """True when ``call`` hands a watched value to a parameter it edits.

    Arguments are matched by position and by keyword, so a call that passes a
    watched object to a parameter the function never touches is not a write.
    ``order is None`` means the name is defined more than once and the shape
    cannot be pinned; an argument the model cannot pin to one parameter
    (``*[common]``, ``**{'x': common}``, past the end) fails closed as soon as a
    watched object is anywhere inside it.  A mutated parameter the call does not
    supply is checked against its default.
    """
    order, stars, mutated, defaults = entry
    if not mutated:
        return False
    if isinstance(call.func, ast.Attribute):
        # ``obj._swap(common)`` / ``_C()._swap(common)``: whether the callee is
        # a bound method (and so whether the first parameter is the instance)
        # cannot be told from the call site, and guessing by the name of that
        # parameter -- ``self``, ``cls`` -- is wrong for ``def _swap(m, x)``.
        # Fail closed: a watched object anywhere in the receiver or the
        # arguments counts as handed to it.
        if _contains_root(call.func.value, roots):
            return True
        if order is None:
            # One name, several definitions: what this call supplies cannot be
            # pinned at all, so any watched object the call or a default can
            # hand over fails closed.
            return _contains_root(call, roots) or any(
                _contains_root(value, roots)
                for values in defaults.values() for value in values)
        pairs = list(_argument_pairs(call, order, stars))
        if any(_contains_root(value, roots) for _, value in pairs):
            return True
        # Without the instance offset the positional arguments may bind one
        # parameter earlier than listed, so the defaults are checked by name:
        # only a *mutated* parameter the call does not visibly supply can still
        # be the object that is written (``_C()._swap()`` with
        # ``def _swap(self, x=common)``).
        supplied = {name for name, _ in pairs if name is not None}
        for name in mutated:
            if name not in supplied and name in defaults \
                    and any(_contains_root(value, roots) for value in defaults[name]):
                return True
        return False
    supplied = set()
    for name, value in _argument_pairs(call, order, stars):
        if name is None:
            if _contains_root(value, roots):
                return True  # expansion this model cannot map: fail closed
            continue
        supplied.add(name)
        if name in mutated and _contains_root(value, roots):
            return True
    for name in mutated:
        if name not in supplied and name in defaults \
                and any(_contains_root(value, roots) for value in defaults[name]):
            return True  # def _swap(x=common): x.update(...)  ->  _swap()
    return False


def _call_values(call):
    """Every argument expression of ``call``, spreads included."""
    values = list(call.args)
    values.extend(word.value for word in call.keywords if word.value is not None)
    return values


# Calls that hand a callback the elements of another argument and nothing else.
# Every other call may keep the lambda and call it later with anything --
# ``partial(lambda o: o.update(...))`` handed to a name, a decorator applied to
# it -- so a nameless lambda anywhere else stays unplaced and fails closed.
_LAMBDA_CONSUMERS = frozenset({'sorted', 'min', 'max', 'map', 'filter',
                               'sort', 'reduce'})


def _rebound_names(tree, span=None):
    """Every name the file binds itself: assignment, ``def``, ``class``, import,
    parameter, ``match`` capture -- or a write into ``builtins``.

    A consumer whitelist is only worth its name while the name at the call site
    is still the builtin (or the method) it stands for.  ``sorted = _apply``,
    ``def sorted(f): ...``, ``def _outer(sorted): ...`` or ``case _ as sorted:``
    all turn ``sorted(lambda ...)`` into an arbitrary call this model cannot see
    into, so a rebound consumer name is no evidence at all.

    A write through ``builtins`` is the same rebinding without a ``Name``
    target: ``builtins.sorted = _apply`` and ``__builtins__['sorted'] = _apply``
    leave no local name behind, and which entry was replaced is not visible, so
    the whole consumer whitelist stops being trustworthy.
    """
    names = set(_parameters(tree, span))
    builtins_names = {'__builtins__'}
    for node in ast.walk(tree):
        if _in_block(node, span) or not isinstance(node, (ast.Import, ast.ImportFrom)):
            continue
        for alias in node.names:
            if alias.name != '*' and alias.name.split('.')[0] == 'builtins':
                builtins_names.add(alias.asname or 'builtins')
    while True:
        # ``_b = builtins`` is the same module under another name, at any depth,
        # and so is ``_b = [builtins][0]`` or ``_b, = (builtins,)`` -- the same
        # container hop the watched and ``sys`` sides already follow, so the
        # three alias systems agree on what "the module" means.
        aliases = set()
        for node in ast.walk(tree):
            if _in_block(node, span):
                continue
            for target, value in _bound_pairs(node):
                if value is not None and (
                        _root_name(value) in builtins_names
                        or _container_value(value, builtins_names)):
                    aliases |= _target_names(target)
        if aliases <= builtins_names:
            break
        builtins_names |= aliases
    for node in ast.walk(tree):
        if _in_block(node, span):
            continue
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            names.add(node.name)
        elif isinstance(node, (ast.Import, ast.ImportFrom)):
            for alias in node.names:
                if alias.name != '*':
                    names.add(alias.asname or alias.name.split('.')[0])
        elif isinstance(node, (ast.MatchAs, ast.MatchStar)):
            if node.name:
                names.add(node.name)
        elif isinstance(node, ast.MatchMapping):
            if node.rest:
                names.add(node.rest)
        elif isinstance(node, ast.ExceptHandler):
            if node.name:
                names.add(node.name)
        elif isinstance(node, (ast.For, ast.AsyncFor)):
            # ``for sorted in _fns:`` binds the name whatever the iterable is --
            # ``_bound_pairs`` only follows a literal sequence, so the loop
            # target is collected here for any iterable, and a comprehension's
            # target likewise.
            names |= _target_names(node.target)
        elif isinstance(node, (ast.ListComp, ast.SetComp, ast.DictComp,
                               ast.GeneratorExp)):
            for generator in node.generators:
                names |= _target_names(generator.target)
        receiver = None
        if isinstance(node, (ast.Attribute, ast.Subscript)) \
                and isinstance(node.ctx, ast.Store):
            receiver = node.value
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) \
                and node.func.attr in _MUTATOR_ATTRS:
            receiver = node.func.value  # __builtins__.update({'sorted': ...})
        if receiver is not None and _root_name(receiver) in builtins_names:
            names |= _LAMBDA_CONSUMERS
        for target, _ in _bound_pairs(node):
            names |= _target_names(target)
    return names


def _unbound_lambda_reaches(tree, unbound, roots, span):
    """True when a nameless lambda that edits a parameter may get a watched object.

    ``sorted(rows, key=lambda r: r.setdefault('k', 0))`` edits only the rows it
    is handed and never sees anything the bootstrap bound, while
    ``map(lambda o: o.update(...), (common,))`` hands it the module object.  The
    lambda has no name, so the call it appears in is the only evidence there is:
    fail closed when the call is not one of the consumers that only feed a
    callback its own elements, or when a watched root sits in the receiver or in
    another argument of that call.  ``partial(lambda ...)``, a decorator and a
    locally defined wrapper all keep the lambda for a later call this model
    cannot see, so they stay unplaced too.

    Only a lambda that *is* one of the call's own arguments is paired with it.
    ``sorted(rows, key=lambda o: _apply(lambda p: p.update(...)))`` looks like
    a consumer call from the outside, but the inner lambda is handed to
    ``_apply``, which may call it with anything -- descending into the argument
    would credit the outer ``sorted`` for a lambda it never receives.
    """
    unplaced = set(unbound)
    rebound = _rebound_names(tree, span)
    for node in ast.walk(tree):
        if _in_block(node, span) or not isinstance(node, ast.Call):
            continue
        values = _call_values(node)
        for value in values:
            if not (isinstance(value, ast.Lambda) and id(value) in unplaced):
                continue
            caller = _call_name(node.func)
            if caller not in _LAMBDA_CONSUMERS or caller in rebound:
                # The callee may call it later with anything -- and a
                # consumer name the file rebound is just a name.
                continue
            unplaced.discard(id(value))
            if _contains_root(node.func, roots) or any(
                    _contains_root(other, roots) for other in values):
                return True
    return bool(unplaced)


def _aliased_mutator(tree, mutating, roots, span):
    """True when a name is bound to a mutating callable instead of being called.

    ``_g = _C()._swap`` then ``_g(common)``: the callee is spelled ``_g``, so the
    attribute-call rule never sees it, and the definition it points at does edit
    its argument.  ``_g, _h = _C()._swap, None`` and
    ``_g = getattr(_C(), '_swap')`` reach the same callable through a container
    or a reflection, so the whole right-hand side is searched.  ``_g =
    common.update`` then ``_g({...})`` is the same trick one level down: the
    bound method of a watched object edits that object, and ``_g =
    getattr(sys.modules, 'update')`` spells the same method as a string.  Which
    object owns a function attribute is not modelled, so this fails closed
    either way.  A ``getattr`` whose arguments this model cannot read (an
    expansion, a variable attribute name) binds a callable nothing else in the
    file names: that is a write exactly when such a name is later *called with*
    a watched object, so those bindings are collected and matched against the
    call sites at the end instead of blocking every reflection in the file.  A
    mutator attribute read off an object this model does not track (``_g =
    dict.update``, ``getattr(dict, 'update')``) joins the same set, and the
    names that carry such a callable are followed to a fixed point (``_h =
    _g``, ``xs = [_g]``, ``_h, _i = _g, None``) before the call sites are
    matched -- one hop is all the reflection needs to walk around a name match.
    Binding order and scope are not modelled: a carrier is matched against
    every call of that name in the file, which can only over-block.
    """
    opaque = set()

    def _holders(target):
        """Names that will carry the callable, or ``None`` when unmodellable.

        ``_o.g = dict.update`` stores it on an attribute of an object: which
        object and which attribute a later ``_o.g(...)`` reaches is not tracked,
        so the target has to fail closed instead of contributing no name at all
        (``_target_names`` on an attribute target is empty).
        """
        if isinstance(target, ast.Name):
            return {target.id}
        if isinstance(target, (ast.Tuple, ast.List)):
            names = _target_names(target)
            return names or None
        return None

    for node in ast.walk(tree):
        if _in_block(node, span):
            continue
        for target, value in _bound_pairs(node):
            if value is None:
                continue
            for child in ast.walk(value):
                if isinstance(child, ast.Attribute):
                    if child.attr in mutating:
                        return True
                    if child.attr in _MUTATOR_ATTRS:
                        if _contains_root(child.value, roots):
                            return True  # _g = common.update / REQUIRED.update
                        # ``_g = dict.update`` / ``_g = [_s].append``: a mutator
                        # read off an object this model does not track.  It is
                        # only a write once it is *called with* a watched
                        # object, so it joins the same call-site match below.
                        holders = _holders(target)
                        if holders is None:
                            return True  # _o.g = dict.update
                        opaque |= holders
                if isinstance(child, ast.Call) and _call_name(child.func) == 'getattr':
                    if child.keywords or any(isinstance(arg, ast.Starred)
                                             for arg in child.args):
                        # ``getattr(*_pair)`` / ``getattr(_C(), name='_swap')``:
                        # which argument is the object and which the attribute
                        # is not visible, so the callable this binds cannot be
                        # named -- nor can what it edits.  (CPython's
                        # ``getattr`` takes no keyword at all, so the keyword
                        # form never binds; that is a runtime accident, not a
                        # reason to model it optimistically.)  Fail closed only
                        # while a watched object is actually inside, exactly as
                        # ``_foreign_writes`` does, so a clean delivery using
                        # ``getattr(*pair)`` on its own names still passes.
                        if any(_contains_root(argument, roots)
                               for argument in _call_values(child)):
                            return True
                        holders = _holders(target)
                        if holders is None:
                            return True  # _o.g = getattr(_C(), _n)
                        opaque |= holders
                        continue
                    if len(child.args) < 2:
                        continue  # getattr(x) is a TypeError, not a write
                    attr = child.args[1]
                    if not (isinstance(attr, ast.Constant)
                            and isinstance(attr.value, str)):
                        # ``_n = '_swap'; _g = getattr(_C(), _n)``: the attribute
                        # name is a string this model cannot read, so ``_g`` may
                        # be any method of any object.
                        if _contains_root(child.args[0], roots):
                            return True
                        holders = _holders(target)
                        if holders is None:
                            return True  # _o.g = getattr(_C(), _n)
                        opaque |= holders
                        continue
                    if attr.value in mutating:
                        return True
                    if attr.value in _MUTATOR_ATTRS:
                        if _contains_root(child.args[0], roots):
                            return True  # _g = getattr(sys.modules, 'update')
                        # ``_g = getattr(dict, 'update')``: a named mutator off an
                        # untracked object, matched at the call site like any
                        # other opaque binding.
                        holders = _holders(target)
                        if holders is None:
                            return True  # _o.g = getattr(_C(), _n)
                        opaque |= holders
    if not opaque:
        return False
    # A name bound to a callable the model cannot name keeps that callable
    # through every spelling ``_alias_roots`` follows -- ``_h = _g``,
    # ``xs = [_g]``, unpacking -- so the call site is matched on the carrier
    # set, not on the one name the reflection was bound to.  Only a name or an
    # element of a carrier can *call* it; ``xs.append(...)`` is a call on the
    # container, not on what it holds.
    carriers = _alias_roots(tree, opaque, span)
    for node in ast.walk(tree):
        if _in_block(node, span) or not isinstance(node, ast.Call):
            continue
        func = node.func
        values = _call_values(node)
        if (isinstance(func, ast.Name) and func.id in carriers) \
                or (isinstance(func, (ast.Subscript, ast.Attribute))
                    and _root_name(func) in carriers):
            # ``_f(common)`` / ``xs[0](common)`` / ``_o.g(common)``: the callee
            # is the reflection one spelling later.
            if any(_contains_root(value, roots) for value in values):
                return True  # _f = getattr(*_pair); _h = _f; _h(common)
        if any(_root_name(value) in carriers for value in values) \
                and any(_contains_root(value, roots) for value in values):
            # ``map(_g, (common,), ({'A2_SCRIPTS': ()},))``: the carrier is
            # handed to a consumer that will call it with the watched object.
            return True
    return False


def _sys_table(node, module_roots):
    """True when ``node`` is ``X.modules`` / ``X.path`` for a ``sys`` alias ``X``.

    The receiver is matched by containment, exactly as ``_foreign_writes``
    matches the receiver of a mutator call: ``[_s][0].modules`` names the table
    without spelling the module as a bare name, and ``_root_name`` stops at the
    list literal.
    """
    return (isinstance(node, ast.Attribute) and node.attr in _SYS_TABLES
            and _contains_root(node.value, module_roots))


def _sys_kinds_in(node, modules, tables):
    """``(module, table)`` kinds a single expression may hand over.

    ``X.modules`` is a table and is not read further: its receiver *is* the
    module name, and counting it as the module object as well would let a
    module-only spelling of the same name shadow the table again.
    """
    is_module = is_table = False
    stack = [node]
    while stack:
        child = stack.pop()
        if isinstance(child, ast.Attribute) and child.attr in _SYS_TABLES \
                and _contains_root(child.value, modules):
            is_table = True
            continue
        if isinstance(child, ast.Name) and isinstance(child.ctx, ast.Load):
            if child.id in modules:
                is_module = True
            if child.id in tables:
                is_table = True
            continue
        stack.extend(ast.iter_child_nodes(child))
    return is_module, is_table


def _container_sys_kinds(value, modules, tables):
    """``(module, table)``: what a container expression behind a name may hold.

    ``rows = [sys.modules]`` keeps a table one hop away and ``mods = [sys]``
    keeps the module itself; both reach the same writes as the bare names once
    the container is unpacked or indexed.  ``_container_elements`` fixes what
    counts as such a container, so a nested literal, a ``*`` expansion, a ``**``
    merge and a comprehension element are read at the same depth as the watched
    side -- the two drifting apart is what let a nested table slip through.

    The two kinds stay apart because ``X.modules`` asks "is this the module?"
    and ``_p.append`` asks "is this a table?", and a name that is both would
    answer differently depending on which question was asked first.  A
    container that hands over a table is therefore recorded as a table and not
    as a module: ``both = [sys, sys.path]`` reaches ``sys.path`` through
    ``both[1]``, and every write that spelling can make is still reached
    through the table side.
    """
    elements = _container_elements(value, modules | tables)
    if elements is None:
        return False, False
    is_module = is_table = False
    for element in elements:
        module, table = _sys_kinds_in(element, modules, tables)
        is_module = is_module or module
        is_table = is_table or table
    if is_table:
        return False, True
    return is_module, False


def _foreign_writes(tree, watched, span):
    """True when code outside the block can still rebind what the block bound.

    Pinning the block line by line says what the bootstrap *is*, not that it is
    the last word: ``global common`` in a later function, ``except ... as
    common``, ``del common``, ``common.A2_SCRIPTS = (...)`` or
    ``sys.modules['apparatus'] = evil`` all leave the block byte-identical while
    the interpreter ends up running something else.  The spellings that reach a
    binding without naming it are matched by attribute name (``__dict__``,
    ``__setattr__``) and by receiver, so ``import sys as _s``,
    ``builtins.setattr(...)`` or ``_c = common; _c.A2_SCRIPTS = ...`` do not
    walk around the check.
    """
    watched_roots = _alias_roots(tree, watched, span)
    module_roots, table_roots, table_bound = _sys_aliases(tree, span)
    # ``sys`` is in both sets because ``import sys`` binds the module, and
    # ``sys.argv.append(...)`` is not a write to the import table -- so plain
    # module names are left out of the receiver check.  A name that was *also*
    # bound to a table stays in: subtracting it is what let ``both = sys`` then
    # ``both = sys.path`` write the table.
    guarded_tables = (table_roots - module_roots) | table_bound
    roots = watched_roots | table_roots
    parameters = _parameters(tree, span)
    mutating, unbound = _mutating_functions(tree, span)
    if _aliased_mutator(tree, mutating, roots, span):
        return True  # _g = _C()._swap; _g(common): the callee is only a name
    if unbound and _unbound_lambda_reaches(tree, unbound, roots, span):
        return True  # a nameless lambda this call site can feed a watched object
    for node in ast.walk(tree):
        if _in_block(node, span):
            continue
        if isinstance(node, (ast.Global, ast.Nonlocal)) and set(node.names) & watched:
            return True
        if isinstance(node, ast.ExceptHandler) and node.name in watched:
            return True
        if isinstance(node, (ast.MatchAs, ast.MatchStar)) and node.name in watched:
            return True
        if isinstance(node, ast.MatchMapping) and node.rest in watched:
            return True
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load) \
                and node.id in _REFLECTIVE_CALLS:
            return True  # _f = setattr; _f(common, 'A2_SCRIPTS', ...)
        if isinstance(node, ast.Attribute) and isinstance(node.ctx, ast.Load) \
                and node.attr in _REFLECTIVE_CALLS | _DUNDER_MUTATORS:
            return True  # _f = builtins.setattr; _si = operator.setitem
        if isinstance(node, ast.ImportFrom):
            if any(alias.name == '*' or alias.name in _REFLECTIVE_CALLS | _DUNDER_MUTATORS
                   for alias in node.names):
                return True  # from builtins import setattr as _f / from operator import setitem
        if isinstance(node, ast.Delete) and any(
                _root_name(target) in roots or _contains_root(target, roots)
                for target in node.targets):
            return True  # del common / del sys.modules['apparatus']
        if isinstance(node, ast.Attribute) and node.attr in _NAMESPACE_ATTRS \
                and _contains_root(node, roots):
            return True  # common.__dict__, taken apart and written through
        if isinstance(node, (ast.Attribute, ast.Subscript)) \
                and isinstance(node.ctx, ast.Store) \
                and (_contains_root(node, roots) or _root_name(node) in parameters):
            # ``sys.modules[...] = ...`` names the table without spelling it as a
            # bare name, so the watched side is matched by containment; a
            # parameter only counts when the target is written *through* it
            # (``x.A = ...``), never when it only appears as the key.
            return True
        if isinstance(node, ast.Call):
            func = node.func
            if not isinstance(func, (ast.Name, ast.Attribute)):
                return True  # getattr(...)(...) -- a callee this model cannot name
            if _call_name(func) in _REFLECTIVE_CALLS:
                return True  # setattr(...) and builtins.setattr(...)
            name = _call_name(func)
            if name in mutating:
                if _watched_argument(node, mutating[name], roots):
                    return True  # def _swap(x): x.update(...); _swap(common)
            if isinstance(func, ast.Attribute) and func.attr in _DUNDER_MUTATORS:
                return True  # object.__setattr__(common, ...), operator.setitem(...)
            if isinstance(func, ast.Attribute) and func.attr in _MUTATOR_ATTRS:
                if _sys_table(func.value, module_roots):
                    return True  # sys.modules.update(...), sys.path.append(...)
                # The receiver is matched by containment: ``[_m][0].update(...)``
                # names the table without spelling it as a bare name.
                if _contains_root(func.value, guarded_tables):
                    return True  # _p = sys.path; _p.append(...) / _m.update(...)
                if _contains_root(func.value, watched_roots - table_roots):
                    return True  # common.__dict__.update(...), _c.clear()
            if _call_name(func) == 'getattr':
                if node.keywords or any(isinstance(arg, ast.Starred)
                                        for arg in node.args):
                    # ``getattr(*[common, _name])``: which argument is the
                    # attribute is not visible, so any watched root inside the
                    # call fails closed.
                    if any(_contains_root(value, roots) for value in _call_values(node)):
                        return True
                elif len(node.args) >= 2:
                    attr = node.args[1]
                    if not (isinstance(attr, ast.Constant) and isinstance(attr.value, str)):
                        if _contains_root(node.args[0], roots):
                            return True  # getattr(common, _name): the name is not visible here
                    elif attr.value in _REFLECTIVE_ATTRS:
                        return True  # getattr(common, '__dict__'), getattr(sys, 'modules')
    return False


def _bindings_confined(tree, canonical):
    """True when no name the bootstrap binds is bound again at module scope.

    ``common = other`` two lines after the block, a second ``_guard`` or
    ``_apparatus`` assignment, ``sys = None``: the reviewed bootstrap still sits
    there while the interpreter runs something else.  Every name the block binds
    itself may occur exactly as often as it does inside the block -- imports
    excluded, since the same module may legitimately be imported twice.
    """
    entry = _BLOCK_COUNTS.get(id(canonical))
    if entry is None:
        block = _parse('\n'.join(canonical))
        if block is None:
            return False
        imported = _import_names(block)
        watched = {name: 0 for name in imported}
        watched.update({name: count
                        for name, count in _module_stores(block).items()
                        if name not in imported})
        entry = (canonical, watched, imported)
        _BLOCK_COUNTS[id(canonical)] = entry
    _, watched, imported = entry
    actual = _module_stores(tree)
    if any(actual[name] != count for name, count in watched.items()):
        return False
    # An ``import ... as common`` binds no ``Name(Store)``: it would slip through
    # the counts while putting another module behind the loader result.
    return not (set(watched) & (_import_names(tree) - imported))


def _bootstrap_ok(tree, anchors, canonical):
    """True when the entry's bootstrap is exactly the block this tool models.

    Everything the entry does with ``common``/``_guard``/``_apparatus`` is
    trusted only while those statements are the reviewed ones: any other path,
    exec source, receiver or helper import changes a byte of the block and the
    answer is "cannot tell", never a guessed inventory.  Drift between the model
    and the real apparatus is a test failure (``test-check-frozen-delivery``),
    not a silently relaxed check.
    """
    entry = _TREE_SOURCE.get(id(tree))
    if entry is None or entry[0] is not tree:
        return False
    span = _bootstrap_span(entry[1], anchors)
    if span is None or _bootstrap_lines(entry[1], anchors) != list(canonical):
        return False
    if not _block_is_code(tree, span, anchors, canonical):
        return False
    if not _bindings_confined(tree, canonical):
        return False
    return not _foreign_writes(tree, set(_BLOCK_COUNTS[id(canonical)][1]), span)


def _file_parents(node):
    """True for ``Path(__file__).resolve().parents[2]``.

    ui-server-parity derives its repo root from the running script only;
    ``SPEC = ROOT / 'specs/ui-server-parity'`` names the directory this tool
    reads only while that derivation is the one the entry actually runs.
    """
    if not (isinstance(node, ast.Subscript)
            and isinstance(node.value, ast.Attribute) and node.value.attr == 'parents'
            and isinstance(node.slice, ast.Constant) and node.slice.value == 2):
        return False
    node = node.value.value  # Path(__file__).resolve()
    return (isinstance(node, ast.Call) and not node.args and not node.keywords
            and isinstance(node.func, ast.Attribute) and node.func.attr == 'resolve'
            and isinstance(node.func.value, ast.Call)
            and not node.func.value.keywords and len(node.func.value.args) == 1
            and isinstance(node.func.value.args[0], ast.Name)
            and node.func.value.args[0].id == '__file__'
            and isinstance(node.func.value.func, ast.Name)
            and node.func.value.func.id == 'Path')


def _loaded_through_common(tree):
    """True when ``common`` is the module's own loader result, not something else.

    The external value (``common.A2_SCRIPTS``) is only trustworthy while
    ``common`` still names ``scripts/gateway-parity/common.py`` -- the file this
    tool read.  The whole bootstrap between its two markers must be the reviewed
    block (fix-r19): ``__file__``-derived ``_here``/``_apparatus``,
    ``_guard_path = _apparatus / '_guard.py'``, the warm ``sys.modules.get`` and
    the cold ``spec_from_file_location``/``module_from_spec``/``exec`` path, and
    ``common = _load('common', _apparatus / 'common.py')``.  ``_evil /
    'common.py'``, ``_spec`` from another path, an exec of other bytes, a helper
    imported from elsewhere or a second binding of any of those names make the
    two disagree, and the answer is "cannot tell".
    """
    if not _bootstrap_ok(tree, _GATEWAY_BOOTSTRAP_ANCHORS, _GATEWAY_BOOTSTRAP):
        return False
    return all(_import_origin(tree, name, module)
               for name, module in _BOOTSTRAP_HELPERS)


def _external_still_holds(tree, name):
    """True when the module does not rebind ``name`` away from ``common.name``.

    The external value comes from another module (gateway's ``common.A2_SCRIPTS``
    read through ``A2_SCRIPTS = common.A2_SCRIPTS``).  A branch that rebinds it
    here -- ``A2_SCRIPTS = common.A2_SCRIPTS + ('extra.py',)``, a literal,
    ``from common import A2_SCRIPTS``, an unpacking target or a module-level
    walrus -- changes the interpreter's value while the external one stays put,
    so the answer is "cannot tell", not the stale tuple.  The binding must be
    the *only* module-scope store of the name (walrus / ``for`` / ``with``
    targets are ``Name(Store)`` too and are counted here).
    """
    if not _loaded_through_common(tree):
        return False
    # One plain module-scope binding of ``common.<name>`` and nothing else: a
    # module-level walrus, ``for``/``with ... as`` target or a second assignment
    # picks a value this parser never saw, and the external tuple would be the
    # stale one.  ``_sole_binding`` carries exactly that rule.
    value = _sole_binding(tree, name)
    return (isinstance(value, ast.Attribute) and value.attr == name
            and isinstance(value.value, ast.Name) and value.value.id == 'common')


def _tuple_value(tree, function, name, external=()):
    """Tuple bound to ``name`` inside ``function`` (local assignment first), else None.

    The local binding must be *the* binding: ``names = (...)`` followed by
    ``names += ('sixth.md',)``, a second assignment, or an annotated one leaves
    the interpreter with a value this parser never saw, so the shape is
    "cannot tell" (None).  Returning the first literal would keep registering a
    member the branch removed -- the same false-READY class ``_module_tuple``
    already guards against.
    """
    scope = list(_scope_nodes(function.body))
    stores = [node for node in scope
              if isinstance(node, ast.Name) and node.id == name
              and isinstance(node.ctx, ast.Store)]
    augments = [node for node in scope
                if isinstance(node, ast.AugAssign) and isinstance(node.target, ast.Name)
                and node.target.id == name]
    if augments or len(stores) > 1 or _unmodelled_binding(function, name):
        return None
    found = None
    if stores:
        for node in scope:
            targets, value = [], None
            if isinstance(node, ast.Assign):
                targets, value = node.targets, node.value
            elif isinstance(node, ast.AnnAssign) and node.value is not None:
                targets, value = [node.target], node.value
            for target in targets:
                if not (isinstance(target, ast.Name) and target.id == name):
                    continue
                try:
                    parsed = ast.literal_eval(value)
                except (ValueError, SyntaxError):
                    return None
                if not isinstance(parsed, tuple) or not all(
                        isinstance(item, str) for item in parsed):
                    return None
                found = parsed
    if stores and found is None:
        # The name is stored somewhere this parser does not model -- a walrus
        # (``names := (...)``), a ``for``/``with ... as`` target, a comprehension
        # target.  The interpreter then uses a value we never saw, so falling
        # back to a module-level copy would answer with the wrong tuple: "cannot
        # tell", never a guess.
        return None
    if found is not None:
        return found
    if name in external:
        return external[name] if _external_still_holds(tree, name) else None
    return _module_tuple(tree, name)


def _template(node):
    """``(prefix, suffix, name)`` for a one-placeholder f-string, else None."""
    parts = node.values
    if not parts:
        return None
    prefix = suffix = ''
    names = []
    for index, part in enumerate(parts):
        if isinstance(part, ast.Constant) and isinstance(part.value, str):
            if index == 0:
                prefix = part.value
            elif index == len(parts) - 1:
                suffix = part.value
            else:
                return None
        elif (isinstance(part, ast.FormattedValue) and isinstance(part.value, ast.Name)
                and part.format_spec is None and part.conversion in (-1, None)):
            names.append(part.value.id)
        else:
            return None
    return (prefix, suffix, names[0]) if len(names) == 1 else None


def _template_members(tree, function, parents, node, name, external=()):
    """Values ``name`` takes at ``node``: a comprehension's tuple, or a plain tuple."""
    cursor = parents.get(id(node))
    while cursor is not None:
        if isinstance(cursor, (ast.ListComp, ast.SetComp, ast.GeneratorExp, ast.DictComp)):
            for generator in cursor.generators:
                if not (isinstance(generator.target, ast.Name) and generator.target.id == name):
                    continue
                source = generator.iter
                if isinstance(source, ast.Name):
                    return _tuple_value(tree, function, source.id, external)
                if isinstance(source, (ast.Tuple, ast.List, ast.Set)):
                    try:
                        value = ast.literal_eval(source)
                    except (ValueError, SyntaxError):
                        return None
                    if isinstance(value, tuple) and all(isinstance(item, str) for item in value):
                        return value
                return None
            return None
        cursor = parents.get(id(cursor))
    return _tuple_value(tree, function, name, external)


def _gateway_manifest(tree, external=()):
    """Paths ``gateway-parity``'s ``manifest_inventory`` would give a FROZEN row.

    Modelled from the function's own expressions rather than from a regex over
    its bytes: members come from ``f'scripts/.../{name}'`` templates expanded
    over the tuple that actually feeds them, full path literals are taken
    verbatim, and the bare tuple used as manifest keys becomes the
    ``specs/gateway-parity/`` names.  A template whose tuple cannot be resolved,
    or a function with no literals at all, is "cannot tell" (None) instead of a
    guessed inventory: a wrong True hides a dropped member (false READY) and a
    wrong False demands chains ``--write`` never needs.
    """
    function = _function_node(tree, 'manifest_inventory')
    if function is None or _nested_scopes(function):
        return None
    parents = _parents(function)
    inventory, consumed, specs = set(), set(), set()
    template_parts = set()
    for node in _scope_nodes(function.body):
        if not isinstance(node, ast.JoinedStr):
            continue
        template = _template(node)
        if template is None:
            return None
        prefix, suffix, name = template
        members = _template_members(tree, function, parents, node, name, external)
        if members is None:
            return None
        for member in members:
            inventory.add(prefix + member + suffix)
            consumed.add(member)
        template_parts.update(id(part) for part in node.values)
    for node in _scope_nodes(function.body):
        if not isinstance(node, ast.Constant) or id(node) in template_parts:
            continue
        if isinstance(parents.get(id(node)), ast.Expr):
            continue  # docstring / bare string statement: prose, not a member
        value = node.value
        if not isinstance(value, str) or value in consumed:
            continue
        if '/' in value:
            if value.startswith('scripts/') or value.startswith('specs/'):
                inventory.add(value)
            continue
        if '.' in value:
            specs.add(f'specs/gateway-parity/{value}')
    if not inventory:
        return None
    return inventory | specs


def _call_name(node):
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        return node.attr
    return None


def _ui_exp008_dir(tree):
    """Directory ``required_files`` registers ``EXP008_FILES`` under, or None."""
    function = _function_node(tree, 'required_files')
    if function is None or _nested_scopes(function):
        return None
    found = set()
    for node in _scope_nodes(function.body):
        if not isinstance(node, (ast.SetComp, ast.ListComp, ast.GeneratorExp)):
            continue
        for generator in node.generators:
            if not (isinstance(generator.iter, ast.Name)
                    and generator.iter.id == 'EXP008_FILES'):
                continue
            element = node.elt
            if not (isinstance(element, ast.BinOp) and isinstance(element.op, ast.Div)):
                return None
            inner = element.left
            if (isinstance(inner, ast.BinOp) and isinstance(inner.op, ast.Div)
                    and isinstance(inner.right, ast.Constant)
                    and isinstance(inner.right.value, str)):
                found.add(inner.right.value)
            else:
                return None
    return found.pop() if len(found) == 1 else None


def _ui_required_names(tree):
    """True when ``REQUIRED`` is derived from ``SPEC_FILES``, else None.

    ``required_files`` registers ``folder / name for name in REQUIRED``; the
    precheck models the names from ``apparatus.py:SPEC_FILES``.  Today the two
    agree because ``check_frozen.py`` writes ``REQUIRED = set(SPEC_FILES)``, but
    a literal copy (or ``set(SPEC_FILES) | {...}``) would drift silently -- the
    same class as the copied spec list parity just lost.  Only the derived shape
    is accepted: anything else is "cannot tell" (None) and blocks.
    """
    stores = [node for node in _module_scope_nodes(tree)
              if isinstance(node, ast.Name) and node.id == 'REQUIRED'
              and isinstance(node.ctx, ast.Store)]
    if len(stores) != 1 or _unmodelled_binding(tree, 'REQUIRED'):
        return None
    # ``SPEC_FILES`` must be the tuple imported from ``apparatus`` -- the file
    # this tool read.  A local rebinding (``SPEC_FILES = SPEC_FILES[:-1]``), a
    # second import from elsewhere, or any other binding form puts a different,
    # invisible list behind ``REQUIRED``.
    if _imported_from(tree, 'SPEC_FILES') != 'apparatus':
        return None
    # ``REQUIRED.discard(...)`` / ``.update(...)`` edit the set in place, so the
    # binding still looks derived while the registered names are not.  Same for
    # the spellings that do not repeat the name: ``_r = REQUIRED; _r.update(...)``,
    # ``getattr(REQUIRED, 'update')(...)`` and ``_swap(REQUIRED)`` where the
    # callee edits its argument.
    aliases = _alias_roots(tree, {'REQUIRED'})
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Attribute) and func.attr in _MUTATOR_ATTRS \
                and _contains_root(func, aliases):
            return None  # [_r][0].update(...) names the set without spelling it
        if _call_name(func) in ('setattr', 'delattr') and node.args \
                and _contains_root(node.args[0], aliases):
            return None
        if _call_name(func) == 'getattr' and len(node.args) >= 2 \
                and _contains_root(node.args[0], aliases) \
                and isinstance(node.args[1], ast.Constant) \
                and node.args[1].value in _MUTATOR_ATTRS:
            return None
        # Handing the set to a function is handing it a way to edit: the callee
        # body may be ``def _swap(x): x.update(...)``.  The real reader only
        # iterates ``REQUIRED`` in place, so nothing legitimate is passed on.
        # ``_swap(*[REQUIRED])`` / ``_swap(**{'x': REQUIRED})`` reach the callee
        # through an expansion, so the whole argument expression is searched.
        if any(_contains_root(arg, aliases) for arg in node.args) \
                or any(_contains_root(word.value, aliases) for word in node.keywords):
            return None
    # A default value is handed to the callee just as an argument is:
    # ``def _swap(x=REQUIRED): x.update(...)`` then ``_swap()`` edits the set
    # without the call site naming it.  ``_g = REQUIRED.update`` then
    # ``_g({'evil.md'})`` is the same edit under another name.
    for node in ast.walk(tree):
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            arguments = node.args
            defaults = list(arguments.defaults) + [value for value in arguments.kw_defaults
                                                   if value is not None]
            if any(_contains_root(value, aliases) for value in defaults):
                return None
        if isinstance(node, ast.Attribute) and node.attr in _MUTATOR_ATTRS \
                and isinstance(node.ctx, ast.Load) \
                and _contains_root(node.value, aliases):
            return None  # REQUIRED.update, bound or called under another name
    for node in tree.body:
        targets, value = [], None
        if isinstance(node, ast.Assign):
            targets, value = node.targets, node.value
        elif isinstance(node, ast.AnnAssign) and node.value is not None:
            targets, value = [node.target], node.value
        for target in targets:
            if not (isinstance(target, ast.Name) and target.id == 'REQUIRED'):
                continue
            if isinstance(value, ast.Name) and value.id == 'SPEC_FILES':
                return True
            if (isinstance(value, ast.Call) and isinstance(value.func, ast.Name)
                    and value.func.id == 'set' and len(value.args) == 1
                    and not value.keywords
                    and isinstance(value.args[0], ast.Name)
                    and value.args[0].id == 'SPEC_FILES'):
                return True
            return None
    return None


def _ui_scan_dirs(tree):
    """The ``scan(root / '<dir>')`` directories in ``apparatus_files``, or None."""
    function = _function_node(tree, 'apparatus_files')
    if function is None or _nested_scopes(function):
        return None
    found = set()
    for node in _scope_nodes(function.body):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id == 'scan' and len(node.args) == 1):
            if isinstance(node, ast.Call) and _call_name(node.func) == 'scan':
                return None  # a scan shape this model cannot read
            continue
        argument = node.args[0]
        if (isinstance(argument, ast.BinOp) and isinstance(argument.op, ast.Div)
                and isinstance(argument.right, ast.Constant)
                and isinstance(argument.right.value, str)):
            found.add(argument.right.value)
        else:
            return None
    return found or None


def parity_spec_names(root, commit=None):
    """Spec-document names parity's ``manifest_inventory`` registers, or None.

    Derived from the apparatus's own ``names`` tuple so a changed list shows up
    as a disagreement instead of a stale copy.  ``None`` means the source could
    not be read or the binding is not a unique literal tuple -- and the caller
    then fails closed.  There is deliberately no fallback to the copied literal:
    a branch that reshapes ``names`` (``names = SPEC_NAMES``) would otherwise
    silently switch the cross-check off, and a renamed/added/removed spec doc
    after that reads as READY while ``--write`` cannot register it.  That is the
    same coarse-fallback class this tool already removed elsewhere.
    """
    commit = commit or pinned_head()
    key = ('parity-spec-names', str(root), commit)
    if key in _SOURCE_CACHE:
        return _SOURCE_CACHE[key]
    names = None
    source = source_text(root, commit, 'scripts/parity/check_frozen.py')
    tree = _parse(source) if source is not None else None
    if tree is not None:
        function = _function_node(tree, 'manifest_inventory')
        if function is not None and not _nested_scopes(function):
            value = _tuple_value(tree, function, 'names')
            if value is not None:
                names = set(value)
    _SOURCE_CACHE[key] = names
    return names


def _parity_manifest_paths(names):
    """Paths parity's ``manifest_path`` derives from its ``names`` tuple, or None.

    Mirrors the apparatus (``scripts/parity/check_frozen.py``): a name that
    already starts with ``scripts/`` or ``specs/`` is registered as written,
    every other name lives under ``specs/parity/``.  A non-canonical name is a
    ``ValueError`` there, i.e. ``--write`` cannot produce a target -- "cannot
    tell" here.
    """
    paths = set()
    for name in names:
        if not isinstance(name, str) or not name:
            return None
        parts = PurePosixPath(name).parts
        if (name.startswith('/') or '..' in parts
                or PurePosixPath(name).as_posix() != name):
            return None
        paths.add(name if name.startswith(('scripts/', 'specs/'))
                  else f'specs/parity/{name}')
    return paths


def gateway_inventory(root, commit=None):
    """Paths gateway-parity's ``--write`` would give a FROZEN row.

    Its inventory is a literal in the apparatus source (``manifest_inventory``
    over ``A2_SCRIPTS`` plus two session fixtures and five a5-19 files), *not*
    every file the source mentions: ``A3_SCRIPTS`` and ``DATA_FILES`` are part of
    the unregistered-file whitelist but get no hash row, so a basename match
    over the sources demands chains for files ``--write`` never registers (a
    dead end).  ``None`` means "cannot tell" -- an unreadable source or a shape
    this parser does not recognise.  There is deliberately no coarse fallback:
    "registrable" now also decides *drops*, so guessing True there would hide a
    removed member (false READY) instead of erring towards blocking.
    """
    commit = commit or pinned_head()
    key = ('gateway-inventory', str(root), commit)
    if key in _SOURCE_CACHE:
        return _SOURCE_CACHE[key]
    inventory = None
    common = source_text(root, commit, 'scripts/gateway-parity/common.py')
    frozen = source_text(root, commit, 'scripts/gateway-parity/check_frozen.py')
    if common is not None and frozen is not None:
        common_tree, frozen_tree = _parse(common), _parse(frozen)
        if common_tree is not None and frozen_tree is not None:
            scripts = _module_tuple(common_tree, 'A2_SCRIPTS')
            manifest = _gateway_manifest(frozen_tree, {'A2_SCRIPTS': scripts})
            if scripts is not None and manifest is not None:
                inventory = set(manifest) | {f'scripts/gateway-parity/{name}' for name in scripts}
    _SOURCE_CACHE[key] = inventory
    return inventory


def ui_inventory(root, commit=None):
    """ui-server-parity's registrable model, or ``None`` when source is unreadable.

    The scanned directories and the spec directory are read from *both* sides
    and must agree: ``apparatus.py:APPARATUS_PATHS`` (used by the git-status
    gate) and ``check_frozen.py``'s ``SPEC`` / ``apparatus_files`` (what
    ``required_files`` actually registers).  Reading only one of them, or
    trusting positions in the tuple, would drift silently the day either side
    changes -- and a wrong model either hides a drop (false READY) or demands
    chains ``--write`` never writes.  ``SPEC_FILES`` and ``TOOLCHAIN_FILES``
    are literals in ``apparatus.py``; a toolchain entry counts only when the
    file exists, because ``toolchain_files()`` is presence-based.
    """
    commit = commit or pinned_head()
    key = ('ui-inventory', str(root), commit)
    if key in _SOURCE_CACHE:
        return _SOURCE_CACHE[key]
    inventory = None
    source = source_text(root, commit, 'scripts/ui-server-parity/apparatus.py')
    frozen = source_text(root, commit, 'scripts/ui-server-parity/check_frozen.py')
    if source is not None and frozen is not None:
        tree, frozen_tree = _parse(source), _parse(frozen)
        if tree is not None and frozen_tree is not None:
            paths = _module_tuple(tree, 'APPARATUS_PATHS')
            specs = _module_tuple(tree, 'SPEC_FILES')
            toolchain = _module_tuple(tree, 'TOOLCHAIN_FILES')
            scan_dirs = _ui_scan_dirs(frozen_tree)
            spec_dir = _module_path(frozen_tree, 'SPEC')
            exp008_dir = _ui_exp008_dir(frozen_tree)
            required_ok = _ui_required_names(frozen_tree)
            # The apparatus module itself is loaded by the entry's own bootstrap
            # (``_boot_dir``/``_boot_source`` from ``__file__``), and ``ROOT``
            # anchors ``SPEC``; both are trustworthy only as the reviewed block.
            # A swapped ``_boot_source`` loads other bytes while this tool keeps
            # modelling the ``apparatus.py`` it read -- the same false READY the
            # gateway side closes with ``_loaded_through_common``.
            bootstrap_ok = _bootstrap_ok(frozen_tree, _UI_BOOTSTRAP_ANCHORS,
                                         _UI_BOOTSTRAP)
            root_ok = _file_parents(_sole_binding(frozen_tree, 'ROOT'))
            helpers_ok = _import_origin(frozen_tree, 'Path', 'pathlib') and all(
                _import_origin(frozen_tree, name, module)
                for name, module in _UI_BOOTSTRAP_HELPERS)
            # ``required_files`` registers exactly what ``SPEC_FILES``,
            # ``EXP008_FILES`` and ``toolchain_files()`` hold and scans what
            # ``scan()`` is pointed at.  All four must come from the
            # ``apparatus.py`` this tool read -- a local redefinition or a second
            # import registers a different list while the model stays put.
            imported_ok = all(_imported_from(frozen_tree, name) == 'apparatus'
                              for name in ('SPEC_FILES', 'EXP008_FILES',
                                           'toolchain_files', 'scan'))
            if (paths is not None and specs is not None and toolchain is not None
                    and scan_dirs is not None and spec_dir is not None
                    and required_ok is True and imported_ok
                    and bootstrap_ok and root_ok and helpers_ok
                    and exp008_dir is not None and exp008_dir in scan_dirs
                    and len(paths) == 3 and all(isinstance(item, str) for item in paths)
                    and all(item.startswith('scripts/') for item in paths[:2])
                    and paths[2].startswith('specs/')
                    and set(paths[:2]) == scan_dirs
                    and paths[2].rstrip('/') == spec_dir.rstrip('/')):
                known, present = True, set()
                for name in toolchain:
                    state = exists_at(root, commit, name)
                    if state is None:
                        known = False
                        break
                    if state:
                        present.add(name)
                if known:
                    inventory = {
                        'dirs': tuple(item.rstrip('/') + '/' for item in sorted(scan_dirs)),
                        'spec': spec_dir.rstrip('/') + '/',
                        'specs': set(specs),
                        'toolchain': present,
                    }
    _SOURCE_CACHE[key] = inventory
    return inventory


def registrable(root, kind, path):
    """True/False/None -- would ``--write`` give ``path`` a FROZEN row now?

    Only such a path can be a first registration, and only an unregistrable one
    can have its row dropped: accepting a chain for anything else blesses a
    failure ``--write`` cannot clear (parity's ``manifest_inventory`` names and
    ``git ls-files scripts/parity/``; gateway-parity's inventory and
    ui-server-parity's ``SPEC_FILES`` are written out in their own sources).

    ``None`` means "cannot tell" (git failed, or the inventory source does not
    have the shape this tool parses).  Callers must fail closed on it: treating
    an unreadable inventory as "not registrable" would let a delivery be judged
    against a ``-`` target the real ``--write`` will never produce.
    """
    if kind == 'parity':
        specs = parity_spec_names(root)
        if specs is None:
            return None  # shape not recognised: never guess in either direction
        manifest = _parity_manifest_paths(specs)
        if manifest is None:
            return None  # a name --write would reject: cannot model the target
        if path in manifest:
            return True
        if not path.startswith(PARITY_AUTO_DIR):
            return False
        return tracked_at_head(root, path)
    if kind == 'gateway-parity':
        inventory = gateway_inventory(root)
        if inventory is None:
            return None  # shape not recognised: never guess in either direction
        # ``--write`` registers exactly the inventory, wherever a member lives:
        # a member added outside the usual directories is still a row.
        return path in inventory
    if kind == 'ui-server-parity':
        inventory = ui_inventory(root)
        if inventory is None:
            return None
        if path in inventory['toolchain']:
            return True
        if path.startswith(inventory['spec']):
            return path[len(inventory['spec']):] in inventory['specs']
        if not path.startswith(inventory['dirs']):
            return False
        return tracked_at_head(root, path)
    return True  # unknown apparatus: keep demanding a chain (conservative)


def tracked_at_head(root, path):
    """True/False/None -- is ``path`` a file committed at HEAD? (one ls-tree per prefix)"""
    prefix = path.split('/', 1)[0] + '/' if '/' in path else path
    key = (str(root), prefix, pinned_head())
    if key not in _SOURCE_CACHE:
        code, out, _ = git(root, 'ls-tree', '-r', '-z', '--name-only',
                           pinned_head(), '--', prefix)
        # ``-z``: without it core.quotePath escapes non-ASCII paths, so the
        # names here would never equal the path being asked about.
        _SOURCE_CACHE[key] = set(filter(None, out.split('\0'))) if code == 0 else None
    listing = _SOURCE_CACHE[key]
    return None if listing is None else path in listing


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

    def consider(name):
        if BYTECODE.search(name):
            return
        state = registered_at(root, kind, name, mb)
        if state is None:
            issues.append(f'{name}: cannot tell whether it had a hash row at the base')
            return
        if state:
            return
        registration = registrable(root, kind, name)
        if registration is None:
            issues.append(f'{name}: cannot tell whether --write would register it')
            return
        if registration:
            added.append(name)

    extra = ()
    if kind == 'ui-server-parity':
        inventory = ui_inventory(root)
        if inventory is None:
            prefixes = ()
            issues.append('cannot tell ui-server-parity\'s inventory: '
                          'cannot tell which files --write registers')
        else:
            # ``SPEC_FILES`` and ``TOOLCHAIN_FILES`` are rows as well, and the
            # spec directory / toolchain entry need not live under the scanned
            # apparatus directories -- same question as the parity branch below.
            prefixes = inventory['dirs']
            known = {inventory['spec'] + name for name in inventory['specs']}
            known |= set(inventory['toolchain'])
            extra = sorted(p for p in known if not p.startswith(prefixes))
    elif kind == 'gateway-parity':
        # gateway registers exactly its inventory, wherever a member lives, so
        # the inventory *is* the scan set -- no directory walk can add to it.
        prefixes = ()
        inventory = gateway_inventory(root)
        if inventory is None:
            issues.append('cannot tell gateway-parity\'s inventory: cannot tell '
                          'which files --write registers')
        else:
            extra = sorted(inventory)
    else:
        # parity auto-registers every tracked file under ``scripts/parity/`` and
        # registers the spec documents its manifest names.  ``specs/parity/``
        # belongs to the inventory too: the apparatus only says "hash list
        # differs" for a new spec document that has no row, so an unscanned spec
        # directory is a false READY that ``--write`` turns into a missing chain.
        prefixes = PARITY_DIRS
        # ``registrable`` says True for every name in ``manifest_inventory``,
        # including one that already carries a ``scripts/``/``specs/`` prefix
        # and therefore lives outside the two scanned directories.  Scanning
        # only the prefixes would leave such a first registration unseen -- the
        # two questions must agree.
        specs = parity_spec_names(root)
        manifest = None if specs is None else _parity_manifest_paths(specs)
        if manifest is None:
            issues.append('cannot tell parity\'s manifest_inventory names: cannot tell '
                          'which files outside the scanned directories --write registers')
        else:
            extra = sorted(p for p in manifest if not p.startswith(prefixes))
    for prefix in prefixes:
        code, out, err = git(root, 'ls-tree', '-r', '--name-only', '-z',
                             pinned_head(), '--', prefix)
        if code:
            issues.append(f'cannot list files under {prefix}: {err.strip()}')
            continue
        for name in filter(None, out.split('\0')):
            consider(name)
    for name in extra:
        # A manifest name that is not in the tree at HEAD is the apparatus's own
        # problem (``--write`` cannot hash it); a first registration is about a
        # file that exists, so only tracked names are considered.
        present = tracked_at_head(root, name)
        if present is None:
            issues.append(f'{name}: cannot tell whether it exists at HEAD')
        elif present:
            consider(name)
    return sorted(set(added)), issues


def dropped_rows(root, kind, mb):
    """Paths that had a hash row at the merge-base but none in the new manifest.

    ``--write`` deletes these rows, so the real gate wants the chain to end at
    ``-`` (``new_hashes.get(name, '-')``).  The apparatus cannot report this
    before ``--write``: its ``new_hashes`` comes from the rows that are still
    there, so an ``old -> new`` chain matching the current bytes looks complete
    and the only failures left are the reducible ones -- a false READY.  This is
    the symmetric half of ``unregistered_added``: enumerate the drops here and
    demand the exact ``Change: <old> -`` chain.
    """
    if not mb:
        return [], []
    rows = manifest_rows(root, kind, mb)
    if rows is None:
        return [], ['cannot read the merge-base manifest rows']
    dropped, issues = [], []
    for name in sorted(rows):
        if BYTECODE.search(name):
            continue
        registration = registrable(root, kind, name)
        if registration is None:
            issues.append(f'{name}: cannot tell whether --write still registers it')
            continue
        if not registration:
            dropped.append(name)
    return dropped, issues


def dropped_path(ctx, name):
    """Repo-relative path when ``name`` is a row --write drops *and* the exact
    ``Change: <old> -`` chain backs the deletion; else None.

    A deletion is legitimate only when the row really disappears from the
    regenerated manifest (``dropped``) and the event chain ends at ``-``.  Both
    halves are checked independently here, so accepting the raw missing-file
    message cannot widen acceptance to any other failure.

    The candidate names come from ``manifest_normalise``, the same rule the drop
    set was built with: a reported bare name (parity's table key) and its
    ``specs/<kind>/`` form both resolve, so the two sides cannot drift apart.
    """
    relative = strip_reported(name)
    if relative is None:
        return None
    dropped = ctx.get('dropped') or ()
    candidates = [relative]
    prefixed = manifest_normalise(ctx['kind'], relative)
    if prefixed != relative:
        candidates.append(prefixed)
    for candidate in candidates:
        if candidate in dropped and chain_backed(ctx['root'], ctx['kind'], ctx['edges'],
                                                 candidate, ctx['mb']):
            return candidate
    return None


def aborted_path(ctx, detail):
    """Repo-relative path named by an aborted ``check_registration``, or None."""
    match = NONREGULAR_FILE.fullmatch(detail)
    if match:
        return strip_reported(match.group(1))
    if '[Errno 2]' in detail:
        # "[Errno 2] <locale text>: '<absolute path>'" -- take the quoted tail.
        tail = detail.rsplit(': ', 1)[-1].strip().strip("'\"")
        return repo_relative_any(ctx, tail)
    return None


def registration_gaps(root, kind, mb, edges, added):
    """Registration facts an aborted ``check_registration`` never got to check.

    Empty means this tool proves everything the aborted half would have decided:
    every merge-base row still matches its pinned bytes or ends at its exact
    chain target (the ``-`` of a dropped row included), every first registration
    has its ``Change: -`` chain, and historical event files are append-only.
    Only then may the raw abort message count as reducible.
    """
    gaps = list(base_event_issues(root, kind, mb))
    rows = manifest_rows(root, kind, mb)
    if rows is None:
        gaps.append('cannot read the merge-base manifest rows')
        return gaps
    for name in sorted(set(rows) | set(added)):
        if BYTECODE.search(name):
            continue
        if not registration_ok(root, kind, edges, name, mb):
            gaps.append(f'{name}: registration is not provable from an exact event chain '
                        '(the aborted check_frozen run never reported it)')
    return gaps


def classify(ctx, item):
    """'ok' | 'pending' | ('block', why, detail_or_None)."""
    root, kind, mb, edges = ctx['root'], ctx['kind'], ctx['mb'], ctx['edges']
    # parity: "<name>: hash mismatch"; ui-server-parity: "invalid path/hash: <name>".
    # A deleted row also surfaces here (the row is still in the manifest before
    # --write), so the provable-drop test is the second half of the same rule.
    for pattern in (r'(.+): hash mismatch', r'invalid path/hash: (.+)'):
        match = re.fullmatch(pattern, item)
        if match and (existing_file(root, kind, match.group(1))
                      or dropped_path(ctx, match.group(1))):
            return 'ok', None, None
    # gateway-parity: the manifest row name itself, unprefixed.
    if ' ' not in item and ':' not in item and (bare_manifest_file(root, kind, item)
                                                or dropped_path(ctx, item)):
        return 'ok', None, None
    # A registered file this branch deletes: before ``--write`` the old row is
    # still in the manifest, so the apparatus reports the missing path (or aborts
    # the registration half).  Both forms are reducible only with the exact
    # ``Change: <old> -`` chain; the row-dropped loop below still reports the
    # missing chain when there is one.
    match = ABORTED_REGISTRATION.fullmatch(item)
    if match:
        named = aborted_path(ctx, match.group(1))
        if (named and dropped_path(ctx, named)
                and not ctx.get('registration_gaps')):
            return 'ok', None, None
        return 'block', 'blocking', None
    match = MISSING_FILE.fullmatch(item)
    if match:
        if dropped_path(ctx, match.group(1)):
            return 'ok', None, None
        return 'block', 'blocking', None
    # First registration: --write adds the row; the added event row must back it.
    match = A2_MISSING.fullmatch(item)
    if match:
        # The real apparatus emits this only for names it collected, so the list
        # is never empty (``', '.join(unregistered)``) and every entry is a path
        # the row grammar accepts.  An empty list, an empty entry or a trailing
        # comma is therefore not this message: classify it as unknown and block
        # rather than let ``not unbacked`` read as reducible.
        names = [name.strip() for name in match.group(1).split(',')]
        # ``split`` always yields at least one element, so an empty entry /
        # trailing comma shows up as a name ``BARE_PATH`` rejects.
        if any(not BARE_PATH.fullmatch(name) for name in names):
            return 'block', 'blocking (malformed A2 hash list)', None
        unbacked = []
        for name in names:
            relative = strip_reported(name)
            if not relative or not chain_backed(root, kind, edges, relative, mb):
                unbacked.append(relative or name)
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
    # ui-server-parity reports a row whose file is no longer required as
    # "unexpected registered file: <absolute path>" -- the shape a member taken
    # out of ``SPEC_FILES`` leaves behind before ``--write``.
    match = UNEXPECTED_REGISTERED.fullmatch(item)
    if match:
        relative = repo_relative_any(ctx, match.group(1))
        if (relative and registered_at(root, kind, relative, mb) is True
                and chain_backed(root, kind, edges, relative, mb)):
            return 'ok', None, None
        return 'block', 'blocking', None
    match = LACKS_CHAIN.fullmatch(item)
    if match:
        name = match.group(1)
        # The name must be in the *old or the new* manifest.  The apparatus also
        # emits this for a path that only an event row mentions (an unused row
        # --write cannot clear), and blessing that is a dead end: --write would
        # never register it.  A member dropped from the inventory is in the old
        # manifest, so deletions still pass.
        in_old = registered_at(root, kind, name, mb) is True
        registration = registrable(root, kind, name)
        if registration is None:
            return 'block', 'blocking (cannot tell whether --write registers it)', None
        if (in_old or registration) \
                and chain_backed(root, kind, edges, name, mb):
            return 'ok', None, None
        return 'block', 'blocking', None
    # A Pending: placeholder can fail the two-family rule; that is expected.
    match = re.fullmatch(r'frozen/([^/:]+): ' + re.escape(PENDING_FAMILY_MSG), item)
    if match and match.group(1) in ctx['pending']:
        return 'pending', None, None
    # Recognised block markers win over the reducible hash-list message so a
    # composite message can never be silently accepted.  The hash-list message
    # itself is matched in full: a known prefix with an unknown tail is a
    # different failure the real gate would refuse.
    for marker in BLOCK_MARKERS:
        if marker in item:
            return 'block', 'blocking', None
    if item == HASH_LIST_MSG:
        return 'ok', None, None
    if 'merge-base == HEAD' in item:
        return 'block', 'blocking (branch has no commits of its own vs base)', None
    return 'block', 'blocking (unclassified, fail-closed)', None


# ------------------------------------------------------------ the two checks
def snapshot_entry(root, entry, snapshot):
    """The apparatus path inside the pinned snapshot, or None when it is absent.

    The checker itself must be committed: an uncommitted or untracked checker is
    not what the delivery is judged on, so it cannot be run from the snapshot.
    """
    try:
        relative = entry.resolve().relative_to(root.resolve())
    except (OSError, ValueError):
        return None
    candidate = snapshot / relative
    return candidate if candidate.is_file() else None


def window_verdict(entries_before, entries_after):
    """``(block, note)`` for one child's worktree-bookkeeping window.

    An unreadable sample is blocking: skipping the observation silently drops
    half of the write-scope promise (a child that reached out would leave no
    trace and the run would still be READY).  A *new* entry is deliberately
    only a note: git records no creator, so a concurrent sibling coordinator's
    ``git worktree add`` and a child escaping its snapshot are the same
    observation, and blocking on it was a false FAIL for siblings.  This run's
    own snapshot bookkeeping is still blocking (`scope_verdict`), and a
    delivery is not refused for someone else's worktree.
    """
    if entries_before is None or entries_after is None:
        return ('cannot verify the apparatus\'s write scope: the git worktree '
                'list was unreadable before or after it ran'), None
    appeared = sorted(entries_after - entries_before)
    if not appeared:
        return None, None
    return None, ('a git worktree entry appeared while an apparatus ran; it cannot '
                  'be attributed to that apparatus or to a concurrent sibling and '
                  'is reported as a diagnostic: ' + '; '.join(appeared[:5]))


def run_apparatus(root, kind, entry, base, mb, snapshot=None):
    """Run one check_frozen.py --base and classify its failures.

    ``snapshot`` is a throwaway checkout of the pinned commit.  The child runs
    there, never in the audited worktree: the audited HEAD (and its bytes) can
    move A -> B -> A inside the run window, and a child that read B while every
    evidence lookup here is pinned to A would yield a false READY.
    """
    result = {'kind': kind, 'blocking': [], 'reducible': [], 'pending': [],
              'pending_events': [], 'note': None}
    if entry is None:
        result['blocking'].append(('blocking', f'specs/{kind}/FROZEN.md exists but '
                                               f'scripts/{kind}/check_frozen.py is absent', None))
        return result
    child, cwd = entry, root
    if snapshot is not None:
        child = snapshot_entry(root, entry, snapshot)
        if child is None:
            result['blocking'].append(('blocking', f'{entry}: the apparatus is not '
                                                   'committed at the pinned snapshot', None))
            return result
        cwd = snapshot
    # Sample the git worktree bookkeeping around the child only: the window is
    # narrowed to the child's own runtime, and an unreadable sample is blocking
    # while an unattributable new entry is a diagnostic (see `window_verdict`).
    entries_before = worktree_entries(root)
    try:
        proc = subprocess.run([sys.executable, '-I', str(child), '--base', base],
                              capture_output=True, text=True, errors='replace',
                              cwd=str(cwd), timeout=CHECK_TIMEOUT)
        run_error = None
    except (OSError, subprocess.SubprocessError) as error:
        proc, run_error = None, error
    entries_after = worktree_entries(root)
    window_block, window_note = window_verdict(entries_before, entries_after)
    if window_block:
        result['blocking'].append(('blocking', window_block, None))
    if window_note:
        result['note'] = window_note
    if run_error is not None:
        result['blocking'].append(('blocking', f'cannot run {child}: {run_error}', None))
        return result
    try:
        report = json.loads(proc.stdout)
    except (ValueError, TypeError):
        tail = (proc.stderr or proc.stdout).strip().splitlines()[-3:]
        result['blocking'].append(('blocking', 'check_frozen did not emit parseable JSON '
                                               f'(rc={proc.returncode}): ' + ' | '.join(tail), None))
        return result
    status, failures, reason, unregistered = normalise(report)
    if reason:
        # Keep the window diagnostic collected above instead of dropping it.
        result['note'] = reason if result['note'] is None else result['note'] + ' | ' + reason
    edges, pending, issues = event_evidence(root, kind, mb)
    result['pending_events'] = sorted(pending)
    result['blocking'].extend(('blocking', message, None) for message in issues)
    # Enumerate the rows --write will add and drop *before* classifying: a
    # deleted row's raw missing-file message is only reducible when that row is
    # provably dropped with an exact chain.
    new_files, new_issues = unregistered_added(root, kind, mb)
    dropped, drop_issues = dropped_rows(root, kind, mb)
    ctx = {'root': root, 'kind': kind, 'mb': mb, 'edges': edges, 'pending': pending,
           'dropped': set(dropped), 'snapshot': snapshot}
    # An aborted ``check_registration`` skips its own chain loop and the
    # append-only check on historical event files.  Before the abort message may
    # count as reducible, re-prove both halves here; any gap keeps it blocking.
    if any(ABORTED_REGISTRATION.fullmatch(item) for item in failures):
        gaps = registration_gaps(root, kind, mb, edges, new_files)
        ctx['registration_gaps'] = gaps
        result['blocking'].extend(('blocking', message, None) for message in gaps)
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
    result['blocking'].extend(('blocking', message, None) for message in new_issues)
    reported = set()
    for name in sorted(set(new_files) | set(unregistered)):
        if BYTECODE.search(name) or name in reported:
            continue  # bytecode is already an apparatus error
        registration = registrable(root, kind, name)
        if registration is False:
            continue  # --write ignores it; demanding a chain would be a dead end
        if registration is None:
            result['blocking'].append(('blocking', f'{name}: cannot tell whether --write '
                                                   'would register it (inventory unreadable)', None))
            continue
        reported.add(name)
        if exists_at(root, pinned_head(), name) is not True:
            result['blocking'].append(('blocking', f'{name}: unregistered file is not '
                                                   'committed (git add/commit required)', None))
        elif not chain_backed(root, kind, edges, name, mb):
            result['blocking'].append(('blocking', f'{name}: first registration without an '
                                                   'exact Change: - <sha256> event chain '
                                                   '(--write would register it and check_frozen '
                                                   '--base would then reject)', None))
    # Dropped registrations: the apparatus cannot report these before --write
    # (its new_hashes still comes from the row that is there), so a member taken
    # out of the inventory -- or a deleted file -- would otherwise read READY.
    result['blocking'].extend(('blocking', message, None) for message in drop_issues)
    reported.update(dropped)  # a dropped row is not also a first registration
    for name in dropped:
        if chain_backed(root, kind, edges, name, mb):
            continue
        result['blocking'].append(('blocking', f'{name}: row dropped from the new manifest '
                                               'without an exact Change: <old> - event chain '
                                               '(--write would delete the row and check_frozen '
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
    code, out, err = git(root, 'diff', '--name-status', '--no-renames', mb, pinned_head(), '--', 'specs')
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
        code, data, err = git_bytes(root, 'cat-file', 'blob', f'{pinned_head()}:{path}')
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


# ------------------------------------------------------------ snapshot
# Every holder this process created and has not yet removed.  ``create_snapshot``
# publishes the directory *before* the (slow, killable) ``git worktree add``, so
# a Ctrl-C/SIGTERM landing inside it still has a record to clean up.
_SNAPSHOT_HOLDERS = []


def worktree_entries(root):
    """Paths git records as worktrees of ``root``; None when it cannot tell.

    This is the tool's *own* write scope beyond the throwaway holder: creating a
    snapshot adds a ``.git/worktrees/<id>`` entry and nothing else.  Comparing
    the list before and after the run is the verifiable half of "the precheck
    writes nothing outside a temporary snapshot" -- the before/after ``git
    status`` fingerprint only proves the working *tree* is unchanged, not that
    the git bookkeeping is.
    """
    code, out, err = git(root, 'worktree', 'list', '--porcelain')
    if code != 0:
        return None
    return {line[len('worktree '):] for line in out.splitlines()
            if line.startswith('worktree ')}


def create_snapshot(root, commit):
    """Throwaway checkout of ``commit`` for the child apparatuses.

    Returns ``(path, None)`` or ``(None, why)``.  The snapshot is built from the
    pinned SHA, not the symbolic ``HEAD``: ``git worktree add --detach`` names
    the commit explicitly, so a concurrent checkout cannot be captured.  Failure
    is blocking (fail closed) -- running a child against the audited worktree is
    exactly the A -> B -> A hole this closes.

    The holder is recorded before the checkout so an interrupted run can still
    drop it (and the ``.git/worktrees`` entry git may already have written).
    """
    holder = Path(tempfile.mkdtemp(prefix='cfd-snapshot-'))
    _SNAPSHOT_HOLDERS.append((root, holder))
    target = holder / 'tree'
    code, out, err = git(root, 'worktree', 'add', '--detach', '--quiet',
                         str(target), commit)
    if code:
        # ``git worktree add`` registers ``.git/worktrees/tree`` before it
        # starts the checkout, so a timed-out/killed add leaves an entry that
        # points at a directory this run is about to delete.
        drop_holder(root, holder)
        return None, (err.strip() or out.strip() or f'git worktree add rc={code}')
    return target, None


def _real_path(path):
    """Canonical spelling so ``/tmp``/``/private/tmp`` (macOS) compare equal."""
    try:
        return os.path.realpath(str(path))
    except (OSError, ValueError):
        return str(path)


def _admin_dir(root):
    """``.git/worktrees`` of the repo at ``root``, or None when it cannot tell."""
    code, out, _ = git(root, 'rev-parse', '--git-common-dir')
    if code != 0 or not out.strip():
        return None
    common = Path(out.strip())
    if not common.is_absolute():
        common = root / common
    return Path(_real_path(common)) / 'worktrees'


# Returned by ``_recorded_gitdir`` when an entry's ``gitdir`` file cannot be
# read.  It is deliberately not ``None``: a plain ``None`` compared unequal to
# every wanted path and made ``admin_entry_present`` answer False ("no entry
# names this target") for bookkeeping it had not read at all -- so a cleanup
# that could not read one entry reported the holder gone and deregistered it
# while the entry was still on disk.  Callers must treat any sighting of this
# value as "not proven clean" (fail closed).
_UNREADABLE_GITDIR = object()
_URL_SCHEME = re.compile(r'^[A-Za-z][A-Za-z0-9+.-]*://')


# ``.git/worktrees`` entries whose ``gitdir`` could not be read in this run.
# ``holder_verdict`` names them so a BLOCK points at the unverifiable
# bookkeeping instead of asserting that the snapshot's own entry is still
# there: the entry may well belong to a concurrent sibling.
_UNVERIFIED_ADMIN_ENTRIES = set()


def _recorded_gitdir(entry):
    """The path an admin entry's ``gitdir`` file names, canonicalised.

    Returns ``_UNREADABLE_GITDIR`` when the file cannot be read; callers must
    not read that as an absence (see the constant).  A readable but empty
    ``gitdir`` gets the same answer: ``git worktree add`` writes the path
    non-atomically, so a truncated file is a real possibility, and
    ``_real_path('')`` resolves to the *cwd* -- compared unequal to every
    wanted path, it read as "proven not ours" and let the cleanup deregister
    a holder whose entry was still on disk.

    A *relative* path is legitimate (``git worktree add --relative-paths`` /
    ``worktree.useRelativePaths``, git >= 2.48) and is resolved against this
    entry's own directory, which is what git writes it relative to.  Rejecting
    it as unreadable would block every cleanup on such a repository.

    Anything that cannot be a gitdir git wrote -- a NUL, a URL, or a path that
    does not end in ``/.git`` -- is corrupt too, and one shape matters: a
    *truncated* non-empty write (``/tmp/snap/tree`` cut to ``/tmp/snap/tr``)
    is readable, differs from the target, and would otherwise prove "not
    ours" while the entry is still on disk.  A cut that happens to land on an
    earlier ``/.git`` component (``/tmp/.git/folder/.../tree/.git`` cut to
    ``/tmp/.git``) still ends in ``/.git``; ``_admin_match`` recognises that
    one as a torn write of our own path, because no other worktree can be
    registered at a directory that contains this run's fresh holder.
    """
    try:
        text = (entry / 'gitdir').read_text(encoding='utf-8',
                                            errors='surrogateescape').strip()
    except OSError:
        return _UNREADABLE_GITDIR
    # A URL is never a gitdir, but ``://`` may also appear inside a perfectly
    # ordinary directory name (``/tmp/cache://v1/wt/.git``); only a leading
    # scheme marks a non-path.
    if (not text or '\x00' in text or _URL_SCHEME.match(text)
            or not text.endswith('/.git')):
        return _UNREADABLE_GITDIR
    path = Path(text)
    if not path.is_absolute():
        # ``.git/worktrees/<id>/gitdir`` holds e.g. ``../../../../wt/.git``
        # relative to ``.git/worktrees/<id>`` (measured with git 2.54).
        path = entry / path
    return _real_path(path)


def _admin_match(recorded, wanted):
    """Classify one readable ``gitdir`` against this holder's ``<tree>/.git``.

    ``ours`` / ``other`` / ``torn``.  A recorded path that is a *strict
    ancestor* of ``wanted`` is unprovable and therefore ``torn``: a real
    linked worktree is registered at its own root, so its gitdir
    (``<wt>/.git``, a file) is never a prefix of a holder that lives inside a
    *directory* (``TMPDIR=<wt>/tmp`` yields ``<wt>/tmp/...``, not
    ``<wt>/.git/...``).  The shapes that can be a prefix -- the repository's
    own ``.git`` directory, a bare gitdir, a path that is gone -- are exactly
    what a truncated write of our own path leaves behind, and calling that
    "proven not ours" would deregister the holder and call a leftover entry
    clean.  Reading a *file* ancestor as another worktree was tried and
    dropped: a torn write that lands on a file is not distinguishable from it
    (two reviewers, one P1), and the layout it was meant to protect does not
    produce an ancestor relationship at all.
    """
    if recorded == wanted:
        return 'ours'
    if wanted.startswith(recorded.rstrip('/') + '/'):
        return 'torn'
    return 'other'


def _scan_admin(admin, wanted):
    """One pass over ``.git/worktrees``.

    Returns ``(state, unreadable)``: state is True / False / None (an entry
    could not be read); ``unreadable`` names the entries that pass could not
    read.  Only the caller that finally answers ``None`` publishes them, so a
    window that a later pass resolves does not stay in ``holder_verdict``.
    """
    unreadable = []
    for entry in admin.iterdir():
        recorded = _recorded_gitdir(entry)
        if recorded is _UNREADABLE_GITDIR:
            unreadable.append(str(entry))
            continue
        verdict = _admin_match(recorded, wanted)
        if verdict == 'ours':
            return True, []
        if verdict == 'torn':
            unreadable.append(str(entry))
    return (None if unreadable else False), unreadable


def admin_entry_present(root, target):
    """True when some ``.git/worktrees`` entry still names ``target``.

    ``None`` means the bookkeeping could not be read, which callers treat as
    "not proven clean" rather than as an absence.  An entry whose ``gitdir``
    cannot be read is likewise unproven: unless a *different* entry is proven
    to name ``target`` (True), the answer is ``None``, never False.

    A lone unreadable entry is rescanned a couple of times first:
    ``git worktree add``/``remove`` creates the entry directory and writes its
    ``gitdir`` separately, so a concurrent sibling can be caught mid-write and
    a single pass would refuse a delivery for a window that is already gone.
    A *persistent* unreadable entry still answers ``None`` -- it may be ours,
    and reading unread bookkeeping as clean is the one direction that must
    stay blocking.
    """
    try:
        admin = _admin_dir(root)
        if admin is None:
            return None
        wanted = _real_path(target / '.git')
        seen = []
        for attempt in range(3):
            try:
                state, seen = _scan_admin(admin, wanted)
            except FileNotFoundError:
                return False      # no ``.git/worktrees`` directory at all
            except OSError:
                # Name what earlier passes already saw: this answer is still
                # None ("not proven clean"), and the verdict should say why.
                _UNVERIFIED_ADMIN_ENTRIES.update(seen)
                return None       # the directory itself cannot be read
            if state is not None:
                return state
            if attempt < 2:
                time.sleep(0.05)
        # Persistent: name these entries, and only these, in the verdict.
        _UNVERIFIED_ADMIN_ENTRIES.update(seen)
        return None
    except OSError:
        return None


def drop_admin_entry(root, target):
    """Remove the ``.git/worktrees/<id>`` entry git made for ``target``.

    ``git worktree prune`` would be repo-wide: it also drops *other* sessions'
    stale bookkeeping, which this run's write scope does not cover -- and the
    before/after ``worktree_entries`` comparison only sees additions, so the
    tool would never notice its own prune.  Matching on the entry's ``gitdir``
    file names exactly the target git registered for this holder, compared by
    canonical path so an equivalent spelling (``/private/tmp`` vs ``/tmp``, a
    symlinked holder root) still matches.

    Returns True only when no entry for ``target`` remains.  ``rmtree`` is
    best-effort, so the result is verified rather than assumed: a silent
    failure here must keep the holder registered for the ``atexit`` retry
    instead of letting a leftover be demoted to someone else's note.
    """
    try:
        admin = _admin_dir(root)
        if admin is None:
            return False
        wanted = _real_path(target / '.git')
        try:
            entries = list(admin.iterdir())
        except FileNotFoundError:
            return True
        except OSError:
            # ``Path.exists()`` answers False for EACCES too, and the old
            # check therefore reported "nothing to remove" for a directory it
            # could not read at all.  Unreadable bookkeeping is not clean.
            return False
        for entry in entries:
            recorded = _recorded_gitdir(entry)
            if recorded is _UNREADABLE_GITDIR:
                # Cannot prove this entry is (or is not) ours: leave it alone.
                # ``admin_entry_present`` below reports None and names it, so
                # the holder stays registered for the atexit retry.
                continue
            verdict = _admin_match(recorded, wanted)
            if verdict == 'torn':
                # A truncated write of our own gitdir: same rule as above.
                continue
            if verdict == 'ours':
                shutil.rmtree(entry, ignore_errors=True)
    except OSError:
        return False
    return admin_entry_present(root, target) is False


def _still_there(path):
    """True when ``path`` is on disk *or* cannot be proven gone.

    ``Path.exists()`` answers False for EACCES as well, so using it to prove a
    holder was removed would deregister one whose directory is still there --
    the same hole ``drop_admin_entry`` closed for ``.git/worktrees``.
    """
    try:
        os.stat(path)
    except FileNotFoundError:
        return False
    except OSError:
        return True
    return True


def drop_holder(root, holder):
    """Remove one snapshot holder and its git bookkeeping.

    The holder stays in ``_SNAPSHOT_HOLDERS`` until it is actually gone.  A
    SIGTERM landing inside the removal raises out of here (the ``SystemExit``
    the handler installed), and the ``atexit`` cleanup must still see the
    record to retry: dropping it first, as this used to, made an interrupted
    holder invisible and leaked it for good.  The same applies to a removal
    that merely *reports* success: both the holder directory and its
    ``.git/worktrees`` entry are verified gone before the record is dropped,
    and the return value says whether that happened.
    """
    target = holder / 'tree'
    entry_gone = None
    try:
        code, _, _ = git(root, 'worktree', 'remove', '--force', str(target))
        if code:
            # Already verifies (with the same backoff) that no entry for this
            # target remains; re-checking below would double the blocking
            # path's scan-and-sleep budget for nothing.
            entry_gone = drop_admin_entry(root, target)
        shutil.rmtree(holder, ignore_errors=True)
    except Exception:  # noqa: BLE001 -- cleanup must not mask the exit path
        return False
    if _still_there(holder):
        return False
    if entry_gone is False:
        return False
    if entry_gone is None and admin_entry_present(root, target) is not False:
        return False
    if (root, holder) in _SNAPSHOT_HOLDERS:
        _SNAPSHOT_HOLDERS.remove((root, holder))
    return True


def remove_snapshot(root, snapshot):
    """Drop the snapshot (and its ``.git/worktrees`` entry); never raise."""
    if snapshot is None:
        return
    drop_holder(root, snapshot.parent)


def holder_verdict(holders):
    """Blocking message for snapshot records this process could not remove.

    ``drop_holder`` only deregisters a holder after verifying both the holder
    directory and its ``.git/worktrees`` entry are gone, so anything left here
    is a real leftover: the run must not be reported READY as if it had written
    nothing, even though the ``atexit`` cleanup will retry once more.
    """
    if not holders:
        return None
    detail = '; '.join(sorted({str(holder) for _root, holder in holders})[:5])
    if _UNVERIFIED_ADMIN_ENTRIES:
        detail += (' -- and .git/worktrees has entries whose gitdir cannot be '
                   'read, or is a truncated prefix of this run\'s snapshot '
                   'path (they may belong to a concurrent sibling, not to this '
                   'snapshot): '
                   + '; '.join(sorted(_UNVERIFIED_ADMIN_ENTRIES)[:5]))
    return ('the precheck could not verify its own snapshot bookkeeping is gone; '
            'this run must not be treated as read-only: ' + detail)


def scope_verdict(before, after, snapshot):
    """``(block, note)`` for the run-level worktree comparison; either may be None.

    Only an entry this precheck created (the snapshot) surviving is blocking:
    the run cannot tell a concurrent sibling's ``git worktree add`` from a
    child reaching out, and a delivery must not be refused for the former.  A
    child's own addition is reported by ``run_apparatus`` as a diagnostic.
    """
    if before is None or after is None:
        return ('cannot verify the precheck\'s write scope: git worktree list is '
                'unreadable, so a leftover snapshot or a child-created worktree '
                'could still be on disk'), None
    leaked = sorted(after - before)
    ours = str(snapshot) if snapshot is not None else None
    # Compare resolved paths: ``$TMPDIR`` on macOS is a symlink (``/var`` ->
    # ``/private/var``), and git may echo either spelling back.  A textual
    # mismatch would silently demote *our own* leftover to a note -- the one
    # direction that must stay blocking.
    def same(path, other):
        try:
            return os.path.realpath(path) == os.path.realpath(other)
        except OSError:
            return path == other
    own = [path for path in leaked if ours is not None and same(path, ours)]
    block = None
    if own:
        block = ('the pinned snapshot\'s git bookkeeping was not removed '
                 f'({own[0]}): the precheck left a .git/worktrees entry behind')
        leaked = [path for path in leaked if path not in own]
    note = None
    if leaked:
        note = ('a git worktree entry this precheck did not create appeared during '
                'the run; it is outside the write-scope comparison and is not '
                'counted as a leak: ' + '; '.join(leaked[:5]))
    return block, note


def cleanup_snapshots():
    """Remove every holder this process still owns; never raise.

    Registered with ``atexit`` from ``main`` so a KeyboardInterrupt / SIGTERM
    that lands between ``create_snapshot`` and the run still leaves no
    ``cfd-snapshot-*`` directory and no ``.git/worktrees`` entry behind.
    """
    for root, holder in list(_SNAPSHOT_HOLDERS):
        drop_holder(root, holder)
    restore_sigterm()


_SIGTERM = {'previous': None, 'installed': False}


def _sigterm_exit(signum, _frame):
    raise SystemExit(128 + signum)


def install_sigterm_cleanup():
    """Make SIGTERM take the same SystemExit path Ctrl-C already takes.

    The default disposition kills the process outright, so neither ``finally``
    nor ``atexit`` runs and a SIGTERM inside ``git worktree add`` leaves the
    holder and its ``.git/worktrees`` entry on disk.  Best effort: a non-main
    thread or a platform without SIGTERM simply keeps the default.
    """
    try:
        _SIGTERM['previous'] = signal.getsignal(signal.SIGTERM)
        signal.signal(signal.SIGTERM, _sigterm_exit)
        _SIGTERM['installed'] = True
    except (ValueError, OSError, AttributeError):
        _SIGTERM['installed'] = False


def restore_sigterm():
    if not _SIGTERM['installed']:
        return
    try:
        signal.signal(signal.SIGTERM, _SIGTERM['previous'])
    except (ValueError, OSError, TypeError):
        pass
    _SIGTERM['installed'] = False


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

    print(f'check-frozen-delivery: root={root}')

    # Pin the SHA the run was started on and compare it at the end: every
    # evidence read (event rows, target bytes, inventory sources, ls-tree) uses
    # this SHA via ``pinned_head()``, not the symbolic ``HEAD``.  Pinning only
    # the merge-base would leave a HEAD A -> C -> A wobble inside the
    # classification window able to feed all lookups from C while the closing
    # snapshot still matches A -- a false READY for a tree the real gate fails.
    # The pin happens *before* discovery: the apparatus set is read from this
    # SHA too, so a checkout A -> B -> A during discovery cannot hide one.
    code, head_before, head_err = git(root, 'rev-parse', '--verify', 'HEAD')
    head_before = head_before.strip()
    if code or not head_before:
        print('=== head === BLOCKED')
        print('  BLOCK[blocking] cannot resolve HEAD: '
              f'{head_err.strip() or "<empty>"}')
        print('BLOCKED 1 / READY 0')
        return 1
    _PINNED_HEAD['sha'] = head_before

    discovery = discover(root, head_before)
    if discovery is None:
        print('=== discovery === BLOCKED')
        print('  BLOCK[blocking] cannot enumerate specs/*/FROZEN.md at the pinned '
              f'commit {head_before[:12]}: the apparatus set is unknown')
        print('BLOCKED 1 / READY 0')
        return 1
    apparatuses, invalid_specs = discovery
    if invalid_specs:
        # A non-tree ``specs/<alias>`` hides a directory the tool cannot
        # enumerate: the apparatus set is unknowable, so nothing this run says
        # can be READY.  Name each offender and its mode instead of dropping it.
        print('=== discovery === BLOCKED')
        for name, mode, otype in invalid_specs:
            print(f'  BLOCK[blocking] {name} is mode {mode} ({otype}), not a '
                  'directory tree: the apparatus set under specs/ cannot be '
                  'enumerated, so READY would be unsound')
        print('BLOCKED 1 / READY 0')
        return 1
    if not apparatuses:
        print(f'check-frozen-delivery: no specs/*/FROZEN.md at {head_before[:12]}')
        print('BLOCKED 1 / READY 0')
        return 1
    kinds = {kind for kind, _ in apparatuses}

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
    # invisible to the apparatus, so they stay a note -- but the full status is
    # also the before/after fingerprint that proves the run left the audited
    # tree untouched (P1: the promise must be verifiable, not asserted).
    code, dirty, dirty_err = git(root, 'status', '--porcelain', '--untracked-files=all')
    status_before = dirty
    tree_block = None
    if code != 0:
        tree_block = ('cannot check working-tree cleanliness, so HEAD could be hiding '
                      'behind uncommitted bytes: '
                      + (dirty_err.strip() or f'git status rc={code}'))
    else:
        tracked = [line for line in dirty.splitlines() if not line.startswith('??')]
        if tracked:
            tree_block = ('working tree has uncommitted tracked changes; the apparatus hashes '
                          'working-tree bytes, so HEAD is not what gets judged: '
                          + '; '.join(sorted(line.strip() for line in tracked)[:5]))

    # The tool's own write scope, sampled before anything of ours exists: a
    # temporary snapshot under ``$TMPDIR`` and the ``.git/worktrees`` entry git
    # creates for it, both removed again before this process exits.  This is the
    # whole run; per-child sampling inside ``run_apparatus`` narrows the window
    # for attributing a new entry to a child, and only an entry that is *ours*
    # surviving blocks here.
    atexit.register(cleanup_snapshots)
    install_sigterm_cleanup()
    worktrees_before = worktree_entries(root)

    # Pin the resolved SHA: pass it (not the symbolic ref) to every apparatus so
    # a concurrent ``git fetch`` cannot move the base between classification and
    # the child check_frozen runs.  event_chain_issues reuses the same SHA.
    # Children run in a throwaway checkout of that SHA, never in the audited
    # worktree: its HEAD can move A -> B -> A while they run, so a child reading
    # the runtime HEAD could judge B while every lookup here is pinned to A.
    snapshot, snapshot_error = create_snapshot(root, head_before)
    if snapshot is None:
        results = [{'kind': 'snapshot',
                    'blocking': [('blocking', 'cannot create the read-only snapshot the '
                                              'apparatuses must run in: ' + snapshot_error, None)],
                    'reducible': [], 'pending': [], 'pending_events': [], 'note': None}]
        issues, added = [], []
    else:
        try:
            results = [run_apparatus(root, kind, entry, mb, mb, snapshot)
                       for kind, entry in apparatuses]
            # The audited tree is out of the apparatuses' reach; this proves the
            # snapshot itself was not rewritten underneath a sibling run.
            write_code, written, write_err = git(snapshot, 'status', '--porcelain',
                                                 '--untracked-files=all')
            if write_code != 0 or written.strip():
                results.append({'kind': 'snapshot',
                                'blocking': [('blocking',
                                              'the apparatus changed its pinned snapshot during '
                                              'the run, so the reports do not describe one tree: '
                                              + (write_err.strip() or written.strip()), None)],
                                'reducible': [], 'pending': [], 'pending_events': [], 'note': None})
        finally:
            remove_snapshot(root, snapshot)
        # `remove_snapshot` only deregisters a holder once the directory *and*
        # its `.git/worktrees` entry are verified gone.  Anything still
        # registered is a leftover the run must not hide behind a READY.
        leftover = holder_verdict(_SNAPSHOT_HOLDERS)
        if leftover:
            results.insert(0, {'kind': 'worktree',
                               'blocking': [('blocking', leftover, None)],
                               'reducible': [], 'pending': [], 'pending_events': [],
                               'note': None})
        issues, added, _ = event_chain_issues(root, mb, kinds, mb=mb)

    # Write-scope proof (P1: the read-only promise must be verifiable, not
    # asserted).  The audited tree never hosts a child, so the only record this
    # run may leave behind is the snapshot's own -- and it must be gone by now
    # (enforced above through the holder registry).  Entries created by someone
    # else are a note, not a block: refusing the delivery because a sibling
    # coordinator created a worktree in the same repo would be a false BLOCK,
    # and git cannot attribute a new entry to a creator.  A child's own
    # addition is likewise reported by its run window as a diagnostic.
    worktrees_after = worktree_entries(root)
    scope_block, scope_note = scope_verdict(worktrees_before, worktrees_after, snapshot)
    if scope_block:
        results.insert(0, {'kind': 'worktree',
                           'blocking': [('blocking', scope_block, None)],
                           'reducible': [], 'pending': [], 'pending_events': [],
                           'note': None})

    # Re-sample the working tree: the first sample is taken before the apparatus
    # runs, so a file written during the run (editor auto-save, a leftover
    # checkout) would otherwise be invisible here -- the same false READY, just
    # inside the window.  A different fingerprint is blocking even when both
    # samples are clean (an A -> B -> A edit of a tracked file).
    code, dirty_after, dirty_err_after = git(root, 'status', '--porcelain',
                                             '--untracked-files=all')
    if tree_block is None and code != 0:
        tree_block = ('cannot re-check the working tree after the run: '
                      + (dirty_err_after.strip() or f'git status rc={code}'))
    elif tree_block is None and dirty_after != status_before:
        changed = '; '.join(sorted(line.strip() for line in
                                   set(dirty_after.splitlines()) ^ set(status_before.splitlines()))[:5])
        tree_block = ('the audited working tree changed during the run, so the deliverable is '
                      'not the tree that was judged: ' + (changed or 'status text differs'))

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

    if scope_note:
        print(f'  note: {scope_note}')

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
