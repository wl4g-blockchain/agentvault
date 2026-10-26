"""Kubernetes lifecycle for the MarketPulse payment E2E.

The Docker and Kubernetes backends intentionally share the same protocol,
ledger, receipt, and balance assertions.  This module replaces only lifecycle
boundaries: local-image loading, Helm installation, port forwarding, Jobs,
logs, and PostgreSQL access.
"""

from __future__ import annotations

from contextlib import ExitStack, contextmanager
from dataclasses import dataclass, replace
import json
import logging
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import time
from typing import Any, Iterator, Mapping, Sequence

from common.context import VerificationContext
from common.payer_identity import LANGUAGES, PAYER_IDENTITIES, payer_identity
from common.process import execute, run
from common.project import CONFIG_ROOT, E2E_ROOT, REPOSITORY_ROOT
from common.resource_import import ConsoleResourceFixture
from common.sdk_matrix import TRANSPORTS, matrix_cases, matrix_languages

from .base import E2EDeployer, FullFlowEvidence
from .docker import (
    AGENTVAULT_CHART,
    AGENTVAULT_CONFIG_TEMPLATE,
    AGENTVAULT_NAMESPACE,
    AGENTVAULT_VALUES,
    COMPOSE_FILE,
    DEFAULTS,
    E2E_LOCAL_IMAGE_REFERENCES,
    FACILITATOR_ADDRESS,
    PAYEE_ADDRESS,
    PAYER_PRIVATE_KEY_DEFAULTS,
    RUNTIME_ROOT,
    SOLANA_CLI_ROOT,
    TOKEN_CONTRACT,
    DockerE2EDeployer,
    _agentvault_build_targets,
    _ensure_matrix_plan,
    _inherit_build_proxy_environment,
    _load_matrix_checkpoint,
)


LOGGER = logging.getLogger("agentvault.e2e.runner.kubernetes.deployer")

NAMESPACE_MANIFEST = E2E_ROOT / "deploy" / "namespaces.yaml"
AGENTVAULT_SHARED_ROOT = E2E_ROOT / "deploy" / "agentvault-helm-shared"
MARKETPULSE_SHARED_ROOT = E2E_ROOT / "deploy" / "mock-payee-marketpulse-config"
QUANTSTREAM_SHARED_ROOT = E2E_ROOT / "deploy" / "mock-payer-quantstream-config"
MARKETPULSE_SHARED_VALUES = MARKETPULSE_SHARED_ROOT / "values-shared-override.yaml"
QUANTSTREAM_SHARED_VALUES = QUANTSTREAM_SHARED_ROOT / "values-shared-override.yaml"
SHARED_KUBERNETES_MANIFESTS = (
    AGENTVAULT_SHARED_ROOT / "local-envoy.configmap.yaml",
    MARKETPULSE_SHARED_ROOT / "local-envoy.configmap.yaml",
    QUANTSTREAM_SHARED_ROOT / "local-envoy.configmap.yaml",
)
NAMESPACE_PREFIX = "e2e-agentvault-"
NAMESPACE_SELECTOR = "app.kubernetes.io/part-of=agentvault-e2e"
INSTANCE_LABEL = "app.kubernetes.io/instance"
FINALIZER_PATCH = '{"metadata":{"finalizers":[]}}'
DEFAULT_KUBECONFIG = Path("/etc/rancher/k3s/k3s.yaml")
RESOURCE_REFERENCE = re.compile(r"^[a-z0-9][a-z0-9.-]*/[A-Za-z0-9_.-]+$")

KUBERNETES_AGENTVAULT_RUNTIME_IMAGE = "gcr.io/distroless/cc-debian12:nonroot"

INFRA_NAMESPACE = "e2e-agentvault-infra-system"
PAYMENT_NAMESPACE = "e2e-agentvault-payment-system"
PAYER_NAMESPACE = "e2e-agentvault-mock-payer"
PAYEE_NAMESPACE = "e2e-agentvault-mock-payee"

LANGUAGE_CHART_NAMES = {
    "rust": "rust",
    "go": "golang",
    "python": "python",
    "typescript": "typescript",
    "java": "java",
}
LOCAL_IMAGES = {
    "agentvault": "localhost/agentvault-e2e:local",
    "admin-ui": "localhost/quantstream-admin-ui-e2e:local",
    **{
        f"payee-{language}": f"localhost/marketpulse-{chart_language}-e2e:local"
        for language, chart_language in LANGUAGE_CHART_NAMES.items()
    },
    **{
        f"payer-{language}": f"localhost/quantstream-ingestor-{chart_language}-e2e:local"
        for language, chart_language in LANGUAGE_CHART_NAMES.items()
    },
}
COMPOSE_IMAGE_ENV = {
    "agentvault": "E2E_AGENTVAULT_IMAGE",
    "admin-ui": "E2E_QUANTSTREAM_UI_IMAGE",
    "payee-rust": "E2E_MARKETPULSE_RUST_IMAGE",
    "payee-go": "E2E_MARKETPULSE_GOLANG_IMAGE",
    "payee-python": "E2E_MARKETPULSE_PYTHON_IMAGE",
    "payee-typescript": "E2E_MARKETPULSE_TYPESCRIPT_IMAGE",
    "payee-java": "E2E_MARKETPULSE_JAVA_IMAGE",
    "payer-rust": "E2E_QUANTSTREAM_RUST_IMAGE",
    "payer-go": "E2E_QUANTSTREAM_GOLANG_IMAGE",
    "payer-python": "E2E_QUANTSTREAM_PYTHON_IMAGE",
    "payer-typescript": "E2E_QUANTSTREAM_TYPESCRIPT_IMAGE",
    "payer-java": "E2E_QUANTSTREAM_JAVA_IMAGE",
}
SERVICE_NAMES = {
    "agentvault-api": (AGENTVAULT_NAMESPACE, "agentvault", 8080),
    "agentvault-management": (AGENTVAULT_NAMESPACE, "agentvault-management", 9091),
    "anvil": (PAYMENT_NAMESPACE, "anvil", 8545),
    "tempo": (PAYMENT_NAMESPACE, "tempo", 8545),
    "solana": (PAYMENT_NAMESPACE, "solana", 8899),
    "facilitator": (PAYMENT_NAMESPACE, "x402-facilitator", 8085),
    "emqx-mqtt": (INFRA_NAMESPACE, "emqx", 1883),
    "emqx-dashboard": (INFRA_NAMESPACE, "emqx", 18083),
    "jaeger": (INFRA_NAMESPACE, "jaeger", 16686),
    "admin-ui": (PAYER_NAMESPACE, "quantstream-ui", 8086),
}


@dataclass(frozen=True, order=True)
class HelmRelease:
    namespace: str
    name: str


class KubernetesE2ECleaner:
    """Uninstall only Helm releases in manifest-declared AgentVault E2E namespaces."""

    def __init__(
        self,
        timeout_seconds: float,
        *,
        kubeconfig: Path | None = None,
        kubectl: str | None = None,
        helm: str | None = None,
    ) -> None:
        if timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be positive")
        configured = kubeconfig or Path(os.environ.get("KUBECONFIG", DEFAULT_KUBECONFIG))
        self._kubeconfig = configured
        self._kubectl = kubectl or shutil.which("kubectl")
        self._helm = helm or shutil.which("helm")
        self._command_timeout = max(5.0, min(30.0, timeout_seconds))
        self._resource_types: tuple[str, ...] | None = None

    def cleanup(self) -> str:
        if not self._kubeconfig.is_file():
            message = f"Kubernetes cleanup skipped; kubeconfig not found: {self._kubeconfig}"
            LOGGER.info(message)
            return message
        if self._kubectl is None or self._helm is None:
            raise RuntimeError("Kubernetes cleanup requires both kubectl and helm")

        namespaces = self._target_namespaces()
        if not namespaces:
            message = "Kubernetes cleanup found no labelled AgentVault E2E namespaces"
            LOGGER.info(message)
            return message

        output: list[str] = []
        releases = self._releases(namespaces)
        for release in releases:
            LOGGER.info(
                "uninstalling E2E Helm release=%s namespace=%s",
                release.name,
                release.namespace,
            )
            result = self._helm_command(self._uninstall_args(release))
            output.append(result.stdout)
            if result.returncode == 0:
                continue

            LOGGER.warning(
                "Helm uninstall did not finish cleanly; recovering scoped resources "
                "release=%s namespace=%s",
                release.name,
                release.namespace,
            )
            output.extend(self._recover_release(release))
            retry = self._helm_command([*self._uninstall_args(release, wait=False), "--no-hooks"])
            output.append(retry.stdout)
            if retry.returncode != 0 and release in self._releases(namespaces):
                output.extend(self._remove_helm_records(release))

        self._wait_for_pods(namespaces)
        output.extend(self._recover_remaining_pods(namespaces))

        remaining_releases = self._releases(namespaces)
        remaining_pods = self._pods(namespaces)
        if remaining_releases or remaining_pods:
            raise RuntimeError(
                "Kubernetes E2E cleanup left releases "
                f"{remaining_releases} and pods {remaining_pods}"
            )
        return "\n".join(part.strip() for part in output if part.strip())

    def _target_namespaces(self) -> tuple[str, ...]:
        declared = self._declared_namespaces()
        result = self._kubectl_command(
            [
                "get",
                "namespaces",
                "--selector",
                NAMESPACE_SELECTOR,
                "--output=json",
                "--request-timeout=5s",
            ],
            timeout_seconds=10,
        )
        if result.returncode != 0:
            raise RuntimeError(f"could not inventory Kubernetes E2E namespaces:\n{result.stdout}")
        items = self._json_items(result.stdout, "Kubernetes namespace inventory")
        discovered = {
            str(item.get("metadata", {}).get("name", ""))
            for item in items
            if isinstance(item, dict)
        }
        return tuple(sorted(declared & discovered))

    @staticmethod
    def _declared_namespaces() -> frozenset[str]:
        content = NAMESPACE_MANIFEST.read_text(encoding="utf-8")
        names = frozenset(
            match.group(1)
            for match in re.finditer(r"(?m)^  name: (e2e-agentvault-[a-z0-9-]+)$", content)
        )
        if not names or any(not name.startswith(NAMESPACE_PREFIX) for name in names):
            raise RuntimeError("E2E namespace manifest contains no safe target namespaces")
        return names

    def _releases(self, namespaces: tuple[str, ...]) -> tuple[HelmRelease, ...]:
        result = self._helm_command(["list", "--all-namespaces", "--all", "--output=json"])
        if result.returncode != 0:
            raise RuntimeError(f"could not inventory Helm releases:\n{result.stdout}")
        try:
            inventory = json.loads(result.stdout or "[]")
        except json.JSONDecodeError as error:
            raise RuntimeError("Helm release inventory was not valid JSON") from error
        if not isinstance(inventory, list):
            raise RuntimeError("Helm release inventory must be a JSON array")
        allowed = set(namespaces)
        return tuple(
            sorted(
                HelmRelease(namespace=str(item["namespace"]), name=str(item["name"]))
                for item in inventory
                if isinstance(item, dict)
                and item.get("namespace") in allowed
                and isinstance(item.get("name"), str)
            )
        )

    def _uninstall_args(self, release: HelmRelease, *, wait: bool = True) -> list[str]:
        arguments = [
            "uninstall",
            release.name,
            "--namespace",
            release.namespace,
            "--ignore-not-found",
            "--cascade=foreground",
            "--timeout",
            f"{int(self._command_timeout)}s",
        ]
        if wait:
            arguments.append("--wait")
        return arguments

    def _recover_release(self, release: HelmRelease) -> list[str]:
        output: list[str] = []
        selector = f"{INSTANCE_LABEL}={release.name}"
        resource_types = [kind for kind in self._namespaced_resource_types() if kind != "pods"]
        for resource_type in (*resource_types, "pods"):
            inventory = self._kubectl_command(
                [
                    "get",
                    resource_type,
                    "--namespace",
                    release.namespace,
                    "--selector",
                    selector,
                    "--output=name",
                    "--ignore-not-found",
                ]
            )
            if inventory.returncode != 0:
                continue
            for resource in self._resource_references(inventory.stdout):
                output.extend(self._clear_and_delete(resource, release.namespace))
        return output

    def _namespaced_resource_types(self) -> tuple[str, ...]:
        if self._resource_types is not None:
            return self._resource_types
        result = self._kubectl_command(
            ["api-resources", "--namespaced=true", "--verbs=list,delete", "--output=name"]
        )
        if result.returncode != 0:
            raise RuntimeError(f"could not discover namespaced Kubernetes resources:\n{result.stdout}")
        self._resource_types = tuple(
            sorted({line.strip() for line in result.stdout.splitlines() if line.strip()})
        )
        return self._resource_types

    def _remove_helm_records(self, release: HelmRelease) -> list[str]:
        inventory = self._kubectl_command(
            [
                "get",
                "secrets",
                "--namespace",
                release.namespace,
                "--selector",
                f"owner=helm,name={release.name}",
                "--output=name",
                "--ignore-not-found",
            ]
        )
        if inventory.returncode != 0:
            raise RuntimeError(
                f"could not inventory Helm records for {release.namespace}/{release.name}:\n"
                f"{inventory.stdout}"
            )
        output: list[str] = []
        for resource in self._resource_references(inventory.stdout):
            output.extend(self._clear_and_delete(resource, release.namespace))
        return output

    @staticmethod
    def _resource_references(output: str) -> tuple[str, ...]:
        return tuple(
            sorted(
                {
                    line.strip()
                    for line in output.splitlines()
                    if RESOURCE_REFERENCE.fullmatch(line.strip())
                }
            )
        )

    def _wait_for_pods(self, namespaces: tuple[str, ...]) -> None:
        deadline = time.monotonic() + min(10.0, self._command_timeout)
        while time.monotonic() < deadline:
            if not self._pods(namespaces):
                return
            time.sleep(0.5)

    def _recover_remaining_pods(self, namespaces: tuple[str, ...]) -> list[str]:
        output: list[str] = []
        for namespace, pod in self._pods(namespaces):
            LOGGER.warning(
                "force-removing stranded E2E pod=%s namespace=%s after graceful uninstall",
                pod,
                namespace,
            )
            output.extend(self._clear_and_delete(f"pod/{pod}", namespace))
        return output

    def _pods(self, namespaces: tuple[str, ...]) -> tuple[tuple[str, str], ...]:
        pods: list[tuple[str, str]] = []
        for namespace in namespaces:
            result = self._kubectl_command(
                [
                    "get",
                    "pods",
                    "--namespace",
                    namespace,
                    "--output=json",
                    "--ignore-not-found",
                ]
            )
            if result.returncode != 0:
                raise RuntimeError(f"could not inventory pods in {namespace}:\n{result.stdout}")
            if not result.stdout.strip():
                continue
            for item in self._json_items(result.stdout, f"pod inventory for {namespace}"):
                if not isinstance(item, dict):
                    continue
                name = item.get("metadata", {}).get("name", "")
                if isinstance(name, str) and name:
                    pods.append((namespace, name))
        return tuple(sorted(pods))

    def _clear_and_delete(self, resource: str, namespace: str) -> list[str]:
        patch = self._kubectl_command(
            [
                "patch",
                resource,
                "--namespace",
                namespace,
                "--type=merge",
                "--patch",
                FINALIZER_PATCH,
            ]
        )
        delete_args = [
            "delete",
            resource,
            "--namespace",
            namespace,
            "--ignore-not-found",
            "--wait=false",
        ]
        if resource.startswith("pod/") or resource.startswith("pods/"):
            delete_args.extend(["--grace-period=0", "--force"])
        deleted = self._kubectl_command(delete_args)
        if deleted.returncode != 0:
            raise RuntimeError(f"could not delete {namespace}/{resource}:\n{deleted.stdout}")
        return [patch.stdout, deleted.stdout]

    @staticmethod
    def _json_items(output: str, description: str) -> list[object]:
        try:
            document = json.loads(output or "{}")
        except json.JSONDecodeError as error:
            raise RuntimeError(f"{description} was not valid JSON") from error
        items = document.get("items") if isinstance(document, dict) else None
        if not isinstance(items, list):
            raise RuntimeError(f"{description} did not contain an items array")
        return items

    def _kubectl_command(
        self,
        arguments: list[str],
        *,
        timeout_seconds: float | None = None,
    ) -> subprocess.CompletedProcess[str]:
        if self._kubectl is None:
            raise RuntimeError("kubectl is not available")
        return self._command(self._kubectl, arguments, timeout_seconds)

    def _helm_command(self, arguments: list[str]) -> subprocess.CompletedProcess[str]:
        if self._helm is None:
            raise RuntimeError("helm is not available")
        return self._command(self._helm, arguments, self._command_timeout + 5)

    def _command(
        self,
        executable: str,
        arguments: list[str],
        timeout_seconds: float | None,
    ) -> subprocess.CompletedProcess[str]:
        environment = {"KUBECONFIG": str(self._kubeconfig)}
        command = [executable, *arguments]
        if not os.access(self._kubeconfig, os.R_OK):
            sudo = shutil.which("sudo")
            if sudo is None:
                raise RuntimeError(f"kubeconfig is not readable: {self._kubeconfig}")
            command = [
                sudo,
                "-n",
                "env",
                f"KUBECONFIG={self._kubeconfig}",
                executable,
                *arguments,
            ]
        return execute(
            command,
            E2E_ROOT,
            environment,
            timeout_seconds or self._command_timeout,
        )


class KubernetesE2EDeployer(DockerE2EDeployer):
    """Run the same fail-closed payment assertions on local Kubernetes.

    Inheritance is limited to backend-neutral evidence probes already owned by
    ``DockerE2EDeployer``.  Every operation that mutates or observes runtime
    state is overridden here and uses Kubernetes resources exclusively.
    """

    backend = "kubernetes"

    def __init__(self, context: VerificationContext) -> None:
        E2EDeployer.__init__(self, context)
        if context.worker_slot is not None:
            raise ValueError("Kubernetes E2E uses one shared cluster and does not accept worker slots")
        self._wallet_addresses_cache: dict[str, str] | None = None
        self.environment = os.environ.copy()
        _inherit_build_proxy_environment(self.environment)
        for name, value in DEFAULTS.items():
            self.environment.setdefault(name, value)
        self.environment.update(context.environment())
        # Compose keeps a tool-rich runtime for its in-container curl healthcheck.
        # Kubelet probes the HTTP endpoint externally, so Kubernetes can retain
        # the production-style runtime without carrying the 1.5 GiB builder.
        self.environment.setdefault(
            "E2E_AGENTVAULT_RUNTIME_IMAGE",
            KUBERNETES_AGENTVAULT_RUNTIME_IMAGE,
        )
        for key, image in LOCAL_IMAGES.items():
            self.environment[COMPOSE_IMAGE_ENV[key]] = image
        self.environment.setdefault("COMPOSE_BAKE", "false")
        self.agentvault_config = RUNTIME_ROOT / "agentvault-kubernetes.yaml"
        self.environment["E2E_AGENTVAULT_CONFIG_PATH"] = str(self.agentvault_config)

        self.kubeconfig = Path(os.environ.get("KUBECONFIG", DEFAULT_KUBECONFIG))
        if not self.kubeconfig.is_file():
            raise RuntimeError(f"Kubernetes deployer kubeconfig does not exist: {self.kubeconfig}")
        self.environment["KUBECONFIG"] = str(self.kubeconfig)
        self._kubectl_executable = shutil.which("kubectl")
        self._helm_executable = shutil.which("helm")
        if self._kubectl_executable is None or self._helm_executable is None:
            raise RuntimeError("Kubernetes deployer requires kubectl and helm")

        probe = execute(["docker", "info"], E2E_ROOT, self.environment, 15)
        self.docker = ["docker"] if probe.returncode == 0 else ["sudo", "-n", "-E", "docker"]
        self.compose = [*self.docker, "compose", "-p", "agentvault-e2e-kubernetes-build", "-f", str(COMPOSE_FILE)]
        self._forwarded_ports: dict[str, int] = {}

    def _run(
        self,
        command: Sequence[str],
        timeout: float | None = None,
        cwd: Path = E2E_ROOT,
    ) -> str:
        operation = DockerE2EDeployer._command_operation(list(command))
        started_at = time.monotonic()
        LOGGER.info("START command=%s backend=kubernetes", operation)
        try:
            output = run(command, cwd, self.environment, timeout or self.context.timeout_seconds)
        except BaseException:
            LOGGER.error(
                "FAIL command=%s backend=kubernetes elapsed_seconds=%.1f",
                operation,
                time.monotonic() - started_at,
            )
            raise
        LOGGER.info(
            "PASS command=%s backend=kubernetes elapsed_seconds=%.1f",
            operation,
            time.monotonic() - started_at,
        )
        return output

    def _privileged_command(self, executable: str, *arguments: str) -> list[str]:
        if os.access(self.kubeconfig, os.R_OK):
            return [executable, *arguments]
        return [
            "sudo",
            "-n",
            "env",
            f"KUBECONFIG={self.kubeconfig}",
            executable,
            *arguments,
        ]

    def _kubectl(self, *arguments: str, timeout: float | None = None) -> str:
        return self._run(
            self._privileged_command(self._kubectl_executable or "kubectl", *arguments),
            timeout,
        )

    def _helm(self, *arguments: str, timeout: float | None = None) -> str:
        return self._run(
            self._privileged_command(self._helm_executable or "helm", *arguments),
            timeout,
        )

    def _apply_document(self, document: Mapping[str, object]) -> str:
        command = self._privileged_command(self._kubectl_executable or "kubectl", "apply", "-f", "-")
        completed = subprocess.run(
            command,
            cwd=E2E_ROOT,
            env=self.environment,
            input=json.dumps(document, separators=(",", ":")),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=min(self.context.timeout_seconds, 60),
            check=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(f"kubectl apply failed with exit code {completed.returncode}:\n{completed.stdout}")
        return completed.stdout

    def render(self) -> str:
        lines = [self._render_agentvault_config()]
        for manifest in (NAMESPACE_MANIFEST, *SHARED_KUBERNETES_MANIFESTS):
            self._kubectl("apply", "--dry-run=client", "--filename", str(manifest))
        charts = sorted(self.context.deploy_root.glob("*/helm"))
        for chart in charts:
            namespace = DockerE2EDeployer._chart_namespace(chart)
            release = DockerE2EDeployer._chart_name(chart)
            self._helm("lint", "--namespace", namespace, str(chart))
            arguments = ["template", release, str(chart), "--namespace", namespace]
            for value_file in self._shared_overrides_for_chart(chart):
                arguments.extend(("--values", str(value_file)))
            self._helm(*arguments)
        self._helm(
            "lint",
            "--namespace",
            AGENTVAULT_NAMESPACE,
            str(AGENTVAULT_CHART),
            "--values",
            str(AGENTVAULT_VALUES),
        )
        lines.append(f"validated Kubernetes rendering for AgentVault and {len(charts)} component charts")
        return "\n".join(lines)

    def cleanup(self) -> str:
        return KubernetesE2ECleaner(
            self.context.timeout_seconds,
            kubeconfig=self.kubeconfig,
        ).cleanup()

    def cleanup_images(self) -> str:
        removed: list[str] = []
        cluster_images = self._cluster_images()
        for reference in sorted(set(LOCAL_IMAGES.values())):
            if reference in cluster_images:
                completed = execute(
                    [
                        "sudo",
                        "-n",
                        "k3s",
                        "ctr",
                        "-n",
                        "k8s.io",
                        "images",
                        "remove",
                        reference,
                    ],
                    E2E_ROOT,
                    self.environment,
                    60,
                )
                if completed.returncode == 0:
                    removed.append(f"cluster:{reference}")
            completed = execute(
                [*self.docker, "image", "rm", reference],
                E2E_ROOT,
                self.environment,
                180,
            )
            if completed.returncode == 0:
                removed.append(f"local:{reference}")
        return "removed local Kubernetes E2E images: " + ", ".join(removed)

    def _build_images(self, payer: str, payee: str) -> None:
        keys = ("agentvault", f"payee-{payee}", "admin-ui", f"payer-{payer}")
        if not self.context.skip_build:
            self._target_budget()
            profiles = (
                "infra",
                "payee",
                "apps",
                f"payee-{payee}",
                f"ingestor-{payer}",
            )
            profile_args = [item for profile in profiles for item in ("--profile", profile)]
            targets = [
                *_agentvault_build_targets(self.environment),
                f"marketpulse-{payee}",
                "admin-ui",
                f"quantstream-{payer}",
            ]
            build_args = [*self.compose, *profile_args, "build"]
            if not self.context.reuse_build:
                build_args.append("--pull")
            self._run([*build_args, *targets], self.context.timeout_seconds)
        for key in keys:
            self._ensure_cluster_image(LOCAL_IMAGES[key])

    def _build_matrix_images(self, languages: tuple[str, ...]) -> None:
        if not self.context.skip_build:
            self._target_budget()
            profiles = (
                "infra",
                "payee",
                "apps",
                *(f"payee-{language}" for language in languages),
                *(f"ingestor-{language}" for language in languages),
            )
            profile_args = [item for profile in profiles for item in ("--profile", profile)]
            targets = [
                *_agentvault_build_targets(self.environment),
                "admin-ui",
                *(f"marketpulse-{language}" for language in languages),
                *(f"quantstream-{language}" for language in languages),
            ]
            build_args = [*self.compose, *profile_args, "build"]
            if not self.context.reuse_build:
                build_args.append("--pull")
            self._run([*build_args, *targets], self.context.timeout_seconds)
        keys = (
            "agentvault",
            "admin-ui",
            *(f"payee-{item}" for item in languages),
            *(f"payer-{item}" for item in languages),
        )
        for key in keys:
            self._ensure_cluster_image(LOCAL_IMAGES[key])

    def _cluster_images(self) -> set[str]:
        output = self._run(["sudo", "-n", "k3s", "ctr", "-n", "k8s.io", "images", "list", "-q"], 60)
        return {line.strip() for line in output.splitlines() if line.strip()}

    def _ensure_cluster_image(self, reference: str) -> None:
        if reference in self._cluster_images():
            return
        inspect = execute([*self.docker, "image", "inspect", reference], E2E_ROOT, self.environment, 30)
        if inspect.returncode != 0:
            raise RuntimeError(
                f"Kubernetes image {reference} is absent from both the cluster and local image engine; "
                "rerun without --skip-build"
            )
        exporter = subprocess.Popen(
            [*self.docker, "save", reference],
            cwd=E2E_ROOT,
            env=self.environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        assert exporter.stdout is not None
        importer = subprocess.Popen(
            ["sudo", "-n", "k3s", "ctr", "-n", "k8s.io", "images", "import", "-"],
            cwd=E2E_ROOT,
            env=self.environment,
            stdin=exporter.stdout,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        exporter.stdout.close()
        importer_output = importer.communicate(timeout=self.context.timeout_seconds)[0].decode("utf-8", "replace")
        exporter_stderr = exporter.communicate(timeout=30)[1].decode("utf-8", "replace")
        if exporter.returncode != 0 or importer.returncode != 0:
            raise RuntimeError(
                f"could not import {reference} into local K3s: "
                f"export={exporter.returncode} import={importer.returncode}\n"
                f"{exporter_stderr[-2048:]}\n{importer_output[-2048:]}"
            )

    def _apply_namespaces(self) -> None:
        self._kubectl("apply", "--filename", str(NAMESPACE_MANIFEST))

    def _apply_secret(self, namespace: str, name: str, values: Mapping[str, str]) -> None:
        if not values or any(not key or "\n" in value for key, value in values.items()):
            raise ValueError(f"invalid Kubernetes E2E Secret payload for {namespace}/{name}")
        self._apply_document(
            {
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": name,
                    "namespace": namespace,
                    "labels": {"app.kubernetes.io/part-of": "agentvault-e2e"},
                },
                "type": "Opaque",
                "stringData": dict(values),
            }
        )

    def _apply_config_map(self, namespace: str, name: str, values: Mapping[str, str]) -> None:
        self._apply_document(
            {
                "apiVersion": "v1",
                "kind": "ConfigMap",
                "metadata": {
                    "name": name,
                    "namespace": namespace,
                    "labels": {"app.kubernetes.io/part-of": "agentvault-e2e"},
                },
                "data": dict(values),
            }
        )

    def _bootstrap_manifests(self, payee_language: str | None = None) -> None:
        shared = {
            "postgres-password": self.environment.get("E2E_POSTGRES_PASSWORD", "agentvault-e2e-only"),
            "emqx-dashboard-password": self.environment["E2E_EMQX_DASHBOARD_PASSWORD"],
            "facilitator-private-key": self.environment.get(
                "E2E_FACILITATOR_PRIVATE_KEY",
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
            ),
            "mqtt-workload-password": self.environment["E2E_QUANTSTREAM_MQTT_PASSWORD"],
            "mqtt-approver-password": self.environment["E2E_QUANTSTREAM_UI_MQTT_PASSWORD"],
            "admin-ui-token": self.environment.get(
                "E2E_QUANTSTREAM_UI_TOKEN",
                "quantstream-e2e-ui-token-0000000000000000001",
            ),
        }
        for namespace in (INFRA_NAMESPACE, PAYMENT_NAMESPACE, PAYER_NAMESPACE):
            self._apply_secret(namespace, "marketpulse-e2e-secrets", shared)
        for language, chart_language in LANGUAGE_CHART_NAMES.items():
            self._apply_secret(
                PAYEE_NAMESPACE,
                f"marketpulse-{chart_language}-e2e-secrets",
                {"agentvault-mqtt-password": self.environment["E2E_MARKETPULSE_MQTT_PASSWORD"]},
            )

        # The manifest refers to the HELPER_SECRET_QUANTSTREAM_* slots;
        # preserve explicit operator overrides while keeping deterministic E2E defaults.
        wallet_values = {
            agentvault_key_env: self.environment.get(
                private_key_env,
                PAYER_PRIVATE_KEY_DEFAULTS[language][index],
            )
            for language in LANGUAGES
            for index, (agentvault_key_env, private_key_env) in enumerate(
                (
                    (
                        PAYER_IDENTITIES[language].primary_agentvault_key_env,
                        PAYER_IDENTITIES[language].primary_private_key_env,
                    ),
                    (
                        PAYER_IDENTITIES[language].secondary_agentvault_key_env,
                        PAYER_IDENTITIES[language].secondary_private_key_env,
                    ),
                )
            )
        }
        agentvault_secret = {
            "HELPER_SECRET_STORAGE_POSTGRES_URL": (
                "postgres://agentvault:"
                + shared["postgres-password"]
                + f"@postgres.{INFRA_NAMESPACE}.svc.cluster.local:5432/agentvault"
            ),
            "HELPER_SECRET_CACHE_REDIS_URL": "redis://127.0.0.1:6379/0",
            "HELPER_SECRET_WALLET_KEK_V1": self.environment.get(
                "E2E_AGENTVAULT_WALLET_KEK",
                "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
            ),
            "HELPER_SECRET_RECOVERY_KEY_V1": self.environment.get(
                "E2E_AGENTVAULT_RECOVERY_KEY",
                "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI",
            ),
            "HELPER_SECRET_APPROVAL_JWT_KEY_V1": self.environment.get(
                "E2E_AGENTVAULT_APPROVAL_KEY",
                "AwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwM",
            ),
            "HELPER_SECRET_MQTT_PASSWORD": self.environment["E2E_AGENTVAULT_MQTT_PASSWORD"],
            "AUTHGUARD__AUTHZ__SCOPE_DELIVERY__DIRECT_CONTEXT_HMAC_KEY": "agentvault-e2e-authguard-disabled-key",
            **wallet_values,
        }
        self._apply_secret(AGENTVAULT_NAMESPACE, "agentvault-secrets", agentvault_secret)
        resource_fixture = CONFIG_ROOT / "agentvault" / "init.import.yaml"
        resource_manifest = resource_fixture.read_text(encoding="utf-8")
        if payee_language is not None:
            if payee_language not in LANGUAGE_CHART_NAMES:
                raise ValueError(f"unsupported payee language: {payee_language}")
            payee_name = self._payee_name(payee_language)
            marketpulse_origin = f"http://{payee_name}.{PAYEE_NAMESPACE}.svc.cluster.local:8081"
            resource_manifest = ConsoleResourceFixture.render_agentvault_resources(resource_fixture, marketpulse_origin)
        self._apply_config_map(
            AGENTVAULT_NAMESPACE,
            "agentvault-e2e-resources",
            {"resources.yaml": resource_manifest},
        )

        evm_root = self.context.deploy_root / "evm"
        self._apply_config_map(
            PAYMENT_NAMESPACE,
            "evm-fixture-script",
            {"deploy.sh": (evm_root / "deploy.sh").read_text(encoding="utf-8")},
        )
        self._apply_config_map(
            PAYMENT_NAMESPACE,
            "evm-fixture-artifacts",
            {
                path.name: path.read_text(encoding="utf-8")
                for path in sorted((evm_root / "artifacts").iterdir())
                if path.is_file()
            },
        )
        for manifest in SHARED_KUBERNETES_MANIFESTS:
            self._kubectl("apply", "--filename", str(manifest))

    def _helm_install(
        self,
        release: str,
        chart: Path,
        namespace: str,
        *,
        values: Sequence[Path] = (),
        set_strings: Mapping[str, str] | None = None,
        wait_for_jobs: bool = False,
        wait: bool = True,
    ) -> str:
        arguments = [
            "upgrade",
            "--install",
            release,
            str(chart),
            "--namespace",
            namespace,
            "--timeout",
            f"{max(30, int(self.context.timeout_seconds))}s",
        ]
        if wait:
            arguments.append("--wait")
        if wait_for_jobs:
            arguments.append("--wait-for-jobs")
        for value_file in values:
            arguments.extend(("--values", str(value_file)))
        for key, value in sorted((set_strings or {}).items()):
            arguments.extend(("--set-string", f"{key}={value}"))
        return self._helm(*arguments, timeout=self.context.timeout_seconds + 30)

    def _component_chart(self, component: str) -> Path:
        return self.context.deploy_root / component / "helm"

    @staticmethod
    def _shared_overrides_for_chart(chart: Path) -> tuple[Path, ...]:
        component = chart.parent.name
        if component.startswith("mock-payee-marketpulse-"):
            return (MARKETPULSE_SHARED_VALUES,)
        if component.startswith("mock-payer-quantstream-service-"):
            return (QUANTSTREAM_SHARED_VALUES,)
        return ()

    def _install_dependencies(self, role: str, *, include_solana: bool) -> None:
        if role not in {"payer", "payee"}:
            raise ValueError(f"unsupported payment role: {role}")
        for release in ("postgres", "emqx", "jaeger"):
            self._helm_install(release, self._component_chart(release), INFRA_NAMESPACE)
        self._helm_install("anvil", self._component_chart("anvil"), PAYMENT_NAMESPACE)
        self._helm_install(
            "evm-fixture",
            self._component_chart("evm"),
            PAYMENT_NAMESPACE,
            wait_for_jobs=True,
        )
        if include_solana:
            self._helm_install("solana", self._component_chart("solana"), PAYMENT_NAMESPACE)
        if role == "payee":
            self._helm_install(
                "x402-facilitator",
                self._component_chart("facilitator"),
                PAYMENT_NAMESPACE,
            )

    def _install_agentvault(self) -> None:
        self._ensure_cluster_image(LOCAL_IMAGES["agentvault"])
        self._helm_install(
            "agentvault",
            AGENTVAULT_CHART,
            AGENTVAULT_NAMESPACE,
            values=(AGENTVAULT_VALUES,),
        )

    def _payee_chart(self, language: str) -> Path:
        chart_language = LANGUAGE_CHART_NAMES[language]
        return self._component_chart(f"mock-payee-marketpulse-{chart_language}-service")

    def _payer_chart(self, language: str) -> Path:
        chart_language = LANGUAGE_CHART_NAMES[language]
        return self._component_chart(f"mock-payer-quantstream-service-{chart_language}")

    def _payee_name(self, language: str) -> str:
        return f"marketpulse-{LANGUAGE_CHART_NAMES[language]}"

    def _payer_name(self, language: str) -> str:
        return f"quantstream-{LANGUAGE_CHART_NAMES[language]}"

    def _install_payee(self, language: str) -> None:
        self._ensure_cluster_image(LOCAL_IMAGES[f"payee-{language}"])
        name = self._payee_name(language)
        fqdn = f"{name}.{PAYEE_NAMESPACE}.svc.cluster.local"
        chart = self._payee_chart(language)
        self._helm_install(
            name,
            chart,
            PAYEE_NAMESPACE,
            values=self._shared_overrides_for_chart(chart),
            set_strings={
                "env.AGENTVAULT_TRANSPORT": self.context.transport,
                "env.AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:50051",
                "env.MARKETPULSE_RESOURCE_URL": f"http://{fqdn}:8081/mcp",
                "env.MARKETPULSE_ORIGIN": f"http://{fqdn}:8081",
            },
        )

    def _install_admin_ui(self, payer: str) -> None:
        self._ensure_cluster_image(LOCAL_IMAGES["admin-ui"])
        identity = payer_identity(payer)
        self._helm_install(
            "quantstream-ui",
            self._component_chart("mock-payer-quantstream-ui"),
            PAYER_NAMESPACE,
            set_strings={
                "env.QUANTSTREAM_CLIENT_ID": identity.client_id,
                "env.QUANTSTREAM_SCENARIO_ID": identity.scenario_id,
                "env.QUANTSTREAM_APPROVER_PRINCIPAL": identity.approver_principal,
                "env.AGENTVAULT_MQTT_APPROVER_USERNAME": identity.approver_mqtt_username,
            },
        )

    def _deploy_base(
        self,
        role: str,
        *,
        include_solana: bool,
        payee_language: str | None = None,
    ) -> None:
        self._apply_namespaces()
        self._bootstrap_manifests(payee_language)
        self._install_dependencies(role, include_solana=include_solana)
        with self._runtime_forwards(("emqx-mqtt", "emqx-dashboard")):
            self._bootstrap_emqx()

    @contextmanager
    def _forward_service(self, namespace: str, service: str, remote_port: int) -> Iterator[int]:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        local_port = int(listener.getsockname()[1])
        listener.close()
        command = self._privileged_command(
            self._kubectl_executable or "kubectl",
            "port-forward",
            "--address=127.0.0.1",
            "--namespace",
            namespace,
            f"service/{service}",
            f"{local_port}:{remote_port}",
        )
        process = subprocess.Popen(
            command,
            cwd=E2E_ROOT,
            env=self.environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if process.poll() is not None:
                stderr = process.communicate(timeout=5)[1]
                raise RuntimeError(f"kubectl port-forward exited early: {stderr[-2048:]}")
            try:
                with socket.create_connection(("127.0.0.1", local_port), timeout=0.5):
                    break
            except OSError:
                time.sleep(0.2)
        else:
            process.terminate()
            raise RuntimeError(f"timed out forwarding {namespace}/{service}:{remote_port}")
        try:
            yield local_port
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)

    @contextmanager
    def _runtime_forwards(self, names: Sequence[str]) -> Iterator[None]:
        environment_names = {
            "agentvault-api": "E2E_AGENTVAULT_PORT",
            "agentvault-management": "E2E_AGENTVAULT_MGMT_PORT",
            "anvil": "E2E_ANVIL_PORT",
            "tempo": "E2E_TEMPO_RPC_PORT",
            "solana": "E2E_SOLANA_RPC_PORT",
            "facilitator": "E2E_FACILITATOR_PORT",
            "emqx-mqtt": "E2E_EMQX_MQTT_PORT",
            "emqx-dashboard": "E2E_EMQX_DASHBOARD_PORT",
            "jaeger": "E2E_JAEGER_UI_PORT",
            "admin-ui": "E2E_QUANTSTREAM_UI_PORT",
        }
        previous: dict[str, str | None] = {}
        with ExitStack() as stack:
            for name in names:
                if name.startswith("payee:"):
                    language = name.partition(":")[2]
                    namespace, service, remote_port = PAYEE_NAMESPACE, self._payee_name(language), 8081
                    environment_name = "E2E_MARKETPULSE_PORT"
                else:
                    namespace, service, remote_port = SERVICE_NAMES[name]
                    environment_name = environment_names[name]
                port = stack.enter_context(self._forward_service(namespace, service, remote_port))
                previous[environment_name] = self.environment.get(environment_name)
                self.environment[environment_name] = str(port)
            try:
                yield
            finally:
                for key, value in previous.items():
                    if value is None:
                        self.environment.pop(key, None)
                    else:
                        self.environment[key] = value

    def smoke(self, role: str) -> str:
        self.cleanup()
        self._deploy_base(role, include_solana=True)
        forwards = ["anvil", "solana", "emqx-dashboard", "jaeger"]
        if role == "payee":
            forwards.append("facilitator")
        with self._runtime_forwards(forwards):
            self._wait_rpc(self._url("E2E_ANVIL_PORT"), "eth_chainId", "0x7a69", 10)
            self._wait_rpc(self._url("E2E_SOLANA_RPC_PORT"), "getHealth", "ok", 10)
            self._wait_http(self._url("E2E_JAEGER_UI_PORT"), 10)
            if role == "payee":
                self._facilitator_capabilities()
        return f"Kubernetes dependency {role} smoke passed; Anvil, Solana, PostgreSQL, EMQX and Jaeger are healthy"

    def _solana_command(self, executable: str, *arguments: str, timeout: float = 60) -> str:
        if executable not in {"solana", "solana-keygen", "spl-token"}:
            raise RuntimeError(f"unsupported Solana fixture command: {executable}")
        return self._kubectl(
            "exec",
            "--namespace",
            PAYMENT_NAMESPACE,
            "deployment/solana",
            "--",
            f"{SOLANA_CLI_ROOT}/{executable}",
            *arguments,
            timeout=timeout,
        ).strip()

    def _solana_keypair_json(self, keypair_path: str) -> str:
        output = self._kubectl(
            "exec",
            "--namespace",
            PAYMENT_NAMESPACE,
            "deployment/solana",
            "--",
            "/bin/cat",
            keypair_path,
        ).strip()
        try:
            keypair = json.loads(output)
        except json.JSONDecodeError as error:
            raise RuntimeError("Solana fixture keypair is not valid JSON") from error
        if (
            not isinstance(keypair, list)
            or len(keypair) != 64
            or any(
                not isinstance(value, int)
                or isinstance(value, bool)
                or value < 0
                or value > 255
                for value in keypair
            )
        ):
            raise RuntimeError("Solana fixture keypair has an invalid byte representation")
        return json.dumps(keypair, separators=(",", ":"))

    def _sql(self, payer: str, payee: str, query: str) -> str:
        del payer, payee
        pod = self._kubectl(
            "get",
            "pods",
            "--namespace",
            INFRA_NAMESPACE,
            "--selector",
            "app.kubernetes.io/name=postgres",
            "--output=jsonpath={.items[0].metadata.name}",
        ).strip()
        if not pod:
            raise RuntimeError("Kubernetes PostgreSQL pod was not found")
        return self._kubectl(
            "exec",
            "--namespace",
            INFRA_NAMESPACE,
            pod,
            "--",
            "sh",
            "-ec",
            'PGPASSWORD="$POSTGRES_PASSWORD" psql -U agentvault -d agentvault -At '
            '--set=ON_ERROR_STOP=1 --command "$1"',
            "agentvault-e2e-query",
            query,
        ).strip()

    def _wallet_addresses(self, payer: str, payee: str) -> dict[str, str]:
        del payer, payee
        if self._wallet_addresses_cache is not None:
            return self._wallet_addresses_cache
        wallet_keys = [
            (wallet_id, self.environment[private_key_env])
            for identity in PAYER_IDENTITIES.values()
            for wallet_id, private_key_env in (
                (identity.primary_wallet_id, identity.primary_private_key_env),
                (identity.secondary_wallet_id, identity.secondary_private_key_env),
            )
        ]
        output = self._kubectl(
            "exec",
            "--namespace",
            PAYMENT_NAMESPACE,
            "deployment/anvil",
            "--",
            "sh",
            "-eu",
            "-c",
            'for key in "$@"; do cast wallet address --private-key "$key"; done',
            "wallet-addresses",
            *(f"0x{key.removeprefix('0x')}" for _, key in wallet_keys),
        )
        addresses = re.findall(r"(?im)^\s*(0x[0-9a-f]{40})\s*$", output)
        if len(addresses) != len(wallet_keys) or len({item.lower() for item in addresses}) != len(wallet_keys):
            raise RuntimeError("the ten Kubernetes payer keys did not resolve to distinct EVM addresses")
        self._wallet_addresses_cache = {
            wallet_id: address
            for (wallet_id, _), address in zip(wallet_keys, addresses, strict=True)
        }
        return self._wallet_addresses_cache

    def _token_balances(self, payer: str, payee: str) -> dict[str, int]:
        addresses = {**self._wallet_addresses(payer, payee), "payee": PAYEE_ADDRESS}
        output = self._kubectl(
            "exec",
            "--namespace",
            PAYMENT_NAMESPACE,
            "deployment/anvil",
            "--",
            "sh",
            "-eu",
            "-c",
            (
                'token="$1"; shift; for address in "$@"; do '
                "cast call --rpc-url http://127.0.0.1:8545 \"$token\" "
                "'balanceOf(address)(uint256)' \"$address\"; done"
            ),
            "wallet-balances",
            TOKEN_CONTRACT,
            *addresses.values(),
        )
        values = [int(value) for value in re.findall(r"(?m)^\s*([0-9]+)(?:\s+\[[^\n]*\])?\s*$", output)]
        if len(values) != len(addresses):
            raise RuntimeError(f"expected {len(addresses)} Kubernetes token balances, got {len(values)}")
        return dict(zip(addresses, values, strict=True))

    def _audit(self, payer: str, payee: str) -> dict[str, Any]:
        del payee
        identity = payer_identity(payer)
        output = self._sql(
            payer,
            "",
            "SELECT run_id,"
            "max(CASE WHEN direction='outbound' THEN id END),"
            "max(CASE WHEN direction='inbound' THEN id END) "
            "FROM pay_intent WHERE status='settled' AND ("
            f"(direction='outbound' AND scenario_id='{identity.scenario_id}') OR "
            "(direction='inbound' AND scenario_id='scenario-marketpulse-feed')) "
            "GROUP BY run_id HAVING count(*) FILTER (WHERE direction='outbound')=1 "
            "AND count(*) FILTER (WHERE direction='inbound')=1 ORDER BY min(created_at);",
        )
        rows = [line.split("|") for line in output.splitlines() if line.strip()]
        if len(rows) != 2 or any(len(row) != 3 or not all(row) for row in rows):
            raise RuntimeError(f"Kubernetes payment audit did not return two bidirectional runs: {output!r}")
        return {
            "payer_language": payer,
            "payee_language": self.context.payee_language,
            "run_ids": [row[0] for row in rows],
            "outbound_intent_ids": [row[1] for row in rows],
            "inbound_intent_ids": [row[2] for row in rows],
            "free_deliveries": 2,
            "paid_deliveries": 2,
            "merchant_delivery_status": "delivered",
            "post_settlement_failure_recoveries": 1,
        }

    def _deployment_logs(self, namespace: str, deployment: str, container: str | None = None) -> str:
        arguments = ["logs", "--namespace", namespace, f"deployment/{deployment}", "--tail=5000"]
        if container is not None:
            arguments.extend(("--container", container))
        return self._kubectl(*arguments)

    def _job_logs(self, job: str) -> str:
        return self._kubectl(
            "logs",
            "--namespace",
            PAYER_NAMESPACE,
            f"job/{job}",
            "--tail=5000",
        )

    def _service_logs(self, payer: str, payee: str, payer_logs: str) -> dict[str, str]:
        return {
            "agentvault": self._deployment_logs(AGENTVAULT_NAMESPACE, "agentvault", "agentvault"),
            "payer": payer_logs,
            "payee": self._deployment_logs(PAYEE_NAMESPACE, self._payee_name(payee), self._payee_name(payee)),
            "facilitator": self._deployment_logs(PAYMENT_NAMESPACE, "x402-facilitator"),
        }

    def _uninstall_release(self, namespace: str, release: str) -> None:
        completed = execute(
            self._privileged_command(
                self._helm_executable or "helm",
                "uninstall",
                release,
                "--namespace",
                namespace,
                "--ignore-not-found",
                "--wait",
                "--timeout=30s",
            ),
            E2E_ROOT,
            self.environment,
            40,
        )
        if completed.returncode != 0:
            raise RuntimeError(f"could not reset Helm release {namespace}/{release}:\n{completed.stdout}")

    def _payer_values(self, payer: str, payee: str) -> dict[str, str]:
        identity = payer_identity(payer)
        payee_service = self._payee_name(payee)
        return {
            "env.MARKETPULSE_URL": (
                f"http://{payee_service}.{PAYEE_NAMESPACE}.svc.cluster.local:8081/mcp"
            ),
            "env.QUANTSTREAM_CLIENT_ID": identity.client_id,
            "env.QUANTSTREAM_SCENARIO_ID": identity.scenario_id,
            "env.AGENTVAULT_TRANSPORT": self.context.transport,
            "env.AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:50051",
            "env.AGENTVAULT_GRPC_TLS": "false",
            "env.AGENTVAULT_GRPC_TIMEOUT_SECONDS": "15",
            "env.AGENTVAULT_MQTT_HOST": "127.0.0.1",
            "env.AGENTVAULT_MQTT_PORT": "1883",
            "env.AGENTVAULT_MQTT_TLS": "false",
            "env.AGENTVAULT_MQTT_USERNAME": identity.mqtt_username,
            "env.AGENTVAULT_MQTT_TIMEOUT_SECONDS": "15",
            "env.AGENTVAULT_APPROVAL_TIMEOUT_SECONDS": "45",
        }

    def _start_payer_job(self, payer: str, payee: str, *, failure_once: bool) -> str:
        self._ensure_cluster_image(LOCAL_IMAGES[f"payer-{payer}"])
        job = self._payer_name(payer)
        self._uninstall_release(PAYER_NAMESPACE, job)
        self._kubectl(
            "delete",
            "job",
            job,
            "--namespace",
            PAYER_NAMESPACE,
            "--ignore-not-found",
            "--wait=true",
        )
        values = self._payer_values(payer, payee)
        if failure_once:
            values["env.QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT"] = "once"
        chart = self._payer_chart(payer)
        self._helm_install(
            job,
            chart,
            PAYER_NAMESPACE,
            values=self._shared_overrides_for_chart(chart),
            set_strings=values,
            wait=False,
        )
        return job

    def _wait_job(self, job: str, timeout_seconds: float = 180) -> str:
        completed = execute(
            self._privileged_command(
                self._kubectl_executable or "kubectl",
                "wait",
                "--namespace",
                PAYER_NAMESPACE,
                f"job/{job}",
                "--for=condition=complete",
                f"--timeout={int(timeout_seconds)}s",
            ),
            E2E_ROOT,
            self.environment,
            timeout_seconds + 10,
        )
        logs = self._job_logs(job)
        if completed.returncode != 0:
            raise RuntimeError(f"Kubernetes payer Job {job} did not complete:\n{completed.stdout}\n{logs}")
        return logs

    def _job_terminal_evidence(self, job: str) -> str | None:
        completed = execute(
            self._privileged_command(
                self._kubectl_executable or "kubectl",
                "get",
                "job",
                job,
                "--namespace",
                PAYER_NAMESPACE,
                "--output=json",
            ),
            E2E_ROOT,
            self.environment,
            15,
        )
        if completed.returncode != 0:
            return None
        status = json.loads(completed.stdout).get("status", {})
        if int(status.get("failed", 0) or 0) <= 0:
            return None
        return f"failed={status.get('failed')} logs={self._job_logs(job)[-4096:]!r}"

    def _wait_and_approve_job(self, payer: str, payee: str, job: str) -> str:
        token = self.environment.get(
            "E2E_QUANTSTREAM_UI_TOKEN",
            "quantstream-e2e-ui-token-0000000000000000001",
        )
        endpoint = self._url("E2E_QUANTSTREAM_UI_PORT", "/api/approvals")
        approval: dict[str, Any] | None = None
        last_status = 0
        last_raw = ""
        stopped_evidence: str | None = None
        for _ in range(60):
            status, _, raw = self._http(endpoint, headers={"authorization": f"Bearer {token}"})
            last_status, last_raw = status, raw
            if status == 200:
                items = json.loads(raw).get("items", [])
                if len(items) == 1:
                    approval = items[0]
                    break
            stopped_evidence = self._job_terminal_evidence(job)
            if stopped_evidence is not None:
                break
            time.sleep(1)
        if approval is None or approval.get("requestedAmount") != "1000000":
            pending = self._sql(
                payer,
                payee,
                "SELECT count(*),coalesce(string_agg(id || ':' || status, ',' ORDER BY id),'') "
                "FROM pay_approval WHERE status IN ('pending','approved');",
            )
            raise RuntimeError(
                "Kubernetes Admin UI did not expose the expected approval; "
                f"status={last_status} response={last_raw[:2048]!r} database={pending!r} "
                f"payer_job={stopped_evidence or 'still running'}"
            )
        approval_id = str(approval.get("approvalId", ""))
        intent_id = str(approval.get("intentId", ""))
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}", approval_id) or not re.fullmatch(
            r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,255}", intent_id
        ):
            raise RuntimeError("Kubernetes Admin UI returned unsafe approval identifiers")
        status, _, raw = self._http(
            f"{endpoint}/{approval_id}/decision",
            "POST",
            b'{"decision":"approve"}',
            {"authorization": f"Bearer {token}", "content-type": "application/json"},
        )
        if status not in {200, 201}:
            raise RuntimeError(f"Kubernetes Admin UI approval failed with HTTP {status}: {raw[:1024]}")
        logs = self._wait_job(job)
        return f"approval {approval_id} for intent {intent_id} was consumed through the Kubernetes Admin UI\n{logs}"

    def _diagnostics(self, payer: str, payee: str) -> None:
        LOGGER.error("collecting Kubernetes diagnostics payer=%s payee=%s", payer, payee)
        for namespace in (
            AGENTVAULT_NAMESPACE,
            INFRA_NAMESPACE,
            PAYMENT_NAMESPACE,
            PAYEE_NAMESPACE,
            PAYER_NAMESPACE,
        ):
            completed = execute(
                self._privileged_command(
                    self._kubectl_executable or "kubectl",
                    "get",
                    "pods,jobs",
                    "--namespace",
                    namespace,
                    "--output=wide",
                ),
                E2E_ROOT,
                self.environment,
                15,
            )
            LOGGER.error("namespace=%s\n%s", namespace, completed.stdout[-4096:])

    def full(
        self,
        payer_language: str,
        payee_language: str,
        *,
        clean_start: bool = True,
        render_config: bool = True,
    ) -> dict[str, str]:
        if payer_language not in LANGUAGES or payee_language not in LANGUAGES:
            raise ValueError("unsupported payer/payee language")
        if self.context.transport not in TRANSPORTS:
            raise ValueError("unsupported AgentVault signing transport")
        if render_config:
            self.render()
        if clean_start:
            self.cleanup()
        try:
            self._build_images(payer_language, payee_language)
            self._deploy_base("payee", include_solana=False, payee_language=payee_language)
            self._install_agentvault()
            self._install_payee(payee_language)
            self._install_admin_ui(payer_language)
            forward_names = (
                "anvil",
                "facilitator",
                "agentvault-api",
                "agentvault-management",
                f"payee:{payee_language}",
                "admin-ui",
            )
            with self._runtime_forwards(forward_names):
                self._wait_http(self._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 30)
                self._wait_http(self._url("E2E_MARKETPULSE_PORT", "/healthz"), 30)
                self._wait_http(self._url("E2E_QUANTSTREAM_UI_PORT", "/healthz"), 30)
                preflight = self._preflight_mcp()
                initial_balances = self._token_balances(payer_language, payee_language)
                initial_facilitator_native = self._native_balance(FACILITATOR_ADDRESS)

                automatic_job = self._start_payer_job(
                    payer_language,
                    payee_language,
                    failure_once=True,
                )
                automatic_logs = self._wait_job(automatic_job)
                time.sleep(2)
                approval_job = self._start_payer_job(
                    payer_language,
                    payee_language,
                    failure_once=False,
                )
                approval_logs = self._wait_and_approve_job(
                    payer_language,
                    payee_language,
                    approval_job,
                )

                audit = self._audit(payer_language, payee_language)
                outbound = list(audit["outbound_intent_ids"])
                inbound = list(audit["inbound_intent_ids"])
                financial = self._wait_financial_evidence(
                    payer_language,
                    payee_language,
                    [*outbound, *inbound],
                )
                identity = payer_identity(payer_language)
                for intent in outbound:
                    self._receipt(identity.scenario_id, intent, "outbound")
                for intent in inbound:
                    self._receipt("scenario-marketpulse-feed", intent, "inbound")
                durable = self._assert_durable_invariants(payer_language, payee_language)
                final_balances = self._token_balances(payer_language, payee_language)
                accounting = self._settlement_accounting(
                    payer_language,
                    payee_language,
                    [*outbound, *inbound],
                    initial_facilitator_native,
                )
                balance_sync = self._wait_balance_snapshots(
                    payer_language,
                    payee_language,
                    final_balances,
                )
                evidence = FullFlowEvidence(
                    payer_language=payer_language,
                    payee_language=payee_language,
                    transport=self.context.transport,
                    audit=audit,
                    initial_balances=initial_balances,
                    final_balances=final_balances,
                    chain_accounting=accounting,
                    logs=self._service_logs(
                        payer_language,
                        payee_language,
                        automatic_logs + "\n" + approval_logs,
                    ),
                    runtime={"facilitator": self._facilitator_capabilities()},
                )
                from verifier.payment import (
                    s21_payer_signing,
                    s22_payee_settlement,
                    s23_balance_sync,
                    s24_gateway_balance,
                )

                return {
                    "s20_full.md": "\n".join(
                        (
                            f"payer={payer_language} payee={payee_language} "
                            f"transport={self.context.transport} deployer=kubernetes",
                            preflight,
                            financial,
                            durable,
                            json.dumps(audit, sort_keys=True),
                        )
                    ),
                    "s21_payer_signing.md": s21_payer_signing.verify_evidence(evidence),
                    "s22_payee_settlement.md": s22_payee_settlement.verify_evidence(evidence),
                    "s23_balance_sync.md": s23_balance_sync.verify_evidence(evidence, balance_sync),
                    "s24_gateway_balance.md": s24_gateway_balance.verify_evidence(evidence),
                }
        except BaseException:
            self._diagnostics(payer_language, payee_language)
            raise

    def _run_live_rust_test(self, test_name: str) -> str:
        self.environment.setdefault("CARGO_INCREMENTAL", "0")
        self.environment.setdefault("CARGO_PROFILE_TEST_DEBUG", "0")
        self._target_budget()
        return self._run(
            [
                "cargo",
                "test",
                "--locked",
                test_name,
                "--",
                "--ignored",
                "--exact",
                "--nocapture",
            ],
            self.context.timeout_seconds,
            REPOSITORY_ROOT,
        )

    def mpp_tempo(self) -> str:
        self.cleanup()
        self._apply_namespaces()
        self._helm_install("tempo", self._component_chart("tempo"), PAYMENT_NAMESPACE)
        with self._runtime_forwards(("tempo",)):
            rpc_url = self._url("E2E_TEMPO_RPC_PORT")
            self._wait_rpc(rpc_url, "eth_chainId", "0x539", 90)
            self._wait_tempo_faucet_window(rpc_url)
            self.environment["TEMPO_RPC_URL"] = rpc_url
            return self._run_live_rust_test(
                "protocol::mpp::tests::"
                "live_tempo_vault_handlers_recover_once_and_reconcile_both_ledgers"
            )

    def x402_solana(self) -> str:
        self.cleanup()
        self._apply_namespaces()
        self._helm_install("solana", self._component_chart("solana"), PAYMENT_NAMESPACE)
        with self._runtime_forwards(("solana",)):
            rpc_url = self._url("E2E_SOLANA_RPC_PORT")
            self._wait_rpc(rpc_url, "getHealth", "ok", 120)

            keypairs = {
                role: f"/tmp/agentvault-x402-{role}.json"
                for role in ("payer", "recipient", "facilitator", "mint")
            }
            for path in keypairs.values():
                self._solana_command(
                    "solana-keygen",
                    "new",
                    "--silent",
                    "--no-bip39-passphrase",
                    "--force",
                    "--outfile",
                    path,
                )

            payer = self._solana_pubkey(keypairs["payer"])
            recipient = self._solana_pubkey(keypairs["recipient"])
            facilitator = self._solana_pubkey(keypairs["facilitator"])
            mint = self._solana_pubkey(keypairs["mint"])
            container_rpc = "http://127.0.0.1:8899"
            self._solana_command(
                "solana",
                "airdrop",
                "10",
                facilitator,
                "--url",
                container_rpc,
                timeout=120,
            )
            created = self._solana_json(
                "spl-token",
                "create-token",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                payer,
                "--decimals",
                "6",
                "--output",
                "json-compact",
                keypairs["mint"],
                timeout=120,
            )
            command_output = created.get("commandOutput")
            if not isinstance(command_output, dict) or command_output.get("address") != mint:
                raise RuntimeError("SPL token fixture created an unexpected mint")

            for owner in (payer, recipient):
                self._solana_json(
                    "spl-token",
                    "create-account",
                    "--url",
                    container_rpc,
                    "--fee-payer",
                    keypairs["facilitator"],
                    "--owner",
                    owner,
                    "--output",
                    "json-compact",
                    mint,
                    timeout=120,
                )
            payer_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                payer,
            )
            recipient_account_wire = self._solana_json(
                "spl-token",
                "address",
                "--url",
                container_rpc,
                "--verbose",
                "--output",
                "json-compact",
                "--token",
                mint,
                "--owner",
                recipient,
            )
            payer_account = payer_account_wire.get("associatedTokenAddress")
            recipient_account = recipient_account_wire.get("associatedTokenAddress")
            if not all(
                isinstance(value, str)
                and re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{32,44}", value)
                for value in (payer_account, recipient_account)
            ):
                raise RuntimeError("SPL token fixture emitted an invalid associated token account")

            self._solana_json(
                "spl-token",
                "mint",
                "--url",
                container_rpc,
                "--fee-payer",
                keypairs["facilitator"],
                "--mint-authority",
                keypairs["payer"],
                "--output",
                "json-compact",
                mint,
                "1000",
                payer_account,
                timeout=120,
            )
            self._wait_solana_token_balance(payer_account, 1_000_000_000)
            self._wait_solana_token_balance(recipient_account, 0)

            genesis = self._solana_rpc_result("getGenesisHash", [])
            if not isinstance(genesis, str) or not re.fullmatch(
                r"[1-9A-HJ-NP-Za-km-z]{32,88}", genesis
            ):
                raise RuntimeError("Solana validator emitted an invalid genesis hash")
            self.environment.update(
                {
                    "SOLANA_RPC_URL": rpc_url,
                    "SOLANA_CHAIN_REFERENCE": genesis[:32],
                    "SOLANA_PAYER_KEYPAIR_JSON": self._solana_keypair_json(keypairs["payer"]),
                    "SOLANA_FACILITATOR_KEYPAIR_JSON": self._solana_keypair_json(
                        keypairs["facilitator"]
                    ),
                    "SOLANA_PAYER_TOKEN_ACCOUNT": payer_account,
                    "SOLANA_RECIPIENT_TOKEN_ACCOUNT": recipient_account,
                    "SOLANA_RECIPIENT_ADDRESS": recipient,
                    "SOLANA_TOKEN_MINT": mint,
                }
            )
            return self._run_live_rust_test(
                "protocol::x402::tests::"
                "live_solana_x402_vault_handlers_reconcile_both_ledgers"
            )

    def matrix(self) -> dict[str, str]:
        RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
        scope = self.context.e2e_sdk_scope
        languages = matrix_languages(scope)
        selected_cases = matrix_cases(scope)
        total = len(selected_cases)
        _ensure_matrix_plan(self.context.report_root / "payment" / "matrix-plan.json", scope)
        progress_path = RUNTIME_ROOT / "matrix-progress.log"
        progress_path.write_text(
            f"matrix scope={scope} 0/{total} initialized backend=kubernetes\n",
            encoding="utf-8",
        )

        def progress(message: str) -> None:
            print(message, flush=True)
            with progress_path.open("a", encoding="utf-8") as stream:
                stream.write(message + "\n")

        def checkpoint(case_number: int, name: str, outputs: dict[str, str]) -> None:
            root = self.context.report_root / "payment" / "matrix-cases"
            root.mkdir(parents=True, exist_ok=True)
            target = root / f"case-{case_number:02d}.json"
            temporary = target.with_suffix(".tmp")
            temporary.write_text(
                json.dumps(
                    {"case": name, "reports": outputs},
                    ensure_ascii=False,
                    sort_keys=True,
                ),
                encoding="utf-8",
            )
            temporary.replace(target)

        completed: list[tuple[int, str, dict[str, str]]] = []
        pending: list[tuple[int, str, str, str]] = []
        for case_number, case in enumerate(selected_cases, 1):
            name = case.name
            target = (
                self.context.report_root
                / "payment"
                / "matrix-cases"
                / f"case-{case_number:02d}.json"
            )
            outputs = _load_matrix_checkpoint(target, name)
            if outputs is None:
                pending.append((case_number, case.transport, case.payer, case.payee))
            else:
                completed.append((case_number, name, outputs))

        progress(
            f"matrix scope={scope} backend=kubernetes resume: "
            f"restored {len(completed)}/{total} validated checkpoints"
        )
        if self.context.matrix_workers != 1:
            progress(
                "matrix backend=kubernetes uses one shared cluster; "
                "cases run serially despite --matrix-workers"
            )

        if pending:
            self.cleanup()
            self._build_matrix_images(languages)
        for case_number, transport, payer, payee in pending:
            name = f"payer={payer} payee={payee} transport={transport}"
            progress(f"matrix scope={scope} case {case_number}/{total} start: {name}")
            case_context = replace(
                self.context,
                payer_language=payer,
                payee_language=payee,
                transport=transport,
                skip_build=True,
                reuse_build=True,
                matrix_workers=1,
                worker_slot=None,
            )
            case_deployer = KubernetesE2EDeployer(case_context)
            outputs = case_deployer.full(
                payer,
                payee,
                clean_start=True,
                render_config=False,
            )
            checkpoint(case_number, name, outputs)
            completed.append((case_number, name, outputs))
            progress(f"matrix scope={scope} case {case_number}/{total} passed: {name}")

        completed.sort(key=lambda item: item[0])
        if len(completed) != total:
            raise RuntimeError(
                f"Kubernetes SDK matrix scope={scope} completed only "
                f"{len(completed)} of {total} cases"
            )
        details: dict[str, list[str]] = {
            "s21_payer_signing.md": [],
            "s22_payee_settlement.md": [],
            "s23_balance_sync.md": [],
            "s24_gateway_balance.md": [],
        }
        passed: list[str] = []
        for _, name, outputs in completed:
            passed.append(name)
            for report, entries in details.items():
                entries.append(f"[{name}]\n{outputs[report]}")
        return {
            "s30_matrix.md": (
                f"scope={scope} languages={','.join(languages)} "
                f"transports={','.join(TRANSPORTS)} deployer=kubernetes\n"
                f"passed {total}/{total} isolated cases\n\n"
                + "\n".join(passed)
            ),
            **{report: "\n\n".join(entries) for report, entries in details.items()},
        }
