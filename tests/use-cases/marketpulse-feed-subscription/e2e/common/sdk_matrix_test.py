import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from common.sdk_matrix import SdkMatrixJournal, matrix_cases


class SdkMatrixJournalTest(unittest.TestCase):
    def setUp(self) -> None:
        directory = TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name) / "payment"
        self.name = matrix_cases("simple")[0].name
        self.outputs = {report: f"verified {report}" for report in SdkMatrixJournal.REPORTS}

    def test_same_backend_resumes_the_original_case_and_evidence(self) -> None:
        journal = SdkMatrixJournal(self.root, "simple", "docker")
        self.assertIsNone(journal.load(1, self.name))
        journal.save(1, self.name, self.outputs)
        resumed = SdkMatrixJournal(self.root, "simple", "docker")
        self.assertEqual(resumed.load(1, self.name), self.outputs)
        self.assertEqual(json.loads((self.root / "matrix-plan.json").read_text())["backend"], "docker")
        payload = json.loads((self.root / "matrix-cases" / "case-01.json").read_text())
        self.assertEqual(payload["backend"], "docker")
        self.assertFalse(list(self.root.rglob("*.tmp")))

    def test_cross_backend_and_scope_plans_are_rejected_without_overwriting(self) -> None:
        SdkMatrixJournal(self.root, "simple", "docker")
        original = (self.root / "matrix-plan.json").read_bytes()
        for scope, backend in [("simple", "kubernetes"), ("full", "docker")]:
            with self.subTest(scope=scope, backend=backend):
                with self.assertRaisesRegex(RuntimeError, "immutable resume plan"):
                    SdkMatrixJournal(self.root, scope, backend)
                self.assertEqual((self.root / "matrix-plan.json").read_bytes(), original)

    def test_legacy_plan_without_a_backend_is_rejected(self) -> None:
        SdkMatrixJournal(self.root, "simple", "docker")
        target = self.root / "matrix-plan.json"
        plan = json.loads(target.read_text())
        del plan["backend"]
        target.write_text(json.dumps(plan))
        with self.assertRaisesRegex(RuntimeError, "immutable resume plan"):
            SdkMatrixJournal(self.root, "simple", "docker")

    def test_foreign_legacy_and_mismatched_checkpoints_cannot_establish_a_pass(self) -> None:
        journal = SdkMatrixJournal(self.root, "simple", "docker")
        target = self.root / "matrix-cases" / "case-01.json"
        for changes in [{"backend": "kubernetes"}, {"backend": None}, {"case": "another case"}]:
            with self.subTest(changes=changes):
                journal.save(1, self.name, self.outputs)
                payload = json.loads(target.read_text())
                payload.update(changes)
                if payload["backend"] is None:
                    del payload["backend"]
                target.write_text(json.dumps(payload))
                with self.assertRaisesRegex(RuntimeError, "backend/identity mismatch"):
                    journal.load(1, self.name)

    def test_empty_or_missing_evidence_is_rejected_on_save_and_restore(self) -> None:
        journal = SdkMatrixJournal(self.root, "simple", "kubernetes")
        target = self.root / "matrix-cases" / "case-01.json"
        for value in [None, 7, "", " "]:
            outputs = {**self.outputs, "s24_gateway_balance.md": value}
            with self.subTest(value=value):
                with self.assertRaisesRegex(RuntimeError, "evidence is empty"):
                    journal.save(1, self.name, outputs)
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(json.dumps({"backend": "kubernetes", "case": self.name, "reports": outputs}))
                with self.assertRaisesRegex(RuntimeError, "evidence is empty"):
                    journal.load(1, self.name)
        with self.assertRaisesRegex(RuntimeError, "evidence is incomplete"):
            journal.save(1, self.name, {"s20_full.md": "one report cannot establish payment correctness"})

    def test_invalid_json_and_non_object_records_fail_closed(self) -> None:
        journal = SdkMatrixJournal(self.root, "simple", "docker")
        target = self.root / "matrix-cases" / "case-01.json"
        target.parent.mkdir(parents=True, exist_ok=True)
        for content in ["{", "[]", "null"]:
            with self.subTest(content=content):
                target.write_text(content)
                with self.assertRaises(RuntimeError):
                    journal.load(1, self.name)
        (self.root / "matrix-plan.json").write_text("{")
        with self.assertRaisesRegex(RuntimeError, "invalid matrix journal"):
            SdkMatrixJournal(self.root, "simple", "docker")

    def test_unknown_cases_and_backends_are_rejected(self) -> None:
        journal = SdkMatrixJournal(self.root, "simple", "docker")
        for number, name in [(0, self.name), (9, self.name), (1, "another case")]:
            with self.subTest(number=number, name=name):
                with self.assertRaisesRegex(ValueError, "selected scope"):
                    journal.save(number, name, self.outputs)
        with self.assertRaisesRegex(ValueError, "unsupported matrix backend"):
            SdkMatrixJournal(self.root, "simple", "unknown")


if __name__ == "__main__":
    unittest.main()
