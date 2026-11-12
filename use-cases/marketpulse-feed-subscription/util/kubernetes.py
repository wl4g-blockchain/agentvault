from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO, Callable, Mapping, Sequence

import yaml

from util.constant import E2EOtherDefinitions as Project
from util.docker import E2EDockerDeployer
from util.images import E2EImagesDefinitions
from util.process import E2EProcessHelper

MANUAL_IMAGE_PIN = "io.cri-containerd.pinned"
MANUAL_IMAGE_PIN_OWNER = "io.agentvault.manual-image-pin"
LOGGER = logging.getLogger("agentvault.e2e.runner.deployer")


NAMESPACE_MANIFEST = Project.AGENTVAULT_CONFIG / "namespaces.yaml"
NAMESPACE_PREFIX = "e2e-agentvault-"
NAMESPACE_SELECTOR = "app.kubernetes.io/part-of=agentvault-e2e"
BUSINESS_EVIDENCE_SELECTOR = (
    NAMESPACE_SELECTOR + ",agentvault.io/e2e-business-evidence=true"
)
INSTANCE_LABEL = "app.kubernetes.io/instance"
FINALIZER_PATCH = '{"metadata":{"finalizers":[]}}'
RESOURCE_REFERENCE = re.compile(r"^[a-z0-9][a-z0-9.-]*/[A-Za-z0-9_.-]+$")


class E2EKubernetesCleaner:
    """Uninstall only Helm releases in manifest-declared AgentVault E2E namespaces."""

    @dataclass(frozen=True, order=True)
    class HelmRelease:
        namespace: str
        name: str

    def __init__(
        self,
        timeout_seconds: float,
        *,
        kubeconfig: str | Path | None = None,
        kubectl: str | None = None,
        helm: str | None = None,
        namespace_manifest: Path = NAMESPACE_MANIFEST,
        namespace_prefix: str = NAMESPACE_PREFIX,
        namespace_selector: str = NAMESPACE_SELECTOR,
        evidence_selector: str = BUSINESS_EVIDENCE_SELECTOR,
    ) -> None:
        if timeout_seconds <= 0:
            raise ValueError("timeout_seconds must be positive")
        self.namespace_manifest = namespace_manifest
        self.namespace_prefix = namespace_prefix
        self.namespace_selector = namespace_selector
        self.evidence_selector = evidence_selector
        configured = kubeconfig or E2EKubernetesDeployer.LocalImageLoader.kubeconfig()
        self._kubeconfig = configured
        self._kubectl = kubectl or shutil.which("kubectl")
        self._helm = helm or shutil.which("helm")
        self._command_timeout = max(5.0, min(30.0, timeout_seconds))
        self._resource_types: tuple[str, ...] | None = None

    def cleanup(self) -> str:
        if not any(
            path.is_file()
            for path in E2EKubernetesDeployer.LocalImageLoader.config_files(
                self._kubeconfig
            )
        ):
            message = (
                f"Kubernetes cleanup skipped; kubeconfig not found: {self._kubeconfig}"
            )
            LOGGER.info(message)
            return message
        if self._kubectl is None or self._helm is None:
            raise RuntimeError("Kubernetes cleanup requires both kubectl and helm")

        namespaces = self._target_namespaces()
        if not namespaces:
            message = "Kubernetes cleanup found no labelled AgentVault E2E namespaces"
            LOGGER.info(message)
            return "\n".join([message, *self._release_manual_image_pins()])

        output: list[str] = self._remove_manual_workloads(namespaces)
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
            retry = self._helm_command(
                [*self._uninstall_args(release, wait=False), "--no-hooks"]
            )
            output.append(retry.stdout)
            if retry.returncode != 0 and release in self._releases(namespaces):
                output.extend(self._remove_helm_records(release))

        output.extend(self._remove_business_evidence(namespaces, "jobs"))
        self._wait_for_pods(namespaces)
        output.extend(self._recover_remaining_pods(namespaces))
        output.extend(
            self._remove_business_evidence(namespaces, "persistentvolumeclaims")
        )

        remaining_releases = self._releases(namespaces)
        remaining_pods = self._pods(namespaces)
        if remaining_releases or remaining_pods:
            raise RuntimeError(
                "Kubernetes E2E cleanup left releases "
                f"{remaining_releases} and pods {remaining_pods}"
            )
        output.extend(self._release_manual_image_pins())
        return "\n".join(part.strip() for part in output if part.strip())

    def _release_manual_image_pins(self) -> list[str]:
        environment = {**os.environ, "KUBECONFIG": str(self._kubeconfig)}
        command = E2EKubernetesDeployer.LocalImageLoader.command(
            self._kubeconfig, self._kubectl or "kubectl"
        )
        return E2EKubernetesDeployer.LocalImageLoader.detect(
            command, environment
        ).release_manual_pins()

    def _remove_manual_workloads(self, namespaces: tuple[str, ...]) -> list[str]:
        if not set(namespaces).issubset(self._declared_namespaces()):
            raise ValueError(
                "manual acceptance cleanup must remain inside declared E2E namespaces"
            )
        output = []
        for namespace in namespaces:
            arguments = [
                "deployment,service,cronjob",
                "--namespace",
                namespace,
                "--selector",
                "agentvault.io/manual-acceptance=true",
            ]
            result = self._kubectl_command(
                [
                    "delete",
                    *arguments,
                    "--ignore-not-found",
                    "--cascade=foreground",
                    "--wait=true",
                    "--timeout=60s",
                ],
                timeout_seconds=70,
            )
            if result.returncode != 0:
                raise RuntimeError(
                    f"could not remove manual acceptance controllers in {namespace}: {result.stdout}"
                )
            output.append(result.stdout)
            remaining = self._kubectl_command(
                ["get", *arguments, "--ignore-not-found", "--output=name"]
            )
            if remaining.returncode != 0 or remaining.stdout.strip():
                raise RuntimeError(
                    f"manual acceptance controllers remain in {namespace}: {remaining.stdout}"
                )
        return output

    def _remove_business_evidence(
        self, namespaces: tuple[str, ...], kind: str
    ) -> list[str]:
        if kind not in {"jobs", "persistentvolumeclaims"} or not set(
            namespaces
        ).issubset(self._declared_namespaces()):
            raise ValueError(
                "business evidence cleanup must remain inside declared E2E namespaces"
            )
        output = []
        for namespace in namespaces:
            result = self._kubectl_command(
                [
                    "delete",
                    kind,
                    "--namespace",
                    namespace,
                    "--selector",
                    self.evidence_selector,
                    "--ignore-not-found",
                    "--wait=true",
                    "--timeout=60s",
                ],
                timeout_seconds=70,
            )
            if result.returncode != 0:
                raise RuntimeError(
                    f"could not remove owned business evidence {namespace}/{kind}: {result.stdout}"
                )
            output.append(result.stdout)
            remaining = self._kubectl_command(
                [
                    "get",
                    kind,
                    "--namespace",
                    namespace,
                    "--selector",
                    self.evidence_selector,
                    "--ignore-not-found",
                    "--output=name",
                ]
            )
            if remaining.returncode != 0 or remaining.stdout.strip():
                raise RuntimeError(
                    f"owned business evidence remains in {namespace}/{kind}: {remaining.stdout}"
                )
        return output

    def _target_namespaces(self) -> tuple[str, ...]:
        declared = self._declared_namespaces()
        result = self._kubectl_command(
            [
                "get",
                "namespaces",
                "--selector",
                self.namespace_selector,
                "--output=json",
                "--request-timeout=5s",
            ],
            timeout_seconds=10,
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"could not inventory Kubernetes E2E namespaces:\n{result.stdout}"
            )
        items = self._json_items(result.stdout, "Kubernetes namespace inventory")
        discovered = {
            str(item.get("metadata", {}).get("name", ""))
            for item in items
            if isinstance(item, dict)
        }
        return tuple(sorted(declared & discovered))

    def _declared_namespaces(self) -> frozenset[str]:
        content = self.namespace_manifest.read_text(encoding="utf-8")
        names = frozenset(
            str(d["metadata"]["name"])
            for d in yaml.safe_load_all(content)
            if d and d.get("kind") == "Namespace"
        )
        if not names or any(
            not name.startswith(self.namespace_prefix) for name in names
        ):
            raise RuntimeError(
                "E2E namespace manifest contains no safe target namespaces"
            )
        return names

    def _releases(
        self, namespaces: tuple[str, ...]
    ) -> tuple[E2EKubernetesCleaner.HelmRelease, ...]:
        result = self._helm_command(
            ["list", "--all-namespaces", "--all", "--output=json"]
        )
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
                E2EKubernetesCleaner.HelmRelease(
                    namespace=str(item["namespace"]), name=str(item["name"])
                )
                for item in inventory
                if isinstance(item, dict)
                and item.get("namespace") in allowed
                and isinstance(item.get("name"), str)
            )
        )

    def _uninstall_args(
        self, release: E2EKubernetesCleaner.HelmRelease, *, wait: bool = True
    ) -> list[str]:
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

    def _recover_release(self, release: E2EKubernetesCleaner.HelmRelease) -> list[str]:
        output: list[str] = []
        selector = f"{INSTANCE_LABEL}={release.name}"
        resource_types = [
            kind for kind in self._namespaced_resource_types() if kind != "pods"
        ]
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
            [
                "api-resources",
                "--namespaced=true",
                "--verbs=list,delete",
                "--output=name",
            ]
        )
        if result.returncode != 0:
            raise RuntimeError(
                f"could not discover namespaced Kubernetes resources:\n{result.stdout}"
            )
        self._resource_types = tuple(
            sorted(
                {line.strip() for line in result.stdout.splitlines() if line.strip()}
            )
        )
        return self._resource_types

    def _remove_helm_records(
        self, release: E2EKubernetesCleaner.HelmRelease
    ) -> list[str]:
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
                raise RuntimeError(
                    f"could not inventory pods in {namespace}:\n{result.stdout}"
                )
            if not result.stdout.strip():
                continue
            for item in self._json_items(
                result.stdout, f"pod inventory for {namespace}"
            ):
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
            raise RuntimeError(
                f"could not delete {namespace}/{resource}:\n{deleted.stdout}"
            )
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
        command = E2EKubernetesDeployer.LocalImageLoader.command(
            self._kubeconfig, executable, *arguments
        )
        return E2EProcessHelper.execute(
            command,
            Project.E2E_ROOT,
            environment,
            timeout_seconds or self._command_timeout,
        )


class E2EKubernetesDeployer:
    """Core kubectl, Helm, and platform-verified local image import operations."""

    @dataclass
    class LocalImageLoader:
        platform: str
        docker: list[str]
        ctr: list[str]
        cri: list[str]
        environment: dict[str, str]
        emulated_platforms: tuple[str, ...] = ()

        @staticmethod
        def manifest_platforms(document: object) -> set[str]:
            result = set()
            if isinstance(document, dict):
                if document.get("os") and document.get("architecture"):
                    result.add(f"{document['os']}/{document['architecture']}")
                for key in ("platform", "Descriptor", "manifests"):
                    result.update(
                        E2EKubernetesDeployer.LocalImageLoader.manifest_platforms(
                            document.get(key)
                        )
                    )
            elif isinstance(document, list):
                for item in document:
                    result.update(
                        E2EKubernetesDeployer.LocalImageLoader.manifest_platforms(item)
                    )
            return result

        @property
        def shared_docker(self) -> bool:
            return not self.ctr

        @staticmethod
        def image_platform(
            docker: list[str],
            environment: dict[str, str],
            reference: str,
            platform: str | None = None,
        ) -> str | None:
            selection = ["--platform", platform] if platform else []
            result = E2EProcessHelper.execute(
                [
                    *docker,
                    "image",
                    "inspect",
                    *selection,
                    "--format={{.Os}}/{{.Architecture}}",
                    reference,
                ],
                Project.E2E_ROOT,
                environment,
                15,
            )
            return result.stdout.strip() if result.returncode == 0 else None

        def deployment_image(self, reference: str, timeout: float) -> str:
            """Load and pin one platform so Kubernetes cannot select a native ARM variant."""
            platform = E2EImagesDefinitions.reference_platform(
                reference, self.environment
            )
            self.validate_platform(platform, reference)
            pull = [*self.docker, "pull", "--platform", platform]
            inspect = [
                *self.docker,
                "image",
                "inspect",
                "--platform",
                platform,
                "--format={{json .}}",
                reference,
            ]
            pulled = False
            if (
                self.image_platform(self.docker, self.environment, reference, platform)
                != platform
            ):
                if E2EImagesDefinitions.is_local(reference):
                    raise RuntimeError(
                        f"local E2E image {reference} has no {platform} variant; rerun without --skip-build"
                    )
                E2EProcessHelper.run(
                    [*pull, reference], Project.E2E_ROOT, self.environment, timeout
                )
                pulled = True
            if E2EImagesDefinitions.is_local(reference):
                return (
                    reference  # E2E application builds produce one selected platform.
                )
            metadata = json.loads(
                E2EProcessHelper.run(inspect, Project.E2E_ROOT, self.environment, 30)
            )
            descriptor = metadata.get("Descriptor", {})
            if descriptor.get("mediaType") in {
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.docker.distribution.manifest.v2+json",
            }:
                pinned = (
                    E2EImagesDefinitions.repository(reference)
                    + "@"
                    + descriptor["digest"]
                )
                if (
                    self.image_platform(self.docker, self.environment, pinned, platform)
                    != platform
                ):
                    # Cached index children need their canonical digest registered.
                    # Refresh the tag first in case it was overwritten by a local build.
                    if not pulled:
                        E2EProcessHelper.run(
                            [*pull, reference],
                            Project.E2E_ROOT,
                            self.environment,
                            timeout,
                        )
                        metadata = json.loads(
                            E2EProcessHelper.run(
                                inspect, Project.E2E_ROOT, self.environment, 30
                            )
                        )
                        pinned = (
                            E2EImagesDefinitions.repository(reference)
                            + "@"
                            + metadata["Descriptor"]["digest"]
                        )
                    E2EProcessHelper.run(
                        [*pull, pinned], Project.E2E_ROOT, self.environment, timeout
                    )
                return pinned
            return reference  # Classic Docker stores only one platform per reference.

        @staticmethod
        def solana_image(
            docker: list[str], environment: dict[str, str], platform: str | None = None
        ) -> str:
            platform = E2EImagesDefinitions.image_platform("solana", environment)
            reference = E2EImagesDefinitions.reference("solana", environment)
            if (
                E2EKubernetesDeployer.LocalImageLoader.image_platform(
                    docker, environment, reference
                )
                != platform
            ):
                manifest = E2EProcessHelper.execute(
                    [*docker, "manifest", "inspect", "--verbose", reference],
                    Project.E2E_ROOT,
                    environment,
                    15,
                )
                try:
                    available = (
                        E2EKubernetesDeployer.LocalImageLoader.manifest_platforms(
                            json.loads(manifest.stdout)
                        )
                        if manifest.returncode == 0
                        else set()
                    )
                except ValueError:
                    available = set()
                if platform not in available:
                    raise RuntimeError(
                        f"No native {platform} Solana image is available: {reference}; "
                        f"available platforms={sorted(available)}. Emulation cannot provide io_uring."
                    )
            environment.update(E2E_SOLANA_IMAGE=reference, E2E_SOLANA_PLATFORM=platform)
            LOGGER.info(
                "Selected Solana image platform=%s reference=%s", platform, reference
            )
            return reference

        @staticmethod
        def kubeconfig(environment: Mapping[str, str] | None = None) -> str:
            environment = os.environ if environment is None else environment
            return environment.get("KUBECONFIG") or str(Path.home() / ".kube/config")

        @staticmethod
        def config_files(config: str | Path) -> list[Path]:
            return [
                Path(item).expanduser()
                for item in str(config).split(os.pathsep)
                if item
            ]

        @staticmethod
        def command(config: str | Path, executable: str, *arguments: str) -> list[str]:
            if any(
                path.is_file() and not os.access(path, os.R_OK)
                for path in E2EKubernetesDeployer.LocalImageLoader.config_files(config)
            ):
                return [
                    "sudo",
                    "-n",
                    "env",
                    f"KUBECONFIG={config}",
                    executable,
                    *arguments,
                ]
            return [executable, *arguments]

        @classmethod
        def detect(
            cls, kubectl: Sequence[str], environment: dict[str, str]
        ) -> E2EKubernetesDeployer.LocalImageLoader:
            requested = environment.get(
                "AGENTVAULT_E2E_KUBERNETES_IMAGE_LOADER", "auto"
            ).lower()
            if requested not in {
                "auto",
                "colima",
                "orbstack",
                "docker",
                "k3s",
                "containerd",
            }:
                raise RuntimeError("unsupported AGENTVAULT_E2E_KUBERNETES_IMAGE_LOADER")
            context = E2EProcessHelper.run(
                [*kubectl, "config", "current-context"],
                Project.E2E_ROOT,
                environment,
                30,
            ).strip()
            nodes = json.loads(
                E2EProcessHelper.run(
                    [*kubectl, "get", "nodes", "-o", "json", "--request-timeout=10s"],
                    Project.E2E_ROOT,
                    environment,
                    15,
                )
            )["items"]
            if len(nodes) != 1:
                raise RuntimeError("local Kubernetes E2E requires exactly one node")
            info = nodes[0]["status"]["nodeInfo"]
            platform = f"{info.get('operatingSystem', 'linux')}/{info['architecture']}"
            if platform not in ("linux/arm64", "linux/amd64"):
                raise RuntimeError(f"unsupported Kubernetes E2E platform: {platform}")
            runtime = info["containerRuntimeVersion"].split(":", 1)[0]
            vm = []
            if context == "colima" or context.startswith("colima-"):
                if requested not in {"auto", "colima"}:
                    raise RuntimeError(
                        "the active Colima context requires the colima image loader"
                    )
                profile = (
                    "default"
                    if context == "colima"
                    else context.removeprefix("colima-")
                )
                vm = ["colima", "-p", profile, "ssh", "--"]
            elif context.startswith("orbstack-"):
                if requested not in {"auto", "orbstack"}:
                    raise RuntimeError(
                        "the active OrbStack machine requires the orbstack image loader"
                    )
                vm = ["orb", "-m", context.removeprefix("orbstack-")]
            if context == "orbstack":
                if requested not in {"auto", "orbstack"}:
                    raise RuntimeError(
                        "the active OrbStack context requires the orbstack image loader"
                    )
                docker = ["docker", "--context", "orbstack"]
                E2EProcessHelper.run(
                    [*docker, "info"], Project.E2E_ROOT, environment, 15
                )
                return cls(platform, docker, [], [], environment)._detect_emulation()
            if not vm and requested in {"colima", "orbstack"}:
                raise RuntimeError(
                    f"the active Kubernetes context is not {requested}: {context}"
                )
            if vm and runtime == "docker":
                docker = ["docker", "--context", context]
                source_id = E2EProcessHelper.run(
                    [*docker, "info", "--format={{.ID}}"],
                    Project.E2E_ROOT,
                    environment,
                    15,
                ).strip()
                node_id = E2EProcessHelper.run(
                    [*vm, "sudo", "-n", "docker", "info", "--format={{.ID}}"],
                    Project.E2E_ROOT,
                    environment,
                    15,
                ).strip()
                if not source_id or source_id != node_id:
                    raise RuntimeError(
                        "Colima Kubernetes and the selected Docker context do not share an image store"
                    )
                return cls(platform, docker, [], [], environment)._detect_emulation(vm)
            if not vm:
                machine_id = Path("/etc/machine-id")
                if (
                    sys.platform != "linux"
                    or not machine_id.is_file()
                    or machine_id.read_text().strip() != info.get("machineID")
                ):
                    raise RuntimeError(
                        "selected Kubernetes node is not this host; select a Colima/OrbStack context or run on the node"
                    )
            if runtime not in ("docker", "containerd"):
                raise RuntimeError(
                    f"unsupported local Kubernetes runtime/context: {runtime}/{context}"
                )
            if requested == "docker" and runtime != "docker":
                raise RuntimeError(
                    "the selected Docker image loader requires a Docker Kubernetes runtime"
                )
            if requested in {"k3s", "containerd"} and runtime != "containerd":
                raise RuntimeError(
                    "the selected containerd image loader requires a containerd Kubernetes runtime"
                )
            docker = cls.docker_command(environment)
            if runtime == "docker":
                daemon = (
                    E2EProcessHelper.run(
                        [*docker, "info", "--format={{.Name}}"],
                        Project.E2E_ROOT,
                        environment,
                        15,
                    )
                    .strip()
                    .lower()
                )
                names = {
                    socket.gethostname().lower(),
                    nodes[0]["metadata"]["name"].lower(),
                }
                if daemon not in names:
                    raise RuntimeError(
                        "Docker daemon does not belong to the active local Kubernetes node"
                    )
                return cls(platform, docker, [], [], environment)._detect_emulation()
            privileged = [*vm, "sudo", "-n"] if vm or os.geteuid() != 0 else []
            ctr = [*privileged, "k3s", "ctr", "-n", "k8s.io"]
            cri = [*privileged, "k3s", "crictl"]
            if (
                requested == "containerd"
                or "k3s" not in info.get("kubeletVersion", "").lower()
                or (not vm and shutil.which("k3s") is None)
                or E2EProcessHelper.execute(
                    [*ctr, "version"], Project.E2E_ROOT, environment, 15
                ).returncode
            ):
                if requested == "k3s":
                    raise RuntimeError(
                        "the selected K3s image loader is not accessible"
                    )
                ctr = [
                    *privileged,
                    "ctr",
                    "--address",
                    "/run/containerd/containerd.sock",
                    "-n",
                    "k8s.io",
                ]
                cri = [
                    *privileged,
                    "crictl",
                    "--runtime-endpoint=unix:///run/containerd/containerd.sock",
                ]
                E2EProcessHelper.run(
                    [*ctr, "version"], Project.E2E_ROOT, environment, 15
                )
            return cls(platform, docker, ctr, cri, environment)._detect_emulation(vm)

        def _detect_emulation(
            self, vm: Sequence[str] = ()
        ) -> E2EKubernetesDeployer.LocalImageLoader:
            if self.platform != "linux/arm64":
                return self
            if vm or sys.platform == "linux":
                for handler in ("rosetta", "qemu-x86_64"):
                    status = E2EProcessHelper.execute(
                        [*vm, "cat", f"/proc/sys/fs/binfmt_misc/{handler}"],
                        Project.E2E_ROOT,
                        self.environment,
                        10,
                    )
                    if status.returncode == 0 and status.stdout.startswith("enabled\n"):
                        self.emulated_platforms = ("linux/amd64",)
                        break
            elif self.shared_docker:
                status = E2EProcessHelper.execute(
                    [*self.docker, "buildx", "inspect"],
                    Project.E2E_ROOT,
                    self.environment,
                    15,
                )
                if status.returncode == 0 and any(
                    line.strip().startswith("Platforms:") and "linux/amd64" in line
                    for line in status.stdout.splitlines()
                ):
                    self.emulated_platforms = ("linux/amd64",)
            return self

        docker_command = staticmethod(E2EDockerDeployer.detect)

        def compose(self) -> list[str]:
            requested = self.environment.get(
                "DOCKER_DEFAULT_PLATFORM", E2EImagesDefinitions.DEFAULT_PLATFORM
            )
            self.validate_platform(requested, "DOCKER_DEFAULT_PLATFORM")
            self.environment["DOCKER_DEFAULT_PLATFORM"] = requested
            if "--context" in self.docker:
                self.environment["DOCKER_CONTEXT"] = self.docker[
                    self.docker.index("--context") + 1
                ]
            return self.compose_command(self.docker, self.environment)

        compose_command = staticmethod(E2EDockerDeployer.compose_command)

        def validate_platform(self, platform: str, reference: str) -> None:
            if platform != self.platform and platform not in self.emulated_platforms:
                raise RuntimeError(
                    f"image platform mismatch for {reference}: node requires {self.platform}, got {platform}"
                )

        def image_references(self, *, label: tuple[str, str] | None = None) -> set[str]:
            if self.shared_docker:
                command = [
                    *self.docker,
                    "image",
                    "ls",
                    "--format={{.Repository}}:{{.Tag}}",
                ]
                if label:
                    command.extend(("--filter", f"label={label[0]}={label[1]}"))
            else:
                command = [*self.ctr, "images", "list", "--quiet"]
                if label:
                    command.append(f'labels."{label[0]}"=="{label[1]}"')
            return set(
                E2EProcessHelper.run(
                    command, Project.E2E_ROOT, self.environment, 30
                ).splitlines()
            )

        def pin_image(
            self, reference: str, namespace: str, apply: Callable, kubectl: Callable
        ) -> None:
            if self.shared_docker:
                # CRI-Docker has no containerd pin labels. A live, idle Pod keeps
                # the payer image in use without running a payment automatically.
                name = (
                    "image-keeper-"
                    + hashlib.sha256(reference.encode()).hexdigest()[:12]
                )
                labels = {
                    "agentvault.io/manual-acceptance": "true",
                    "app.kubernetes.io/name": name,
                }
                image_id = E2EProcessHelper.run(
                    [*self.docker, "image", "inspect", "--format={{.Id}}", reference],
                    Project.E2E_ROOT,
                    self.environment,
                    30,
                ).strip()
                apply(
                    {
                        "apiVersion": "apps/v1",
                        "kind": "Deployment",
                        "metadata": {
                            "name": name,
                            "namespace": namespace,
                            "labels": labels,
                        },
                        "spec": {
                            "replicas": 1,
                            "selector": {
                                "matchLabels": {"app.kubernetes.io/name": name}
                            },
                            "template": {
                                "metadata": {
                                    "labels": labels,
                                    "annotations": {"agentvault.io/image-id": image_id},
                                },
                                "spec": {
                                    "automountServiceAccountToken": False,
                                    "containers": [
                                        {
                                            "name": "image-keeper",
                                            "image": reference,
                                            "imagePullPolicy": "Never",
                                            "command": [
                                                "/bin/sh",
                                                "-c",
                                                "while :; do sleep 3600; done",
                                            ],
                                            "securityContext": {
                                                "runAsNonRoot": True,
                                                "runAsUser": 65532,
                                                "allowPrivilegeEscalation": False,
                                                "readOnlyRootFilesystem": True,
                                                "capabilities": {"drop": ["ALL"]},
                                            },
                                            "resources": {
                                                "requests": {
                                                    "cpu": "1m",
                                                    "memory": "4Mi",
                                                },
                                                "limits": {
                                                    "cpu": "10m",
                                                    "memory": "16Mi",
                                                },
                                            },
                                        }
                                    ],
                                },
                            },
                        },
                    }
                )
                kubectl(
                    "rollout",
                    "status",
                    f"deployment/{name}",
                    "--namespace",
                    namespace,
                    "--timeout=60s",
                    timeout=70,
                )
                return
            pinned = reference in self.image_references(
                label=(MANUAL_IMAGE_PIN, "pinned")
            )
            owned = reference in self.image_references(
                label=(MANUAL_IMAGE_PIN_OWNER, "true")
            )
            if not pinned or owned:
                E2EProcessHelper.run(
                    [
                        *self.ctr,
                        "images",
                        "label",
                        reference,
                        f"{MANUAL_IMAGE_PIN}=pinned",
                        f"{MANUAL_IMAGE_PIN_OWNER}=true",
                    ],
                    Project.E2E_ROOT,
                    self.environment,
                    30,
                )
            for _ in range(20):
                status = E2EProcessHelper.execute(
                    [*self.cri, "inspecti", reference],
                    Project.E2E_ROOT,
                    self.environment,
                    10,
                )
                if (
                    status.returncode == 0
                    and json.loads(status.stdout).get("status", {}).get("pinned")
                    is True
                ):
                    return
                time.sleep(0.25)
            raise RuntimeError(
                f"containerd did not expose the retained payer image as pinned: {reference}"
            )

        def release_manual_pins(self) -> list[str]:
            if self.shared_docker:
                return []  # Scoped Kubernetes cleanup removes the image keepers.
            output = []
            for reference in sorted(
                self.image_references(label=(MANUAL_IMAGE_PIN_OWNER, "true"))
            ):
                E2EProcessHelper.run(
                    [
                        *self.ctr,
                        "images",
                        "label",
                        reference,
                        f"{MANUAL_IMAGE_PIN}=",
                        f"{MANUAL_IMAGE_PIN_OWNER}=",
                    ],
                    Project.E2E_ROOT,
                    self.environment,
                    30,
                )
                output.append(f"released manual-owned image pin: {reference}")
            return output

    def __init__(
        self, environment: dict[str, str] | None = None, timeout_seconds: float = 120
    ) -> None:
        from types import SimpleNamespace

        self.environment = dict(os.environ if environment is None else environment)
        E2EImagesDefinitions.configure(self.environment)
        self.context = SimpleNamespace(timeout_seconds=timeout_seconds)
        self.kubeconfig = E2EKubernetesDeployer.LocalImageLoader.kubeconfig(
            self.environment
        )
        self.environment["KUBECONFIG"] = self.kubeconfig
        self._kubectl_executable = shutil.which("kubectl") or "kubectl"
        self._helm_executable = shutil.which("helm") or "helm"
        self._image_loader = E2EKubernetesDeployer.LocalImageLoader.detect(
            self._privileged_command(self._kubectl_executable), self.environment
        )
        self.docker = self._image_loader.docker

    def _run(self, command, timeout=None, cwd=Project.E2E_ROOT):
        return E2EProcessHelper.run(
            command, cwd, self.environment, timeout or self.context.timeout_seconds
        )

    def kubectl(self, *arguments: str, timeout: float | None = None) -> str:
        return self._kubectl(*arguments, timeout=timeout)

    def apply(self, document: Mapping[str, object]) -> str:
        return self._apply_document(document)

    def import_image(self, reference: str) -> str:
        return self._prepare_deployment_image(reference)

    def install(
        self,
        release: str,
        chart: Path,
        namespace: str,
        image_key: str,
        values: tuple[Path, ...] = (),
        image_prefix: str = "image",
    ) -> str:
        reference = self.import_image(
            E2EImagesDefinitions.reference(image_key, self.environment)
        )
        selected = E2EImagesDefinitions.helm_values(reference, image_prefix)
        for source in (chart / "values.yaml", *values):
            if source.is_file():
                selected.update(
                    E2EImagesDefinitions.sidecar_images(
                        yaml.safe_load(source.read_text()), self.environment
                    )
                )
        for key, value in tuple(selected.items()):
            if key.endswith(".image"):
                selected[key] = self.import_image(value)
        flags = [item for path in values for item in ("-f", str(path))]
        flags += [
            item
            for key, value in selected.items()
            for item in ("--set-string", f"{key}={value}")
        ]
        return self._helm(
            "upgrade",
            "--install",
            release,
            str(chart),
            "--namespace",
            namespace,
            *flags,
            "--wait",
            "--timeout",
            f"{int(self.context.timeout_seconds)}s",
        )

    def _privileged_command(self, executable: str, *arguments: str) -> list[str]:
        return E2EKubernetesDeployer.LocalImageLoader.command(
            self.kubeconfig, executable, *arguments
        )

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
        command = self._privileged_command(
            self._kubectl_executable or "kubectl", "apply", "-f", "-"
        )
        completed = subprocess.run(
            command,
            cwd=Project.E2E_ROOT,
            env=self.environment,
            input=json.dumps(document, separators=(",", ":")),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=min(self.context.timeout_seconds, 60),
            check=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(
                f"kubectl apply failed with exit code {completed.returncode}:\n{completed.stdout}"
            )
        return completed.stdout

    def _cluster_images(self) -> set[str]:
        return self._image_loader.image_references()

    def _ensure_cluster_image(self, reference: str) -> None:
        platform = E2EImagesDefinitions.reference_platform(reference, self.environment)
        inspect = E2EProcessHelper.execute(
            [
                *self.docker,
                "image",
                "inspect",
                "--platform",
                platform,
                "--format={{.Id}} {{.Os}}/{{.Architecture}}",
                reference,
            ],
            Project.E2E_ROOT,
            self.environment,
            30,
        )
        if inspect.returncode != 0:
            raise RuntimeError(
                f"Kubernetes image {reference} is absent from the local source-image engine; "
                "rerun without --skip-build"
            )
        fields = inspect.stdout.strip().split()
        if len(fields) != 2:
            raise RuntimeError(f"invalid source image metadata for {reference}")
        if fields[1] != platform:
            raise RuntimeError(
                f"image platform mismatch for {reference}: catalog requires {platform}, got {fields[1]}"
            )
        image_id = fields[0].removeprefix("sha256:")
        if not re.fullmatch(r"[a-f0-9]{64}", image_id):
            raise RuntimeError(f"invalid source image ID for {reference}")
        self._image_loader.validate_platform(fields[1], reference)
        if self._image_loader.shared_docker:
            return
        cluster_id = self._cluster_image_id(reference)
        if cluster_id == image_id:
            return
        with tempfile.TemporaryFile() as archive:
            exporter = subprocess.run(
                [*self.docker, "save", "--platform", platform, reference],
                cwd=Project.E2E_ROOT,
                env=self.environment,
                stdout=archive,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=self.context.timeout_seconds,
            )
            if exporter.returncode != 0:
                raise RuntimeError(
                    f"could not export {reference} for Kubernetes:\n{exporter.stderr[-2048:]}"
                )
            # Docker's containerd store reports a manifest/index ID; CRI uses
            # the config digest. Hash the config from the exact archive imported.
            image_id = self._archive_image_id(archive)
            if cluster_id == image_id:
                return
            archive.seek(0)
            importer = subprocess.run(
                [
                    *self._image_loader.ctr,
                    "images",
                    "import",
                    "--platform",
                    fields[1],
                    "-",
                ],
                cwd=Project.E2E_ROOT,
                env=self.environment,
                stdin=archive,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
                timeout=self.context.timeout_seconds,
            )
            if importer.returncode != 0:
                raise RuntimeError(
                    f"could not import {reference} into Kubernetes runtime:\n{importer.stdout[-2048:]}"
                )
        imported_id = self._cluster_image_id(reference)
        if imported_id != image_id:
            raise RuntimeError(
                f"imported Kubernetes image does not match the source config digest: {reference}; "
                f"expected sha256:{image_id}, observed {imported_id or 'missing'}"
            )

    @staticmethod
    def _archive_image_id(archive: BinaryIO) -> str:
        archive.seek(0)
        with tarfile.open(fileobj=archive) as image:
            manifest_file = image.extractfile("manifest.json")
            if manifest_file is None:
                raise RuntimeError("Docker image archive is missing its manifest")
            manifests = json.load(manifest_file)
            if len(manifests) != 1:
                raise RuntimeError(
                    "Kubernetes image import requires a single-platform Docker archive"
                )
            config = image.extractfile(manifests[0]["Config"])
            if config is None:
                raise RuntimeError("Docker image archive is missing its config")
            return hashlib.sha256(config.read()).hexdigest()

    def _cluster_image_id(self, reference: str) -> str | None:
        result = E2EProcessHelper.execute(
            [*self._image_loader.cri, "inspecti", reference],
            Project.E2E_ROOT,
            self.environment,
            30,
        )
        if result.returncode != 0:
            return None
        value = json.loads(result.stdout).get("status", {}).get("id", "")
        image_id = str(value).removeprefix("sha256:")
        return image_id if re.fullmatch(r"[a-f0-9]{64}", image_id) else None

    def _prepare_deployment_image(self, reference: str) -> str:
        selected = self._image_loader.deployment_image(
            reference, self.context.timeout_seconds
        )
        self._ensure_cluster_image(selected)
        return selected
