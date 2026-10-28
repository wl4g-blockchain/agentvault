"""Deployment rendering rejects unknown or mismatched component directories."""
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import Mock

ROOT=Path(__file__).resolve().parents[6]
sys.path.insert(0,str(ROOT / "use-cases/marketpulse-feed-subscription/e2e"))
from common.project import DEPLOY_COMPONENTS
from deploy.deployer.docker import DockerE2EDeployer


class DockerManifestCatalogTest(unittest.TestCase):
    def setUp(self):
        temporary=TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.root=Path(temporary.name)
        for component in DEPLOY_COMPONENTS:
            directory=self.root/component
            (directory/"helm").mkdir(parents=True)
            (directory/"docker-compose.yaml").write_text("services: {}\n")
            (directory/"helm/Chart.yaml").write_text(
                f"apiVersion: v2\nname: {component}\nannotations:\n"
                "  agentvault.io/e2e-namespace: e2e-agentvault-payment-system\n")
        self.port=object.__new__(DockerE2EDeployer)
        self.port.context=SimpleNamespace(deploy_root=self.root)
        self.port.docker=["docker"]
        self.port._render_agentvault_config=Mock(return_value="typed configuration")
        self.port._compose=Mock();self.port._full=Mock();self.port._run=Mock(return_value="")

    def test_exact_shared_catalog_renders_all_component_pairs(self):
        output=self.port.render()
        self.assertIn("20 component Compose manifests and Helm charts",output)
        self.assertEqual(self.port._full.call_count,25)
        self.assertEqual(self.port._run.call_count,3*len(DEPLOY_COMPONENTS))

    def test_same_count_with_unknown_component_does_not_pass(self):
        component=DEPLOY_COMPONENTS[0]
        (self.root/component).rename(self.root/"unreviewed-component")
        with self.assertRaisesRegex(RuntimeError,"manifests drifted"):self.port.render()
        self.port._run.assert_not_called()

    def test_missing_compose_or_extra_chart_does_not_pass(self):
        (self.root/DEPLOY_COMPONENTS[0]/"docker-compose.yaml").unlink()
        with self.assertRaisesRegex(RuntimeError,"manifests drifted"):self.port.render()
        self.port._run.assert_not_called()
