from __future__ import annotations

import json
import logging
import math
import os
import re
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.request
from contextlib import ExitStack, contextmanager
from pathlib import Path
from typing import Any, Iterable, Iterator, Mapping, Sequence

import yaml
from util.constant import E2EOtherDefinitions as Project
from util.docker import E2EDockerCleaner, E2EDockerDeployer
from util.images import E2EImagesDefinitions
from util.kubernetes import E2EKubernetesCleaner, E2EKubernetesDeployer
from util.process import E2EProcessHelper

from verifier.context import (
    LANGUAGES,
    PAYER_IDENTITIES,
    ConsoleResourceFixture,
    E2EDeployer,
    VerificationContext,
    create_deployer,
    payer_identity,
)

NAMESPACE_MANIFEST = Project.AGENTVAULT_CONFIG / "namespaces.yaml"

AGENTVAULT_NAMESPACE = "e2e-agentvault-system"
AGENTVAULT_SHARED_ROOT = Project.AGENTVAULT_CONFIG

MARKETPULSE_SHARED_ROOT = (
    Project.E2E_ROOT / "deploy" / "mock-payee-marketpulse-shared-helm"
)

QUANTSTREAM_SHARED_ROOT = (
    Project.E2E_ROOT / "deploy" / "mock-payer-quantstream-shared-helm"
)

MARKETPULSE_SHARED_VALUES = MARKETPULSE_SHARED_ROOT / "e2e-values.yaml"

QUANTSTREAM_SHARED_VALUES = QUANTSTREAM_SHARED_ROOT / "e2e-values.yaml"

SHARED_KUBERNETES_MANIFESTS = (
    AGENTVAULT_SHARED_ROOT / "e2e-envoy.configmap.yaml",
    MARKETPULSE_SHARED_ROOT / "e2e-envoy.configmap.yaml",
    QUANTSTREAM_SHARED_ROOT / "e2e-envoy.configmap.yaml",
)

BUSINESS_EVIDENCE_LABEL = "agentvault.io/e2e-business-evidence"

INFRA_NAMESPACE = "e2e-agentvault-infra-system"

PAYMENT_NAMESPACE = "e2e-agentvault-payment-system"

PAYER_NAMESPACE = "e2e-agentvault-mock-payer"

PAYEE_NAMESPACE = "e2e-agentvault-mock-payee"

SERVICE_NAMES = {
    "agentvault-api": (AGENTVAULT_NAMESPACE, "agentvault", 8080),
    "agentvault-management": (AGENTVAULT_NAMESPACE, "agentvault-management", 9091),
    "anvil": (PAYMENT_NAMESPACE, "anvil", 8545),
    "tempo": (PAYMENT_NAMESPACE, "tempo", 8545),
    "solana": (PAYMENT_NAMESPACE, "solana-validator", 8899),
    "facilitator": (PAYMENT_NAMESPACE, "x402-facilitator", 8085),
    "emqx-mqtt": (INFRA_NAMESPACE, "emqx", 1883),
    "emqx-dashboard": (INFRA_NAMESPACE, "emqx", 18083),
    "jaeger": (INFRA_NAMESPACE, "jaeger", 16686),
    "postgres": (INFRA_NAMESPACE, "postgres", 5432),
    "agentvault-grpc": (AGENTVAULT_NAMESPACE, "agentvault", 50051),
    "admin-ui": (PAYER_NAMESPACE, "quantstream-ui", 8086),
}

LOGGER = logging.getLogger("agentvault.e2e.runner.deployer")

PROJECT = "agentvault-e2e"

COMPOSE_FILE = Project.E2E_ROOT / "docker-compose.yaml"

RUNTIME_ROOT = Project.E2E_ROOT / "reports" / ".runtime"

AGENTVAULT_CHART = Project.AGENTVAULT_CHART

AGENTVAULT_VALUES = Project.AGENTVAULT_CONFIG / "e2e-values.yaml"

AGENTVAULT_CONFIG_TEMPLATE = "templates/platform/runtime-config-map.yaml"


TOKEN_CONTRACT = "0x5fbdb2315678afecb367f032d93f642f64180aa3"

PAYEE_ADDRESS = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"

FACILITATOR_ADDRESS = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"

PAYER_PRIVATE_KEY_DEFAULTS = {
    "rust": (
        "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
        "0" * 63 + "2",
    ),
    "go": ("0" * 63 + "3", "0" * 63 + "4"),
    "python": ("0" * 63 + "5", "0" * 63 + "6"),
    "typescript": ("0" * 63 + "7", "0" * 63 + "8"),
    "java": ("0" * 63 + "9", "0" * 63 + "a"),
}

DEFAULTS = {
    "E2E_POSTGRES_PORT": "25432",
    "E2E_EMQX_MQTT_PORT": "28883",
    "E2E_EMQX_DASHBOARD_PORT": "38084",
    "E2E_JAEGER_UI_PORT": "26686",
    "E2E_JAEGER_OTLP_GRPC_PORT": "24317",
    "E2E_JAEGER_OTLP_HTTP_PORT": "24318",
    "E2E_ANVIL_PORT": "28545",
    "E2E_TEMPO_RPC_PORT": "28546",
    "E2E_SOLANA_RPC_PORT": "28899",
    "E2E_SOLANA_FAUCET_PORT": "28900",
    "E2E_FACILITATOR_PORT": "38085",
    "E2E_AGENTVAULT_PORT": "38080",
    "E2E_AGENTVAULT_GRPC_PORT": "45051",
    "E2E_AGENTVAULT_MGMT_PORT": "39091",
    "E2E_MARKETPULSE_PORT": "38081",
    "E2E_QUANTSTREAM_UI_PORT": "38083",
    "E2E_EMQX_DASHBOARD_PASSWORD": "agentvault-e2e-dashboard",
    "E2E_AGENTVAULT_MQTT_PASSWORD": "agentvault-e2e-vault",
    "E2E_MARKETPULSE_MQTT_PASSWORD": "agentvault-e2e-payee",
    "E2E_QUANTSTREAM_MQTT_PASSWORD": "agentvault-e2e-workload",
    "E2E_QUANTSTREAM_UI_MQTT_PASSWORD": "agentvault-e2e-approver",
    **{
        PAYER_IDENTITIES[language].primary_private_key_env: keys[0]
        for language, keys in PAYER_PRIVATE_KEY_DEFAULTS.items()
    },
    **{
        PAYER_IDENTITIES[language].secondary_private_key_env: keys[1]
        for language, keys in PAYER_PRIVATE_KEY_DEFAULTS.items()
    },
}

# Every declared host binding must move with its matrix worker.
HOST_PORT_NAMES = tuple(name for name in DEFAULTS if name.endswith("_PORT"))


BUILD_PROXY_PREFIXES = (
    "E2E_AGENTVAULT_BUILD",
    "E2E_MARKETPULSE_BUILD",
    "E2E_QUANTSTREAM_BUILD",
    "E2E_QUANTSTREAM_UI_BUILD",
    "E2E_MANUAL_PROVIDER_BUILD",
)

ANSI_ESCAPE_PATTERN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")

EVM_ADDRESS_PATTERN = re.compile(r"(?im)^\s*(0x[0-9a-f]{40})\s*$")

UINT_OUTPUT_PATTERN = re.compile(r"(?m)^\s*([0-9]+)(?:\s+\[[^\n]*\])?\s*$")

COMPOSE_PROVIDER_PREFIX = ">>>> Executing external compose provider "


class E2ES00InfrastructureVerifier:
    class EnvironmentOperations:
        @staticmethod
        def _clean_compose_stdout(output: str) -> str:
            """Remove the external Compose provider banner without hiding command output."""
            lines = ANSI_ESCAPE_PATTERN.sub("", output).replace("\r", "").splitlines()
            return "\n".join(
                line
                for line in lines
                if not line.strip().startswith(COMPOSE_PROVIDER_PREFIX)
            ).strip()

        @staticmethod
        def _inherit_build_proxy_environment(environment: dict[str, str]) -> None:
            """Map conventional proxy variables into each explicit Compose build boundary."""
            https_proxy = environment.get("HTTPS_PROXY") or environment.get(
                "https_proxy"
            )
            http_proxy = environment.get("HTTP_PROXY") or environment.get("http_proxy")
            no_proxy = environment.get("NO_PROXY") or environment.get("no_proxy")
            for prefix in BUILD_PROXY_PREFIXES:
                if https_proxy:
                    environment.setdefault(f"{prefix}_HTTPS_PROXY", https_proxy)
                if http_proxy:
                    environment.setdefault(f"{prefix}_HTTP_PROXY", http_proxy)
                if no_proxy:
                    environment.setdefault(f"{prefix}_NO_PROXY", no_proxy)

        @staticmethod
        def _agentvault_build_targets(environment: dict[str, str]) -> tuple[str, ...]:
            """Resolve the runtime image build boundary shared by single and matrix runs."""
            if environment.get("E2E_RUNNER_PREBUILT_AGENTVAULT") != "1":
                return ("agentvault",)
            if not environment.get("E2E_AGENTVAULT_IMAGE"):
                raise RuntimeError("prebuilt AgentVault requires E2E_AGENTVAULT_IMAGE")
            return ()

        @staticmethod
        def _matrix_build_profiles(languages: tuple[str, ...]) -> tuple[str, ...]:
            """Activate every profile referenced by the selected matrix build targets."""
            return (
                "infra",
                "payee",
                "apps",
                *(f"payee-{language}" for language in languages),
                *(f"ingestor-{language}" for language in languages),
            )

        @staticmethod
        def _safe_id(value: str) -> bool:
            return bool(re.fullmatch(r"[A-Za-z0-9._-]+", value))

        @staticmethod
        def _command_operation(command: list[str]) -> str:
            """Return a useful operation name without logging command arguments or secrets."""
            if not command:
                return "unknown"
            index = 0
            if Path(command[0]).name == "sudo":
                index = 1
                while index < len(command) and command[index].startswith("-"):
                    index += 1
            if index >= len(command):
                return "unknown"
            executable = Path(command[index]).name
            arguments = command[index + 1 :]
            if executable == "env":
                while (
                    arguments
                    and "=" in arguments[0]
                    and not arguments[0].startswith("=")
                ):
                    arguments = arguments[1:]
                if not arguments:
                    return "env"
                executable = Path(arguments[0]).name
                arguments = arguments[1:]
            if executable in {"docker", "podman"}:
                if "compose" in arguments:
                    compose_index = arguments.index("compose")
                    compose_arguments = arguments[compose_index + 1 :]
                    actions = {
                        "build",
                        "config",
                        "down",
                        "exec",
                        "logs",
                        "ps",
                        "pull",
                        "run",
                        "up",
                    }
                    action = next(
                        (item for item in compose_arguments if item in actions),
                        "command",
                    )
                    details: list[str] = []
                    profiles = [
                        compose_arguments[position + 1]
                        for position, item in enumerate(compose_arguments[:-1])
                        if item == "--profile"
                    ]
                    if profiles:
                        details.append(f"profiles={','.join(profiles)}")
                    target = E2ES00InfrastructureVerifier.EnvironmentOperations._compose_target(
                        compose_arguments, action
                    )
                    if target:
                        details.append(f"target={target}")
                    suffix = " " + " ".join(details) if details else ""
                    return f"{executable} compose {action}{suffix}"
                first = next(
                    (item for item in arguments if not item.startswith("-")), "command"
                )
                if first in {"image", "volume"}:
                    tail = arguments[arguments.index(first) + 1 :]
                    second = next(
                        (item for item in tail if not item.startswith("-")), "command"
                    )
                    return f"{executable} {first} {second}"
                return f"{executable} {first}"
            if executable in {"cargo", "helm"}:
                action = next(
                    (item for item in arguments if not item.startswith("-")), "command"
                )
                if executable == "helm":
                    chart = next(
                        (
                            Path(item)
                            for item in arguments
                            if (Path(item) / "Chart.yaml").is_file()
                        ),
                        None,
                    )
                    if chart is not None:
                        component = (
                            chart.parent.name if chart.name == "helm" else chart.name
                        )
                        return f"{executable} {action} target={component}"
                return f"{executable} {action}"
            return executable

        @staticmethod
        def _compose_target(arguments: list[str], action: str) -> str | None:
            if action not in {"build", "exec", "logs", "pull", "up"}:
                return None
            action_index = arguments.index(action)
            skip_value = False
            options_with_values = {
                "-e",
                "--env",
                "--entrypoint",
                "--name",
                "-u",
                "--user",
                "-w",
                "--workdir",
            }
            for item in arguments[action_index + 1 :]:
                if skip_value:
                    skip_value = False
                    continue
                if item in options_with_values:
                    skip_value = True
                    continue
                if item.startswith("-"):
                    continue
                return item
            return None

        def _compose(
            self, profiles: Iterable[str], *arguments: str, timeout: float | None = None
        ) -> str:
            if "solana" in arguments and any(
                action in arguments for action in ("up", "pull", "create", "run")
            ):
                E2EKubernetesDeployer.LocalImageLoader.solana_image(
                    self.docker, self.environment
                )
            selected: list[str] = []
            for profile in profiles:
                selected.extend(("--profile", profile))
            return self._run([*self.compose, *selected, *arguments], timeout)

        def _full(
            self, payer: str, payee: str, *arguments: str, timeout: float | None = None
        ) -> str:
            return self._compose(
                ("infra", "payee", "apps", f"payee-{payee}", f"ingestor-{payer}"),
                *arguments,
                timeout=timeout,
            )

        def _chart_image_flags(
            self, chart: Path, values: Iterable[Path] = ()
        ) -> list[str]:
            name = chart.parent.name if chart.name == "helm" else chart.name
            key = E2EImagesDefinitions.COMPONENT_KEYS[name]
            prefix = "agentvault.backend.image" if key == "agentvault" else "image"
            selected = E2EImagesDefinitions.helm_values(
                E2EImagesDefinitions.reference(key, self.environment), prefix
            )
            selected.update(
                {
                    f"env.{name}": value
                    for name, value in E2EImagesDefinitions.runtime_environment(
                        key, self.environment
                    ).items()
                }
            )
            for source in (chart / "values.yaml", *values):
                if source.is_file():
                    selected.update(
                        E2EImagesDefinitions.sidecar_images(
                            yaml.safe_load(source.read_text()), self.environment
                        )
                    )
            return [
                item
                for key, value in selected.items()
                for item in ("--set-string", f"{key}={value}")
            ]

        @staticmethod
        def _chart_namespace(chart: Path) -> str:
            if chart.resolve() == AGENTVAULT_CHART.resolve():
                return AGENTVAULT_NAMESPACE
            metadata = (chart / "Chart.yaml").read_text(encoding="utf-8")
            match = re.search(
                r"(?m)^  agentvault\.io/e2e-namespace: (e2e-agentvault-[a-z0-9-]+)$",
                metadata,
            )
            if match is None:
                raise RuntimeError(
                    f"component Chart does not declare its E2E namespace: {chart}"
                )
            return match.group(1)

        @staticmethod
        def _chart_name(chart: Path) -> str:
            metadata = (chart / "Chart.yaml").read_text(encoding="utf-8")
            match = re.search(r"(?m)^name: ([a-z0-9-]+)$", metadata)
            if match is None:
                raise RuntimeError(
                    f"component Chart does not declare a valid name: {chart}"
                )
            name = match.group(1)
            if name.startswith("e2e-"):
                raise RuntimeError(
                    f"component Chart name must rely on namespace isolation, got: {name}"
                )
            return name

        def _render_agentvault_config(self) -> str:
            image_flags = self._chart_image_flags(
                AGENTVAULT_CHART, (AGENTVAULT_VALUES,)
            )
            self._run(
                [
                    "helm",
                    "lint",
                    "--namespace",
                    AGENTVAULT_NAMESPACE,
                    str(AGENTVAULT_CHART),
                    "-f",
                    str(AGENTVAULT_VALUES),
                    *image_flags,
                ]
            )
            rendered = self._run(
                [
                    "helm",
                    "template",
                    "agentvault",
                    str(AGENTVAULT_CHART),
                    "--namespace",
                    AGENTVAULT_NAMESPACE,
                    "-f",
                    str(AGENTVAULT_VALUES),
                    "--show-only",
                    AGENTVAULT_CONFIG_TEMPLATE,
                    *image_flags,
                ]
            )
            marker = "  agentvault.yaml: |\n"
            if marker not in rendered:
                raise RuntimeError("production Helm chart omitted agentvault.yaml")
            body: list[str] = []
            for line in rendered.split(marker, 1)[1].splitlines():
                if line.startswith("    "):
                    body.append(line[4:])
                elif not line:
                    body.append("")
                else:
                    break
            if not any(line.strip() for line in body):
                raise RuntimeError(
                    "production Helm chart rendered an empty AgentVault configuration"
                )
            RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
            self.agentvault_config.write_text("\n".join(body) + "\n", encoding="utf-8")
            return f"rendered runtime config from {AGENTVAULT_CHART}"

        def _url(self, port_name: str, path: str = "/") -> str:
            return f"http://127.0.0.1:{self.environment[port_name]}{path}"

        def _http(
            self,
            url: str,
            method: str = "GET",
            body: bytes | None = None,
            headers: dict[str, str] | None = None,
            timeout: float = 10,
        ) -> tuple[int, dict[str, str], str]:
            request = urllib.request.Request(
                url, data=body, method=method, headers=headers or {}
            )
            try:
                with urllib.request.urlopen(request, timeout=timeout) as response:
                    return (
                        response.status,
                        dict(response.headers.items()),
                        response.read().decode("utf-8"),
                    )
            except urllib.error.HTTPError as error:
                return (
                    error.code,
                    dict(error.headers.items()),
                    error.read().decode("utf-8", "replace"),
                )

        def _wait_http(self, url: str, attempts: int) -> None:
            for _ in range(attempts):
                try:
                    status, _, _ = self._http(url, timeout=2)
                    if 200 <= status < 300:
                        return
                except OSError:
                    pass
                time.sleep(1)
            raise RuntimeError(f"timed out waiting for {url}")

        def _wait_rpc(
            self, url: str, method: str, expected: object, attempts: int
        ) -> None:
            body = json.dumps(
                {"jsonrpc": "2.0", "id": 1, "method": method, "params": []}
            ).encode()
            for _ in range(attempts):
                try:
                    status, _, raw = self._http(
                        url, "POST", body, {"content-type": "application/json"}, 2
                    )
                    if status == 200 and json.loads(raw).get("result") == expected:
                        return
                except (OSError, ValueError):
                    pass
                time.sleep(1)
            raise RuntimeError(f"timed out waiting for {method} at {url}")

        def _wait_tempo_faucet_window(self, url: str, attempts: int = 240) -> None:
            diagnostic = "Tempo has not produced a current block"
            for _ in range(attempts):
                try:
                    latest = self._json_rpc_result(
                        url, "Tempo", "eth_getBlockByNumber", ["latest", False]
                    )
                    if not isinstance(latest, dict):
                        raise RuntimeError("Tempo latest block is not an object")
                    block_number = int(str(latest.get("number", "")), 16)
                    block_timestamp = int(str(latest.get("timestamp", "")), 16)
                    clock_delta = abs(int(time.time()) - block_timestamp)
                    if block_number > 0 and clock_delta <= 300:
                        return
                    diagnostic = (
                        f"Tempo latest block={block_number} timestamp={block_timestamp} "
                        f"clock_delta={clock_delta}s"
                    )
                except (OSError, RuntimeError, TypeError, ValueError) as error:
                    diagnostic = str(error)
                time.sleep(0.5)
            raise RuntimeError(
                f"timed out waiting for Tempo faucet nonce window: {diagnostic[-2048:]}"
            )

        def _json_rpc_result(
            self, url: str, service: str, method: str, params: list[object]
        ) -> object:
            body = json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": "e2e-accounting",
                    "method": method,
                    "params": params,
                }
            ).encode()
            status, _, raw = self._http(
                url,
                "POST",
                body,
                {"content-type": "application/json"},
                10,
            )
            try:
                response = json.loads(raw)
            except json.JSONDecodeError as error:
                raise RuntimeError(
                    f"{service} returned non-JSON for {method}: {raw[:2048]}"
                ) from error
            if (
                status != 200
                or not isinstance(response, dict)
                or response.get("error") is not None
                or "result" not in response
            ):
                raise RuntimeError(
                    f"{service} RPC {method} failed: HTTP {status}: {raw[:2048]}"
                )
            return response["result"]

        def _rpc_result(self, method: str, params: list[object]) -> object:
            return self._json_rpc_result(
                self._url("E2E_ANVIL_PORT"), "Anvil", method, params
            )

        def _solana_rpc_result(self, method: str, params: list[object]) -> object:
            return self._json_rpc_result(
                self._url("E2E_SOLANA_RPC_PORT"), "Solana", method, params
            )

        def _solana_pubkey(self, keypair_path: str) -> str:
            address = self._solana_command(
                "solana-keygen", "pubkey", keypair_path
            ).strip()
            if not re.fullmatch(r"[1-9A-HJ-NP-Za-km-z]{32,44}", address):
                raise RuntimeError(
                    f"Solana keypair emitted an invalid public key: {address!r}"
                )
            return address

        def _solana_json(
            self, executable: str, *arguments: str, timeout: float = 60
        ) -> dict[str, Any]:
            output = self._solana_command(executable, *arguments, timeout=timeout)
            try:
                payload = json.loads(output)
            except json.JSONDecodeError as error:
                raise RuntimeError(
                    f"Solana fixture command returned invalid JSON: {output[-2048:]}"
                ) from error
            if not isinstance(payload, dict):
                raise RuntimeError(
                    "Solana fixture command did not return a JSON object"
                )
            return payload

        def _wait_solana_token_balance(
            self, account: str, expected: int, attempts: int = 180
        ) -> None:
            for _ in range(attempts):
                try:
                    result = self._solana_rpc_result(
                        "getTokenAccountBalance",
                        [account, {"commitment": "finalized"}],
                    )
                    value = result.get("value") if isinstance(result, dict) else None
                    amount = value.get("amount") if isinstance(value, dict) else None
                    if (
                        isinstance(amount, str)
                        and amount.isdigit()
                        and int(amount) == expected
                    ):
                        return
                except RuntimeError:
                    pass
                time.sleep(0.5)
            raise RuntimeError(
                f"timed out waiting for finalized Solana token balance on {account}"
            )

        def _native_balance(self, address: str) -> int:
            value = self._rpc_result("eth_getBalance", [address, "latest"])
            if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-fA-F]+", value):
                raise RuntimeError(
                    f"Anvil returned an invalid native balance for {address}: {value!r}"
                )
            return int(value, 16)

        def _emqx_api(
            self,
            token: str,
            method: str,
            path: str,
            body: dict[str, object] | None = None,
        ) -> tuple[int, str]:
            payload = (
                None
                if body is None
                else json.dumps(body, separators=(",", ":")).encode()
            )
            status, _, raw = self._http(
                self._url("E2E_EMQX_DASHBOARD_PORT", path),
                method,
                payload,
                {
                    "authorization": f"Bearer {token}",
                    "content-type": "application/json",
                },
            )
            return status, raw

        def _bootstrap_emqx(self) -> None:
            self._wait_http(self._url("E2E_EMQX_DASHBOARD_PORT", "/status"), 90)
            status, _, raw = self._http(
                self._url("E2E_EMQX_DASHBOARD_PORT", "/api/v5/login"),
                "POST",
                json.dumps(
                    {
                        "username": "admin",
                        "password": self.environment["E2E_EMQX_DASHBOARD_PASSWORD"],
                    }
                ).encode(),
                {"content-type": "application/json"},
            )
            if status != 200 or not isinstance(json.loads(raw).get("token"), str):
                raise RuntimeError(
                    f"EMQX dashboard authentication failed with HTTP {status}"
                )
            token = json.loads(raw)["token"]
            status, authentication = self._emqx_api(
                token, "GET", "/api/v5/authentication"
            )
            if status != 200:
                raise RuntimeError(
                    f"read EMQX authentication configuration failed with HTTP {status}"
                )
            if "password_based:built_in_database" not in authentication:
                status, _ = self._emqx_api(
                    token,
                    "POST",
                    "/api/v5/authentication",
                    {
                        "mechanism": "password_based",
                        "backend": "built_in_database",
                        "user_id_type": "username",
                        "password_hash_algorithm": {
                            "name": "sha256",
                            "salt_position": "suffix",
                        },
                    },
                )
                if status not in {200, 201, 204, 409}:
                    raise RuntimeError(
                        f"create EMQX password authenticator failed with HTTP {status}"
                    )
            users = [
                ("agentvault-vault", "E2E_AGENTVAULT_MQTT_PASSWORD"),
                ("marketpulse-payee", "E2E_MARKETPULSE_MQTT_PASSWORD"),
                *(
                    (identity.mqtt_username, "E2E_QUANTSTREAM_MQTT_PASSWORD")
                    for identity in PAYER_IDENTITIES.values()
                ),
                *(
                    (
                        identity.approver_mqtt_username,
                        "E2E_QUANTSTREAM_UI_MQTT_PASSWORD",
                    )
                    for identity in PAYER_IDENTITIES.values()
                ),
            ]
            for user, password_name in users:
                path = "/api/v5/authentication/password_based%3Abuilt_in_database/users"
                status, _ = self._emqx_api(
                    token,
                    "POST",
                    path,
                    {
                        "user_id": user,
                        "password": self.environment[password_name],
                        "is_superuser": False,
                    },
                )
                if status == 409:
                    status, _ = self._emqx_api(
                        token,
                        "PUT",
                        f"{path}/{user}",
                        {
                            "password": self.environment[password_name],
                            "is_superuser": False,
                        },
                    )
                if status not in {200, 201, 204}:
                    raise RuntimeError(
                        f"EMQX user {user} provisioning failed with HTTP {status}"
                    )
            probe_environment = {
                **self.environment,
                "MQTT_PORT": self.environment["E2E_EMQX_MQTT_PORT"],
                "MQTT_WORKLOAD_USERNAME": self.environment[
                    "E2E_QUANTSTREAM_MQTT_USERNAME"
                ],
                "MQTT_APPROVER_USERNAME": self.environment[
                    "E2E_QUANTSTREAM_UI_MQTT_USERNAME"
                ],
                "MQTT_CLIENT_ID": self.environment["E2E_QUANTSTREAM_CLIENT_ID"],
                "MQTT_APPROVER_PRINCIPAL": self.environment[
                    "E2E_QUANTSTREAM_UI_APPROVER_PRINCIPAL"
                ],
                "MQTT_WORKLOAD_PASSWORD": self.environment[
                    "E2E_QUANTSTREAM_MQTT_PASSWORD"
                ],
                "MQTT_APPROVER_PASSWORD": self.environment[
                    "E2E_QUANTSTREAM_UI_MQTT_PASSWORD"
                ],
                "MQTT_VAULT_PASSWORD": self.environment["E2E_AGENTVAULT_MQTT_PASSWORD"],
                "MQTT_PAYEE_PASSWORD": self.environment[
                    "E2E_MARKETPULSE_MQTT_PASSWORD"
                ],
            }
            E2EProcessHelper.run(
                ["python3", str(Project.E2E_ROOT / "verifier/infra/s02_emqx_acl.py")],
                Project.E2E_ROOT,
                probe_environment,
                90,
            )

        def _target_budget(self) -> None:
            target = Project.REPOSITORY_ROOT / "target"
            if not target.exists():
                return
            output = self._run(["du", "-sm", str(target)], 30)
            if int(output.split()[0]) > 20_480:
                self._run(["cargo", "clean"], 900, Project.REPOSITORY_ROOT)

        def _facilitator_capabilities(self) -> str:
            status, _, raw = self._http(self._url("E2E_FACILITATOR_PORT", "/supported"))
            try:
                encoded = json.dumps(json.loads(raw), sort_keys=True)
            except json.JSONDecodeError as error:
                raise RuntimeError(
                    "facilitator returned invalid /supported JSON"
                ) from error
            if status != 200 or "eip155:31337" not in encoded or "exact" not in encoded:
                raise RuntimeError(
                    "facilitator does not advertise x402 exact on eip155:31337"
                )
            return encoded

    class DockerEnvironment(EnvironmentOperations, E2EDockerCleaner, E2EDeployer):
        backend = "docker"

        def __init__(self, context: VerificationContext) -> None:
            E2EDeployer.__init__(self, context)
            self._wallet_addresses_cache: dict[str, str] | None = None
            self.environment = os.environ.copy()
            E2EImagesDefinitions.configure(self.environment)
            E2ES00InfrastructureVerifier.EnvironmentOperations._inherit_build_proxy_environment(
                self.environment
            )
            for name, value in DEFAULTS.items():
                self.environment.setdefault(name, value)
            if context.worker_slot is None:
                self.project = PROJECT
                self.agentvault_config = RUNTIME_ROOT / "agentvault.yaml"
            else:
                offset = (context.worker_slot + 1) * 100
                self.project = f"{PROJECT}-w{context.worker_slot + 1}"
                self.agentvault_config = (
                    RUNTIME_ROOT / f"agentvault-w{context.worker_slot + 1}.yaml"
                )
                for name in HOST_PORT_NAMES:
                    self.environment[name] = str(int(self.environment[name]) + offset)
            self.environment["E2E_AGENTVAULT_CONFIG_PATH"] = str(self.agentvault_config)
            self.environment.update(context.environment())
            self.docker = E2EDockerDeployer.detect(self.environment)
            self.compose = [
                *E2EDockerDeployer.compose_command(self.docker, self.environment),
                "-p",
                self.project,
                "-f",
                str(COMPOSE_FILE),
            ]

        def _run(
            self,
            command: Iterable[str],
            timeout: float | None = None,
            cwd: Path = Project.E2E_ROOT,
        ) -> str:
            arguments = list(command)
            operation = self._command_operation(arguments)
            started_at = time.monotonic()
            LOGGER.info("START command=%s", operation)
            try:
                output = E2EProcessHelper.run(
                    arguments,
                    cwd,
                    self.environment,
                    timeout or self.context.timeout_seconds,
                )
            except BaseException:
                LOGGER.error(
                    "FAIL command=%s elapsed_seconds=%.1f",
                    operation,
                    time.monotonic() - started_at,
                )
                raise
            LOGGER.info(
                "PASS command=%s elapsed_seconds=%.1f",
                operation,
                time.monotonic() - started_at,
            )
            return output

        def _try(
            self,
            command: Iterable[str],
            timeout: float = 30,
            cwd: Path = Project.E2E_ROOT,
        ) -> tuple[int, str]:
            completed = E2EProcessHelper.execute(
                list(command), cwd, self.environment, timeout
            )
            return completed.returncode, completed.stdout

        def render(self) -> str:
            lines = [self._render_agentvault_config()]
            self._compose(("infra", "payer"), "config", "--quiet")
            self._compose(("infra", "payee"), "config", "--quiet")
            self._compose(("tempo",), "config", "--quiet")
            for payer in LANGUAGES:
                for payee in LANGUAGES:
                    self._full(payer, payee, "config", "--quiet")
            compose_files = sorted(
                self.context.deploy_root.glob("*/docker-compose.yaml")
            )
            charts = [
                Project.component_chart(component)
                for component in Project.DEPLOY_COMPONENTS
            ]
            expected = set(Project.DEPLOY_COMPONENTS)
            compose_components = {path.parent.name for path in compose_files}
            chart_components = {
                "agentvault"
                if path.resolve() == AGENTVAULT_CHART.resolve()
                else path.parent.name
                for path in charts
            }
            if compose_components != expected or chart_components != expected:
                raise RuntimeError(
                    f"paired component manifests drifted: expected={sorted(expected)}, "
                    f"Compose={sorted(compose_components)}, Helm={sorted(chart_components)}"
                )
            for compose in compose_files:
                self._run(
                    [
                        *self.compose[: self.compose.index("-p")],
                        "-f",
                        str(compose),
                        "config",
                        "--quiet",
                    ]
                )
            for chart in charts:
                namespace = self._chart_namespace(chart)
                release_name = self._chart_name(chart)
                values = (
                    (AGENTVAULT_VALUES,)
                    if chart.resolve() == AGENTVAULT_CHART.resolve()
                    else ()
                )
                flags = self._chart_image_flags(chart, values)
                flags += [item for path in values for item in ("--values", str(path))]
                self._run(
                    ["helm", "lint", "--namespace", namespace, str(chart), *flags]
                )
                self._run(
                    [
                        "helm",
                        "template",
                        release_name,
                        str(chart),
                        "--namespace",
                        namespace,
                        *flags,
                    ]
                )
            lines.append(
                f"validated {len(compose_files)} component Compose manifests and Helm charts"
            )
            return "\n".join(lines)

        def cleanup(self) -> str:
            return self.cleanup_owned()

        def cleanup_images(self) -> str:
            images = self._owned_images()
            if not images:
                return ""
            if images == ["<image inventory unavailable>"]:
                raise RuntimeError("E2E cleanup could not inventory its owned images")
            status, output = self._try([*self.docker, "image", "rm", *images], 180)
            remaining = self._owned_images()
            if remaining:
                raise RuntimeError(
                    f"E2E cleanup could not remove owned images {remaining}; exit={status}:\n{output[-4096:]}"
                )
            return output

        def _owned_images(self) -> list[str]:
            status, output = self._try(
                [
                    *self.docker,
                    "image",
                    "ls",
                    "--format",
                    "{{.Repository}}:{{.Tag}}",
                ]
            )
            if status != 0:
                return ["<image inventory unavailable>"]
            images: list[str] = []
            for line in output.splitlines():
                reference = line.strip()
                canonical = reference.removeprefix("localhost/")
                if canonical in E2EImagesDefinitions.local_image_references():
                    images.append(reference)
            return sorted(set(images))

        def smoke(self, role: str) -> str:
            self.deploy_dependencies(role)
            self.initialize_dependencies(role)
            return self.verify_dependencies(role)

        def _dependency_up(self, role: str, *, include_solana: bool = True) -> None:
            self._dependency_deploy(role, include_solana=include_solana)
            self._dependency_initialize(role, include_solana=include_solana)

        def _dependency_deploy(self, role: str, *, include_solana: bool = True) -> None:
            services = ["anvil", "postgres", "emqx", "jaeger"]
            if include_solana:
                services.insert(1, "solana")
            self._compose(("infra", role), "up", "-d", *services)
            if role == "payee":
                self._compose(("infra", role), "up", "-d", "--no-deps", "facilitator")

        def _dependency_initialize(
            self, role: str, *, include_solana: bool = True
        ) -> None:
            self._wait_rpc(self._url("E2E_ANVIL_PORT"), "eth_chainId", "0x7a69", 60)
            if include_solana:
                self._wait_rpc(self._url("E2E_SOLANA_RPC_PORT"), "getHealth", "ok", 90)
            self._compose(("infra", role), "up", "--no-deps", "evm-fixture")
            self._bootstrap_emqx()
            if role == "payee":
                self._wait_http(self._url("E2E_FACILITATOR_PORT", "/health"), 60)

        def deploy_dependencies(self, role: str) -> None:
            self.cleanup()
            self._dependency_deploy(role)

        def initialize_dependencies(self, role: str) -> None:
            self._dependency_initialize(role)

        def verify_dependencies(self, role: str) -> str:
            return self._dependency_health(role)

        def _dependency_health(self, role: str, *, include_solana: bool = True) -> str:
            profiles = ("infra", role)
            self._wait_rpc(self._url("E2E_ANVIL_PORT"), "eth_chainId", "0x7a69", 5)
            if include_solana:
                self._wait_rpc(self._url("E2E_SOLANA_RPC_PORT"), "getHealth", "ok", 5)
            self._compose(
                profiles,
                "exec",
                "-T",
                "postgres",
                "pg_isready",
                "-U",
                "agentvault",
                "-d",
                "agentvault",
            )
            self._compose(
                profiles, "exec", "-T", "emqx", "/opt/emqx/bin/emqx", "ctl", "status"
            )
            self._wait_http(self._url("E2E_JAEGER_UI_PORT"), 5)
            if role == "payee":
                self._facilitator_capabilities()
            else:
                output = self._compose(profiles, "ps", "facilitator")
                if "Up" in output:
                    raise RuntimeError(
                        "payer profile unexpectedly started the facilitator"
                    )
            chains = "Anvil and Solana" if include_solana else "Anvil"
            return f"dependency {role} smoke passed; real {chains}, PostgreSQL, EMQX and Jaeger are healthy"

        def _solana_command(
            self, executable: str, *arguments: str, timeout: float = 60
        ) -> str:
            if executable not in {"solana", "solana-keygen", "spl-token"}:
                raise RuntimeError(f"unsupported Solana fixture command: {executable}")
            output = self._compose(
                ("payer",),
                "exec",
                "-T",
                "solana",
                executable,
                *arguments,
                timeout=timeout,
            )
            return E2ES00InfrastructureVerifier.EnvironmentOperations._clean_compose_stdout(
                output
            )

        def _solana_keypair_json(self, keypair_path: str) -> str:
            output = E2ES00InfrastructureVerifier.EnvironmentOperations._clean_compose_stdout(
                self._compose(
                    ("payer",),
                    "exec",
                    "-T",
                    "solana",
                    "/bin/cat",
                    keypair_path,
                )
            )
            try:
                keypair = json.loads(output)
            except json.JSONDecodeError as error:
                raise RuntimeError(
                    "Solana fixture keypair is not valid JSON"
                ) from error
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
                raise RuntimeError(
                    "Solana fixture keypair has an invalid byte representation"
                )
            return json.dumps(keypair, separators=(",", ":"))

        def _build_images(self, payer: str, payee: str) -> None:
            if self.context.skip_build:
                return
            self._target_budget()
            targets = [
                *E2ES00InfrastructureVerifier.EnvironmentOperations._agentvault_build_targets(
                    self.environment
                ),
                f"marketpulse-{payee}",
                "admin-ui",
                f"quantstream-{payer}",
            ]
            arguments = ["build"]
            if not self.context.reuse_build:
                arguments.append("--pull")
            self._full(payer, payee, *arguments, *targets)

        def _build_matrix_images(self, languages: tuple[str, ...]) -> None:
            if self.context.skip_build:
                return
            self._target_budget()
            targets = [
                *E2ES00InfrastructureVerifier.EnvironmentOperations._agentvault_build_targets(
                    self.environment
                ),
                "admin-ui",
                *(f"marketpulse-{language}" for language in languages),
                *(f"quantstream-{language}" for language in languages),
            ]
            arguments = ["build"]
            if not self.context.reuse_build:
                arguments.append("--pull")
            self._compose(
                E2ES00InfrastructureVerifier.EnvironmentOperations._matrix_build_profiles(
                    languages
                ),
                *arguments,
                *targets,
            )

        def _sql(self, payer: str, payee: str, query: str) -> str:
            return E2ES00InfrastructureVerifier.EnvironmentOperations._clean_compose_stdout(
                self._full(
                    payer,
                    payee,
                    "exec",
                    "-T",
                    "postgres",
                    "psql",
                    "-U",
                    "agentvault",
                    "-d",
                    "agentvault",
                    "-Atc",
                    query,
                )
            )

        def _service_logs(
            self, payer: str, payee: str, payer_logs: str
        ) -> dict[str, str]:
            """Collect stable evidence without depending on removed `run --rm` containers."""
            return {
                "agentvault": self._full(
                    payer, payee, "logs", "--no-color", "agentvault"
                ),
                "payer": payer_logs,
                "payee": self._full(
                    payer, payee, "logs", "--no-color", f"marketpulse-{payee}"
                ),
                "facilitator": self._full(
                    payer, payee, "logs", "--no-color", "facilitator"
                ),
            }

        def _diagnostics(self, payer: str, payee: str) -> None:
            commands = (
                [*self.compose, "ps"],
                [
                    *self.compose,
                    "logs",
                    "--no-color",
                    "--tail=200",
                    "agentvault",
                    f"marketpulse-{payee}",
                    "facilitator",
                    "emqx",
                    "postgres",
                    "anvil",
                    f"quantstream-{payer}",
                ],
            )
            for command in commands:
                status, output = self._try(command, 60)
                print(f"diagnostic exit={status}\n{output}", flush=True)

    class KubernetesEnvironment(
        E2EKubernetesDeployer, EnvironmentOperations, E2EDeployer
    ):
        backend = "kubernetes"
        payment_namespace = PAYMENT_NAMESPACE

        def __init__(self, context: VerificationContext) -> None:
            E2EDeployer.__init__(self, context)
            if context.worker_slot is not None:
                raise ValueError(
                    "Kubernetes E2E uses one shared cluster and does not accept worker slots"
                )
            self._wallet_addresses_cache: dict[str, str] | None = None
            self.environment = os.environ.copy()
            E2EImagesDefinitions.configure(self.environment)
            E2ES00InfrastructureVerifier.EnvironmentOperations._inherit_build_proxy_environment(
                self.environment
            )
            for name, value in DEFAULTS.items():
                self.environment.setdefault(name, value)
            self.environment.update(context.environment())
            self.images = {
                key: self.environment.get(
                    E2EImagesDefinitions.COMPONENT_ENV[key], image
                )
                for key, image in E2EImagesDefinitions.LOCAL_IMAGES.items()
            }
            for key, image in self.images.items():
                self.environment[E2EImagesDefinitions.COMPONENT_ENV[key]] = image
            self.environment.setdefault("COMPOSE_BAKE", "false")
            self.agentvault_config = RUNTIME_ROOT / "agentvault-kubernetes.yaml"
            self.environment["E2E_AGENTVAULT_CONFIG_PATH"] = str(self.agentvault_config)

            self.kubeconfig = E2EKubernetesDeployer.LocalImageLoader.kubeconfig()
            if not any(
                path.is_file()
                for path in E2EKubernetesDeployer.LocalImageLoader.config_files(
                    self.kubeconfig
                )
            ):
                raise RuntimeError(
                    f"Kubernetes deployer kubeconfig does not exist: {self.kubeconfig}"
                )
            self.environment["KUBECONFIG"] = str(self.kubeconfig)
            self._kubectl_executable = shutil.which("kubectl")
            self._helm_executable = shutil.which("helm")
            if self._kubectl_executable is None or self._helm_executable is None:
                raise RuntimeError("Kubernetes deployer requires kubectl and helm")

            self._image_loader = E2EKubernetesDeployer.LocalImageLoader.detect(
                self._privileged_command(self._kubectl_executable), self.environment
            )
            self.docker = self._image_loader.docker
            self.compose = [
                *self._image_loader.compose(),
                "-p",
                "agentvault-e2e-kubernetes-build",
                "-f",
                str(COMPOSE_FILE),
            ]

        def _run(
            self,
            command: Sequence[str],
            timeout: float | None = None,
            cwd: Path = Project.E2E_ROOT,
        ) -> str:
            operation = (
                E2ES00InfrastructureVerifier.EnvironmentOperations._command_operation(
                    list(command)
                )
            )
            started_at = time.monotonic()
            LOGGER.info("START command=%s backend=kubernetes", operation)
            try:
                output = E2EProcessHelper.run(
                    command,
                    cwd,
                    self.environment,
                    timeout or self.context.timeout_seconds,
                )
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

        def render(self) -> str:
            lines = [self._render_agentvault_config()]
            for manifest in (NAMESPACE_MANIFEST, *SHARED_KUBERNETES_MANIFESTS):
                self._kubectl("apply", "--dry-run=client", "--filename", str(manifest))
            charts = [
                Project.component_chart(component)
                for component in Project.DEPLOY_COMPONENTS
            ]
            for chart in charts:
                namespace = (
                    E2ES00InfrastructureVerifier.EnvironmentOperations._chart_namespace(
                        chart
                    )
                )
                release = (
                    E2ES00InfrastructureVerifier.EnvironmentOperations._chart_name(
                        chart
                    )
                )
                flags = self._chart_image_flags(
                    chart, self._shared_overrides_for_chart(chart)
                )
                self._helm("lint", "--namespace", namespace, str(chart), *flags)
                arguments = ["template", release, str(chart), "--namespace", namespace]
                for value_file in self._shared_overrides_for_chart(chart):
                    arguments.extend(("--values", str(value_file)))
                self._helm(*arguments, *flags)
            lines.append(
                f"validated Kubernetes rendering for {len(charts)} component charts including production AgentVault"
            )
            return "\n".join(lines)

        def cleanup(self) -> str:
            return E2EKubernetesCleaner(
                self.context.timeout_seconds,
                kubeconfig=self.kubeconfig,
            ).cleanup()

        def cleanup_images(self) -> str:
            removed: list[str] = []
            cluster_images = self._cluster_images()
            for reference in sorted(
                set(E2EImagesDefinitions.LOCAL_IMAGES.values())
                & set(self.images.values())
            ):
                if reference in cluster_images and not self._image_loader.shared_docker:
                    completed = E2EProcessHelper.execute(
                        [*self._image_loader.ctr, "images", "remove", reference],
                        Project.E2E_ROOT,
                        self.environment,
                        60,
                    )
                    if completed.returncode == 0:
                        removed.append(f"cluster:{reference}")
                completed = E2EProcessHelper.execute(
                    [*self.docker, "image", "rm", reference],
                    Project.E2E_ROOT,
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
                profile_args = [
                    item for profile in profiles for item in ("--profile", profile)
                ]
                targets = [
                    *E2ES00InfrastructureVerifier.EnvironmentOperations._agentvault_build_targets(
                        self.environment
                    ),
                    f"marketpulse-{payee}",
                    "admin-ui",
                    f"quantstream-{payer}",
                ]
                build_args = [*self.compose, *profile_args, "build"]
                if not self.context.reuse_build:
                    build_args.append("--pull")
                for target in targets:
                    self._run([*build_args, target], self.context.timeout_seconds)
            for key in keys:
                self._ensure_cluster_image(self.images[key])

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
                profile_args = [
                    item for profile in profiles for item in ("--profile", profile)
                ]
                targets = [
                    *E2ES00InfrastructureVerifier.EnvironmentOperations._agentvault_build_targets(
                        self.environment
                    ),
                    "admin-ui",
                    *(f"marketpulse-{language}" for language in languages),
                    *(f"quantstream-{language}" for language in languages),
                ]
                build_args = [*self.compose, *profile_args, "build"]
                if not self.context.reuse_build:
                    build_args.append("--pull")
                for target in targets:
                    self._run([*build_args, target], self.context.timeout_seconds)
            keys = (
                "agentvault",
                "admin-ui",
                *(f"payee-{item}" for item in languages),
                *(f"payer-{item}" for item in languages),
            )
            for key in keys:
                self._ensure_cluster_image(self.images[key])

        def _apply_namespaces(self) -> None:
            self._kubectl("apply", "--filename", str(NAMESPACE_MANIFEST))

        def _apply_secret(
            self, namespace: str, name: str, values: Mapping[str, str]
        ) -> None:
            if not values or any(
                not key or "\n" in value for key, value in values.items()
            ):
                raise ValueError(
                    f"invalid Kubernetes E2E Secret payload for {namespace}/{name}"
                )
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

        def _apply_config_map(
            self, namespace: str, name: str, values: Mapping[str, str]
        ) -> None:
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
                "postgres-password": self.environment.get(
                    "E2E_POSTGRES_PASSWORD", "agentvault-e2e-only"
                ),
                "emqx-dashboard-password": self.environment[
                    "E2E_EMQX_DASHBOARD_PASSWORD"
                ],
                "facilitator-private-key": self.environment.get(
                    "E2E_FACILITATOR_PRIVATE_KEY",
                    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
                ),
                "mqtt-workload-password": self.environment[
                    "E2E_QUANTSTREAM_MQTT_PASSWORD"
                ],
                "mqtt-approver-password": self.environment[
                    "E2E_QUANTSTREAM_UI_MQTT_PASSWORD"
                ],
                "admin-ui-token": self.environment.get(
                    "E2E_QUANTSTREAM_UI_TOKEN",
                    "quantstream-e2e-ui-token-0000000000000000001",
                ),
            }
            for namespace in (INFRA_NAMESPACE, PAYMENT_NAMESPACE, PAYER_NAMESPACE):
                self._apply_secret(namespace, "marketpulse-e2e-secrets", shared)
            for (
                language,
                chart_language,
            ) in E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES.items():
                self._apply_secret(
                    PAYEE_NAMESPACE,
                    f"marketpulse-{chart_language}-e2e-secrets",
                    {
                        "agentvault-mqtt-password": self.environment[
                            "E2E_MARKETPULSE_MQTT_PASSWORD"
                        ]
                    },
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
                "HELPER_SECRET_MQTT_PASSWORD": self.environment[
                    "E2E_AGENTVAULT_MQTT_PASSWORD"
                ],
                "AUTHGUARD__AUTHZ__SCOPE_DELIVERY__DIRECT_CONTEXT_HMAC_KEY": "agentvault-e2e-authguard-disabled-key",
                **wallet_values,
            }
            self._apply_secret(
                AGENTVAULT_NAMESPACE, "agentvault-secrets", agentvault_secret
            )
            resource_fixture = Project.AGENTVAULT_CONFIG / "init.import.yaml"
            resource_manifest = resource_fixture.read_text(encoding="utf-8")
            if payee_language is not None:
                if payee_language not in E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES:
                    raise ValueError(f"unsupported payee language: {payee_language}")
                payee_name = self._payee_name(payee_language)
                marketpulse_origin = (
                    f"http://{payee_name}.{PAYEE_NAMESPACE}.svc.cluster.local:8081"
                )
                resource_manifest = ConsoleResourceFixture.render_agentvault_resources(
                    resource_fixture, marketpulse_origin
                )
            self._apply_config_map(
                AGENTVAULT_NAMESPACE,
                "agentvault-e2e-resources",
                {"resources.yaml": resource_manifest},
            )

            evm_root = self.context.deploy_root / "evm" / "source"
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
            selected = dict(set_strings or {})
            defaults = chart / "values.yaml"
            if defaults.is_file():
                values = (defaults, *values)
            for value_file in values:
                if value_file.is_file():
                    shared = dict(
                        E2EImagesDefinitions.sidecar_images(
                            yaml.safe_load(value_file.read_text()), self.environment
                        )
                    )
                    selected = {**shared, **selected}
            for key, reference in tuple(selected.items()):
                if key.endswith(".image"):
                    selected[key] = self._prepare_deployment_image(reference)
            if chart.parent.name == "solana" and "image.repository" not in selected:
                self.environment["E2E_SOLANA_IMAGE"] = self._image_loader.solana_image(
                    self._image_loader.docker,
                    self.environment,
                    self._image_loader.platform,
                )
            component = E2EImagesDefinitions.COMPONENT_KEYS.get(chart.parent.name)
            selected = {
                **{
                    f"env.{name}": value
                    for name, value in E2EImagesDefinitions.runtime_environment(
                        component, self.environment
                    ).items()
                },
                **selected,
            }
            if component and not any(
                key.endswith("image.repository") for key in selected
            ):
                reference = E2EImagesDefinitions.reference(component, self.environment)
                selected = {
                    **self._image_reference_values(
                        self._prepare_deployment_image(reference)
                    ),
                    **selected,
                }
            set_strings = selected
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
            return Project.component_chart(component)

        @staticmethod
        def _shared_overrides_for_chart(chart: Path) -> tuple[Path, ...]:
            if chart.resolve() == AGENTVAULT_CHART.resolve():
                return (AGENTVAULT_VALUES,)
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
                self._helm_install(
                    release, self._component_chart(release), INFRA_NAMESPACE
                )
            self._helm_install(
                "anvil", self._component_chart("anvil"), PAYMENT_NAMESPACE
            )
            if include_solana:
                self._helm_install(
                    "solana", self._component_chart("solana"), PAYMENT_NAMESPACE
                )
            if role == "payee":
                self._helm_install(
                    "x402-facilitator",
                    self._component_chart("facilitator"),
                    PAYMENT_NAMESPACE,
                    wait=False,
                )

        def _initialize_base(self, role: str = "payee") -> None:
            self._helm_install(
                "evm-fixture",
                self._component_chart("evm"),
                PAYMENT_NAMESPACE,
                wait_for_jobs=True,
            )

            with self._runtime_forwards(("emqx-mqtt", "emqx-dashboard")):
                self._bootstrap_emqx()
            if role == "payee":
                # The official facilitator requires the initialized Permit2 contracts.
                # Deployment must not wait on readiness before init creates them.
                self._kubectl(
                    "rollout",
                    "restart",
                    "deployment/x402-facilitator",
                    "--namespace",
                    PAYMENT_NAMESPACE,
                )
                self._kubectl(
                    "rollout",
                    "status",
                    "deployment/x402-facilitator",
                    "--namespace",
                    PAYMENT_NAMESPACE,
                    "--timeout=90s",
                    timeout=100,
                )

        def _image_values(self, key: str, prefix: str = "image") -> dict[str, str]:
            return self._image_reference_values(self.images[key], prefix)

        @staticmethod
        def _image_reference_values(
            reference: str, prefix: str = "image"
        ) -> dict[str, str]:
            return E2EImagesDefinitions.helm_values(reference, prefix)

        @staticmethod
        def _business_claim(role: str, language: str) -> str:
            if role not in {"payer", "payee"} or language not in LANGUAGES:
                raise ValueError("invalid business evidence owner")
            return f"business-{role}-{language}"

        def _apply_business_claims(self, payer: str, payee: str) -> None:
            for role, language, namespace in (
                ("payer", payer, PAYER_NAMESPACE),
                ("payee", payee, PAYEE_NAMESPACE),
            ):
                self._apply_document(
                    {
                        "apiVersion": "v1",
                        "kind": "PersistentVolumeClaim",
                        "metadata": {
                            "name": self._business_claim(role, language),
                            "namespace": namespace,
                            "labels": {
                                "app.kubernetes.io/part-of": "agentvault-e2e",
                                BUSINESS_EVIDENCE_LABEL: "true",
                            },
                        },
                        "spec": {
                            "accessModes": ["ReadWriteOnce"],
                            "resources": {"requests": {"storage": "64Mi"}},
                        },
                    }
                )

        def _install_agentvault(self) -> None:
            self._ensure_cluster_image(self.images["agentvault"])
            self._helm_install(
                "agentvault",
                AGENTVAULT_CHART,
                AGENTVAULT_NAMESPACE,
                values=(AGENTVAULT_VALUES,),
                set_strings=self._image_values(
                    "agentvault", "agentvault.backend.image"
                ),
            )

        def _payee_chart(self, language: str) -> Path:
            chart_language = E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES[language]
            return self._component_chart(
                f"mock-payee-marketpulse-{chart_language}-service"
            )

        def _payer_chart(self, language: str) -> Path:
            chart_language = E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES[language]
            return self._component_chart(
                f"mock-payer-quantstream-service-{chart_language}"
            )

        def _payee_name(self, language: str) -> str:
            return f"marketpulse-{E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES[language]}"

        def _payer_name(self, language: str) -> str:
            return f"quantstream-{E2EImagesDefinitions.LANGUAGE_IMAGE_NAMES[language]}"

        def _install_payee(self, language: str) -> None:
            self._ensure_cluster_image(self.images[f"payee-{language}"])
            name = self._payee_name(language)
            fqdn = f"{name}.{PAYEE_NAMESPACE}.svc.cluster.local"
            chart = self._payee_chart(language)
            self._helm_install(
                name,
                chart,
                PAYEE_NAMESPACE,
                values=self._shared_overrides_for_chart(chart),
                set_strings={
                    **self._image_values(f"payee-{language}"),
                    "persistence.existingClaim": self._business_claim(
                        "payee", language
                    ),
                    "env.AGENTVAULT_TRANSPORT": self.context.transport,
                    "env.AGENTVAULT_GRPC_ENDPOINT": "127.0.0.1:50051",
                    "env.MARKETPULSE_RESOURCE_URL": f"http://{fqdn}:8081/mcp",
                    "env.MARKETPULSE_ORIGIN": f"http://{fqdn}:8081",
                },
            )

        def _install_admin_ui(self, payer: str) -> None:
            self._ensure_cluster_image(self.images["admin-ui"])
            identity = payer_identity(payer)
            self._helm_install(
                "quantstream-ui",
                self._component_chart("mock-payer-quantstream-ui"),
                PAYER_NAMESPACE,
                set_strings={
                    **self._image_values("admin-ui"),
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

        @contextmanager
        def _forward_service(
            self, namespace: str, service: str, remote_port: int
        ) -> Iterator[int]:
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
                cwd=Project.E2E_ROOT,
                env=self.environment,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    stderr = process.communicate(timeout=5)[1]
                    raise RuntimeError(
                        f"kubectl port-forward exited early: {stderr[-2048:]}"
                    )
                try:
                    with socket.create_connection(
                        ("127.0.0.1", local_port), timeout=0.5
                    ):
                        break
                except OSError:
                    time.sleep(0.2)
            else:
                process.terminate()
                raise RuntimeError(
                    f"timed out forwarding {namespace}/{service}:{remote_port}"
                )
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
                "postgres": "E2E_POSTGRES_PORT",
                "agentvault-grpc": "E2E_AGENTVAULT_GRPC_PORT",
                "admin-ui": "E2E_QUANTSTREAM_UI_PORT",
            }
            previous: dict[str, str | None] = {}
            with ExitStack() as stack:
                for name in names:
                    if name.startswith("payee:"):
                        language = name.partition(":")[2]
                        namespace, service, remote_port = (
                            PAYEE_NAMESPACE,
                            self._payee_name(language),
                            8081,
                        )
                        environment_name = "E2E_MARKETPULSE_PORT"
                    else:
                        namespace, service, remote_port = SERVICE_NAMES[name]
                        environment_name = environment_names[name]
                    port = stack.enter_context(
                        self._forward_service(namespace, service, remote_port)
                    )
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

        def deploy_dependencies(self, role: str) -> None:
            self.cleanup()
            self._deploy_base(role, include_solana=True)

        def initialize_dependencies(self, role: str) -> None:
            self._initialize_base(role)

        def smoke(self, role: str) -> str:
            self.deploy_dependencies(role)
            self.initialize_dependencies(role)
            return self.verify_dependencies(role)

        def verify_dependencies(self, role: str) -> str:
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

        def _solana_command(
            self, executable: str, *arguments: str, timeout: float = 60
        ) -> str:
            if executable not in {"solana", "solana-keygen", "spl-token"}:
                raise RuntimeError(f"unsupported Solana fixture command: {executable}")
            return self._kubectl(
                "exec",
                "--namespace",
                PAYMENT_NAMESPACE,
                "deployment/solana-validator",
                "--",
                executable,
                *arguments,
                timeout=timeout,
            ).strip()

        def _solana_keypair_json(self, keypair_path: str) -> str:
            output = self._kubectl(
                "exec",
                "--namespace",
                PAYMENT_NAMESPACE,
                "deployment/solana-validator",
                "--",
                "/bin/cat",
                keypair_path,
            ).strip()
            try:
                keypair = json.loads(output)
            except json.JSONDecodeError as error:
                raise RuntimeError(
                    "Solana fixture keypair is not valid JSON"
                ) from error
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
                raise RuntimeError(
                    "Solana fixture keypair has an invalid byte representation"
                )
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

        def _deployment_logs(
            self, namespace: str, deployment: str, container: str | None = None
        ) -> str:
            arguments = [
                "logs",
                "--namespace",
                namespace,
                f"deployment/{deployment}",
                "--tail=5000",
            ]
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
                timeout=15,
            )

        def _service_logs(
            self, payer: str, payee: str, payer_logs: str
        ) -> dict[str, str]:
            return {
                "agentvault": self._deployment_logs(
                    AGENTVAULT_NAMESPACE, "agentvault", "agentvault"
                ),
                "payer": payer_logs,
                "payee": self._deployment_logs(
                    PAYEE_NAMESPACE, self._payee_name(payee), self._payee_name(payee)
                ),
                "facilitator": self._deployment_logs(
                    PAYMENT_NAMESPACE, "x402-facilitator"
                ),
            }

        def _uninstall_release(self, namespace: str, release: str) -> None:
            completed = E2EProcessHelper.execute(
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
                Project.E2E_ROOT,
                self.environment,
                40,
            )
            if completed.returncode != 0:
                raise RuntimeError(
                    f"could not reset Helm release {namespace}/{release}:\n{completed.stdout}"
                )

        def _payer_values(self, payer: str, payee: str) -> dict[str, str]:
            identity = payer_identity(payer)
            payee_service = self._payee_name(payee)
            return {
                **self._image_values(f"payer-{payer}"),
                "persistence.existingClaim": self._business_claim("payer", payer),
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

        def _start_payer_job(
            self, payer: str, payee: str, *, failure_once: bool
        ) -> str:
            self._ensure_cluster_image(self.images[f"payer-{payer}"])
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
            if timeout_seconds <= 0 or not math.isfinite(timeout_seconds):
                raise ValueError("Job timeout must be finite and positive")
            deadline = time.monotonic() + timeout_seconds
            observation = "Job status has not been observed"
            while (remaining := deadline - time.monotonic()) > 0:
                try:
                    completed = E2EProcessHelper.execute(
                        self._privileged_command(
                            self._kubectl_executable or "kubectl",
                            "get",
                            "job",
                            job,
                            "--namespace",
                            PAYER_NAMESPACE,
                            "--output=json",
                            f"--request-timeout={min(10, remaining):.3f}s",
                        ),
                        Project.E2E_ROOT,
                        self.environment,
                        min(15, remaining),
                    )
                except RuntimeError as error:
                    observation = str(error)
                else:
                    if completed.returncode != 0:
                        observation = completed.stdout[-4096:]
                    else:
                        try:
                            document = json.loads(completed.stdout)
                            status = document.get("status", {})
                            if not isinstance(status, dict):
                                raise ValueError("Job status is not an object")
                        except (AttributeError, ValueError):
                            observation = "Job status response is not a JSON object"
                        else:
                            observation = json.dumps(status, sort_keys=True)
                            failed = self._job_terminal_evidence(job, status)
                            if failed is not None:
                                raise RuntimeError(
                                    f"Kubernetes payer Job {job} failed:\n{failed}"
                                )
                            if any(
                                condition.get("type") == "Complete"
                                and condition.get("status") == "True"
                                for condition in status.get("conditions", [])
                            ):
                                return self._job_logs(job)
                if (remaining := deadline - time.monotonic()) > 0:
                    time.sleep(min(2, remaining))
            try:
                logs = self._job_logs(job)
            except RuntimeError as error:
                logs = f"Job logs unavailable: {error}"
            raise RuntimeError(
                f"Kubernetes payer Job {job} did not complete within {timeout_seconds}s; "
                f"last_status={observation}\n{logs}"
            )

        def _job_terminal_evidence(
            self, job: str, status: dict | None = None
        ) -> str | None:
            if status is None:
                completed = E2EProcessHelper.execute(
                    self._privileged_command(
                        self._kubectl_executable or "kubectl",
                        "get",
                        "job",
                        job,
                        "--namespace",
                        PAYER_NAMESPACE,
                        "--output=json",
                        "--request-timeout=10s",
                    ),
                    Project.E2E_ROOT,
                    self.environment,
                    15,
                )
                if completed.returncode != 0:
                    return None
                status = json.loads(completed.stdout).get("status", {})
            # Failed Pod counts alone are not terminal while backoff retries remain.
            failed = next(
                (
                    condition
                    for condition in status.get("conditions", [])
                    if condition.get("type") == "Failed"
                    and condition.get("status") == "True"
                ),
                None,
            )
            if failed is None:
                return None
            try:
                logs = self._job_logs(job)[-4096:]
            except RuntimeError as error:
                logs = f"Job logs unavailable: {error}"
            return f"condition={json.dumps(failed, sort_keys=True)} failed_pods={status.get('failed', 0)} logs={logs!r}"

        def _diagnostic_output(
            self, namespace: str, filename: str, arguments: list[str]
        ) -> str:
            directory = (
                self.context.report_root
                / "payment"
                / "diagnostics"
                / (
                    f"{self.context.payer_language}-{self.context.payee_language}-{self.context.transport}"
                )
                / namespace
            )
            directory.mkdir(parents=True, exist_ok=True)
            try:
                completed = E2EProcessHelper.execute(
                    self._privileged_command(
                        self._kubectl_executable or "kubectl", *arguments
                    ),
                    Project.E2E_ROOT,
                    self.environment,
                    30,
                )
                output = completed.stdout
            except Exception as error:
                output = f"diagnostic command failed: {error}"
            (directory / filename).write_text(output, encoding="utf-8")
            return output

        def _diagnostics(self, payer: str, payee: str) -> None:
            LOGGER.error(
                "collecting Kubernetes diagnostics payer=%s payee=%s", payer, payee
            )
            for namespace in (
                AGENTVAULT_NAMESPACE,
                INFRA_NAMESPACE,
                PAYMENT_NAMESPACE,
                PAYEE_NAMESPACE,
                PAYER_NAMESPACE,
            ):
                self._diagnostic_output(
                    namespace,
                    "resources.txt",
                    ["get", "pods,jobs", "--namespace", namespace, "--output=wide"],
                )
                self._diagnostic_output(
                    namespace,
                    "events.txt",
                    [
                        "get",
                        "events",
                        "--namespace",
                        namespace,
                        "--sort-by=.lastTimestamp",
                    ],
                )
                try:
                    raw = self._kubectl(
                        "get",
                        "pods",
                        "--namespace",
                        namespace,
                        "--output=json",
                        timeout=15,
                    )
                    pods = json.loads(raw).get("items", [])
                except Exception as error:
                    LOGGER.error(
                        "could not inventory diagnostic pods namespace=%s error=%s",
                        namespace,
                        error,
                    )
                    continue
                for pod in pods:
                    name = pod.get("metadata", {}).get("name", "")
                    if not isinstance(name, str) or not re.fullmatch(
                        r"[a-z0-9][a-z0-9.-]+", name
                    ):
                        continue
                    # Status is useful for scheduling/probe failures without exposing Pod env or Secrets.
                    directory = (
                        self.context.report_root
                        / "payment"
                        / "diagnostics"
                        / f"{payer}-{payee}-{self.context.transport}"
                        / namespace
                    )
                    (directory / f"{name}-status.json").write_text(
                        json.dumps(pod.get("status", {}), indent=2) + "\n",
                        encoding="utf-8",
                    )
                    if any(
                        item.get("name") == "postgres"
                        for item in pod.get("status", {}).get("containerStatuses", [])
                    ):
                        self._diagnostic_output(
                            namespace,
                            f"{name}-processes.txt",
                            [
                                "exec",
                                "--namespace",
                                namespace,
                                name,
                                "--container=postgres",
                                "--",
                                "sh",
                                "-c",
                                "grep -HE '^Name:|^State:|^PPid:|^Uid:|^Gid:' /proc/[0-9]*/status; "
                                "grep -H . /proc/[0-9]*/wchan; "
                                "readlink /proc/1/exe; "
                                "ls -ld /var/lib/postgresql/data /var/run/postgresql",
                            ],
                        )
                    for previous in (False, True):
                        arguments = [
                            "logs",
                            "--namespace",
                            namespace,
                            name,
                            "--all-containers=true",
                            "--prefix=true",
                            "--tail=200",
                        ]
                        if previous:
                            arguments.append("--previous")
                        self._diagnostic_output(
                            namespace,
                            f"{name}-{'previous' if previous else 'current'}.log",
                            arguments,
                        )

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
                Project.REPOSITORY_ROOT,
            )

    @staticmethod
    def deploy(context: VerificationContext) -> None:
        if context.scenario in {"payer", "payee"}:
            create_deployer(context).deploy_dependencies(context.scenario)

    @staticmethod
    def init(context: VerificationContext) -> None:
        if context.scenario in {"payer", "payee"}:
            create_deployer(context).initialize_dependencies(context.scenario)

    @staticmethod
    def verify(context: VerificationContext) -> str:
        runtime = create_deployer(context)
        return (
            runtime.verify_dependencies(context.scenario)
            if context.scenario in {"payer", "payee"}
            else runtime.render()
        )

    @staticmethod
    def session(context: VerificationContext):
        runtime = context.require("purchase")["runtime"]
        if context.deployer == "kubernetes":
            session = context.state.setdefault("session", ExitStack())
            if not context.state.get("forwarding"):
                session.enter_context(
                    runtime._runtime_forwards(
                        (
                            "agentvault-api",
                            "agentvault-management",
                            "anvil",
                            "jaeger",
                            f"payee:{context.payee_language}",
                        )
                    )
                )
                context.state["forwarding"] = True
        return runtime

    @staticmethod
    def request(
        context: VerificationContext,
        method: str,
        path: str,
        body=None,
        expected=(200,),
        headers=None,
    ):
        import hashlib

        runtime = E2ES00InfrastructureVerifier.session(context)
        status, _, raw = runtime._http(
            runtime._url("E2E_AGENTVAULT_PORT", "/api/v1" + path),
            method,
            json.dumps(body).encode() if body is not None else None,
            {"content-type": "application/json", **(headers or {})},
        )
        context.state.setdefault("requests", []).append(
            {
                "method": method,
                "path": path,
                "status": status,
                "response_sha256": hashlib.sha256(raw.encode()).hexdigest(),
            }
        )
        if status not in expected:
            raise RuntimeError(
                f"{method} {path}: expected {expected}, got {status}; {raw[:512]}"
            )
        if not raw.strip():
            return None
        try:
            return json.loads(raw)
        except json.JSONDecodeError:
            if status < 400:
                raise RuntimeError(f"successful {method} {path} returned invalid JSON")
            return None

    @staticmethod
    def api_evidence(context: VerificationContext, assertions: list[str]) -> str:
        output = {
            "assertions": assertions,
            "requests": context.state.get("requests", []),
            "scope": "live Vault HTTP API",
        }
        directory = context.report_root / context.scenario
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "evidence.json").write_text(json.dumps(output, indent=2) + "\n")
        return json.dumps(output, sort_keys=True)

    @staticmethod
    def configure(context: VerificationContext, changes: dict):
        runtime = E2ES00InfrastructureVerifier.session(context)
        config = yaml.safe_load(
            yaml.safe_load(AGENTVAULT_VALUES.read_text())["agentvault"][
                "agentvault-config"
            ]
        )
        if runtime.backend == "docker" and runtime.agentvault_config.is_file():
            config = yaml.safe_load(runtime.agentvault_config.read_text())
        for path, value in changes.items():
            node = config
            parts = path.split(".")
            for key in parts[:-1]:
                node = node[key]
            node[parts[-1]] = value
        if runtime.backend == "docker":
            runtime.agentvault_config.write_text(
                yaml.safe_dump(config, sort_keys=False)
            )
            runtime._full(
                context.payer_language, context.payee_language, "restart", "agentvault"
            )
        else:
            session = context.state.pop("session", None)
            if session:
                session.close()
            context.state["forwarding"] = False
            overlay = context.report_root / "configuration-overrides.yaml"
            environment = {
                "AGENTVAULT__SERVER__AUTHGUARD__ENABLED": str(
                    config["server"]["authguard"]["enabled"]
                ).lower()
            }
            overlay.write_text(
                yaml.safe_dump(
                    {
                        "agentvault": {
                            "agentvault-config": yaml.safe_dump(config),
                            "backend": {"env": environment},
                        }
                    }
                )
            )
            runtime._helm_install(
                "agentvault",
                AGENTVAULT_CHART,
                AGENTVAULT_NAMESPACE,
                values=(AGENTVAULT_VALUES, overlay),
                set_strings=runtime._image_values(
                    "agentvault", "agentvault.backend.image"
                ),
            )
            E2ES00InfrastructureVerifier.session(context)
        runtime._wait_http(runtime._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 60)
        return runtime

    @staticmethod
    def transaction_database(context: VerificationContext) -> None:
        runtime = E2ES00InfrastructureVerifier.session(context)
        database = (
            "av_verify_" + context.run_id + "_" + context.scenario.replace("-", "_")
        )
        runtime._sql(
            context.payer_language,
            context.payee_language,
            f"CREATE DATABASE {database}",
        )
        context.state["transaction_database"] = database
        ddl = (
            Project.REPOSITORY_ROOT / "migrations/postgres/001_init.ddl.sql"
        ).read_text()
        dml = (
            Project.REPOSITORY_ROOT / "migrations/postgres/001_init.dml.sql"
        ).read_text()
        E2ES00InfrastructureVerifier.database_sql(context, ddl + "\n" + dml)

    @staticmethod
    def database_sql(context: VerificationContext, statement: str) -> str:
        runtime = E2ES00InfrastructureVerifier.session(context)
        database = context.state["transaction_database"]
        if not re.fullmatch(r"av_verify_[0-9]{14}_[a-z_]+", database):
            raise RuntimeError(
                "transaction database must have an isolated verifier name"
            )
        arguments = [
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "agentvault",
            "-d",
            database,
            "-c",
            statement,
        ]
        if runtime.backend == "docker":
            return runtime._compose(("infra",), "exec", "-T", "postgres", *arguments)
        return runtime._kubectl(
            "exec", "-n", INFRA_NAMESPACE, "deployment/postgres", "--", *arguments
        )

    @staticmethod
    def transaction_contract(
        context: VerificationContext, cases: dict[str, str]
    ) -> str:
        from urllib.parse import quote

        runtime = E2ES00InfrastructureVerifier.session(context)
        environment = runtime.environment.copy()
        database = context.state["transaction_database"]
        directory = context.report_root / context.scenario
        directory.mkdir(parents=True, exist_ok=True)
        if runtime.backend == "kubernetes":
            session = context.state["session"]
            session.enter_context(runtime._runtime_forwards(("postgres",)))
        port = int(environment["E2E_POSTGRES_PORT"])
        if runtime.backend == "kubernetes":
            port = int(runtime.environment["E2E_POSTGRES_PORT"])
        password = quote(
            environment.get("E2E_POSTGRES_PASSWORD", "agentvault-e2e-only"), safe=""
        )
        environment["AGENTVAULT_TEST_POSTGRES_URL"] = (
            f"postgres://agentvault:{password}@127.0.0.1:{port}/{database}"
        )
        environment.setdefault("CARGO_BUILD_JOBS", "2")
        environment.setdefault("CARGO_INCREMENTAL", "0")
        environment.setdefault("CARGO_PROFILE_TEST_DEBUG", "0")
        result = {}
        try:
            for dialect, name in cases.items():
                runtime._target_budget()
                output = E2EProcessHelper.run(
                    [
                        "cargo",
                        "test",
                        "--locked",
                        "--lib",
                        name,
                        "--",
                        "--include-ignored",
                        "--exact",
                        "--nocapture",
                        "--test-threads=1",
                    ],
                    Project.REPOSITORY_ROOT,
                    environment,
                    context.timeout_seconds,
                )
                (directory / f"{dialect}.log").write_text(output)
                if (
                    "1 passed; 0 failed; 0 ignored" not in output
                    or f"test {name} ... ok" not in output
                ):
                    raise RuntimeError(
                        f"transaction scenario did not execute exactly one named {dialect} contract"
                    )
                result[dialect] = name
        finally:
            runtime._sql(
                context.payer_language,
                context.payee_language,
                f"DROP DATABASE IF EXISTS {database} WITH (FORCE)",
            )
        return json.dumps(
            {
                "scope": "real SQL transaction integration; complements live purchase; no additional chain-settlement claim",
                "contracts": result,
            },
            sort_keys=True,
        )
