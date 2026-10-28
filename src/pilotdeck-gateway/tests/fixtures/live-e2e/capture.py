#!/usr/bin/env python3
# anchor-selfcheck: unsupported real TS capture orchestration, no source mutation definitions
"""Capture the fixed TS baseline. This path never reads expected files."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from evidence import observation, canonical_capture
from selftest import (FIXTURE, SCENARIOS, INJECTIONS, INJECTION_PLANS, check,
                      free_port, run_case, terminate_group, port_free, install_termination_handler)


def main():
    root = Path(sys.argv[1]).resolve()
    root.mkdir(parents=True, exist_ok=False)
    observations = {}
    for scenario in SCENARIOS:
        for injection in INJECTIONS:
            name = f'{scenario}-{injection}'
            case = root / name
            case.mkdir()
            if injection != 'passthrough':
                (case / 'plan.json').write_text(json.dumps(INJECTION_PLANS[injection]) + '\n')
            port = free_port()
            check(port != 18789, '#969 capture: forbidden port')
            log = case / 'mock.log'
            env = {**os.environ, 'HOME': str(case / 'home'),
                   'LIVE_E2E_INJECTION': injection, 'LIVE_E2E_MOCK_PORT': str(port), 'LIVE_E2E_MOCK_LOG': str(log)}
            with (case / 'mock-stderr.log').open('x') as stderr:
                mock = subprocess.Popen(['node', str(FIXTURE / 'mock_provider.mjs')],
                                        env=env, stdout=subprocess.DEVNULL,
                                        stderr=stderr, start_new_session=True)
                try:
                    deadline = time.monotonic() + 10
                    while not log.exists():
                        check(mock.poll() is None and time.monotonic() < deadline,
                              f'#969 capture {name}: mock startup failed')
                        time.sleep(.01)
                    result, frames, summary, _ = run_case(name, scenario, injection, port, case)
                    observations[name] = canonical_capture(observation(result, frames, summary, log, case), case, port)
                finally:
                    terminate_group(mock)
            check(port_free(port), f'#969 capture {name}: mock port not released')
            print(f'#969 captured {name}', flush=True)
    (root / 'observations.json').write_text(json.dumps(observations, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    install_termination_handler()
    main()
