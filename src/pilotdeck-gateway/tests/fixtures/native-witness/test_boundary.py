#!/usr/bin/env python3
# anchor-selfcheck: unsupported documentation and data boundary controls, no source mutations
"""#1206 same-collector binding copies are a cooperative-writer boundary."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[5]


class BoundaryTests(unittest.TestCase):
    def test_documented_boundary(self):
        marker = '#1206 document the cooperative-writer boundary'
        documents = {
            'scripts/gateway-parity/README.md': '只防协作写者',
            'scripts/native-witness/README.md': '能够同时重写三者的对抗写者不在此威胁模型内',
            'src/pilotdeck-gateway/tests/fixtures/session-domain-native/README.md': '不防共同重钉',
        }
        for name, expected in documents.items():
            with self.subTest(document=name):
                try:
                    content = (ROOT / name).read_text()
                except Exception as error:
                    self.fail(marker + ': ' + repr(error))
                self.assertTrue(expected in content, marker)

    def test_binding_writer_boundary(self):
        marker = '#1206 observer-only edit rejected, joint rewrite is a known boundary'
        try:
            path = ROOT / 'scripts/gateway-parity/controls/session_domain.py'
            spec = importlib.util.spec_from_file_location('binding_boundary_controls', path)
            c = importlib.util.module_from_spec(spec)
            exec(compile(path.read_bytes(), str(path), 'exec'), c.__dict__)
            s = c.session_domain_module
            with tempfile.TemporaryDirectory(prefix='pdrs-1206-') as temporary:
                folder = c.native_capture_fixture(Path(temporary))

                def repin():
                    receipt = c.read(folder / 'capture/receipt.json')
                    receipt.update({key: c.digest(folder / name) for name, key in c.NATIVE_RECEIPT_KEYS.items()})
                    c.save(folder / 'capture/receipt.json', receipt)

                self.assertTrue(c.control_deadline_report(folder)['cells'][-1]['status'] == 'PASS', marker)
                observer = c.read(folder / 'session-domain/observer.json')
                binding = {'connectionId': 'joint-rewrite', 'generation': 2}
                row = observer['rows'][0]
                row['frame']['interactionBinding'] = binding
                observer['rows'][0] = c.control_wire(row['frame'])
                c.save(folder / 'session-domain/observer.json', observer)
                repin()
                failure = None
                try:
                    c.control_deadline_report(folder)
                except s.EvidenceError as error:
                    failure = error.rule
                self.assertTrue(failure == 'BINDING', marker)
                driver = c.read(folder / 'session-domain/driver.json')
                driver['binding'] = binding
                c.save(folder / 'session-domain/driver.json', driver)
                repin()
                self.assertTrue(c.control_deadline_report(folder)['cells'][-1]['status'] == 'PASS', marker)
        except Exception as error:
            self.fail(marker + ': ' + repr(error))


if __name__ == '__main__':
    unittest.main()
