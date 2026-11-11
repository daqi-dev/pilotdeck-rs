"""Focused ownership/cleanup regressions; execute on the Linux build host."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('host_cohort', sys.argv[1])
cohort = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cohort)
sys.argv = [sys.argv[0]]


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.child = types.SimpleNamespace(pid=12345, returncode=0, wait=lambda **kwargs: 0)
        self.common = types.SimpleNamespace(LIVE_GROUPS={12345})

    def check(self, identities, stop=None, group_error=None):
        stop = stop or subprocess.CompletedProcess([], 0, '', '')
        with patch.object(cohort, 'created_container_id', return_value='a' * 64), \
             patch.object(cohort, 'container_identity', side_effect=identities), \
             patch.object(cohort.subprocess, 'run', return_value=stop) as stopper, \
             patch.object(cohort.os, 'killpg', side_effect=group_error):
            result = cohort.cleanup_owned_container(self.child, 'owned', Path('/unused'), self.common)
        return result, stopper

    def test_already_removed_after_stop_error_releases_verified_owner(self):
        row, _ = self.check(['a' * 64, None], subprocess.CompletedProcess([], 1, '', 'No such container'), ProcessLookupError())
        self.assertTrue(row['containerAbsentVerified'])
        self.assertEqual(row['pendingOwners'], [])
        self.assertEqual(row['cleanupDiagnostics'][0]['code'], 1)

    def test_dead_attach_client_does_not_skip_container_stop(self):
        row, stopper = self.check(['a' * 64, None], group_error=ProcessLookupError())
        self.assertTrue(row['containerAbsentVerified'])
        self.assertEqual(stopper.call_args.args[0][-1], 'a' * 64)

    def test_remaining_container_retains_owner(self):
        row, _ = self.check(['a' * 64] * 5, group_error=ProcessLookupError())
        self.assertFalse(row['containerAbsentVerified'])
        self.assertEqual(row['pendingOwners'], [12345])

    def test_daemon_query_failure_never_means_empty(self):
        row, _ = self.check([subprocess.CalledProcessError(1, ['docker']), subprocess.CalledProcessError(1, ['docker'])], group_error=ProcessLookupError())
        self.assertFalse(row['containerAbsentVerified'])
        self.assertEqual(row['pendingOwners'], [12345])

    def test_failed_first_inventory_still_stops_the_owned_id(self):
        row, stopper = self.check([subprocess.CalledProcessError(1, ['docker']), None], group_error=ProcessLookupError())
        self.assertEqual(stopper.call_args.args[0][-1], 'a' * 64)
        self.assertTrue(row['containerAbsentVerified'])

    def test_missing_create_confirmation_cannot_release_after_client_death(self):
        with patch.object(cohort, 'created_container_id', return_value=None), \
             patch.object(cohort, 'container_identity', return_value=None), \
             patch.object(cohort.os, 'killpg', side_effect=ProcessLookupError()):
            row = cohort.cleanup_owned_container(self.child, 'owned', Path('/unused'), self.common)
        self.assertFalse(row['containerAbsentVerified'])
        self.assertFalse(row['creationConfirmed'])
        self.assertEqual(row['pendingOwners'], [12345])

    def test_absent_container_but_present_client_group_retains_owner(self):
        row, _ = self.check([None, None])
        self.assertFalse(row['containerAbsentVerified'])
        self.assertEqual(row['clientGroupState'], 'PRESENT')

    def test_group_permission_error_never_means_empty(self):
        row, _ = self.check([None], group_error=PermissionError())
        self.assertFalse(row['containerAbsentVerified'])

    def test_full_inventory_requires_exact_owned_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            cidfile = Path(directory) / 'cid'
            cidfile.write_text('a' * 64)
            listing = json.dumps({'ID': 'b' * 64, 'Names': 'owned'}) + '\n'
            with patch.object(cohort.subprocess, 'check_output', return_value=listing) as command:
                with self.assertRaises(AssertionError):
                    cohort.container_identity('owned', cidfile)
            self.assertNotIn('--filter', command.call_args.args[0])


if __name__ == '__main__':
    unittest.main(verbosity=2)
