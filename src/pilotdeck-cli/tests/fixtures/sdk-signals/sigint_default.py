#!/usr/bin/env python3
"""Restore SIGINT before exec, including when a job inherited SIG_IGN."""
import os
import signal
import sys

if len(sys.argv) < 2:
    raise SystemExit("#1145 SIGINT wrapper requires an executable")
signal.signal(signal.SIGINT, signal.SIG_DFL)
os.execvp(sys.argv[1], sys.argv[1:])
