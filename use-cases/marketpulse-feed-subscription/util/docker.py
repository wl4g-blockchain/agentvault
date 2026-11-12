from __future__ import annotations

import os
import shutil
import sys
import time
from pathlib import Path
from urllib.parse import urlsplit, urlunsplit

from util.constant import E2EOtherDefinitions as Project
from util.images import E2EImagesDefinitions
from util.process import E2EProcessHelper


class E2EDockerDeployer:
    @staticmethod
    def detect(environment: dict[str, str]) -> list[str]:
        requested = environment.get("E2E_DOCKER_CONTEXT") or environment.get(
            "DOCKER_CONTEXT"
        )
        if requested:
            candidates = [["docker", "--context", requested]]
        else:
            candidates = [["docker"]]
            if sys.platform == "darwin":
                candidates.extend(
                    [["docker", "--context", name] for name in ("orbstack", "colima")]
                )
        for command in candidates:
            if (
                E2EProcessHelper.execute(
                    [*command, "info"], Project.E2E_ROOT, environment, 15
                ).returncode
                == 0
            ):
                return command
        if sys.platform == "linux" and not requested:
            command = ["sudo", "-n", "-E", "docker"]
            E2EProcessHelper.run([*command, "info"], Project.E2E_ROOT, environment, 15)
            return command
        raise RuntimeError(
            "No accessible Docker engine; set E2E_DOCKER_CONTEXT to a native, OrbStack or Colima context"
        )

    @staticmethod
    def compose_command(docker: list[str], environment: dict[str, str]) -> list[str]:
        E2EImagesDefinitions.configure(environment)
        if "--context" in docker:
            environment["DOCKER_CONTEXT"] = docker[docker.index("--context") + 1]
        if sys.platform == "darwin":
            for name, value in tuple(environment.items()):
                if (
                    name.startswith("E2E_")
                    and name.endswith(("_BUILD_HTTPS_PROXY", "_BUILD_HTTP_PROXY"))
                    and value
                ):
                    proxy = urlsplit(value)
                    if proxy.hostname in {"127.0.0.1", "localhost", "::1"}:
                        credentials = proxy.netloc.rpartition("@")[0]
                        host = (
                            f"host.docker.internal:{proxy.port}"
                            if proxy.port
                            else "host.docker.internal"
                        )
                        environment[name] = urlunsplit(
                            proxy._replace(
                                netloc=f"{credentials}@{host}" if credentials else host
                            )
                        )
        if (
            E2EProcessHelper.execute(
                [*docker, "compose", "version"], Project.E2E_ROOT, environment, 15
            ).returncode
            == 0
        ):
            return [*docker, "compose"]
        standalone = shutil.which("docker-compose")
        if standalone and docker[0] == "docker":
            return [standalone]
        raise RuntimeError("E2E builds require Docker Compose")

    @staticmethod
    def compose_run(
        manifest: Path,
        project: str,
        arguments: list[str],
        environment: dict[str, str],
        timeout: float = 120,
    ) -> str:
        docker = E2EDockerDeployer.detect(environment)
        command = E2EDockerDeployer.compose_command(docker, environment)
        return E2EProcessHelper.run(
            [*command, "-p", project, "-f", str(manifest), *arguments],
            manifest.parent,
            environment,
            timeout,
        )


class E2EDockerCleaner:
    def __init__(
        self,
        manifest: Path,
        project: str,
        environment: dict[str, str] | None = None,
        timeout_seconds: float = 120,
    ) -> None:
        if not project or project == "default":
            raise ValueError("cleanup requires an explicit project name")
        self.environment = dict(os.environ if environment is None else environment)
        self.project = project
        self.timeout_seconds = timeout_seconds
        self.docker = E2EDockerDeployer.detect(self.environment)
        self.compose = [
            *E2EDockerDeployer.compose_command(self.docker, self.environment),
            "-p",
            project,
            "-f",
            str(manifest),
        ]

    def _try(self, command, timeout=30):
        result = E2EProcessHelper.execute(
            command, Project.E2E_ROOT, self.environment, timeout
        )
        return result.returncode, result.stdout

    def cleanup(self) -> str:
        return self.cleanup_owned()

    def cleanup_owned(self) -> str:
        command = [*self.compose, "down", "--volumes", "--remove-orphans"]
        first = E2EProcessHelper.execute(
            command, Project.E2E_ROOT, self.environment, 180
        )
        inventory = self._owned_container_inventory()
        volumes = self._owned_volumes()
        if first.returncode == 0 and not inventory and not volumes:
            return first.stdout.strip()

        # Shared network namespaces must be removed from dependants to owners.
        # Discover those relationships within this project's labelled inventory.
        removal_output: list[str] = []
        for container_ids in self._removal_batches(inventory):
            removed = E2EProcessHelper.execute(
                [*self.docker, "rm", "--force", *container_ids],
                Project.E2E_ROOT,
                self.environment,
                60,
            )
            removal_output.append(removed.stdout)

        second = E2EProcessHelper.execute(
            command, Project.E2E_ROOT, self.environment, 180
        )
        volume_output = self._remove_owned_volumes()
        for _ in range(20):
            if not self._owned_containers() and not self._owned_volumes():
                break
            time.sleep(0.25)
        remaining_containers = self._owned_containers()
        remaining_volumes = self._owned_volumes()
        if remaining_containers or remaining_volumes:
            raise RuntimeError(
                f"E2E cleanup left owned containers {remaining_containers} and volumes {remaining_volumes}:\n"
                + first.stdout[-4096:]
                + "\n"
                + "".join(removal_output)[-4096:]
                + "\n"
                + second.stdout[-4096:]
                + "\n"
                + volume_output[-4096:]
            )
        return (
            first.stdout + "".join(removal_output) + second.stdout + volume_output
        ).strip()

    def _remove_owned_volumes(self) -> str:
        volumes = self._owned_volumes()
        if not volumes:
            return ""
        if volumes == ["<volume inventory unavailable>"]:
            raise RuntimeError("E2E cleanup could not inventory its owned volumes")
        status, output = self._try(
            [*self.docker, "volume", "rm", "--force", *volumes], 60
        )
        remaining = self._owned_volumes()
        if remaining:
            raise RuntimeError(
                f"E2E cleanup could not remove owned volumes {remaining}; exit={status}:\n{output[-4096:]}"
            )
        return output

    def _owned_container_inventory(self) -> list[tuple[str, str, str]]:
        status, output = self._try(
            [
                *self.docker,
                "ps",
                "-aq",
                "--filter",
                f"label=com.docker.compose.project={self.project}",
            ]
        )
        if status != 0:
            return [("<inventory-unavailable>", "<inventory-unavailable>", "")]
        container_ids = [line.strip() for line in output.splitlines() if line.strip()]
        inventory: list[tuple[str, str, str]] = []
        for container_id in container_ids:
            inspect_status, inspect_output = self._try(
                [
                    *self.docker,
                    "inspect",
                    "--format",
                    "{{.Id}}\t{{.Name}}\t{{.HostConfig.NetworkMode}}",
                    container_id,
                ]
            )
            if inspect_status != 0:
                inventory.append((container_id, container_id, ""))
                continue
            fields = inspect_output.strip().split("\t")
            if len(fields) != 3:
                inventory.append((container_id, container_id, ""))
                continue
            resolved_id, name, network_mode = fields
            inventory.append((resolved_id, name.lstrip("/"), network_mode))
        return inventory

    @staticmethod
    def _removal_batches(inventory: list[tuple[str, str, str]]) -> list[list[str]]:
        """Remove network dependants first, including nested container joins."""
        remaining = {
            container_id: (name, mode) for container_id, name, mode in inventory
        }
        batches = []
        while remaining:
            owners = set()
            for _, mode in remaining.values():
                if not mode.startswith("container:"):
                    continue
                target = mode.removeprefix("container:").lstrip("/")
                owners.update(
                    container_id
                    for container_id, (name, _) in remaining.items()
                    if target and (container_id.startswith(target) or name == target)
                )
            batch = [
                container_id for container_id in remaining if container_id not in owners
            ]
            if not batch:
                raise RuntimeError(
                    "cyclic network namespace dependencies in owned containers"
                )
            batches.append(batch)
            for container_id in batch:
                del remaining[container_id]
        return batches

    def _owned_containers(self) -> list[str]:
        status, output = self._try(
            [
                *self.docker,
                "ps",
                "-a",
                "--filter",
                f"label=com.docker.compose.project={self.project}",
                "--format",
                "{{.Names}}",
            ]
        )
        if status != 0:
            return ["<container inventory unavailable>"]
        return [line.strip() for line in output.splitlines() if line.strip()]

    def _owned_volumes(self) -> list[str]:
        status, output = self._try(
            [
                *self.docker,
                "volume",
                "ls",
                "--quiet",
                "--filter",
                f"label=com.docker.compose.project={self.project}",
            ]
        )
        if status != 0:
            return ["<volume inventory unavailable>"]
        return [line.strip() for line in output.splitlines() if line.strip()]
