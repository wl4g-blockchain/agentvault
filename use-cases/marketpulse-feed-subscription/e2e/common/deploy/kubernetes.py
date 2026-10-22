from __future__ import annotations

import json
import logging
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

from common.process import execute
from common.project import E2E_ROOT


LOGGER = logging.getLogger("agentvault.e2e.runner.kubernetes")
NAMESPACE_MANIFEST = E2E_ROOT / "deploy" / "namespaces.yaml"
NAMESPACE_PREFIX = "e2e-agentvault-"
NAMESPACE_SELECTOR = "app.kubernetes.io/part-of=agentvault-e2e"
INSTANCE_LABEL = "app.kubernetes.io/instance"
FINALIZER_PATCH = '{"metadata":{"finalizers":[]}}'
DEFAULT_KUBECONFIG = Path("/etc/rancher/k3s/k3s.yaml")
RESOURCE_REFERENCE = re.compile(r"^[a-z0-9][a-z0-9.-]*/[A-Za-z0-9_.-]+$")


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
