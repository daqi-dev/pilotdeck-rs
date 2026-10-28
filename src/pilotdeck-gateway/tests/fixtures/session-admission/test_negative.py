# anchor-selfcheck: unsupported mutation boundary unit tests on in-memory product text, no product source replacement table
"""Regression checks for the abort mutant's boundaries, using real product text."""
import importlib.util
from pathlib import Path
import unittest

FIXTURE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("session_admission_negative", FIXTURE / "negative.py")
negative = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(negative)

from anchor_selfcheck import extract


class AbortMutationAnchors(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.source = (FIXTURE.parents[2] / "src/runtime/session/mod.rs").read_text()
        cls.start = "        // One admission snapshot"
        cls.end = "        allowed.then(|| abort(run_id.cloned(), reason))\n"

    def test_real_source_preserves_function_tail_and_old_abort_behavior(self):
        before, after = negative.restore_original_abort(self.source)
        mutated = self.source.replace(before, after, 1)
        suffix = "    }\n}\n\n#[cfg(test)]\nmod tests;\n"
        self.assertTrue(mutated.endswith(suffix), "abort mutation must preserve function/file tail")
        self.assertTrue(before.startswith(self.start), "abort mutation must include start anchor")
        self.assertTrue(before.endswith(self.end), "abort mutation must include return expression")
        self.assertTrue(after.endswith("        allowed.then(|| abort(None, reason))\n"),
                        "old abort must return the unfenced sink operation")
        self.assertEqual(after.count("self.admission.active_turn_run_id(session_key)"), 2,
                         "old abort must perform two active reads")
        self.assertNotIn("abort(run_id.cloned(), reason)", mutated,
                         "old abort must remove the run fence")
        self.assertEqual(mutated[:mutated.index(after)], self.source[:self.source.index(before)],
                         "abort mutation must preserve the preceding function")

    def test_missing_start_is_anchor_expired(self):
        with self.assertRaisesRegex(ValueError, "ANCHOR_EXPIRED.*expected one start and one end",
                                    msg="missing start must be ANCHOR_EXPIRED"):
            negative.restore_original_abort(self.source.replace(self.start, "", 1))

    def test_static_table_matches_runtime_abort_mutation(self):
        anchors, _, _ = extract(FIXTURE / "negative.py", FIXTURE.parents[4])
        abort_anchors = [anchor for anchor in anchors if anchor.name == "restore-original-abort"]
        self.assertEqual(len(abort_anchors), 1, "abort mutation must have one static anchor")
        before, _ = negative.restore_original_abort(self.source)
        self.assertEqual(abort_anchors[0].before, before,
                         "static and runtime abort boundaries must agree")

    def test_duplicate_start_is_anchor_expired(self):
        with self.assertRaisesRegex(ValueError, "ANCHOR_EXPIRED.*expected one start and one end",
                                    msg="duplicate start must be ANCHOR_EXPIRED"):
            negative.restore_original_abort(self.source + self.start)

    def test_missing_end_is_anchor_expired(self):
        with self.assertRaisesRegex(ValueError, "ANCHOR_EXPIRED.*expected one start and one end",
                                    msg="missing end must be ANCHOR_EXPIRED"):
            negative.restore_original_abort(self.source.replace(self.end, "", 1))

    def test_duplicate_end_is_anchor_expired(self):
        with self.assertRaisesRegex(ValueError, "ANCHOR_EXPIRED.*expected one start and one end",
                                    msg="duplicate end must be ANCHOR_EXPIRED"):
            negative.restore_original_abort(self.source + self.end)

    def test_end_before_start_is_anchor_expired(self):
        reversed_source = self.end + self.source.replace(self.end, "", 1)
        with self.assertRaisesRegex(ValueError, "ANCHOR_EXPIRED.*end must follow start",
                                    msg="reversed anchors must be ANCHOR_EXPIRED"):
            negative.restore_original_abort(reversed_source)


if __name__ == "__main__":
    unittest.main()
