"""#1314: audit four actual host/sidecar cells against the existing expectations.

Only the authenticated original TS or native capture artifacts are accepted.
The frozen caller supplies its existing parsers, projection, and score functions.
"""
import copy
import hashlib
import json
import subprocess
from typing import NamedTuple
from collections import Counter
from pathlib import Path

MATRIX = tuple((host, sidecar) for host in ('ts', 'rust') for sidecar in ('ts', 'rust'))
K = 5
BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
PROGRAMS = {'pilotdeck', 'pilotdeck-control', 'pilotdeck-gateway', 'pilotdeck-agent-loop-sidecar'}


class HostError(ValueError):
    def __init__(self, rule, detail):
        self.rule = rule
        super().__init__(detail)


def need(condition, rule, detail):
    if not condition:
        raise HostError(rule, detail)


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def read(path):
    return json.loads(Path(path).read_text())


def artifact(ref):
    need(isinstance(ref, dict) and set(ref) == {'path', 'sha256'}, 'IDENTITY', 'artifact reference shape')
    path = Path(ref['path'])
    need(path.is_absolute() and path.is_file() and sha(path) == ref['sha256'], 'IDENTITY', str(path))
    return path.resolve()


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], timeout=10)


def commit_source(baseline, rows):
    hashes = {}
    for row in rows:
        path = Path(row['path'])
        need(not path.is_absolute() and '..' not in path.parts, 'TS_SOURCE_IDENTITY', str(path))
        need(hashes.setdefault(path, row['sha256']) == row['sha256'], 'TS_SOURCE_IDENTITY', 'source loaded with different bytes')
    need(bool(hashes), 'TS_SOURCE_IDENTITY', 'empty fixed-source witness')
    for path, digest in hashes.items():
        original = git(baseline, 'show', BASELINE + ':' + path.as_posix())
        need(hashlib.sha256(original).hexdigest() == digest == sha(baseline / path), 'TS_SOURCE_IDENTITY', str(path))
    return len(hashes)


def source_binding(manifest, root):
    source_file, binary_file = (artifact(manifest[name]) for name in ('sourceManifest', 'binaryManifest'))
    source = read(source_file)
    need(git(root, 'rev-parse', 'HEAD').decode().strip() == source['head'] and
         not git(root, 'status', '--porcelain', '--untracked-files=no').strip(), 'IDENTITY', 'fixed clean source head required')
    files = source['files']
    need(bool(files) and len({row['path'] for row in files}) == len(files), 'IDENTITY', 'empty/duplicate build source input')
    for row in files:
        path = Path(row['path'])
        need(not path.is_absolute() and '..' not in path.parts and sha(root / path) == row['sha256'], 'IDENTITY', str(path))
    binaries = read(binary_file)
    binaries = binaries if isinstance(binaries, list) else binaries['binaries']
    need(len(binaries) == len(PROGRAMS) and {row['path'] for row in binaries} == PROGRAMS, 'IDENTITY', 'all four programs required')
    for row in binaries:
        need(sha(binary_file.parent / row['path']) == row['sha256'], 'IDENTITY', row['path'])
    node = artifact(manifest['node'])
    versions = json.loads(subprocess.check_output([str(node), '-p', 'JSON.stringify(process.versions)'], timeout=5))
    need([versions.get(key) for key in ('node', 'icu', 'uv')] == ['22.22.0', '78.2', '1.52.1'], 'IDENTITY', 'fixed Node/ICU/libuv')
    baseline = Path(manifest['baseline']).resolve(strict=True)
    need(git(baseline, 'rev-parse', 'HEAD').decode().strip() == BASELINE and
         not git(baseline, 'status', '--porcelain', '--untracked-files=no').strip(), 'IDENTITY', 'fixed clean TS baseline')
    return source['head'], node, baseline, {row['path']: binary_file.parent / row['path'] for row in binaries}


def roles(contract, cell, node, baseline, bins, root):
    host, sidecar = cell
    need(contract['host'] == host, 'HOST_IDENTITY', 'wrong host declaration')
    harness = root / 'scripts/a5-19/six_cells.mts'
    cli = baseline / 'src/cli/pilotdeck-agent-loop-sidecar.ts'
    loader = root / 'scripts/a5-19/host-sidecar-source-loader.mjs'
    host_bin = node if host == 'ts' else bins['pilotdeck-gateway']
    sidecar_bin = node if sidecar == 'ts' else bins['pilotdeck-agent-loop-sidecar']
    for role, executable, rule in (('host', host_bin, 'HOST_IDENTITY'), ('sidecar', sidecar_bin, 'SIDECAR_IDENTITY')):
        claimed = contract['binaries'][role]
        need(claimed['path'] == str(executable) and claimed['sha256'] == sha(executable), rule, 'launch identity differs from pinned role')
    if host == 'ts':
        need(contract['binaries']['host']['argv'] == [str(node), '--experimental-transform-types', str(harness)], 'HOST_IDENTITY', 'wrong TS host source entry')
    wanted = [str(sidecar_bin), '--import', str(loader), str(cli)] if sidecar == 'ts' else [str(sidecar_bin)]
    need(contract['binaries']['sidecar']['argv'] == wanted, 'SIDECAR_IDENTITY', 'wrong sidecar source/argv')


class TsPartition(NamedTuple):
    measurements: list
    witness: dict
    contract: dict
    registration: dict
    raw_rows: list


def ts_capture(folder, ref, preflight, session, expected, cell, node, baseline, bins, root):
    raw, contract, evidence = session.native_evidence.load_capture(folder / 'capture')
    roles(contract, cell, node, baseline, bins, root)
    checked = session.native_evidence.checked_evidence(raw, contract, evidence)
    need(checked['cleanup']['allStopped'] is True and
         all(checked[key]['complete'] is True for key in ('execWitness', 'networkWitness', 'transcriptWitness')),
         'PROCESS_WITNESS', 'TS host requires complete independent exec/network/disk and cleanup')
    fixture = root / 'scripts/a5-19'
    pins = {**preflight['originals'], 'run-host.py': preflight['producer']}
    names = {'six_cells.mts', 'host-sidecar-adapter.mjs', 'host-sidecar-source-loader.mjs',
             'host-sidecar-stimulus.mjs', 'run-host.py'}
    need(set(pins) == names and contract['fixtureSources'] == pins,
         'TS_SOURCE_IDENTITY', 'prelaunch and preflight must bind every executable fixture')
    for name, pin in pins.items():
        need(artifact(pin) == fixture / name and sha(folder / name) == pin['sha256'],
             'TS_SOURCE_IDENTITY', 'captured fixture identity differs: ' + name)
    raw_folder = folder / 'raw'
    manifest = read(raw_folder / 'manifest.json')
    case = ref['case']
    need(case in expected and manifest['baselineCommit'] == BASELINE and manifest['expectedCases'] == expected and
         manifest['executedCases'] == [case] and manifest['totalMeasurements'] == K and
         manifest['collectionScope'] == 'FILTERED_FORMAL_SCENES_NOT_WHOLE_HOST' and
         manifest['missingCases'] == [name for name in expected if name != case], 'COVERAGE', 'TS trace partition must retain its original case and k5')
    adapter = root / 'scripts/a5-19/host-sidecar-adapter.mjs'
    need(manifest['scriptSha256'] == sha(root / 'scripts/a5-19/six_cells.mts') and
         manifest['externalSidecarAdapter']['sha256'] == sha(adapter) and
         manifest['externalSidecarAdapter']['sidecarSpec']['language'] == cell[1], 'HOST_IDENTITY', 'wrong actual adapter or topology')
    provider = read(raw_folder / 'provider.json')
    required_provider = case in session.native_evidence.required_provider_cases(
        session.read(session.SCRIPT / 'fixtures/session-expectations.json'))
    calls = checked['networkWitness']['calls']
    if required_provider:
        endpoint = provider['endpoint']
        host = next(row['pid'] for row in checked['processes'] if row['role'] == 'host')
        need(isinstance(endpoint, str) and endpoint.startswith('127.0.0.1:') and
             contract['externalEndpoints'] == [endpoint] and calls and
             all(call['target'] == endpoint and call['ownerPid'] == host for call in calls),
             'NETWORK_WITNESS', 'model traffic must originate from the real host and target its owned loopback')
        need(len(provider['requests']) == K and all(json.loads(request['raw']).get('stream') is True for request in provider['requests']) and
             session.native_evidence.provider_cases(provider['requests'], [case]) == [case] * K,
             'PROVIDER_WITNESS', 'each required repetition must reach the same owned case via the real model port')
    else:
        need(provider['endpoint'] is None and not provider['requests'] and not calls and not contract['externalEndpoints'],
             'NETWORK_WITNESS', 'unrequired model traffic must stay absent')
    raw_pins = ref['rawSha256']
    need(raw_pins == {path.name: sha(path) for path in raw_folder.glob('*.json')}, 'IDENTITY', 'TS raw artifacts not bound')
    need(Counter(row['role'] for row in checked['processes']) ==
         Counter({'host': 1, 'sidecar': K, 'baselineGitProbe': 1, 'worktreeGitProbe': 1}), 'PROCESS_WITNESS', 'wrong actual process role multiset')
    all_pids = {row['pid'] for row in checked['processes'] if row['role'] == 'sidecar'}
    transcripts = {row['path'] for row in checked['transcriptWitness']['opens']}
    commit_source(baseline, manifest['importedSources'])
    registration = read(raw_folder / 'preregistration.json')
    registered = [f"{cell['name']}/{variant['name']}/{phase}"
                  for cell in registration['cells'] for variant in cell['variants'] for phase in registration['phases']]
    need(registered == expected and registration['repeats'] == K, 'COVERAGE', 'original independent registration differs')
    observations, case_pids, child_loads = [], set(), []
    group = read(raw_folder / (case.split('/')[0] + '.json'))['observations']
    need([row['id'] for row in group] == [f'{case}/{k}' for k in range(1, K + 1)], 'COVERAGE', 'missing or duplicate partition repetitions')
    for row in group:
        path = (root / row['durableStatus']['source']).resolve()
        need(path.is_relative_to(folder) and str(path) in transcripts and
             path.read_text() == row['transcriptRawText'], 'DISK_IDENTITY', row['id'])
        journal = row['timeline']
        spawned = [entry['pid'] for entry in journal if entry['kind'] == 'sidecar.spawn']
        need(len(spawned) == 1 and spawned[0] in all_pids and spawned[0] not in case_pids,
             'PROCESS_WITNESS', 'each measurement must bind a distinct actual sidecar')
        pid = spawned[0]
        case_pids.add(pid)
        need(any(entry['kind'] == 'sidecar.exit' and entry['pid'] == pid for entry in journal) and
             any(entry['kind'] == 'sidecar.receipt' and entry['receipt']['pid'] == pid and entry['receipt']['phase'] == 'final' for entry in journal),
             'TERMINAL_WITNESS', row['id'])
        if required_provider:
            seen = [entry['receipt'] for entry in journal if entry['kind'] == 'sidecar.receipt' and entry['receipt']['site'] == 'model']
            need(len(seen) == 1 and seen[0]['pid'] == pid and seen[0]['key'] == row['id'] and seen[0]['phase'] == 'completed',
                 'PROVIDER_WITNESS', 'actual sidecar did not consume the real model response before the original outcome')
        if cell[1] == 'ts':
            loaded = [json.loads(line) for line in (path.parent.parent / 'child-loaded.ndjson').read_text().splitlines()]
            need(bool(loaded) and {entry['pid'] for entry in loaded} == {pid}, 'TS_SOURCE_IDENTITY', row['id'])
            child_loads.extend(loaded)
        actual = {key: row[key] for key in ('frames', 'durableStatus', 'registry')}
        need(session._native_observation_structure(actual) is None, 'OBSERVATION_STRUCTURE', row['id'])
        observations.append((case, row['repetition'], actual, None))
    need(case_pids == all_pids, 'PROCESS_WITNESS', 'unattributed sidecar execution')
    if cell[1] == 'ts':
        commit_source(baseline, child_loads)
    return TsPartition(observations, {'path': str(folder), 'processes': checked['processes'],
                       'network': checked['networkWitness'], 'cleanup': checked['cleanup']},
                       contract, registration, group)


def rust_capture(folder, ref, session, expected, cell, node, baseline, bins, root):
    partition = ref.get('cases')
    capture = session.load_native_capture(folder, expected_partition=partition)
    roles(capture['contract'], cell, node, baseline, bins, root)
    session.audit_native(capture['evidence'], capture['contract'])
    verdicts = {row['case']: row for row in session.score_native_cases(capture['rows'], expected, capture['providerCases'])}
    evidence = capture['evidence']
    sources = read(folder / 'session-domain/sources.json')
    paths = evidence['transcriptPaths']
    need(len(paths) == len(sources), 'DISK_IDENTITY', 'one original transcript path per case')
    for path, source in zip(paths, sources):
        transcript = Path(path).resolve(strict=True)
        need(str(transcript) == path and transcript.is_relative_to(folder) and
             transcript.read_text() == source['transcriptText'],
             'DISK_IDENTITY', 'native transcript differs from the actual traced file: ' + path)
    registry = ref['registry']
    need(registry in ('on', 'off') and len(sources) == len(capture['expected']) and
         all(row.get('stimulusEvidence', {}).get('construction', {}).get('runRegistry') == registry for row in sources),
         'REGISTRY_IDENTITY', 'must bind one real process-frozen registry setting')
    if cell[1] == 'ts':
        proof = read(artifact(ref['tsSidecarProvenance']))
        need(proof['baselineCommit'] == BASELINE and artifact(proof['factory']) == root / 'scripts/a5-19/host-sidecar-stimulus.mjs' and
             artifact(proof['loader']) == root / 'scripts/a5-19/host-sidecar-source-loader.mjs', 'TS_SOURCE_IDENTITY', 'wrong TS controlled sidecar inputs')
        pids = {row['pid'] for row in evidence['processes'] if row['role'] == 'sidecar'}
        need({row['pid'] for row in proof['loaded']} == pids, 'TS_SOURCE_IDENTITY', 'sidecar source loads must match actual processes')
        commit_source(baseline, proof['loaded'])
    selected = [(row['case'], ref['repetition'], row.get('observation'), verdicts[row['case']])
                for row in capture['rows']
                if row['case'].startswith('registry-') == (registry == 'on')]
    return selected, {'path': str(folder), 'processes': evidence['processes'], 'network': evidence['networkWitness'],
                      'cleanup': evidence['cleanup'], 'surfaces': evidence['surfaces'], 'registry': registry}, capture['contract']


def run_ts_oracle(session, registration, raw_rows, expected, output, sidecar):
    """Feed the original raw dict rows, never the projected scoring tuples."""
    order = {case: index for index, case in enumerate(expected)}
    raw_rows.sort(key=lambda row: (order[row['id'].rsplit('/', 1)[0]], row['repetition']))
    observed_ids = [row['id'] for row in raw_rows]
    need(observed_ids == [f'{case}/{k}' for case in expected for k in range(1, K + 1)],
         'COVERAGE', 'the original oracle requires all 360 unique raw observation IDs')
    executed = list(dict.fromkeys(row['id'].rsplit('/', 1)[0] for row in raw_rows))
    session.calibration_coverage(registration, {'expectedCases': expected, 'executedCases': executed})
    oracle = session.observation_check({'observations': raw_rows, 'controls': True}, output,
                                       name=f'host-ts-{sidecar}-oracle')
    controls = oracle['controls']
    need(len(controls) == 9 and len({row['name'] for row in controls}) == 9 and
         all(row['detected'] is True and row['hitRule'] == row['expectedRule'] for row in controls),
         'CONTROLS', 'all nine original controls must hit their registered rules')
    session.validate_expectations(registration, raw_rows)
    return oracle


def report(manifest_path, *, session, output):
    """Called under the existing frozen session CLI's owned parity lease."""
    root = session.ROOT.resolve()
    manifest = read(manifest_path)
    head, node, baseline, bins = source_binding(manifest, root)
    expected = session.read(session.SCRIPT / 'fixtures/session-calibration.json')['cases']
    targets = {row['case']: row for row in session.read(session.SCRIPT / 'fixtures/session-expectations.json')['cases']}
    need(len(expected) == 72 and list(targets) == expected, 'EXPECTATION_COVERAGE', 'original 72-case manifest required')
    declarations = manifest['cells']
    need([(row['host'], row['sidecar']) for row in declarations] == list(MATRIX), 'MATRIX', 'four ordered independent cells required')
    output = Path(output)
    cells, seen_receipts, control_seed, native_seed = [], set(), None, None
    for cell, declaration in zip(MATRIX, declarations):
        rows, witnesses, errors = {}, [], []
        original_ts_rows, original_registration, original_oracle = [], None, None
        for ref in declaration['captures']:
            try:
                receipt = artifact(ref['receipt'])
                need(receipt.name == 'receipt.json' and receipt.parent.name == 'capture' and
                     ref['receipt']['sha256'] not in seen_receipts, 'ROUND_IDENTITY', 'cannot count the same capture as a second round')
                seen_receipts.add(ref['receipt']['sha256'])
                folder = receipt.parent.parent
                if cell[0] == 'ts':
                    preflight = read(artifact(ref['preflight']))
                    need(preflight['head'] == head and preflight['sourceManifest'] == manifest['sourceManifest'] and
                         preflight['binaryManifest'] == manifest['binaryManifest'], 'IDENTITY', 'TS capture used a different candidate generation')
                    partition = ts_capture(folder, ref, preflight, session, expected, cell, node, baseline, bins, root)
                    data, witness, contract = partition.measurements, partition.witness, partition.contract
                    need(original_registration is None or partition.registration == original_registration,
                         'COVERAGE', 'TS partitions use different registrations')
                    original_registration = partition.registration
                    original_ts_rows.extend(partition.raw_rows)
                else:
                    data, witness, contract = rust_capture(folder, ref, session, expected, cell, node, baseline, bins, root)
                    if cell == ('rust', 'rust'):
                        native_seed = contract
                witnesses.append(witness)
                for case, repetition, actual, verdict in data:
                    key = case, repetition
                    need(case in targets and type(repetition) is int and 1 <= repetition <= K and key not in rows,
                         'ROUND_IDENTITY', f'invalid/duplicate measurement {key!r}')
                    if verdict is None:
                        surfaces = session._expectation_diff_surfaces(session.project_observation(actual), targets[case][cell[0]])
                        verdict = {'case': case, 'status': 'FAIL' if surfaces else 'PASS', 'surfaces': surfaces}
                    rows[key] = dict(verdict, repetition=repetition, capture=str(folder))
                    if control_seed is None and actual is not None and all(
                        not session._expectation_diff_surfaces(session.project_observation(actual), targets[case][host]) for host in ('ts', 'rust')):
                        control_seed = case, actual
            except (HostError, session.EvidenceError, session.native_evidence.Incomplete,
                    OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                errors.append({'rule': getattr(error, 'rule', 'HOST_EVIDENCE'), 'detail': str(error), 'capture': ref.get('receipt')})
        if cell[0] == 'ts' and len(rows) == len(expected) * K and not errors:
            try:
                original_oracle = run_ts_oracle(session, original_registration, original_ts_rows, expected, output, cell[1])
            except (HostError, session.EvidenceError, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
                errors.append({'rule': getattr(error, 'rule', 'ORIGINAL_TS_ORACLE'), 'detail': str(error)})
        cases = []
        for case in expected:
            measurements = [rows.get((case, k), {'case': case, 'repetition': k, 'status': 'NOT_RUN', 'rule': 'COVERAGE'}) for k in range(1, K+1)]
            status = 'FAIL' if any(row['status'] == 'FAIL' for row in measurements) else 'PASS' if all(row['status'] == 'PASS' for row in measurements) else 'NOT_RUN'
            cases.append({'case': case, 'status': status, 'measurements': measurements})
        complete = all(row['status'] != 'NOT_RUN' for row in cases)
        if not complete:
            errors.append({'rule': 'COVERAGE', 'detail': 'original 72 × k5 is incomplete'})
        status = 'PASS' if not errors and all(row['status'] == 'PASS' for row in cases) else 'FAIL'
        cells.append({'host': cell[0], 'sidecar': cell[1], 'status': status, 'repeats': K, 'cases': cases,
                      'captures': witnesses, 'errors': errors, 'expectedCases': expected, 'originalTsOracle': original_oracle,
                      'executionStatus': status if complete else 'NOT_RUN'})
    controls = []
    if control_seed:
        case, actual = control_seed
        bad = copy.deepcopy(actual)
        bad['frames'].append({'type': '__registered_same_bad_control__'})
        hit = all(session._expectation_diff_surfaces(session.project_observation(bad), targets[case][host]) for host in ('ts', 'rust'))
        controls.append({'name': 'same-bad-both-hosts', 'expectedRule': 'EXPECTATION_MISMATCH', 'hitRule': 'EXPECTATION_MISMATCH' if hit else None})
    if native_seed:
        for role, rule, name in (('host', 'HOST_IDENTITY', 'hidden-ts-host'), ('sidecar', 'SIDECAR_IDENTITY', 'rust-sidecar-replaced-by-ts')):
            bad = copy.deepcopy(native_seed)
            bad['binaries'][role].update(path=str(node), sha256=sha(node))
            hit = None
            try:
                roles(bad, ('rust', 'rust'), node, baseline, bins, root)
            except HostError as error:
                hit = error.rule
            controls.append({'name': name, 'expectedRule': rule, 'hitRule': hit})
    controls_pass = len(controls) == 3 and all(row['expectedRule'] == row['hitRule'] for row in controls)
    status = 'PASS' if all(cell['status'] == 'PASS' for cell in cells) else 'FAIL'
    if status == 'PASS' and not controls_pass:
        status = 'FAIL'
    return {'suite': 'host', 'sourceHead': head, 'status': status, 'executionStatus': status if all(cell['executionStatus'] != 'NOT_RUN' for cell in cells) else 'NOT_RUN', 'acceptanceStatus': 'NOT_RUN',
            'cells': cells, 'controls': {'status': 'PASS' if controls_pass else 'FAIL' if len(controls) == 3 else 'NOT_RUN', 'results': controls}}
