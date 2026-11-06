#!/usr/bin/env python3
"""Retirement outcome-parity 负控执行器（issue #1477 §6.2，N1–N11）。

只做数据变异：从 TS 参考数据加载轮次，内存里改 methods / frames，写到输出目录下
的临时轮次目录，再以子进程跑 ``check_outcome_parity.py check``。**不改任何源码
文本**（不触碰仓库任何被跟踪文件，也就不进文本锚点体系）。

判据分两步（§6.3）：
  1. 注入是否落地 —— 落盘文件重新解析后核对被注入维度确实变了（没落地记 INVALID）；
  2. 归因是否落到被注入对象 —— 期望规则 fire 且 violation 点名被注入的方法
     （规则名可能因该轮自有违规而 fire，那种情形不算 KILLED）。

逐条记 KILLED / SURVIVED / INVALID + 原因，**全部跑完再汇总**（不遇第一个失败就
中止）；任一非 KILLED 即非零退出；``summary.json`` 原子发布（先写 ``*.tmp`` 再
``rename``，缺失即失败）。
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
CHECKER = HERE / 'check_outcome_parity.py'
DEFAULT_DATA = Path('_tmp/20261106-issue-1477/r1/data')
DEFAULT_ORACLE = HERE / 'ts-oracle.json'
INJECTED_MESSAGE = 'injected by negative controls for retirement-outcome-parity'
ALT_CODE = 'UPLOAD_NOT_FOUND'
CAPABILITY_UNAVAILABLE = 'CAPABILITY_UNAVAILABLE'


def load_checker():
    """以显式路径加载判据实现（-I 隔离模式下 sys.path 不含脚本目录）。"""
    spec = importlib.util.spec_from_file_location('check_outcome_parity', CHECKER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CHECK = load_checker()


def read_round(data_dir, name):
    # 只按 "\n" 切行（共享实现，见 check_outcome_parity.load_frames）。
    rows, _ = CHECK.load_rows(Path(data_dir) / name)
    frames, _ = CHECK.load_frames(Path(data_dir) / name)
    return [dict(row) for row in rows], [json.loads(json.dumps(frame)) for frame in frames]


def write_round(out_dir, rows, frames):
    driver = Path(out_dir) / 'driver'
    driver.mkdir(parents=True, exist_ok=True)
    (driver / 'methods.json').write_text(
        json.dumps({'methods': rows}, ensure_ascii=False) + '\n', encoding='utf-8')
    (driver / 'frames.jsonl').write_text(
        '\n'.join(json.dumps(frame, ensure_ascii=False) for frame in frames) + '\n',
        encoding='utf-8')


def reread_round(out_dir):
    """落盘后重读（注入落地核对的读法与判据实现共享）。"""
    rows, _ = CHECK.load_rows(out_dir)
    frames, _ = CHECK.load_frames(out_dir)
    return rows, frames


def find_row(rows, method):
    matches = [row for row in rows if row['method'] == method]
    if len(matches) != 1:
        raise AssertionError(f'expected exactly one row for {method}, got {len(matches)}')
    return matches[0]


def send_id(frames, method):
    for frame in frames:
        wire = frame.get('frame')
        if (frame.get('direction') == 'send' and isinstance(wire, dict)
                and wire.get('type') == 'request' and wire.get('method') == method):
            return wire['id']
    raise AssertionError(f'no send frame for {method}')


def response_frame(frames, identifier):
    for frame in frames:
        wire = frame.get('frame')
        if (frame.get('direction') == 'receive' and isinstance(wire, dict)
                and wire.get('type') == 'response' and wire.get('id') == identifier):
            return frame
    return None


def rewrite_response(frames, identifier, body):
    frame = response_frame(frames, identifier)
    if frame is None:
        raise AssertionError(f'no response frame for {identifier}')
    frame['frame'] = body
    frame['raw'] = json.dumps(body, ensure_ascii=False)


def error_response(identifier, code, message):
    return {'type': 'response', 'id': identifier, 'ok': False,
            'error': {'code': code, 'message': message}}


def ok_response(identifier):
    return {'type': 'response', 'id': identifier, 'ok': True,
            'result': {'injected': True}}


def set_error(rows, frames, method, code):
    row = find_row(rows, method)
    row['outcome'] = 'error'
    row['errorCode'] = code
    row['error'] = INJECTED_MESSAGE
    rewrite_response(frames, send_id(frames, method), error_response(send_id(frames, method), code, INJECTED_MESSAGE))


def set_ok(rows, frames, method):
    row = find_row(rows, method)
    row['outcome'] = 'ok'
    row.pop('errorCode', None)
    row.pop('error', None)
    rewrite_response(frames, send_id(frames, method), ok_response(send_id(frames, method)))


def drop_response(frames, identifier):
    frames[:] = [frame for frame in frames
                 if not (frame.get('direction') == 'receive'
                         and isinstance(frame.get('frame'), dict)
                         and frame['frame'].get('type') == 'response'
                         and frame['frame'].get('id') == identifier)]


class Control:
    def __init__(self, cid, title, expected_rules, probe, build, landed, extra=None):
        self.id = cid
        self.title = title
        self.expected_rules = set(expected_rules)
        self.probe = probe
        self.build = build          # () -> {round_name: (rows, frames)}
        self.landed = landed       # {round_name: (rows, frames)} -> (bool, reason)
        self.extra = extra or {}    # 归因加严项：N7 双方码、N11 漂移轮次


def build_controls(base_rows, base_frames, round2_rows, round2_frames):
    ok_request = next(row['method'] for row in base_rows
                      if row['outcome'] == 'ok' and row['shape'] == 'request')
    error_probe = next(row['method'] for row in base_rows
                       if row['outcome'] == 'error'
                       and row.get('errorCode') != CAPABILITY_UNAVAILABLE)
    ok_rows = [row['method'] for row in base_rows if row['outcome'] == 'ok']
    first_row = base_rows[0]['method']
    n10_keep, n10_probe = ok_rows[0], ok_rows[2]
    gated = 'memory_list'
    assert gated in CHECK.REGISTERED_UNAVAILABLE_METHODS
    assert find_row(base_rows, gated)['errorCode'] == CAPABILITY_UNAVAILABLE
    assert ALT_CODE != CAPABILITY_UNAVAILABLE

    def clone(*samples):
        return [([dict(row) for row in rows], [json.loads(json.dumps(frame)) for frame in frames])
                for rows, frames in samples]

    def one(mutate):
        def build():
            rows, frames = clone((base_rows, base_frames))[0]
            mutate(rows, frames)
            return {'round-01': (rows, frames)}
        return build

    def n5():
        rows, frames = clone((base_rows, base_frames))[0]
        rows[:] = [row for row in rows if row['method'] != first_row]
        return {'round-01': (rows, frames)}

    def n6():
        rows, frames = clone((base_rows, base_frames))[0]
        rows.append(dict(find_row(rows, first_row)))
        return {'round-01': (rows, frames)}

    def n10():
        rows, frames = clone((base_rows, base_frames))[0]
        for row in rows:
            if row['method'] not in (ok_rows[0], ok_rows[1]):
                identifier = send_id(frames, row['method'])
                row['outcome'] = 'error'
                row['errorCode'] = CAPABILITY_UNAVAILABLE
                row['error'] = INJECTED_MESSAGE
                rewrite_response(frames, identifier,
                                 error_response(identifier, CAPABILITY_UNAVAILABLE, INJECTED_MESSAGE))
        return {'round-01': (rows, frames)}

    def n11():
        first = clone((base_rows, base_frames))[0]
        second = clone((round2_rows, round2_frames))[0]
        set_error(second[0], second[1], error_probe, ALT_CODE)
        return {'round-01': first, 'round-02': second}

    def landed_row(field, expected, method):
        def check(bundles):
            row = find_row(bundles['round-01'][0], method)
            if row.get(field) != expected:
                return False, f'{method}.{field} is {row.get(field)!r}, expected {expected!r}'
            return True, ''
        return check

    def landed_wire(code, method):
        def check(bundles):
            identifier = send_id(bundles['round-01'][1], method)
            response = response_frame(bundles['round-01'][1], identifier)
            wire_code = ((response['frame'].get('error') or {}).get('code')
                         if response else None)
            if wire_code != code:
                return False, f'wire code for {method} is {wire_code!r}, expected {code!r}'
            return True, ''
        return check

    def landed_no_response(method):
        def check(bundles):
            identifier = send_id(bundles['round-01'][1], method)
            if response_frame(bundles['round-01'][1], identifier) is not None:
                return False, f'response frame for {method} still present'
            return True, ''
        return check

    def landed_absent(method):
        def check(bundles):
            if any(row['method'] == method for row in bundles['round-01'][0]):
                return False, f'{method} row still present'
            return True, ''
        return check

    def landed_duplicate(method):
        def check(bundles):
            names = [row['method'] for row in bundles['round-01'][0]]
            if names.count(method) != 2:
                return False, f'{method} appears {names.count(method)} times, expected 2'
            return True, ''
        return check

    def landed_n10():
        def check(bundles):
            rows = bundles['round-01'][0]
            unavailable = [row['method'] for row in rows
                           if row['outcome'] == 'error'
                           and row.get('errorCode') == CAPABILITY_UNAVAILABLE]
            if len(unavailable) != 99:
                return False, f'{len(unavailable)} unavailable rows, expected 99'
            if ok_rows[0] not in [row['method'] for row in rows if row['outcome'] == 'ok']:
                return False, 'kept ok row missing'
            return True, ''
        return check

    def landed_n11():
        def check(bundles):
            first = find_row(bundles['round-01'][0], error_probe)
            second = find_row(bundles['round-02'][0], error_probe)
            if first.get('errorCode') == second.get('errorCode'):
                return False, 'round-02 code did not drift from round-01'
            identifier = send_id(bundles['round-02'][1], error_probe)
            response = response_frame(bundles['round-02'][1], identifier)
            wire_code = ((response['frame'].get('error') or {}).get('code')
                         if response else None)
            if wire_code != second.get('errorCode'):
                return False, 'round-02 wire not synced to the drifted code'
            return True, ''
        return check

    return [
        Control('N1', f'ok 方法 {ok_request} 改 error（线缆同步）',
                ['P3-OUTCOME'], ok_request,
                one(lambda rows, frames: set_error(rows, frames, ok_request, ALT_CODE)),
                landed_row('outcome', 'error', ok_request)),
        Control('N2', f'error 方法 {error_probe} 的 errorCode 改 {ALT_CODE}（线缆同步）',
                ['P4-ERRORCODE'], error_probe,
                one(lambda rows, frames: set_error(rows, frames, error_probe, ALT_CODE)),
                landed_row('errorCode', ALT_CODE, error_probe)),
        Control('N3', f'非 gated ok 方法 {ok_request} 改 CAPABILITY_UNAVAILABLE（线缆同步）',
                ['P5-UNAVAILABLE-SET'], ok_request,
                one(lambda rows, frames: set_error(rows, frames, ok_request, CAPABILITY_UNAVAILABLE)),
                landed_row('errorCode', CAPABILITY_UNAVAILABLE, ok_request)),
        Control('N4', f'gated 方法 {gated} 改 ok（线缆同步）',
                ['P5-UNAVAILABLE-SET'], gated,
                one(lambda rows, frames: set_ok(rows, frames, gated)),
                landed_row('outcome', 'ok', gated)),
        Control('N5', f'删掉方法行 {first_row}',
                ['P1-COVERAGE'], first_row, n5, landed_absent(first_row)),
        Control('N6', f'复制方法行 {first_row}（同名两行）',
                ['P1-COVERAGE', 'P8-FAIL-CLOSED'], first_row, n6, landed_duplicate(first_row)),
        Control('N7', f'线缆 error.code 与 outcome 码不一致（{error_probe}）',
                ['P6-WIRE-CODE'], error_probe,
                one(lambda rows, frames: rewrite_response(
                    frames, send_id(frames, error_probe),
                    error_response(send_id(frames, error_probe), ALT_CODE, INJECTED_MESSAGE))),
                landed_wire(ALT_CODE, error_probe),
                {'probeCode': find_row(base_rows, error_probe)['errorCode']}),
        Control('N8', f'删掉 {error_probe} 的 response 帧',
                ['P6-WIRE-CODE'], error_probe,
                one(lambda rows, frames: drop_response(frames, send_id(frames, error_probe))),
                landed_no_response(error_probe)),
        Control('N9', f'{ok_request} 的 outcome 改 timeout',
                ['P1-COVERAGE', 'P3-OUTCOME'], ok_request,
                one(lambda rows, frames: find_row(rows, ok_request).update(
                    outcome='timeout', error='injected timeout')),
                landed_row('outcome', 'timeout', ok_request)),
        Control('N10', '99/101 改 CAPABILITY_UNAVAILABLE（线缆同步，保留 2 个 ok）',
                ['P5-UNAVAILABLE-SET'], n10_probe, n10, landed_n10()),
        Control('N11', f'第 2 轮 {error_probe} 的码与第 1 轮不同（其余不变）',
                ['P7-ROUND-STABILITY'], error_probe, n11, landed_n11(),
                {'driftRound': 'round-02'}),
    ]


def run_control(control, out_root, oracle):
    out_dir = Path(out_root) / control.id
    record = {'id': control.id, 'title': control.title, 'probe': control.probe,
              'expectedRules': sorted(control.expected_rules), 'verdict': None,
              'reason': None, 'firedRules': None, 'rc': None}
    try:
        bundles = control.build()
    except Exception as error:  # noqa: BLE001 - 记录为 INVALID，不中止其余控件
        record.update(verdict='INVALID', reason=f'build failed: {error!r}')
        return record
    for name, (rows, frames) in bundles.items():
        write_round(out_dir / name, rows, frames)
    # 第一步：注入是否落地（重读落盘文件核对）。
    try:
        reread = {name: reread_round(out_dir / name) for name in bundles}
        landed, reason = control.landed({name: reread[name] for name in bundles})
    except Exception as error:  # noqa: BLE001
        record.update(verdict='INVALID', reason=f'mutated files unreadable: {error!r}')
        return record
    if not landed:
        record.update(verdict='INVALID', reason=f'injection did not land: {reason}')
        return record
    # 第二步：跑判据，归因核对。
    command = [sys.executable, '-I', str(CHECKER), 'check', '--oracle', str(oracle)]
    for name in sorted(bundles):
        command += ['--round', str(out_dir / name)]
    result = subprocess.run(command, capture_output=True, text=True)
    record['rc'] = result.returncode
    (out_dir / 'check-output.json').write_text(result.stdout, encoding='utf-8')
    if result.returncode == 0:
        record.update(verdict='SURVIVED',
                      reason='checker returned PASS on mutated data')
        return record
    if result.returncode != 1:
        record.update(verdict='INVALID',
                      reason=f'checker exit {result.returncode} (input unusable?): '
                             f'{result.stdout.strip()[:400]}')
        return record
    try:
        report = json.loads(result.stdout)
        violations = report.get('violations', [])
    except ValueError as error:
        record.update(verdict='INVALID', reason=f'cannot parse checker output: {error}')
        return record
    fired = sorted({violation['rule'] for violation in violations})
    record['firedRules'] = fired
    matched = [violation for violation in violations
               if violation['rule'] in control.expected_rules
               and violation.get('method') == control.probe]
    if control.id == 'N7' and matched:
        # N7 必须点名双方码（outcome 码与线缆码都在 detail 里）。
        probe_code = control.extra['probeCode']
        if not all(ALT_CODE in violation['detail'] and probe_code in violation['detail']
                   for violation in matched):
            matched = []
    if control.id == 'N11' and matched:
        # N11 必须点名漂移轮次。
        drift_round = control.extra['driftRound']
        if not any(drift_round in f"{violation['round']} {violation['detail']}"
                   for violation in matched):
            matched = []
    if matched:
        record.update(verdict='KILLED', reason='expected rule fired naming the injected method')
    else:
        record.update(verdict='SURVIVED',
                      reason=f'expected {sorted(control.expected_rules)} naming '
                             f'{control.probe}; fired rules: {fired}')
    return record


def main(argv=None):
    parser = argparse.ArgumentParser(
        description='Negative controls N1-N11 for the retirement outcome-parity '
                    'checker (#1477). Data-only mutations; no source text is touched.')
    parser.add_argument('output', help='output directory for mutated rounds + summary')
    parser.add_argument('--data', default=str(DEFAULT_DATA),
                        help='TS reference data root (default: %(default)s)')
    parser.add_argument('--oracle', default=str(DEFAULT_ORACLE),
                        help='ts-oracle.json (default: %(default)s)')
    args = parser.parse_args(argv)

    data = Path(args.data)
    base_rows, base_frames = read_round(data, 'round-01')
    round2_rows, round2_frames = read_round(data, 'round-02')
    controls = build_controls(base_rows, base_frames, round2_rows, round2_frames)

    out_root = Path(args.output)
    out_root.mkdir(parents=True, exist_ok=True)
    records = []
    for control in controls:  # 全部跑完再汇总，不遇败即停
        record = run_control(control, out_root, Path(args.oracle))
        records.append(record)
        print(f"{control.id}: {record['verdict']} ({record['reason']})")

    counts = {verdict: sum(1 for record in records if record['verdict'] == verdict)
              for verdict in ('KILLED', 'SURVIVED', 'INVALID')}
    summary = {'status': 'PASS' if counts['SURVIVED'] == 0 and counts['INVALID'] == 0 else 'FAIL',
               'controls': records, 'counts': counts,
               'oracle': str(args.oracle), 'data': str(args.data)}
    target = out_root / 'summary.json'
    tmp = out_root / 'summary.json.tmp'
    tmp.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n',
                   encoding='utf-8')
    tmp.rename(target)  # 原子发布：先写 *.tmp 再 rename
    print(json.dumps({'status': summary['status'], 'counts': counts,
                      'summary': str(target)}, ensure_ascii=False))
    return 0 if summary['status'] == 'PASS' else 1


if __name__ == '__main__':
    sys.exit(main())
