#!/usr/bin/env python3
# anchor-selfcheck: unsupported bounded TS capture adapter, no source mutations
"""Reuse the real-sidecar process owner, including timeout and signal cleanup."""
from pathlib import Path
import sys
fixture = Path(__file__).resolve().parent
sys.path.insert(0, str(fixture.parent / 'live-e2e'))
from capture import main
if __name__ == '__main__':
    main(fixture / 'probe.mts')
