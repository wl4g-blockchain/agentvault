from __future__ import annotations

import os
import platform
import re
import subprocess
import sys
import urllib.error
import urllib.request
from functools import lru_cache
from pathlib import Path
from typing import Any, Iterator, Mapping


class E2EImagesDefinitions:
    IN_CN_GFW = "IN_CN_GFW"
    IN_GFW = IN_CN_GFW
    NON_GFW = "NON_GFW"
    AMD64 = "amd64"
    ARM64 = "arm64"
    DEFAULT_PLATFORM = "linux/amd64"
    # OTP 25's perf JIT mode avoids Rosetta's NIF-loading failure at startup.
    EMQX_ROSETTA_ERL_FLAGS = "+JPperf true"
    DOCKER_HUB = "docker.io"
    ALIYUN = "registry.cn-shenzhen.aliyuncs.com/wl4g"
    SOLANA_VERSION = "3.1.14"
    SOLANA_ARM64 = f"{ALIYUN}/anza_solana:{SOLANA_VERSION}-arm64"
    LOCAL_SOLANA_ARM64 = f"solana:{SOLANA_VERSION}-arm64"

    LANGUAGE_IMAGE_NAMES = {
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
            f"payee-{language}": f"localhost/marketpulse-{name}-e2e:local"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
        **{
            f"payer-{language}": f"localhost/quantstream-ingestor-{name}-e2e:local"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
    }
    _OFFICIAL = {
        "dockerfile": f"{DOCKER_HUB}/docker/dockerfile:1.7",
        "rust": f"{DOCKER_HUB}/library/rust:1.99.1-slim",
        "debian": f"{DOCKER_HUB}/library/debian:trixie-slim",
        "golang": f"{DOCKER_HUB}/library/golang:1.26-alpine",
        "alpine": f"{DOCKER_HUB}/library/alpine:3.21",
        "python": f"{DOCKER_HUB}/library/python:3.11-slim",
        "node": f"{DOCKER_HUB}/library/node:22-alpine",
        "web-runtime": f"{DOCKER_HUB}/nginxinc/nginx-unprivileged:1.27-alpine",
        "maven": f"{DOCKER_HUB}/library/maven:3.9.9-eclipse-temurin-17",
        "postgres": f"{DOCKER_HUB}/pgvector/pgvector:pg16",
        "emqx": f"{DOCKER_HUB}/emqx/emqx:5.5.0",
        "jaeger": f"{DOCKER_HUB}/jaegertracing/all-in-one:1.76.0",
        "anvil": "ghcr.io/foundry-rs/foundry:v1.7.1",
        "solana": f"{DOCKER_HUB}/anzaxyz/agave:v{SOLANA_VERSION}",
        "tempo": "ghcr.io/tempoxyz/tempo-localnet@sha256:86f199f161be85d3610d990e5bc2653ece29de3ee9c91e8c8e768cf2b8c05c3b",
        "facilitator": f"{DOCKER_HUB}/ukstv/x402-facilitator:1.4.9",
        "envoy": f"{DOCKER_HUB}/envoyproxy/envoy:distroless-v1.36.4",
        "auditor": f"{DOCKER_HUB}/library/python:3.12-slim",
        "volume-helper": f"{DOCKER_HUB}/rancher/mirrored-library-busybox:1.37.0",
        "manual-provider": "localhost/manual-payment-provider:local",
        **LOCAL_IMAGES,
    }
    # Every third-party dependency has an explicit domestic counterpart.
    # Local fixture outputs remain local in both network modes.
    _MIRRORED = {
        **_OFFICIAL,
        "dockerfile": f"{ALIYUN}/docker_dockerfile:1.7-amd64",
        "debian": f"{ALIYUN}/debian:trixie-slim",
        "python": f"{ALIYUN}/python:3.11-slim-amd64",
        "web-runtime": f"{ALIYUN}/nginxinc_nginx-unprivileged:1.27-alpine",
        "auditor": f"{ALIYUN}/python:3.12-slim-amd64",
        "volume-helper": f"{ALIYUN}/rancher_mirrored-library-busybox:1.37.0-amd64",
        "facilitator": f"{ALIYUN}/ukstv_x402-facilitator:1.4.9",
        "tempo": f"{ALIYUN}/tempoxyz_tempo-localnet:sha256-86f199f161be85d3610d990e5bc2653ece29de3ee9c91e8c8e768cf2b8c05c3b",
        "rust": f"{ALIYUN}/rust:1.99.1-slim",
        "golang": f"{ALIYUN}/golang:1.26-alpine",
        "alpine": f"{ALIYUN}/alpine:3.21",
        "node": f"{ALIYUN}/node:22-alpine",
        "maven": f"{ALIYUN}/maven:3.9.9-eclipse-temurin-17",
        "postgres": f"{ALIYUN}/pgvector_pgvector:pg16",
        "emqx": f"{ALIYUN}/emqx_emqx:5.5.0",
        "jaeger": f"{ALIYUN}/jaegertracing_all-in-one:1.76.0",
        "anvil": f"{ALIYUN}/foundry_anvil:1.7.1",
        "solana": f"{ALIYUN}/anza_solana:{SOLANA_VERSION}",
        "envoy": f"{ALIYUN}/envoyproxy_envoy:distroless-v1.36.4",
    }
    # The macOS policy selects native ARM64 Solana; other hosts use AMD64.
    # All remaining components run as AMD64 in every matrix dimension.
    MATRIX = {
        IN_CN_GFW: {AMD64: _MIRRORED, ARM64: {**_MIRRORED, "solana": SOLANA_ARM64}},
        NON_GFW: {AMD64: _OFFICIAL, ARM64: {**_OFFICIAL, "solana": SOLANA_ARM64}},
    }
    COMPONENT_ENV = {
        **{
            key: f"E2E_{key.upper().replace('-', '_')}_IMAGE"
            for key in (
                "postgres",
                "emqx",
                "jaeger",
                "anvil",
                "solana",
                "tempo",
                "facilitator",
                "envoy",
                "auditor",
                "volume-helper",
                "manual-provider",
            )
        },
        "agentvault": "E2E_AGENTVAULT_IMAGE",
        "admin-ui": "E2E_QUANTSTREAM_UI_IMAGE",
        **{
            f"payee-{language}": f"E2E_MARKETPULSE_{name.upper()}_IMAGE"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
        **{
            f"payer-{language}": f"E2E_QUANTSTREAM_{name.upper()}_IMAGE"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
    }
    COMPONENT_KEYS = {
        **{
            name: name
            for name in (
                "postgres",
                "emqx",
                "jaeger",
                "anvil",
                "solana",
                "tempo",
                "facilitator",
                "agentvault",
            )
        },
        "evm": "anvil",
        "mock-mpp-tempo-service": "manual-provider",
        "mock-payer-quantstream-ui": "admin-ui",
        **{
            f"mock-payee-marketpulse-{name}-service": f"payee-{language}"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
        **{
            f"mock-payer-quantstream-service-{name}": f"payer-{language}"
            for language, name in LANGUAGE_IMAGE_NAMES.items()
        },
    }
    BUILD_ENV = {
        **{
            f"E2E_{name.upper()}_IMAGE": name
            for name in ("postgres", "emqx", "jaeger", "anvil", "tempo", "facilitator")
        },
        "BUILDKIT_SYNTAX": "dockerfile",
        "E2E_EVM_IMAGE": "anvil",
        "E2E_AGENTVAULT_BUILD_IMAGE": "rust",
        "E2E_AGENTVAULT_RUNTIME_IMAGE": "debian",
        "E2E_QUANTSTREAM_UI_BUILD_IMAGE": "node",
        "E2E_QUANTSTREAM_UI_RUNTIME_IMAGE": "node",
        "RUST_BUILDER_IMAGE": "rust",
        "RUNTIME_IMAGE": "debian",
        "NODE_IMAGE": "node",
        "WEB_IMAGE": "web-runtime",
    }
    for _owner in ("MARKETPULSE", "QUANTSTREAM"):
        for _language, _build, _runtime in (
            ("RUST", "rust", "debian"),
            ("GOLANG", "golang", "alpine"),
            ("PYTHON", "python", "python"),
            ("TYPESCRIPT", "node", "node"),
            ("JAVA", "maven", "debian"),
        ):
            BUILD_ENV[f"E2E_{_owner}_{_language}_BUILD_IMAGE"] = _build
            BUILD_ENV[f"E2E_{_owner}_{_language}_RUNTIME_IMAGE"] = _runtime
        for _language, _name in (("go", "GOLANG"), ("java", "JAVA")):
            BUILD_ENV[f"E2E_{_owner}_{_name}_PREBUILT_RUNTIME_IMAGE"] = (
                f"{'payee' if _owner == 'MARKETPULSE' else 'payer'}-{_language}"
            )
    del _owner, _language, _name, _build, _runtime

    @staticmethod
    @lru_cache(maxsize=1)
    def _docker_hub_reachable() -> bool:
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with opener.open("https://registry-1.docker.io/v2/", timeout=3):
                return True
        except urllib.error.HTTPError as error:
            return error.code == 401
        except OSError:
            return False

    @classmethod
    def network(cls, environment: Mapping[str, str]) -> str:
        requested = environment.get("E2E_IMAGE_NETWORK", "auto").upper()
        requested = {"IN_GFW": cls.IN_CN_GFW}.get(requested, requested)
        if requested == "AUTO":
            return cls.NON_GFW if cls._docker_hub_reachable() else cls.IN_CN_GFW
        if requested not in cls.MATRIX:
            raise ValueError("E2E_IMAGE_NETWORK must be auto, IN_CN_GFW, or NON_GFW")
        return requested

    @classmethod
    def host_architecture(cls) -> str:
        return cls.ARM64 if sys.platform == "darwin" else cls.AMD64

    @classmethod
    def catalog_references(cls, key: str) -> set[str]:
        return {
            images[key]
            for network in cls.MATRIX.values()
            for images in network.values()
        }

    @classmethod
    def reference(cls, key: str, environment: Mapping[str, str]) -> str:
        variable = cls.COMPONENT_ENV.get(key)
        override = environment.get(variable) if variable else None
        if override and override not in cls.catalog_references(key):
            return override
        return cls.selected(key, environment)

    @classmethod
    def image_platform(cls, key: str, environment: Mapping[str, str]) -> str:
        if key == "solana":
            default = (
                "linux/arm64"
                if cls.host_architecture() == cls.ARM64
                else cls.DEFAULT_PLATFORM
            )
            return environment.get("E2E_SOLANA_PLATFORM", default)
        return environment.get("DOCKER_DEFAULT_PLATFORM", cls.DEFAULT_PLATFORM)

    @classmethod
    def is_local(cls, reference: str) -> bool:
        return reference.startswith("localhost/") or reference == cls.LOCAL_SOLANA_ARM64

    @classmethod
    def reference_platform(cls, reference: str, environment: Mapping[str, str]) -> str:
        key = (
            "solana"
            if cls.repository(reference)
            == cls.repository(cls.reference("solana", environment))
            else "default"
        )
        return cls.image_platform(key, environment)

    @staticmethod
    def repository(reference: str) -> str:
        name = reference.split("@", 1)[0]
        return name.rsplit(":", 1)[0] if ":" in name.rsplit("/", 1)[-1] else name

    @staticmethod
    def helm_values(reference: str, prefix: str = "image") -> dict[str, str]:
        repository, separator, tag = reference.rpartition(":")
        if (
            not separator
            or not repository
            or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}", tag)
        ):
            raise ValueError(f"E2E image requires a version tag or digest: {reference}")
        return {
            f"{prefix}.repository": repository,
            f"{prefix}.tag": tag,
            f"{prefix}.pullPolicy": "Never",
        }

    @staticmethod
    def runtime_environment(
        component: str | None, environment: Mapping[str, str]
    ) -> dict[str, str]:
        flags = environment.get("E2E_EMQX_ERL_FLAGS", "")
        return {"ERL_FLAGS": flags} if component == "emqx" and flags else {}

    @classmethod
    def selected(cls, key: str, environment: Mapping[str, str]) -> str:
        """Resolve a matrix default independently of a previous projected value."""
        return cls.MATRIX[cls.network(environment)][cls.host_architecture()][key]

    @classmethod
    def project_reference(
        cls, variable: str, key: str, environment: dict[str, str]
    ) -> None:
        defaults = cls.catalog_references(key)
        if not environment.get(variable) or environment[variable] in defaults:
            environment[variable] = cls.selected(key, environment)

    @classmethod
    def configure(cls, environment: dict[str, str]) -> None:
        environment["E2E_IMAGE_NETWORK"] = cls.network(environment)
        environment.setdefault("DOCKER_DEFAULT_PLATFORM", cls.DEFAULT_PLATFORM)
        if environment["DOCKER_DEFAULT_PLATFORM"] != cls.DEFAULT_PLATFORM:
            raise ValueError(
                "E2E components require linux/amd64; macOS Solana is the linux/arm64 exception"
            )
        environment["E2E_IMAGE_PLATFORM"] = environment["DOCKER_DEFAULT_PLATFORM"]
        emulated = (
            sys.platform == "darwin"
            and platform.machine().lower() in {"arm64", "aarch64"}
            and environment["DOCKER_DEFAULT_PLATFORM"] == cls.DEFAULT_PLATFORM
        )
        environment.setdefault(
            "E2E_EMQX_ERL_FLAGS", cls.EMQX_ROSETTA_ERL_FLAGS if emulated else ""
        )
        environment.setdefault(
            "E2E_SOLANA_PLATFORM", cls.image_platform("solana", environment)
        )
        expected_solana_platform = (
            "linux/arm64"
            if cls.host_architecture() == cls.ARM64
            else cls.DEFAULT_PLATFORM
        )
        if environment["E2E_SOLANA_PLATFORM"] != expected_solana_platform:
            raise ValueError(f"Solana requires {expected_solana_platform} on this host")
        if cls.host_architecture() == cls.ARM64 and environment.get(
            "E2E_SOLANA_IMAGE", cls.SOLANA_ARM64
        ) not in {cls.SOLANA_ARM64, cls.LOCAL_SOLANA_ARM64}:
            raise ValueError(
                "macOS Solana must use the pinned Aliyun ARM64 image or its local solana:3.1.14-arm64 alias"
            )
        for key, variable in cls.COMPONENT_ENV.items():
            cls.project_reference(variable, key, environment)
        for variable, key in cls.BUILD_ENV.items():
            cls.project_reference(variable, key, environment)

    @classmethod
    def local_image_references(cls) -> frozenset[str]:
        return frozenset(
            {
                reference.removeprefix("localhost/")
                for reference in cls.LOCAL_IMAGES.values()
            }
            | {
                "marketpulse-e2e:local",
                "marketpulse-go-e2e:local",
                "quantstream-ingestor-go-e2e:local",
            }
        )

    @classmethod
    def build_arguments(
        cls, dockerfile: Path, environment: Mapping[str, str]
    ) -> list[str]:
        names = [
            "BUILDKIT_SYNTAX",
            *re.findall(r"(?m)^ARG (\w*IMAGE)(?:=|$)", dockerfile.read_text()),
        ]
        aliases = (
            {
                "RUST_BUILDER_IMAGE": "E2E_AGENTVAULT_BUILD_IMAGE",
                "RUNTIME_IMAGE": "E2E_AGENTVAULT_RUNTIME_IMAGE",
            }
            if dockerfile.parent.name == "agentvault"
            else {}
        )
        return [
            value
            for name in names
            for value in (
                "--build-arg",
                f"{name}={environment[aliases.get(name, name)]}",
            )
        ]

    @classmethod
    def sidecar_images(
        cls, document: Any, environment: Mapping[str, str], prefix: str = ""
    ) -> Iterator[tuple[str, str]]:
        if isinstance(document, dict):
            if document.get("name") == "local-envoy-cm" and isinstance(
                document.get("image"), str
            ):
                yield f"{prefix}.image", cls.reference("envoy", environment)
            for key, value in document.items():
                if key != "image":
                    yield from cls.sidecar_images(
                        value, environment, f"{prefix}.{key}" if prefix else key
                    )
        elif isinstance(document, list):
            for index, item in enumerate(document):
                yield from cls.sidecar_images(item, environment, f"{prefix}[{index}]")

    @staticmethod
    def main() -> None:
        environment = os.environ.copy()
        E2EImagesDefinitions.configure(environment)
        command = sys.argv[1:]
        if command[:1] == ["--"]:
            command = command[1:]
        if command:
            if (
                Path(command[0]).stem in {"docker", "podman"}
                and "build" in command
                and "compose" not in command
            ):
                dockerfile = Path(
                    command[command.index("-f") + 1]
                    if "-f" in command
                    else command[command.index("--file") + 1]
                    if "--file" in command
                    else "Dockerfile"
                )
                key = E2EImagesDefinitions.COMPONENT_KEYS.get(dockerfile.parent.name)
                position = command.index("build") + 1
                if not any(
                    item == "--platform" or item.startswith("--platform=")
                    for item in command
                ):
                    command[position:position] = [
                        "--platform",
                        E2EImagesDefinitions.image_platform(
                            key or "default", environment
                        ),
                    ]
                command[position:position] = E2EImagesDefinitions.build_arguments(
                    dockerfile, environment
                )
                # BuildKit ships a Dockerfile directory independently of the
                # context; a relative link escaping that directory cannot resolve.
                if dockerfile.is_symlink():
                    option = (
                        "-f"
                        if "-f" in command
                        else "--file"
                        if "--file" in command
                        else None
                    )
                    if option:
                        command[command.index(option) + 1] = str(dockerfile.resolve())
                if not any(
                    item in {"-t", "--tag"} or item.startswith("--tag=")
                    for item in command
                ):
                    key = E2EImagesDefinitions.COMPONENT_KEYS[dockerfile.parent.name]
                    command[position:position] = [
                        "--tag",
                        E2EImagesDefinitions.reference(key, environment),
                    ]
            raise SystemExit(subprocess.run(command, env=environment).returncode)
        variables = {
            *E2EImagesDefinitions.COMPONENT_ENV.values(),
            *E2EImagesDefinitions.BUILD_ENV,
            "E2E_IMAGE_NETWORK",
            "DOCKER_DEFAULT_PLATFORM",
            "E2E_IMAGE_PLATFORM",
            "E2E_SOLANA_PLATFORM",
            "E2E_EMQX_ERL_FLAGS",
        }
        print("\n".join(f"{name}={environment[name]}" for name in sorted(variables)))


if __name__ == "__main__":
    E2EImagesDefinitions.main()
