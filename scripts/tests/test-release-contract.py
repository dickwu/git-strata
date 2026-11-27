"""Fast C1 checks for release selection and the release-binary preview witness."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("contract", Path(__file__).with_name("release-contract.py"))
contract = importlib.util.module_from_spec(spec)
spec.loader.exec_module(contract)


class ContractTests(unittest.TestCase):
    def test_floor_tag_and_two_published_binaries(self):
        tags = ["v0.2.3", "v0.3.0", "v0.3.1", "v0.3.2", "v0.3.3-rc1"]
        published = ["v0.2.3", "v0.3.1", "v0.3.2", "v0.4.0"]
        self.assertEqual(contract.published_window("0.3.0", published, tags), [
            {"version": "0.3.0", "source": "tag"}, {"version": "0.3.1", "source": "tarball"},
            {"version": "0.3.2", "source": "tarball"}])
        self.assertEqual(contract.published_window("0.3.2", published, tags),
                         [{"version": "0.3.2", "source": "tarball"}])

    def check_preview(self, current, supports_flag, plan, detail=None, extra=()):
        proxy = SimpleNamespace(requests=[], plans=[])

        def cli(_bin, _home, args):
            if "--dry-run" in args and not supports_flag:
                return {"ok": False, "error": {"code": "InvalidInput", "message": "--dry-run",
                                               "detail": {"reason": "args"}}}
            proxy.requests.extend({"method": m} for m in ["checkpoint", "publish_plan", *extra])
            proxy.plans.append(plan)
            if supports_flag:
                return plan
            return {"ok": False, "error": {"code": "NotTty", "detail": detail or {}}}

        with patch.object(contract, "cli", cli):
            contract.publish_preview(None, None, proxy, "session:tool:path", current, (0, 3, 2))

    def test_preview_requires_the_real_plan_and_only_historical_fallback(self):
        plan = {"ok": True, "data": {"repos": ["tool"]}}
        self.check_preview(True, True, plan)
        self.check_preview(False, False, plan)
        for args in [
            (True, False, plan), (False, False, plan, {"plan_error": {"code": "InvalidInput"}}),
            (False, False, {"ok": False}), (False, False, {"ok": True, "data": {"repos": []}}),
            (True, True, {"ok": True, "data": {"repos": ["front"]}}),
            (True, True, {"ok": True, "data": {"repos": ["tool"], "plan_error": {}}}),
            (False, False, plan, {}, ["publish"]),
        ]:
            with self.subTest(args=args), self.assertRaises(AssertionError):
                self.check_preview(*args)


if __name__ == "__main__":
    unittest.main()
