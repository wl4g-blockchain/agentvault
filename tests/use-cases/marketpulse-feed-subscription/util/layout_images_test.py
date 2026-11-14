import os
import re
import subprocess
import unittest
from unittest.mock import patch

from util.constant import E2EOtherDefinitions as Project
from util.images import E2EImagesDefinitions as Images
from util.kubernetes import E2EKubernetesDeployer
from util.process import E2EProcessHelper as Process


class LayoutImagesTest(unittest.TestCase):
    def test_project_uses_production_links_and_only_mutable_config_below_config(self):
        directory = Project.AGENTVAULT_ROOT
        self.assertEqual(
            {p.name for p in directory.iterdir()},
            {"Dockerfile", "Dockerfile.dockerignore", "config", "docker-compose.yaml"},
        )
        for name in ("Dockerfile", "Dockerfile.dockerignore"):
            self.assertTrue((directory / name).is_symlink())
            self.assertEqual(
                (directory / name).resolve(),
                Project.REPOSITORY_ROOT / "deploy/docker" / name,
            )
        self.assertEqual(
            Project.component_chart("agentvault"),
            Project.REPOSITORY_ROOT / "deploy/helm/agentvault",
        )

    def test_all_network_matrix_dependencies_have_explicit_correct_origins(self):
        for host in Images.MATRIX[Images.IN_GFW]:
            for key, reference in Images.MATRIX[Images.IN_GFW][host].items():
                if reference.startswith("localhost/"):
                    continue
                self.assertTrue(
                    reference.startswith(Images.ALIYUN + "/"), (key, reference)
                )
            for key, reference in Images.MATRIX[Images.NON_GFW][host].items():
                if reference.startswith("localhost/") or (
                    host == Images.ARM64 and key == "solana"
                ):
                    continue
                self.assertTrue(
                    reference.startswith(("docker.io/", "ghcr.io/")), (key, reference)
                )

    def test_platform_policy_includes_macos_linux_and_windows(self):
        for system in ("darwin", "linux", "win32"):
            for cpu in ("arm64", "x86_64"):
                for network in ("IN_GFW", "NON_GFW"):
                    with (
                        self.subTest(system=system, cpu=cpu, network=network),
                        patch("util.images.sys.platform", system),
                        patch("util.images.platform.machine", return_value=cpu),
                    ):
                        env = {"E2E_IMAGE_NETWORK": network}
                        Images.configure(env)
                        self.assertEqual(env["E2E_IMAGE_PLATFORM"], "linux/amd64")
                        self.assertEqual(
                            env["E2E_SOLANA_PLATFORM"],
                            "linux/arm64" if system == "darwin" else "linux/amd64",
                        )
                        if system == "darwin":
                            self.assertEqual(
                                env["E2E_SOLANA_IMAGE"], Images.SOLANA_ARM64
                            )

    def test_network_switch_reprojects_defaults_and_preserves_custom_outputs(self):
        env = {
            "E2E_IMAGE_NETWORK": "IN_GFW",
            "E2E_AGENTVAULT_IMAGE": "localhost/custom-vault:review",
        }
        Images.configure(env)
        self.assertTrue(env["E2E_EMQX_IMAGE"].startswith(Images.ALIYUN))
        env["E2E_IMAGE_NETWORK"] = "NON_GFW"
        self.assertTrue(Images.reference("emqx", env).startswith("docker.io/"))
        Images.configure(env)
        self.assertTrue(env["E2E_EMQX_IMAGE"].startswith("docker.io/"))
        self.assertEqual(env["E2E_AGENTVAULT_IMAGE"], "localhost/custom-vault:review")

    def test_production_docker_build_uses_declared_arguments_and_component_overrides(
        self,
    ):
        env = {
            "E2E_IMAGE_NETWORK": "NON_GFW",
            "E2E_AGENTVAULT_BUILD_IMAGE": "localhost/custom-rust:review",
        }
        Images.configure(env)
        args = Images.build_arguments(Project.AGENTVAULT_ROOT / "Dockerfile", env)
        assignments = dict(item.split("=", 1) for item in args if "=" in item)
        declared = set(
            re.findall(
                r"(?m)^ARG (\w*IMAGE)",
                (Project.AGENTVAULT_ROOT / "Dockerfile").read_text(),
            )
        )
        self.assertEqual(set(assignments), declared | {"BUILDKIT_SYNTAX"})
        self.assertEqual(
            assignments["RUST_BUILDER_IMAGE"], "localhost/custom-rust:review"
        )
        self.assertEqual(
            assignments["RUNTIME_IMAGE"], env["E2E_AGENTVAULT_RUNTIME_IMAGE"]
        )

    def test_local_solana_alias_is_never_pulled_or_rewritten_as_registry_digest(self):
        env = {
            "E2E_IMAGE_NETWORK": "IN_GFW",
            "E2E_SOLANA_IMAGE": Images.LOCAL_SOLANA_ARM64,
            "E2E_SOLANA_PLATFORM": "linux/arm64",
        }
        loader = E2EKubernetesDeployer.LocalImageLoader(
            "linux/arm64", ["docker"], [], [], env
        )
        with (
            patch.object(
                Process,
                "execute",
                return_value=subprocess.CompletedProcess([], 0, "linux/arm64\n"),
            ),
            patch.object(Process, "run") as run,
        ):
            self.assertEqual(
                loader.deployment_image(Images.LOCAL_SOLANA_ARM64, 20),
                Images.LOCAL_SOLANA_ARM64,
            )
            run.assert_not_called()
        with (
            patch.object(
                Process,
                "execute",
                return_value=subprocess.CompletedProcess([], 1, "missing"),
            ),
            patch.object(Process, "run") as run,
        ):
            with self.assertRaises(RuntimeError):
                loader.deployment_image(Images.LOCAL_SOLANA_ARM64, 20)
            run.assert_not_called()

    def test_standalone_build_injects_the_matrix_platform(self):
        dockerfile = Project.AGENTVAULT_ROOT / "Dockerfile"
        with (
            patch.dict(os.environ, {"E2E_IMAGE_NETWORK": "NON_GFW"}, clear=True),
            patch(
                "sys.argv",
                [
                    "images.py",
                    "--",
                    "docker",
                    "buildx",
                    "build",
                    "--load",
                    "-f",
                    str(dockerfile),
                    ".",
                ],
            ),
            patch(
                "subprocess.run", return_value=subprocess.CompletedProcess([], 0)
            ) as run,
        ):
            with self.assertRaises(SystemExit) as result:
                Images.main()
            self.assertEqual(result.exception.code, 0)
            command = run.call_args.args[0]
            self.assertEqual(command[command.index("--platform") + 1], "linux/amd64")
            self.assertEqual(
                command[command.index("-f") + 1], str(dockerfile.resolve())
            )

    def test_anvil_launches_the_binary_in_both_compose_layouts_and_helm(self):
        import yaml

        for path in (
            Project.E2E_ROOT / "docker-compose.yaml",
            Project.DEPLOY_ROOT / "anvil/docker-compose.yaml",
        ):
            with self.subTest(path=path):
                service = yaml.safe_load(path.read_text())["services"]["anvil"]
                self.assertEqual(service.get("entrypoint"), ["anvil"])
                self.assertIn("--chain-id=31337", service["command"])
        values = yaml.safe_load(
            (Project.DEPLOY_ROOT / "anvil/helm/values.yaml").read_text()
        )
        self.assertEqual(values["command"], ["anvil"])
        self.assertIn("--chain-id=31337", values["args"])
