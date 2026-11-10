#!/usr/bin/env python3
# anchor-selfcheck: unsupported actual SOP capture orchestrator; no product source mutations
import importlib.util
import pathlib
import runpy
HERE=pathlib.Path(__file__).resolve().parent
ROOT=HERE.parents[4]
spec=importlib.util.spec_from_file_location("sop_consumer_common",ROOT/"scripts/gateway-parity/common.py")
common=importlib.util.module_from_spec(spec)
spec.loader.exec_module(common)
with common.parity_lock(timeout=1):
 runpy.run_path(str(HERE/"producer.py"),run_name="__main__")
