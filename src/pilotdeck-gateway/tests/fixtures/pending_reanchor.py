# anchor-selfcheck: unsupported shared pending-reanchor registry, no mutation definitions
"""#1048: one strict registry for the checker and text-mutation executors."""
from dataclasses import dataclass
from datetime import date
from pathlib import Path, PurePosixPath
import re
import sys

FIXTURES = Path('src/pilotdeck-gateway/tests/fixtures')
REGISTRY = 'pending-reanchor.txt'
SKIPPED = 'SKIPPED(pending-reanchor)'


@dataclass(frozen=True)
class Entry:
    fixture: str
    mutation: str
    source_pr: str
    issue: str
    registered: str

    def matches(self, fixture, mutation):
        return self.fixture == fixture and self.mutation in ('*', mutation)

    def reason(self):
        return f'pending-reanchor: {self.source_pr} -> {self.issue}, {self.registered}'


def load(root):
    """Missing registry is compatible with old/synthetic trees; bad rows fail closed."""
    directory = Path(root).resolve() / FIXTURES
    path = directory / REGISTRY
    try:
        lines = path.read_text(encoding='utf-8').splitlines()
    except FileNotFoundError:
        return []
    entries = []
    for number, line in enumerate(lines, 1):
        if not line.strip() or line.lstrip().startswith('#'):
            continue
        fields = [part.strip() for part in line.split('·')]
        try:
            if len(fields) != 5 or not all(fields):
                raise ValueError('expected five nonempty fields separated by ·')
            fixture, mutation, source_pr, issue, registered = fields
            relative = PurePosixPath(fixture)
            if (relative.is_absolute() or '..' in relative.parts or '\\' in fixture
                    or relative.as_posix() != fixture or relative.suffix != '.py'):
                raise ValueError('fixture must be a canonical relative Python script path')
            script = (directory / fixture).resolve()
            if not script.is_relative_to(directory) or not script.is_file():
                raise ValueError('fixture must exist inside tests/fixtures')
            if mutation != '*' and not re.fullmatch(r'[\w.-]+', mutation):
                raise ValueError('mutation must be an exact name or *')
            if not all(re.fullmatch(r'#[1-9][0-9]*', value) for value in (source_pr, issue)):
                raise ValueError('source PR and reanchor issue must use #N')
            if date.fromisoformat(registered).isoformat() != registered:
                raise ValueError('date must use YYYY-MM-DD')
            entry = Entry(*fields)
            if any(old.fixture == fixture and (old.mutation == mutation
                   or '*' in (old.mutation, mutation)) for old in entries):
                raise ValueError('duplicate or overlapping registration')
            entries.append(entry)
        except ValueError as error:
            raise ValueError(f'{REGISTRY}:{number}: {error}') from error
    return entries


def for_script(root, script=None):
    """CLI identity defaults to argv[0]; imported callers can pass their script."""
    entries = load(root)
    if not entries:
        return []
    path = Path(sys.argv[0] if script is None else script).resolve()
    directory = Path(root).resolve() / FIXTURES
    if not path.is_relative_to(directory):
        raise ValueError('pending-reanchor requires a fixture script identity')
    relative = path.relative_to(directory).as_posix()
    return [entry for entry in entries if entry.fixture == relative]


def match(entries, name):
    return next((entry for entry in entries if entry.mutation in ('*', name)), None)


def counts(rows, states=('KILLED', 'SURVIVED', 'INVALID')):
    """Keep the old empty-registry shape; skipped controls have a separate count."""
    result = {state: sum(row['status'] == state for row in rows) for state in states}
    skipped = sum(row['status'] == SKIPPED for row in rows)
    if skipped:
        result['pending-reanchor'] = skipped
    return result


# Audited #1048 callers: every execution mode reaches one shared skip entry,
# with the same mutation names as anchor_selfcheck. New clients need that
# audit before enrollment; a dead/imported call is not an execution guarantee.
TEXT_HOSTS = {
    'file-history-cwd/negative_controls.py',
    'flake-progress/negative_controls.py',
    'live-session/negative_controls.py',
    'locked-storage/negative_controls.py',
    'metadata-from-env/negative_controls.py',
    'metadata-process-cwd/negative_controls.py',
    'model-catalog-cwd/negative_controls.py',
    'projector-cwd/negative_controls.py',
    'skills-external/negative_controls_949.py',
    'stats-cwd/negative_controls.py',
    'live-recording/negative.py',
    'manager-gateway/negative_controls.py',
    'turn-runner/forward_negative.py',
    'turn-runner/integrity_negative.py',
    'background-output/negative.py',
    'network-timing/negative_controls.py',
}


def supports_host(relative):
    return relative in TEXT_HOSTS
