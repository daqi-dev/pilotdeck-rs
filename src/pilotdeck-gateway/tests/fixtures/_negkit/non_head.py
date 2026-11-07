# anchor-selfcheck: unsupported #1540 shared non-HEAD-baseline verdict; no source mutation table
"""The shared exit-42 contract for negative-control baseline provenance (#1540).

A fixture runner that accepts ``PDRS_NEGATIVE_BASELINE`` (the sandbox
pre-commit escape hatch: the sandbox cannot commit, so it pins a
``git stash create`` dangling commit of the reviewed tree) follows one rule,
modelled on ``settings-gateway/negative_controls.py`` (#1318 fix-r3, the
exemplar): a run whose baseline came from that variable is *not host
evidence* — the host gate only trusts ``HEAD`` — so even a fully-killed run
must not exit like a green host run.  It prints ``NON-HOST EVIDENCE`` on
stdout, records ``baseline.json`` in the evidence directory, and exits with
the dedicated code ``NON_HEAD_EXIT`` (42), which the host gate reads as
"non-host evidence", never as PASS and never as an ordinary failure.  A run
that is not fully killed keeps its own non-zero code first, so 42 can only
ever mean "green, but not HEAD".

Wiring a runner takes two calls:

    baseline_source, baseline, baseline_sha = non_head.gate(root)
    ...
    with mutating_sources(root, paths, baseline=baseline) as originals:
        ...  # unchanged; the guard keeps resolving the ref itself
    sys.exit(non_head_exit(baseline_source, baseline_sha,
                           code=verdict, evidence=out))

``gate`` resolves the override to a full commit SHA once and announces it, but
the *ref* (not the SHA) is what callers keep passing to ``mutating_sources``,
so the byte guard behaves exactly as before this module existed; a HEAD run
prints nothing and runs no extra subprocess (its stdout stays byte-identical
to the pre-#1540 behavior).  An override that does not resolve is a refusal
(exit 2, "never started"), decided before any mutant runs — a run must not
start against a baseline it cannot even name.

``ignored=True`` covers the one suite that no longer honours the override at
all (``network-timing``; #994 made its formal path HEAD-only).  Its green
runs did test HEAD, so they are not called "non-HEAD" — the marker says the
requested override was ignored and the run therefore cannot certify the tree
the caller named, which is the mistake exit 0 invited.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_selection import refuse

# A dedicated exit code for "everything was killed, but the baseline was not
# HEAD, so this run is not host evidence" — distinct from 1 (a mutant not
# killed / INVALID) and from 2 (never started), so automation and humans
# cannot mistake a green non-HEAD sandbox run for a host-accepted run.
NON_HEAD_EXIT = 42


def gate(root, environ=None, *, ignored=False):
    """Resolve the caller's baseline choice; announce a non-HEAD one.

    Returns ``(source, ref, sha)``: ``source`` is ``'PDRS_NEGATIVE_BASELINE'``
    when the variable names any non-empty value (the exemplar treats a set
    variable as non-HEAD even if it spells ``HEAD`` — the gate cannot trust
    what a caller might still move), ``ref`` is that value verbatim for
    ``mutating_sources``, and ``sha`` is the resolved commit for the evidence
    markers.  With the variable unset or empty the triple is
    ``('HEAD', 'HEAD', None)`` and nothing is printed, so the HEAD path is
    byte-identical to a runner without this module.
    """
    env = os.environ if environ is None else environ
    override = env.get('PDRS_NEGATIVE_BASELINE')
    if not override:
        return 'HEAD', 'HEAD', None
    try:
        resolved = subprocess.run(
            ['git', '-C', str(root), 'rev-parse', '--verify', override + '^{commit}'],
            capture_output=True, text=True, errors='replace', timeout=60)
    except (subprocess.SubprocessError, OSError) as error:
        # #1540 review r1 boundary#3: a resolver crash (timeout, missing git)
        # is a refusal, never a traceback that the host triage would misread
        # as exit 1.
        refuse(f'cannot resolve PDRS_NEGATIVE_BASELINE {override!r}: {error}')
    if resolved.returncode != 0:
        refuse(f'cannot resolve PDRS_NEGATIVE_BASELINE {override!r}: '
               f'{resolved.stderr.strip()}')
    sha = resolved.stdout.strip()
    if ignored:
        print(f'NON-HEAD BASELINE REQUEST: {sha} via PDRS_NEGATIVE_BASELINE; '
              'this suite ignores the override and always guards HEAD; '
              'the host gate only trusts HEAD evidence', flush=True)
    else:
        print(f'NON-HEAD BASELINE: {sha} via PDRS_NEGATIVE_BASELINE; '
              'the host gate only trusts HEAD evidence', flush=True)
    return 'PDRS_NEGATIVE_BASELINE', override, sha


def non_head_exit(baseline_source, baseline_sha=None, *, code=0, evidence=None,
                  ignored=False):
    """Return the process exit code for a finished run (call it last).

    A non-green verdict keeps its own code untouched — a surviving mutant
    (1) or a never-started refusal (2) must never be reported as non-host
    evidence instead.  Only a green run against a non-HEAD baseline is
    downgraded: the ``NON-HOST EVIDENCE`` marker goes to stdout, a
    ``baseline.json`` provenance record is published atomically into
    ``evidence`` (the evidence directory, when the caller has one), and the
    dedicated ``NON_HEAD_EXIT`` replaces the green 0.  An unusable verdict
    value (non-int, bool, or outside the POSIX 0–255 window) is normalised
    to 1 first: ``SystemExit(None)`` would exit 0 and an oversized code would
    truncate to it, both fail-open against the same mistake.
    """
    if isinstance(code, bool) or not isinstance(code, int) or not 0 <= code < 256:
        # #1540 review r1 boundary#2: never let an unusable verdict become a
        # green exit.
        code = 1
    if code != 0:
        return code
    if baseline_source == 'HEAD':
        return 0
    if ignored:
        detail = (f'{baseline_sha} via PDRS_NEGATIVE_BASELINE, which this suite '
                  'ignores (its guard always requires HEAD); the run cannot '
                  'certify the named baseline')
    else:
        detail = f'{baseline_sha} came from PDRS_NEGATIVE_BASELINE, not HEAD'
    print(f'NON-HOST EVIDENCE (非宿主证据): baseline {detail}; '
          f'the host gate only trusts HEAD evidence; exit {NON_HEAD_EXIT}',
          flush=True)
    if evidence is not None:
        document = {'baseline': baseline_sha,
                    'baselineSource': baseline_source,
                    'nonHeadBaseline': True,
                    'hostEvidence': False,
                    'exit': NON_HEAD_EXIT}
        if ignored:
            # #1540 review r1 boundary#4 / r2 boundary#4: the record must not
            # imply the requested tree was tested when the guard ignored the
            # request and stayed on HEAD — including via ``nonHeadBaseline``,
            # which keeps its repo-wide meaning "the tested baseline is not
            # HEAD" (settings-gateway/native-settings define it the same way).
            document.update(baseline='HEAD', requestedBaseline=baseline_sha,
                            overrideIgnored=True, nonHeadBaseline=False)
        temporary = Path(evidence) / 'baseline.json.tmp'
        temporary.write_text(json.dumps(document, indent=2) + '\n', encoding='utf-8')
        temporary.replace(Path(evidence) / 'baseline.json')
    return NON_HEAD_EXIT
