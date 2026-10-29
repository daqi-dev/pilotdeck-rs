"""Draft normalization policy: explicit context + complete JSON field paths only."""
# Isolated bootstrap: verify the source loader before executing local code.
import sys as _boot_sys
if not (_boot_sys.flags.isolated and _boot_sys.flags.ignore_environment
        and _boot_sys.flags.no_user_site and _boot_sys.pycache_prefix is None):
    print('{"verdict":"FAIL","errors":["Python isolation required: use python3 -I; no pycache prefix"]}')
    raise SystemExit(1)
if _boot_sys.version_info < (3, 11) or 'os' not in _boot_sys.modules:
    print('{"verdict":"FAIL","errors":["os must be preloaded from CPython 3.11+ frozen modules"]}')
    raise SystemExit(1)
import os as _boot_os
_boot_sys.dont_write_bytecode = True
_boot_dir = _boot_os.path.dirname(_boot_os.path.abspath(__file__))
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (fix-r3 P1, same defect as #701 fix-r2). Nonexistent sys.path entries skip.
if any(
        _boot_os.path.isdir(entry or _boot_os.curdir)
        and any(_boot_os.path.isdir(root)
                and _boot_os.path.samefile(entry or _boot_os.curdir, root)
                for root in (_boot_dir, _boot_os.path.join(_boot_dir, 'controls')))
        for entry in _boot_sys.path):
    print('{"verdict":"FAIL","errors":["apparatus directory on sys.path; run the entry file directly with -I"]}')
    raise SystemExit(1)
_boot_source = _boot_os.path.join(_boot_dir, 'apparatus.py')
if _boot_os.path.islink(_boot_dir) or _boot_os.path.islink(_boot_source):
    print('{"verdict":"FAIL","errors":["apparatus bootstrap symlink"]}')
    raise SystemExit(1)
import hashlib as _boot_hashlib
import subprocess as _boot_subprocess
_boot_root = _boot_os.path.dirname(_boot_os.path.dirname(_boot_dir))
_boot_manifest = _boot_os.path.join(_boot_root, 'specs/ui-server-parity/FROZEN.md')
with open(_boot_source, 'rb') as _boot_file:
    _boot_bytes = _boot_file.read()
try:
    if _boot_os.path.isfile(_boot_manifest):
        with open(_boot_manifest) as _boot_file:
            _boot_pins = [line.split()[0] for line in _boot_file
                          if line.rstrip().endswith('  ../../scripts/ui-server-parity/apparatus.py')]
        if len(_boot_pins) != 1 or _boot_hashlib.sha256(_boot_bytes).hexdigest() != _boot_pins[0]:
            raise ValueError('apparatus bootstrap hash mismatch')
    else:
        _boot_trusted = _boot_subprocess.check_output(
            ['git', '-C', _boot_root, 'show', 'HEAD:scripts/ui-server-parity/apparatus.py'])
        if _boot_bytes != _boot_trusted:
            raise ValueError('apparatus bootstrap differs from HEAD without manifest')
except (OSError, ValueError, _boot_subprocess.CalledProcessError) as _boot_error:
    print(str(_boot_error), file=_boot_sys.stderr)
    print('{"verdict":"FAIL","errors":["apparatus bootstrap hash admission failed"]}')
    raise SystemExit(1)
_boot_namespace = {'__file__': _boot_source, '__name__': '_apparatus_bootstrap'}
exec(compile(_boot_bytes, _boot_source, 'exec'), _boot_namespace)
_boot_namespace['preflight'](_boot_dir)
import json
import re

# Applied only AFTER complete-path lookup, never to arbitrary JSON strings.
UUID = r'[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}'
UUID_VALUE = re.compile(UUID)
BLOCK_VALUE = re.compile(r'(' + UUID + r')(:[a-z_]+:[0-9]+)')
MESSAGE_VALUE = re.compile(r'(status|stream_delta|text|complete)_(' + UUID + r')')
# Driver-owned inputs, not inferred from response/list/broadcast arrival order.
CHAT_FIXTURE_SESSIONS = tuple(f'web:33333333-3333-4333-8333-00000000000{i}' for i in (1, 2))

# '*' matches exactly one array index, never an arbitrary object key.
HTTP_IDS = {
    'auth.register': {'user.id': 'user', 'token': 'token'},
    'auth.login': {'user.id': 'user', 'token': 'token'},
    'auth.user': {'user.id': 'user'},
    'upload.create': {'uploadId': 'upload', 'contentUrl': 'upload-url', 'eventsUrl': 'upload-url'},
    'upload.content': {'uploadId': 'upload', 'attachments.*.id': 'attachment'},
    'upload.read': {'uploadId': 'upload', 'attachments.*.id': 'attachment'},
}
HTTP_TIMES = {
    'upload.create': {'expiresAt'}, 'upload.content': {'expiresAt'},
    'upload.read': {'expiresAt'},
}
WS_IDS = {
    'pilotdeck-command': {'options.sessionId': 'session'},
    'watch-session': {'sessionId': 'session'},
    'unwatch-session': {'sessionId': 'session'},
    'session-activity': {'activity.sessionId': 'session', 'activity.runId': 'run',
                         'activity.completedRunId': 'run'},
    'session-activity-snapshot': {'activities.*.sessionId': 'session',
                                'activities.*.runId': 'run', 'activities.*.completedRunId': 'run'},
    'session-input-accepted': {'sessionId': 'session', 'runId': 'run'},
    'model-selection-changed': {'sessionId': 'session', 'runId': 'run'},
    'input-queue-state': {'sessionId': 'session', 'items.*.id': 'run', 'items.*.runId': 'run'},
    'status': {'id': 'run', 'sessionId': 'session', 'runId': 'run'},
    'stream_delta': {'id': 'run', 'sessionId': 'session', 'runId': 'run',
                     'timeline.id': 'timeline', 'timeline.turnId': 'turn', 'blockId': 'block'},
    'text': {'id': 'run', 'sessionId': 'session', 'runId': 'run',
             'timeline.id': 'timeline', 'timeline.turnId': 'turn', 'blockId': 'block'},
    'complete': {'id': 'run', 'sessionId': 'session', 'runId': 'run',
                 'timeline.id': 'timeline', 'timeline.turnId': 'turn', 'blockId': 'block'},
}
WS_TIMES = {'config:reloaded': {'timestamp'}}
# Watcher broadcasts use the same legacy session schema as readonly.projects.
SESSION_CLOCK_NAMES = ('createdAt', 'created_at', 'lastActivity', 'updated_at')
WS_TIMES['projects_updated'] = {
    'projects.*.sessions.*.' + name for name in SESSION_CLOCK_NAMES
} | {'projects.*.lastActivity', 'projectListRevision', 'timestamp'}
SSE_IDS = {'uploadId': 'upload', 'attachments.*.id': 'attachment'}
SSE_TIMES = {'expiresAt'}
# Unknown files/fields remain literal. No global UUID, timestamp, PID or path regex.
DISK_IDS = {'home/auth.db': {'tables.users.rows.*.id': 'user'}}
DISK_TIMES = {'home/auth.db': {'tables.users.rows.*.created_at', 'tables.users.rows.*.last_login'}}
HEADER_OMIT = {'connection', 'keep-alive', 'transfer-encoding', 'content-length',
               'x-pilotdeck-handler'}
SSE_DELAY_NS = 250_000_000
SSE_ABSOLUTE_NS = 45_000_000_000


def field_key(key):
    # A literal dotted or '*' object key must never impersonate a nested path.
    return json.dumps(key, ensure_ascii=False) if any(c in key for c in '.*"\\') else key


def pattern(path):
    return '.'.join('*' if type(part) is int else field_key(str(part)) for part in path)


def fields(channel, context):
    if channel == 'http':
        return HTTP_IDS.get(context, {}), HTTP_TIMES.get(context, set())
    if channel == 'ws':
        return WS_IDS.get(context, {}), WS_TIMES.get(context, set())
    if channel == 'sse' and context in ('upload.events:upload_started', 'upload.events:upload_progress', 'upload.events:upload_completed'):
        return SSE_IDS, SSE_TIMES
    if channel == 'disk':
        return DISK_IDS.get(context, {}), DISK_TIMES.get(context, set())
    return {}, set()

# Source/schema paths seen in the TS direct recorder, not inferred from DUT output.
HTTP_IDS['readonly.projects'] = {'*.sessions.*.id': 'session'}
HTTP_IDS['readonly.sessions'] = {'sessions.*.id': 'session'}
HTTP_IDS['readonly.messages'] = {
    'messages.*.id': 'entry', 'messages.*.sessionId': 'session', 'messages.*.entryId': 'entry',
    'messages.*.turnId': 'turn', 'messages.*.runId': 'run', 'stream.runId': 'run',
    'stream.messages.*.id': 'run', 'stream.messages.*.sessionId': 'session', 'stream.messages.*.runId': 'run',
    'messages.*.blockId': 'block', 'messages.*.timeline.id': 'timeline',
    'messages.*.timeline.turnId': 'turn',
    'stream.messages.*.blockId': 'block', 'stream.messages.*.timeline.id': 'timeline',
    'stream.messages.*.timeline.turnId': 'turn',
}
HTTP_TIMES['readonly.projects'] = {'*.lastActivity', '*.sessions.*.createdAt', '*.sessions.*.created_at',
                                    '*.sessions.*.updated_at', '*.sessions.*.lastActivity'}
HTTP_TIMES['readonly.sessions'] = {'sessions.*.createdAt', 'sessions.*.created_at',
                                    'sessions.*.updated_at', 'sessions.*.lastActivity'}
HTTP_TIMES['readonly.messages'] = {'messages.*.timestamp', 'stream.messages.*.timestamp'}
HTTP_TIMES['shadow.files'] = {'items.*.mtimeMs'}
for context in ('status', 'stream_delta', 'text', 'complete'):
    WS_TIMES[context] = {'timestamp'}
# Attachment ID is named attachmentId in publicRecord, not the generic id.
for context in ('upload.content', 'upload.read'):
    HTTP_IDS[context] = {'uploadId': 'upload', 'attachments.*.attachmentId': 'attachment'}
SSE_IDS.clear()
SSE_IDS.update({'uploadId': 'upload', 'attachments.*.attachmentId': 'attachment'})
DISK_TIMES['home/auth.db'].update({'tables.app_config.rows.*.created_at', 'tables.vapid_keys.rows.*.created_at'})
DISK_IDS['@upload'] = {'uploadId': 'upload', 'attachments.*.attachmentId': 'attachment'}
DISK_TIMES['@upload'] = {'createdAt', 'updatedAt', 'expiresAt'}
CHAT_IDS = {'*.sessionId': 'session', '*.turnId': 'turn', '*.entryId': 'entry',
            '*.parentEntryId': 'entry', '*.operationId': 'operation', '*.detail.runId': 'run',
            '*.message.content.*.blockId': 'block', '*.message.content.*.timeline.id': 'timeline',
            '*.message.content.*.timeline.turnId': 'turn', '*.result.sessionId': 'session',
            '*.result.turnId': 'turn', '*.result.finalMessage.content.*.blockId': 'block',
            '*.result.finalMessage.content.*.timeline.id': 'timeline',
            '*.result.finalMessage.content.*.timeline.turnId': 'turn'}
DISK_IDS['@chat'] = CHAT_IDS
DISK_TIMES['@chat'] = {'*.createdAt', '*.metadata.updatedAt', '*.result.startedAt', '*.result.completedAt'}

# Complete projection paths from the TS direct persistence schema.
DISK_IDS['@projection'] = {'anchor.entryId': 'entry',
 'anchor.parentEntryId': 'entry',
 'anchor.turnId': 'turn',
 'projections.agent-transcript.conversation.state.completedTurnIds.*': 'turn',
 'projections.agent-transcript.conversation.state.entries.*.entry.entryId': 'entry',
 'projections.agent-transcript.conversation.state.entries.*.entry.message.content.*.blockId': 'block',
 'projections.agent-transcript.conversation.state.entries.*.entry.message.content.*.timeline.id': 'timeline',
 'projections.agent-transcript.conversation.state.entries.*.entry.message.content.*.timeline.turnId': 'turn',
 'projections.agent-transcript.conversation.state.entries.*.entry.parentEntryId': 'entry',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.finalMessage.content.*.blockId': 'block',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.finalMessage.content.*.timeline.id': 'timeline',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.finalMessage.content.*.timeline.turnId': 'turn',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.sessionId': 'session',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.turnId': 'turn',
 'projections.agent-transcript.conversation.state.entries.*.entry.sessionId': 'session',
 'projections.agent-transcript.conversation.state.entries.*.entry.turnId': 'turn',
 'projections.web-history.agent-statuses.state.*.detail.runId': 'run',
 'projections.web-history.agent-statuses.state.*.entryId': 'entry',
 'projections.web-history.agent-statuses.state.*.parentEntryId': 'entry',
 'projections.web-history.agent-statuses.state.*.sessionId': 'session',
 'projections.web-history.agent-statuses.state.*.turnId': 'turn',
 'projections.web-history.incomplete-turns.state.completedTurnIds.*': 'turn',
 'projections.web-history.incomplete-turns.state.durableMessageTurnIds.*': 'turn',
 'sessionId': 'session'}
DISK_TIMES['@projection'] = {'anchor.createdAt',
 'projections.agent-transcript.conversation.state.entries.*.entry.createdAt',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.completedAt',
 'projections.agent-transcript.conversation.state.entries.*.entry.result.startedAt',
 'projections.agent-transcript.metadata.state.updatedAt',
 'projections.web-history.agent-statuses.state.*.createdAt'}
DISK_IDS['@registry'] = {'records.*.sessionKey': 'session', 'records.*.runId': 'run', 'records.*.result.runId': 'run', 'events.*.entries.*.event.runId': 'run'}
DISK_IDS['@registry'].update({
    'events.*.entries.*.event.blockId': 'block',
    'events.*.entries.*.event.timeline.id': 'timeline',
    'events.*.entries.*.event.timeline.turnId': 'turn',
})
DISK_TIMES['@registry'] = {'records.*.acceptedAt', 'records.*.updatedAt'}
# Untagged S1 readiness probe, with exact successful health field paths.
HTTP_IDS['_probe:GET:/health'] = {'instanceId': 'instance'}
HTTP_TIMES['_probe:GET:/health'] = {'timestamp', 'startedAt'}

# These are literal projection object keys containing dots, not nested keys.
PROJECTION_KEYS = ('agent-transcript.conversation', 'agent-transcript.metadata',
                   'web-history.agent-statuses', 'web-history.incomplete-turns')

def projection_path(path):
    for key in PROJECTION_KEYS:
        prefix = 'projections.' + key + '.'
        if path.startswith(prefix):
            return 'projections.' + field_key(key) + '.' + path[len(prefix):]
    return path

DISK_IDS['@projection'] = {projection_path(p): c for p, c in DISK_IDS['@projection'].items()}
DISK_TIMES['@projection'] = {projection_path(p) for p in DISK_TIMES['@projection']}

# Round 7: representation is a property of the complete source path, never
# inferred from the candidate value. See normalization.md for producer lines.
ISO_MS = 'iso-ms-utc'
SQL_S = 'sql-s-utc'
EPOCH_MS = 'epoch-ms'
MTIME_MS = 'epoch-ms-fractional'
CLOCK_FORMATS = {
    ('http', 'readonly.projects'): {
        '*.lastActivity': EPOCH_MS,
        **{'*.sessions.*.' + name: ISO_MS for name in SESSION_CLOCK_NAMES}},
    ('http', 'readonly.sessions'): {'sessions.*.' + name: ISO_MS for name in SESSION_CLOCK_NAMES},
    ('http', 'readonly.messages'): {'messages.*.timestamp': ISO_MS, 'stream.messages.*.timestamp': ISO_MS},
    ('http', 'shadow.files'): {'items.*.mtimeMs': MTIME_MS},
    ('http', '_probe:GET:/health'): {'timestamp': ISO_MS, 'startedAt': ISO_MS},
    ('ws', 'projects_updated'): {
        **{'projects.*.sessions.*.' + name: ISO_MS for name in SESSION_CLOCK_NAMES},
        'projects.*.lastActivity': EPOCH_MS, 'projectListRevision': EPOCH_MS, 'timestamp': ISO_MS},
    ('disk', 'home/auth.db'): {
        'tables.users.rows.*.created_at': SQL_S, 'tables.users.rows.*.last_login': SQL_S,
        'tables.app_config.rows.*.created_at': SQL_S, 'tables.vapid_keys.rows.*.created_at': SQL_S},
    ('disk', '@upload'): {'createdAt': ISO_MS, 'updatedAt': ISO_MS, 'expiresAt': ISO_MS},
    ('disk', '@chat'): {'*.createdAt': ISO_MS, '*.metadata.updatedAt': ISO_MS,
                         '*.result.startedAt': ISO_MS, '*.result.completedAt': ISO_MS},
    ('disk', '@projection'): {projection_path(p): ISO_MS for p in (
        'anchor.createdAt', 'projections.agent-transcript.conversation.state.entries.*.entry.createdAt',
        'projections.agent-transcript.conversation.state.entries.*.entry.result.completedAt',
        'projections.agent-transcript.conversation.state.entries.*.entry.result.startedAt',
        'projections.agent-transcript.metadata.state.updatedAt',
        'projections.web-history.agent-statuses.state.*.createdAt')},
    ('disk', '@registry'): {'records.*.acceptedAt': ISO_MS, 'records.*.updatedAt': ISO_MS},
}
for context in ('upload.create', 'upload.content', 'upload.read'):
    CLOCK_FORMATS['http', context] = {'expiresAt': ISO_MS}
for context in ('upload_started', 'upload_progress', 'upload_completed'):
    CLOCK_FORMATS['sse', 'upload.events:' + context] = {'expiresAt': ISO_MS}
for context in ('config:reloaded', 'status', 'stream_delta', 'text', 'complete'):
    CLOCK_FORMATS['ws', context] = {'timestamp': ISO_MS}
# Fail immediately if a new normalization path lacks a representation contract.
for channel, contexts in (('http', HTTP_TIMES), ('ws', WS_TIMES), ('disk', DISK_TIMES)):
    for context, paths in contexts.items():
        assert set(CLOCK_FORMATS[channel, context]) == paths

# Complete object paths and semantic <= relations; '*' is only an array index.
SESSION_CLOCK_PAIRS = tuple((a, b) for a in ('createdAt', 'created_at')
                            for b in ('lastActivity', 'updated_at'))
CLOCK_PAIRS = {
    ('http', 'readonly.projects'): {'*.sessions.*': SESSION_CLOCK_PAIRS},
    ('http', 'readonly.sessions'): {'sessions.*': SESSION_CLOCK_PAIRS},
    ('ws', 'projects_updated'): {'projects.*.sessions.*': SESSION_CLOCK_PAIRS},
    ('http', '_probe:GET:/health'): {'': (('startedAt', 'timestamp'),)},
    ('disk', '@upload'): {'': (('createdAt', 'updatedAt'),)},
    ('disk', '@chat'): {'*.result': (('startedAt', 'completedAt'),)},
    ('disk', '@projection'): {projection_path('projections.agent-transcript.conversation.state.entries.*.entry.result'):
                             (('startedAt', 'completedAt'),)},
    ('disk', '@registry'): {'records.*': (('acceptedAt', 'updatedAt'),)},
    ('disk', 'home/auth.db'): {'tables.users.rows.*': (('created_at', 'last_login'),)},
}
# These lists expose cross-entity ordering to the frontend. Rank the entire
# response/broadcast. Other pairs retain order/equality within their object.
CLOCK_RESPONSE_ORDER = {('http', 'readonly.projects'), ('http', 'readonly.sessions'),
                        ('ws', 'projects_updated')}
