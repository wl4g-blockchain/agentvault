from __future__ import annotations

import json
import os
import re
import threading
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from dataclasses import replace
from pathlib import Path
from typing import Any, Iterable

from common.context import VerificationContext
from common.payer_identity import LANGUAGES, PAYER_IDENTITIES, payer_identity
from common.process import execute, run
from common.project import E2E_ROOT, REPOSITORY_ROOT
from common.sdk_matrix import TRANSPORTS, matrix_cases, matrix_languages

from .base import E2EDeployer, FullFlowEvidence


MATRIX_REPORTS = frozenset({
    "s20_full.md", "s21_payer_signing.md", "s22_payee_settlement.md",
    "s23_balance_sync.md", "s24_gateway_balance.md",
})
PROJECT = "agentvault-e2e"
COMPOSE_FILE = E2E_ROOT / "docker-compose.yaml"
RUNTIME_ROOT = E2E_ROOT / "reports" / ".runtime"
AGENTVAULT_CHART = REPOSITORY_ROOT / "deploy" / "helm" / "agentvault"
AGENTVAULT_VALUES = E2E_ROOT / "common" / "agentvault-values.yaml"
AGENTVAULT_CONFIG_TEMPLATE = "templates/platform/runtime-config-map.yaml"
TOKEN_CONTRACT = "0x5fbdb2315678afecb367f032d93f642f64180aa3"
PAYEE_ADDRESS = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc"

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
    "E2E_SOLANA_RPC_PORT": "28899",
    "E2E_SOLANA_FAUCET_PORT": "28900",
    "E2E_FACILITATOR_PORT": "38085",
    "E2E_AGENTVAULT_PORT": "38080",
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

HOST_PORT_NAMES = (
    "E2E_POSTGRES_PORT", "E2E_EMQX_MQTT_PORT", "E2E_EMQX_DASHBOARD_PORT",
    "E2E_JAEGER_UI_PORT", "E2E_JAEGER_OTLP_GRPC_PORT", "E2E_JAEGER_OTLP_HTTP_PORT",
    "E2E_ANVIL_PORT", "E2E_SOLANA_RPC_PORT", "E2E_SOLANA_FAUCET_PORT",
    "E2E_FACILITATOR_PORT", "E2E_AGENTVAULT_PORT", "E2E_AGENTVAULT_MGMT_PORT",
    "E2E_MARKETPULSE_PORT", "E2E_QUANTSTREAM_UI_PORT",
)

BUILD_PROXY_PREFIXES = (
    "E2E_AGENTVAULT_BUILD",
    "E2E_MARKETPULSE_BUILD",
    "E2E_QUANTSTREAM_BUILD",
    "E2E_QUANTSTREAM_UI_BUILD",
)

ANSI_ESCAPE_PATTERN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
EVM_ADDRESS_PATTERN = re.compile(r"(?im)^\s*(0x[0-9a-f]{40})\s*$")
UINT_OUTPUT_PATTERN = re.compile(r"(?m)^\s*([0-9]+)(?:\s+\[[^\n]*\])?\s*$")


def _load_matrix_checkpoint(target: Path, name: str) -> dict[str, str] | None:
    if not target.is_file():
        return None
    try:
        payload = json.loads(target.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"invalid matrix checkpoint {target}: {error}") from error
    if payload.get("case") != name:
        raise RuntimeError(f"matrix checkpoint identity mismatch: {target}")
    outputs = payload.get("reports")
    if not isinstance(outputs, dict) or not MATRIX_REPORTS.issubset(outputs):
        raise RuntimeError(f"matrix checkpoint evidence is incomplete: {target}")
    if any(not isinstance(outputs[key], str) or not outputs[key].strip() for key in MATRIX_REPORTS):
        raise RuntimeError(f"matrix checkpoint evidence is empty: {target}")
    return {str(key): str(value) for key, value in outputs.items()}


def _ensure_matrix_plan(target: Path, scope: str) -> dict[str, Any]:
    plan = {
        "scope": scope,
        "languages": list(matrix_languages(scope)),
        "transports": list(TRANSPORTS),
        "cases": [case.name for case in matrix_cases(scope)],
    }
    if target.is_file():
        try:
            stored = json.loads(target.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise RuntimeError(f"invalid matrix plan {target}: {error}") from error
        if stored != plan:
            raise RuntimeError(
                f"matrix scope does not match the immutable resume plan: "
                f"requested={scope}, stored={stored.get('scope') if isinstance(stored, dict) else 'invalid'}"
            )
        return plan
    target.parent.mkdir(parents=True, exist_ok=True)
    temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(plan, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(target)
    return plan


COMPOSE_PROVIDER_PREFIX = ">>>> Executing external compose provider "


def _parse_cast_address(output: str) -> str:
    """Extract the final canonical address from noisy Compose/cast output."""
    matches = EVM_ADDRESS_PATTERN.findall(ANSI_ESCAPE_PATTERN.sub("", output))
    if not matches:
        raise RuntimeError(f"cast output omitted a canonical EVM address: {output[-2048:]}")
    return matches[-1]


def _parse_cast_uint(output: str) -> int:
    """Extract the final uint result without trusting Compose diagnostic prefixes."""
    matches = UINT_OUTPUT_PATTERN.findall(ANSI_ESCAPE_PATTERN.sub("", output))
    if not matches:
        raise RuntimeError(f"cast output omitted an unsigned integer result: {output[-2048:]}")
    return int(matches[-1])


def _clean_compose_stdout(output: str) -> str:
    """Remove the compatibility provider banner without hiding command output."""
    lines = ANSI_ESCAPE_PATTERN.sub("", output).replace("\r", "").splitlines()
    return "\n".join(line for line in lines if not line.strip().startswith(COMPOSE_PROVIDER_PREFIX)).strip()


def _inherit_build_proxy_environment(environment: dict[str, str]) -> None:
    """Map conventional proxy variables into each explicit Compose build boundary."""
    https_proxy = environment.get("HTTPS_PROXY") or environment.get("https_proxy")
    http_proxy = environment.get("HTTP_PROXY") or environment.get("http_proxy")
    no_proxy = environment.get("NO_PROXY") or environment.get("no_proxy")
    for prefix in BUILD_PROXY_PREFIXES:
        if https_proxy:
            environment.setdefault(f"{prefix}_HTTPS_PROXY", https_proxy)
        if http_proxy:
            environment.setdefault(f"{prefix}_HTTP_PROXY", http_proxy)
        if no_proxy:
            environment.setdefault(f"{prefix}_NO_PROXY", no_proxy)


def _agentvault_build_targets(environment: dict[str, str]) -> tuple[str, ...]:
    """Resolve the runtime image build boundary shared by single and matrix runs."""
    if environment.get("E2E_RUNNER_PREBUILT_AGENTVAULT") != "1":
        return ("agentvault",)
    if not environment.get("E2E_AGENTVAULT_IMAGE"):
        raise RuntimeError("prebuilt AgentVault requires E2E_AGENTVAULT_IMAGE")
    return ()


def _matrix_build_profiles(languages: tuple[str, ...]) -> tuple[str, ...]:
    """Activate every profile referenced by the selected matrix build targets."""
    return (
        "infra",
        "payee",
        "apps",
        *(f"payee-{language}" for language in languages),
        *(f"ingestor-{language}" for language in languages),
    )


class DockerE2EDeployer(E2EDeployer):
    """Python-owned Docker Compose lifecycle for the isolated E2E project."""

    backend = "docker"

    def __init__(self, context: VerificationContext) -> None:
        super().__init__(context)
        self._wallet_addresses_cache: dict[str, str] | None = None
        self.environment = os.environ.copy()
        _inherit_build_proxy_environment(self.environment)
        for name, value in DEFAULTS.items():
            self.environment.setdefault(name, value)
        if context.worker_slot is None:
            self.project = PROJECT
            self.agentvault_config = RUNTIME_ROOT / "agentvault.yaml"
        else:
            offset = (context.worker_slot + 1) * 100
            self.project = f"{PROJECT}-w{context.worker_slot + 1}"
            self.agentvault_config = RUNTIME_ROOT / f"agentvault-w{context.worker_slot + 1}.yaml"
            for name in HOST_PORT_NAMES:
                self.environment[name] = str(int(self.environment[name]) + offset)
        self.environment["E2E_AGENTVAULT_CONFIG_PATH"] = str(self.agentvault_config)
        self.environment.update(context.environment())
        probe = execute(["docker", "info"], E2E_ROOT, self.environment, 15)
        self.docker = ["docker"] if probe.returncode == 0 else ["sudo", "-n", "-E", "docker"]
        self.compose = [*self.docker, "compose", "-p", self.project, "-f", str(COMPOSE_FILE)]

    def _run(self, command: Iterable[str], timeout: float | None = None, cwd: Path = E2E_ROOT) -> str:
        return run(list(command), cwd, self.environment, timeout or self.context.timeout_seconds)

    def _try(self, command: Iterable[str], timeout: float = 30, cwd: Path = E2E_ROOT) -> tuple[int, str]:
        completed = execute(list(command), cwd, self.environment, timeout)
        return completed.returncode, completed.stdout

    def _compose(self, profiles: Iterable[str], *arguments: str, timeout: float | None = None) -> str:
        selected: list[str] = []
        for profile in profiles:
            selected.extend(("--profile", profile))
        return self._run([*self.compose, *selected, *arguments], timeout)

    def _full(self, payer: str, payee: str, *arguments: str, timeout: float | None = None) -> str:
        return self._compose(
            ("infra", "payee", "apps", f"payee-{payee}", f"ingestor-{payer}"),
            *arguments,
            timeout=timeout,
        )

    def render(self) -> str:
        lines = [self._render_agentvault_config()]
        self._compose(("infra", "payer"), "config", "--quiet")
        self._compose(("infra", "payee"), "config", "--quiet")
        for payer in LANGUAGES:
            for payee in LANGUAGES:
                self._full(payer, payee, "config", "--quiet")
        compose_files = sorted(self.context.deploy_root.glob("*/docker-compose.yaml"))
        charts = sorted(self.context.deploy_root.glob("*/helm"))
        if len(compose_files) != 18 or len(charts) != len(compose_files):
            raise RuntimeError(
                f"expected 18 paired component manifests, found {len(compose_files)} Compose and {len(charts)} Helm"
            )
        for compose in compose_files:
            self._run([*self.docker, "compose", "-f", str(compose), "config", "--quiet"])
        for index, chart in enumerate(charts, 1):
            self._run(["helm", "lint", str(chart)])
            self._run(["helm", "template", f"agentvault-e2e-{index}", str(chart)])
        lines.append(f"validated {len(compose_files)} component Compose manifests and Helm charts")
        return "\n".join(lines)

    def _render_agentvault_config(self) -> str:
        self._run(["helm", "lint", str(AGENTVAULT_CHART), "-f", str(AGENTVAULT_VALUES)])
        rendered = self._run(
            [
                "helm", "template", "agentvault-e2e", str(AGENTVAULT_CHART),
                "-f", str(AGENTVAULT_VALUES), "--show-only", AGENTVAULT_CONFIG_TEMPLATE,
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
            raise RuntimeError("production Helm chart rendered an empty AgentVault configuration")
        RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
        self.agentvault_config.write_text("\n".join(body) + "\n", encoding="utf-8")
        return f"rendered runtime config from {AGENTVAULT_CHART}"

    def cleanup(self) -> str:
        command = [*self.compose, "down", "--volumes", "--remove-orphans"]
        first = execute(command, E2E_ROOT, self.environment, 180)
        inventory = self._owned_container_inventory()
        if not inventory:
            return first.stdout.strip()

        # Every service shares Anvil's network namespace. Podman Compose can
        # therefore race while removing the namespace anchor and leave exited
        # one-off containers behind. Remove only containers carrying this
        # Compose project's label, dependants first and the anchor last.
        dependants = [container_id for container_id, _, service in inventory if service != "anvil"]
        anchors = [container_id for container_id, _, service in inventory if service == "anvil"]
        removal_output: list[str] = []
        for container_ids in (dependants, anchors):
            if not container_ids:
                continue
            removed = execute([*self.docker, "rm", "--force", *container_ids], E2E_ROOT, self.environment, 60)
            removal_output.append(removed.stdout)

        second = execute(command, E2E_ROOT, self.environment, 180)
        for _ in range(20):
            if not self._owned_containers():
                break
            time.sleep(0.25)
        remaining = self._owned_containers()
        if remaining:
            raise RuntimeError(
                f"E2E cleanup left owned containers {remaining}:\n"
                + first.stdout[-4096:] + "\n" + "".join(removal_output)[-4096:] + "\n"
                + second.stdout[-4096:]
            )
        return (first.stdout + "".join(removal_output) + second.stdout).strip()

    def _owned_container_inventory(self) -> list[tuple[str, str, str]]:
        status, output = self._try([
            *self.docker,
            "ps",
            "-aq",
            "--filter",
            f"label=com.docker.compose.project={self.project}",
        ])
        if status != 0:
            return [("<inventory-unavailable>", "<inventory-unavailable>", "")]
        container_ids = [line.strip() for line in output.splitlines() if line.strip()]
        inventory: list[tuple[str, str, str]] = []
        for container_id in container_ids:
            inspect_status, inspect_output = self._try([
                *self.docker,
                "inspect",
                "--format",
                '{{.Id}}\t{{.Name}}\t{{index .Config.Labels "com.docker.compose.service"}}',
                container_id,
            ])
            if inspect_status != 0:
                inventory.append((container_id, container_id, ""))
                continue
            fields = inspect_output.strip().split("\t")
            if len(fields) != 3:
                inventory.append((container_id, container_id, ""))
                continue
            resolved_id, name, service = fields
            inventory.append((resolved_id, name.lstrip("/"), service))
        return inventory

    def _owned_containers(self) -> list[str]:
        status, output = self._try([
            *self.docker,
            "ps",
            "-a",
            "--filter",
            f"label=com.docker.compose.project={self.project}",
            "--format",
            "{{.Names}}",
        ])
        if status != 0:
            return ["<container inventory unavailable>"]
        return [line.strip() for line in output.splitlines() if line.strip()]

    def smoke(self, role: str) -> str:
        if role not in {"payer", "payee"}:
            raise ValueError(f"unsupported payment role: {role}")
        self.cleanup()
        try:
            self._dependency_up(role)
            return self._dependency_health(role)
        except BaseException:
            self._diagnostics("rust", "rust")
            raise

    def _dependency_up(self, role: str) -> None:
        profiles = ("infra", role)
        self._compose(profiles, "up", "-d", "anvil", "solana", "postgres", "emqx", "jaeger")
        self._wait_rpc(self._url("E2E_ANVIL_PORT"), "eth_chainId", "0x7a69", 60)
        self._wait_rpc(self._url("E2E_SOLANA_RPC_PORT"), "getHealth", "ok", 90)
        self._compose(profiles, "up", "--no-deps", "evm-fixture")
        self._bootstrap_emqx()
        if role == "payee":
            self._compose(profiles, "up", "-d", "--no-deps", "facilitator")
            self._wait_http(self._url("E2E_FACILITATOR_PORT", "/health"), 60)

    def _dependency_health(self, role: str) -> str:
        profiles = ("infra", role)
        self._wait_rpc(self._url("E2E_ANVIL_PORT"), "eth_chainId", "0x7a69", 5)
        self._wait_rpc(self._url("E2E_SOLANA_RPC_PORT"), "getHealth", "ok", 5)
        self._compose(profiles, "exec", "-T", "postgres", "pg_isready", "-U", "agentvault", "-d", "agentvault")
        self._compose(profiles, "exec", "-T", "emqx", "/opt/emqx/bin/emqx", "ctl", "status")
        self._wait_http(self._url("E2E_JAEGER_UI_PORT"), 5)
        if role == "payee":
            self._facilitator_capabilities()
        else:
            output = self._compose(profiles, "ps", "facilitator")
            if "Up" in output:
                raise RuntimeError("payer profile unexpectedly started the facilitator")
        return f"dependency {role} smoke passed; real Anvil, Solana, PostgreSQL, EMQX and Jaeger are healthy"

    def _facilitator_capabilities(self) -> str:
        status, _, raw = self._http(self._url("E2E_FACILITATOR_PORT", "/supported"))
        try:
            encoded = json.dumps(json.loads(raw), sort_keys=True)
        except json.JSONDecodeError as error:
            raise RuntimeError("facilitator returned invalid /supported JSON") from error
        if status != 200 or "eip155:31337" not in encoded or "exact" not in encoded:
            raise RuntimeError("facilitator does not advertise x402 exact on eip155:31337")
        return encoded

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
        request = urllib.request.Request(url, data=body, method=method, headers=headers or {})
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return response.status, dict(response.headers.items()), response.read().decode("utf-8")
        except urllib.error.HTTPError as error:
            return error.code, dict(error.headers.items()), error.read().decode("utf-8", "replace")

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

    def _wait_rpc(self, url: str, method: str, expected: object, attempts: int) -> None:
        body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": []}).encode()
        for _ in range(attempts):
            try:
                status, _, raw = self._http(url, "POST", body, {"content-type": "application/json"}, 2)
                if status == 200 and json.loads(raw).get("result") == expected:
                    return
            except (OSError, ValueError):
                pass
            time.sleep(1)
        raise RuntimeError(f"timed out waiting for {method} at {url}")

    def _emqx_api(self, token: str, method: str, path: str, body: dict[str, object] | None = None) -> tuple[int, str]:
        payload = None if body is None else json.dumps(body, separators=(",", ":")).encode()
        status, _, raw = self._http(
            self._url("E2E_EMQX_DASHBOARD_PORT", path), method, payload,
            {"authorization": f"Bearer {token}", "content-type": "application/json"},
        )
        return status, raw

    def _bootstrap_emqx(self) -> None:
        self._wait_http(self._url("E2E_EMQX_DASHBOARD_PORT", "/status"), 90)
        status, _, raw = self._http(
            self._url("E2E_EMQX_DASHBOARD_PORT", "/api/v5/login"), "POST",
            json.dumps({"username": "admin", "password": self.environment["E2E_EMQX_DASHBOARD_PASSWORD"]}).encode(),
            {"content-type": "application/json"},
        )
        if status != 200 or not isinstance(json.loads(raw).get("token"), str):
            raise RuntimeError(f"EMQX dashboard authentication failed with HTTP {status}")
        token = json.loads(raw)["token"]
        status, authentication = self._emqx_api(token, "GET", "/api/v5/authentication")
        if status != 200:
            raise RuntimeError(f"read EMQX authentication configuration failed with HTTP {status}")
        if "password_based:built_in_database" not in authentication:
            status, _ = self._emqx_api(token, "POST", "/api/v5/authentication", {
                "mechanism": "password_based", "backend": "built_in_database", "user_id_type": "username",
                "password_hash_algorithm": {"name": "sha256", "salt_position": "suffix"},
            })
            if status not in {200, 201, 204, 409}:
                raise RuntimeError(f"create EMQX password authenticator failed with HTTP {status}")
        users = [
            ("agentvault-vault", "E2E_AGENTVAULT_MQTT_PASSWORD"),
            ("marketpulse-payee", "E2E_MARKETPULSE_MQTT_PASSWORD"),
            *((identity.mqtt_username, "E2E_QUANTSTREAM_MQTT_PASSWORD")
              for identity in PAYER_IDENTITIES.values()),
            *((identity.approver_mqtt_username, "E2E_QUANTSTREAM_UI_MQTT_PASSWORD")
              for identity in PAYER_IDENTITIES.values()),
        ]
        for user, password_name in users:
            path = "/api/v5/authentication/password_based%3Abuilt_in_database/users"
            status, _ = self._emqx_api(token, "POST", path, {
                "user_id": user, "password": self.environment[password_name], "is_superuser": False,
            })
            if status == 409:
                status, _ = self._emqx_api(token, "PUT", f"{path}/{user}", {
                    "password": self.environment[password_name], "is_superuser": False,
                })
            if status not in {200, 201, 204}:
                raise RuntimeError(f"EMQX user {user} provisioning failed with HTTP {status}")
        probe_environment = {
            **self.environment,
            "MQTT_PORT": self.environment["E2E_EMQX_MQTT_PORT"],
            "MQTT_WORKLOAD_USERNAME": self.environment["E2E_QUANTSTREAM_MQTT_USERNAME"],
            "MQTT_APPROVER_USERNAME": self.environment["E2E_QUANTSTREAM_UI_MQTT_USERNAME"],
            "MQTT_CLIENT_ID": self.environment["E2E_QUANTSTREAM_CLIENT_ID"],
            "MQTT_APPROVER_PRINCIPAL": self.environment["E2E_QUANTSTREAM_UI_APPROVER_PRINCIPAL"],
            "MQTT_WORKLOAD_PASSWORD": self.environment["E2E_QUANTSTREAM_MQTT_PASSWORD"],
            "MQTT_APPROVER_PASSWORD": self.environment["E2E_QUANTSTREAM_UI_MQTT_PASSWORD"],
            "MQTT_VAULT_PASSWORD": self.environment["E2E_AGENTVAULT_MQTT_PASSWORD"],
            "MQTT_PAYEE_PASSWORD": self.environment["E2E_MARKETPULSE_MQTT_PASSWORD"],
        }
        run(["python3", str(self.context.deploy_root / "emqx/verify_acl.py")], E2E_ROOT, probe_environment, 90)

    def _target_budget(self) -> None:
        target = REPOSITORY_ROOT / "target"
        if not target.exists():
            return
        output = self._run(["du", "-sm", str(target)], 30)
        if int(output.split()[0]) >= 10_240:
            self._run(["cargo", "clean"], 900, REPOSITORY_ROOT)

    def _build_images(self, payer: str, payee: str) -> None:
        if self.context.skip_build:
            return
        self._target_budget()
        targets = [
            *_agentvault_build_targets(self.environment),
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
            *_agentvault_build_targets(self.environment),
            "admin-ui",
            *(f"marketpulse-{language}" for language in languages),
            *(f"quantstream-{language}" for language in languages),
        ]
        arguments = ["build"]
        if not self.context.reuse_build:
            arguments.append("--pull")
        self._compose(_matrix_build_profiles(languages), *arguments, *targets)

    def _preflight_mcp(self) -> str:
        endpoint = self._url("E2E_MARKETPULSE_PORT", "/mcp")
        headers = {"content-type": "application/json", "x-run-id": "preflight-free"}
        free = {"jsonrpc": "2.0", "id": "free", "method": "tools/call", "params": {"name": "marketpulse.yesterday_news", "arguments": {}}}
        status, _, raw = self._http(endpoint, "POST", json.dumps(free).encode(), headers)
        if status != 200:
            raise RuntimeError(f"free MCP preflight returned HTTP {status}")
        value = json.loads(raw)
        result = value.get("result", {})
        delivery = result.get("structuredContent", {})
        if delivery.get("paid") is not False or not delivery.get("news") or result.get("isError") is not False:
            raise RuntimeError("free MCP response is not a canonical business delivery")
        paid = {"jsonrpc": "2.0", "id": "paid", "method": "tools/call", "params": {"name": "marketpulse.realtime_news", "arguments": {}}}
        headers["x-run-id"] = "preflight-paid"
        status, response_headers, raw = self._http(endpoint, "POST", json.dumps(paid).encode(), headers)
        challenge = json.loads(raw)
        if status != 402 or challenge.get("x402Version") != 2 or not challenge.get("accepts"):
            raise RuntimeError(f"paid MCP preflight is not an x402 v2 challenge: HTTP {status}: {raw[:2048]}")
        if not any(name.lower() == "payment-required" for name in response_headers):
            raise RuntimeError("paid MCP 402 omitted PAYMENT-REQUIRED")
        return "free tool delivered without payment; paid tool returned a valid x402 v2 challenge"

    def _sql(self, payer: str, payee: str, query: str) -> str:
        return _clean_compose_stdout(self._full(
            payer, payee, "exec", "-T", "postgres", "psql", "-U", "agentvault", "-d", "agentvault", "-Atc", query,
        ))

    def _wallet_addresses(self, payer: str, payee: str) -> dict[str, str]:
        if self._wallet_addresses_cache is not None:
            return self._wallet_addresses_cache
        wallet_keys: list[tuple[str, str]] = []
        for identity in PAYER_IDENTITIES.values():
            wallet_keys.extend((
                (identity.primary_wallet_id, self.environment[identity.primary_private_key_env]),
                (identity.secondary_wallet_id, self.environment[identity.secondary_private_key_env]),
            ))
        command = 'for key in "$@"; do cast wallet address --private-key "$key"; done'
        raw = self._full(
            payer,
            payee,
            "exec",
            "-T",
            "anvil",
            "sh",
            "-eu",
            "-c",
            command,
            "wallet-addresses",
            *(f"0x{key.removeprefix('0x')}" for _, key in wallet_keys),
        )
        addresses = EVM_ADDRESS_PATTERN.findall(ANSI_ESCAPE_PATTERN.sub("", raw))
        if len(addresses) != len(wallet_keys) or len({address.lower() for address in addresses}) != len(wallet_keys):
            raise RuntimeError("the ten Payer wallet keys did not resolve to ten distinct EVM addresses")
        self._wallet_addresses_cache = {
            wallet_id: address for (wallet_id, _), address in zip(wallet_keys, addresses, strict=True)
        }
        return self._wallet_addresses_cache

    def _token_balances(self, payer: str, payee: str) -> dict[str, int]:
        addresses = {**self._wallet_addresses(payer, payee), "payee": PAYEE_ADDRESS}
        command = (
            'token="$1"; shift; '
            'for address in "$@"; do '
            'cast call --rpc-url http://127.0.0.1:8545 "$token" '
            "'balanceOf(address)(uint256)' \"$address\"; "
            "done"
        )
        raw = self._full(
            payer,
            payee,
            "exec",
            "-T",
            "anvil",
            "sh",
            "-eu",
            "-c",
            command,
            "wallet-balances",
            TOKEN_CONTRACT,
            *addresses.values(),
        )
        values = [int(value) for value in UINT_OUTPUT_PATTERN.findall(ANSI_ESCAPE_PATTERN.sub("", raw))]
        if len(values) != len(addresses):
            raise RuntimeError(f"expected {len(addresses)} token balances, received {len(values)}: {raw[-2048:]}")
        return dict(zip(addresses, values, strict=True))

    def _wait_and_approve(self, payer: str, payee: str, container: str) -> str:
        token = self.environment.get("E2E_QUANTSTREAM_UI_TOKEN", "quantstream-e2e-ui-token-0000000000000000001")
        endpoint = self._url("E2E_QUANTSTREAM_UI_PORT", "/api/approvals")
        approval: dict[str, Any] | None = None
        last_status = 0
        last_raw = ""
        stopped_evidence: str | None = None
        for _ in range(45):
            status, _, raw = self._http(endpoint, headers={"authorization": f"Bearer {token}"})
            last_status, last_raw = status, raw
            if status == 200:
                items = json.loads(raw).get("items", [])
                if len(items) == 1:
                    approval = items[0]
                    break
            stopped_evidence = self._stopped_container_evidence(container)
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
                "Admin UI did not expose the expected pending approval; "
                f"status={last_status} response={last_raw[:2048]!r} database={pending!r} "
                f"ingestor={stopped_evidence or 'still running'}"
            )
        approval_id = str(approval.get("approvalId", ""))
        intent_id = str(approval.get("intentId", ""))
        if not _safe_id(approval_id) or not _safe_id(intent_id):
            raise RuntimeError("Admin UI returned unsafe approval evidence")
        status, _, raw = self._http(
            f"{endpoint}/{approval_id}/decision", "POST", b'{"decision":"approve"}',
            {"authorization": f"Bearer {token}", "content-type": "application/json"},
        )
        if status not in {200, 201}:
            raise RuntimeError(f"Admin UI approval failed with HTTP {status}: {raw[:1024]}")
        exit_code = self._run([*self.docker, "wait", container], 180).strip()
        logs = self._run([*self.docker, "logs", container], 30)
        if exit_code != "0":
            raise RuntimeError(f"approved ingestor exited with code {exit_code}\n{logs}")
        self._run([*self.docker, "rm", container], 30)
        return f"approval {approval_id} for intent {intent_id} was consumed through the business Admin UI\n{logs}"

    def _stopped_container_evidence(self, container: str) -> str | None:
        status, raw_state = self._try([
            *self.docker,
            "inspect",
            "--format",
            "{{.State.Status}}|{{.State.ExitCode}}",
            container,
        ], 10)
        if status != 0:
            return None
        state = raw_state.strip()
        lifecycle = state.partition("|")[0]
        if lifecycle not in {"dead", "exited"}:
            return None
        logs_status, logs = self._try([*self.docker, "logs", container], 30)
        return f"state={state!r} logs_status={logs_status} logs={logs[-4096:]!r}"

    def _audit(self, payer: str, payee: str) -> dict[str, Any]:
        output = self._full(payer, payee, "run", "-T", "--rm", "--no-deps", "e2e-auditor")
        records = [line for line in output.splitlines() if line.startswith("{")]
        if not records:
            raise RuntimeError("business evidence auditor returned no JSON")
        value = json.loads(records[-1])
        if len(value.get("outbound_intent_ids", [])) != 2 or len(value.get("inbound_intent_ids", [])) != 2:
            raise RuntimeError("auditor did not return two bidirectional payment pairs")
        return value

    def _wait_financial_evidence(self, payer: str, payee: str, intents: list[str]) -> str:
        if len(intents) != 4 or any(not _safe_id(value) for value in intents):
            raise RuntimeError("four safe settled intent IDs are required")
        quoted = ",".join(f"'{value}'" for value in intents)
        query = (
            "SELECT (SELECT count(*) FROM pay_intent WHERE id IN (" + quoted + ") AND status='settled' AND settled_amount=1000000),"
            "(SELECT count(*) FROM pay_ledger WHERE intent_id IN (" + quoted + ") AND entry_type='payment' AND amount=1000000 AND tx_hash IS NOT NULL),"
            "(SELECT count(*) FROM pay_ledger WHERE intent_id IN (" + quoted + ") AND entry_type='payment' AND direction='outbound'),"
            "(SELECT count(*) FROM pay_ledger WHERE intent_id IN (" + quoted + ") AND entry_type='payment' AND direction='inbound'),"
            "(SELECT count(*) FROM pay_event WHERE intent_id IN (" + quoted + ") AND event_type IN ('settled','reconciled'));"
        )
        result = ""
        for _ in range(45):
            result = self._sql(payer, payee, query)
            fields = result.split("|")
            if len(fields) == 5 and fields[:4] == ["4", "4", "2", "2"] and int(fields[4] or 0) >= 4:
                return f"authoritative evidence converged: {result}"
            time.sleep(1)
        raise RuntimeError(f"financial evidence did not converge: {result}")

    def _receipt(self, scenario: str, intent: str, direction: str) -> None:
        status, _, raw = self._http(
            self._url("E2E_AGENTVAULT_PORT", f"/api/v1/payments/{intent}/receipt?scenario_id={scenario}")
        )
        value = json.loads(raw)
        asset = value.get("asset", {})
        if (
            status != 200 or value.get("intent_id") != intent or value.get("status") != "settled"
            or value.get("direction") != direction or value.get("protocol") != "x402"
            or asset.get("network") != "eip155:31337" or asset.get("asset") != "USDC"
            or asset.get("decimals") != 6 or str(asset.get("token_contract", "")).lower() != TOKEN_CONTRACT
            or str(value.get("recipient", "")).lower() != PAYEE_ADDRESS
            or not value.get("tx_hash") or not value.get("evidence_hash") or value.get("settled_amount") != "1000000"
        ):
            raise RuntimeError(f"invalid settled receipt for {intent}: {raw[:2048]}")

    def _assert_durable_invariants(self, payer: str, payee: str) -> str:
        identity = payer_identity(payer)
        approval = self._sql(
            payer, payee,
            "SELECT count(*) FROM pay_approval WHERE status='consumed' AND requested_amount=1000000 "
            f"AND approved_amount=1000000 AND approved_by='{identity.approver_principal}' AND consumed_at IS NOT NULL "
            "AND length(grant_jti)>0 AND grant_claims_hash ~ '^[0-9a-f]{64}$' "
            "AND (sealed_grant_token->>'version')::INTEGER>0 AND sealed_grant_token->>'key_id'='recovery-v1' "
            "AND length(sealed_grant_token->>'nonce')>16 AND length(sealed_grant_token->>'ciphertext')>32;",
        )
        if approval != "1":
            raise RuntimeError("one-time encrypted approval grant was not atomically consumed")
        selection = self._sql(
            payer, payee,
            "SELECT count(*),count(DISTINCT wallet_id),string_agg(DISTINCT wallet_id, ',' ORDER BY wallet_id) "
            f"FROM pay_intent WHERE scenario_id='{identity.scenario_id}' "
            "AND direction='outbound' AND status='settled';",
        )
        expected_wallets = ",".join(sorted(identity.wallet_ids))
        if selection != f"2|2|{expected_wallets}":
            raise RuntimeError(f"AgentVault wallet-pool selection is invalid: {selection}")
        membership = self._sql(
            payer, payee,
            f"SELECT count(*) FROM pay_policy_wallet WHERE policy_id='{identity.policy_id}' "
            f"AND wallet_id IN ('{identity.primary_wallet_id}','{identity.secondary_wallet_id}');",
        )
        if membership != "2":
            raise RuntimeError("policy is not bound to both wallet-pool members")
        foreign_usage = self._sql(
            payer,
            payee,
            "SELECT count(*) FROM pay_intent WHERE direction='outbound' AND status='settled' "
            f"AND (scenario_id<>'{identity.scenario_id}' OR wallet_id NOT IN "
            f"('{identity.primary_wallet_id}','{identity.secondary_wallet_id}'));",
        )
        if foreign_usage != "0":
            raise RuntimeError(f"Payer wallet isolation was violated by {foreign_usage} settled intents")
        return (
            f"approval grant consumed once; payer={payer}; client={identity.client_id}; "
            f"scenario={identity.scenario_id}; wallet pool evidence={selection}; foreign_usage=0"
        )

    def _wait_balance_snapshots(self, payer: str, payee: str, expected: dict[str, int]) -> str:
        query = (
            "SELECT wallet_id,asset_available FROM pay_wallet_balance WHERE network='eip155:31337' "
            "AND asset='USDC' AND lower(token_contract)='" + TOKEN_CONTRACT + "' ORDER BY wallet_id;"
        )
        last = ""
        expected_rows = {
            wallet_id: expected[wallet_id]
            for identity in PAYER_IDENTITIES.values()
            for wallet_id in identity.wallet_ids
        }
        for _ in range(30):
            last = self._sql(payer, payee, query)
            rows = {}
            for line in last.splitlines():
                if "|" in line:
                    wallet, amount = line.split("|", 1)
                    rows[wallet] = int(amount)
            if all(rows.get(wallet) == amount for wallet, amount in expected_rows.items()):
                return f"periodic SQL balance snapshots converged: {json.dumps(rows, sort_keys=True)}"
            time.sleep(1)
        raise RuntimeError(f"periodic wallet balance snapshots did not converge: {last}")

    def _service_logs(self, payer: str, payee: str, payer_logs: str) -> dict[str, str]:
        """Collect stable evidence without depending on removed `run --rm` containers."""
        return {
            "agentvault": self._full(payer, payee, "logs", "--no-color", "agentvault"),
            "payer": payer_logs,
            "payee": self._full(payer, payee, "logs", "--no-color", f"marketpulse-{payee}"),
            "facilitator": self._full(payer, payee, "logs", "--no-color", "facilitator"),
        }

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
        self.environment["E2E_AGENTVAULT_TRANSPORT"] = self.context.transport
        identity = payer_identity(payer_language)
        if render_config:
            self._render_agentvault_config()
        if clean_start:
            self.cleanup()
        try:
            self._build_images(payer_language, payee_language)
            self._dependency_up("payee")
            self._dependency_health("payee")
            self._full(payer_language, payee_language, "up", "--no-deps", "agentvault-provision")
            self._full(payer_language, payee_language, "up", "-d", "--no-deps", "agentvault")
            self._wait_http(self._url("E2E_AGENTVAULT_MGMT_PORT", "/readyz"), 90)
            self._full(
                payer_language, payee_language, "up", "-d", "--no-deps",
                f"marketpulse-{payee_language}", "admin-ui",
            )
            self._wait_http(self._url("E2E_MARKETPULSE_PORT", "/healthz"), 60)
            self._wait_http(self._url("E2E_QUANTSTREAM_UI_PORT", "/healthz"), 60)
            preflight = self._preflight_mcp()
            initial_balances = self._token_balances(payer_language, payee_language)
            automatic_logs = self._full(
                payer_language, payee_language, "run", "--rm", "--no-deps",
                "-e", "QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT=once", f"quantstream-{payer_language}",
            )
            time.sleep(2)
            container = f"{self.project}-{payer_language}-{payee_language}-{self.context.transport}-approval"
            if container in self._run([*self.docker, "ps", "-a", "--format", "{{.Names}}"], 30).splitlines():
                self._run([*self.docker, "rm", "-f", container], 30)
            self._full(
                payer_language, payee_language, "run", "-d", "--name", container,
                "--no-deps", f"quantstream-{payer_language}",
            )
            approval_logs = self._wait_and_approve(payer_language, payee_language, container)
            audit = self._audit(payer_language, payee_language)
            outbound = list(audit["outbound_intent_ids"])
            inbound = list(audit["inbound_intent_ids"])
            financial = self._wait_financial_evidence(payer_language, payee_language, [*outbound, *inbound])
            for intent in outbound:
                self._receipt(identity.scenario_id, intent, "outbound")
            for intent in inbound:
                self._receipt("scenario-marketpulse-feed", intent, "inbound")
            durable = self._assert_durable_invariants(payer_language, payee_language)
            final_balances = self._token_balances(payer_language, payee_language)
            balance_sync = self._wait_balance_snapshots(payer_language, payee_language, final_balances)
            evidence = FullFlowEvidence(
                payer_language=payer_language,
                payee_language=payee_language,
                transport=self.context.transport,
                audit=audit,
                initial_balances=initial_balances,
                final_balances=final_balances,
                logs=self._service_logs(
                    payer_language,
                    payee_language,
                    automatic_logs + "\n" + approval_logs,
                ),
                runtime={"facilitator": self._facilitator_capabilities()},
            )
            from verifier.payment import s21_payer_signing, s22_payee_settlement, s23_balance_sync, s24_gateway_balance

            return {
                "s20_full.md": "\n".join((
                    f"payer={payer_language} payee={payee_language} transport={self.context.transport}",
                    preflight, financial, durable, json.dumps(audit, sort_keys=True),
                )),
                "s21_payer_signing.md": s21_payer_signing.verify_evidence(evidence),
                "s22_payee_settlement.md": s22_payee_settlement.verify_evidence(evidence),
                "s23_balance_sync.md": s23_balance_sync.verify_evidence(evidence, balance_sync),
                "s24_gateway_balance.md": s24_gateway_balance.verify_evidence(evidence),
            }
        except BaseException:
            self._diagnostics(payer_language, payee_language)
            raise

    def matrix(self) -> dict[str, str]:
        RUNTIME_ROOT.mkdir(parents=True, exist_ok=True)
        scope = self.context.e2e_sdk_scope
        languages = matrix_languages(scope)
        selected_cases = matrix_cases(scope)
        total = len(selected_cases)
        _ensure_matrix_plan(
            self.context.report_root / "payment" / "matrix-plan.json",
            scope,
        )
        progress_path = RUNTIME_ROOT / "matrix-progress.log"
        progress_path.write_text(f"matrix scope={scope} 0/{total} initialized\n", encoding="utf-8")
        progress_lock = threading.Lock()
        stop = threading.Event()

        def progress(message: str) -> None:
            with progress_lock:
                print(message, flush=True)
                with progress_path.open("a", encoding="utf-8") as stream:
                    stream.write(message + "\n")

        def checkpoint(case_number: int, name: str, outputs: dict[str, str]) -> None:
            root = self.context.report_root / "payment" / "matrix-cases"
            root.mkdir(parents=True, exist_ok=True)
            target = root / f"case-{case_number:02d}.json"
            temporary = target.with_suffix(".tmp")
            temporary.write_text(
                json.dumps({"case": name, "reports": outputs}, ensure_ascii=False, sort_keys=True),
                encoding="utf-8",
            )
            temporary.replace(target)

        def restore_checkpoint(case_number: int, name: str) -> dict[str, str] | None:
            target = self.context.report_root / "payment" / "matrix-cases" / f"case-{case_number:02d}.json"
            return _load_matrix_checkpoint(target, name)

        cases = [
            (case_number, case.transport, case.payer, case.payee)
            for case_number, case in enumerate(selected_cases, 1)
        ]
        completed: list[tuple[int, str, dict[str, str]]] = []
        pending: list[tuple[int, str, str, str]] = []
        for case_number, transport, payer, payee in cases:
            name = f"payer={payer} payee={payee} transport={transport}"
            outputs = restore_checkpoint(case_number, name)
            if outputs is None:
                pending.append((case_number, transport, payer, payee))
            else:
                completed.append((case_number, name, outputs))
        progress(f"matrix scope={scope} resume: restored {len(completed)}/{total} validated checkpoints")

        workers = min(self.context.matrix_workers, len(TRANSPORTS))
        groups: list[list[tuple[int, str, str, str]]] = [[] for _ in range(workers)]
        for case in pending:
            transport_index = TRANSPORTS.index(case[1])
            groups[transport_index % workers].append(case)

        self.cleanup()
        self._build_matrix_images(languages)

        def run_group(
            worker_slot: int,
            group: list[tuple[int, str, str, str]],
        ) -> list[tuple[int, str, dict[str, str]]]:
            worker_context = replace(
                self.context,
                worker_slot=worker_slot,
                transport=group[0][1],
                skip_build=True,
                reuse_build=True,
            )
            worker = DockerE2EDeployer(worker_context)
            worker._render_agentvault_config()
            worker.cleanup()
            completed: list[tuple[int, str, dict[str, str]]] = []
            for case_number, transport, payer, payee in group:
                if stop.is_set():
                    break
                progress(
                    f"matrix scope={scope} case {case_number}/{total} start: payer={payer} payee={payee} "
                    f"transport={transport} worker={worker_slot + 1}",
                )
                case_context = replace(
                    self.context,
                    payer_language=payer,
                    payee_language=payee,
                    transport=transport,
                    skip_build=True,
                    reuse_build=True,
                    worker_slot=worker_slot,
                )
                case = DockerE2EDeployer(case_context)
                name = f"payer={payer} payee={payee} transport={transport}"
                try:
                    outputs = case.full(payer, payee, clean_start=False, render_config=False)
                    checkpoint(case_number, name, outputs)
                    completed.append((case_number, name, outputs))
                except BaseException as error:
                    stop.set()
                    progress(
                        f"matrix scope={scope} case {case_number}/{total} failed: payer={payer} payee={payee} "
                        f"transport={transport} worker={worker_slot + 1} error={error!r}",
                    )
                    raise
                finally:
                    case.cleanup()
                progress(
                    f"matrix scope={scope} case {case_number}/{total} passed: "
                    f"{name} worker={worker_slot + 1}"
                )
                worker._wait_host_capacity()
            return completed

        failure: BaseException | None = None
        worker_groups = [(slot, group) for slot, group in enumerate(groups) if group]
        with ThreadPoolExecutor(max_workers=max(1, len(worker_groups)), thread_name_prefix="agentvault-e2e") as executor:
            futures = [executor.submit(run_group, slot, group) for slot, group in worker_groups]
            for future in as_completed(futures):
                try:
                    completed.extend(future.result())
                except BaseException as error:
                    stop.set()
                    if failure is None:
                        failure = error
        if failure is not None:
            raise failure
        completed.sort(key=lambda item: item[0])
        if len(completed) != total:
            raise RuntimeError(
                f"SDK matrix scope={scope} completed only {len(completed)} of {total} cases"
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
                f"scope={scope} languages={','.join(languages)} transports={','.join(TRANSPORTS)}\n"
                f"passed {total}/{total} isolated cases\n\n" + "\n".join(passed)
            ),
            **{report: "\n\n".join(entries) for report, entries in details.items()},
        }

    def _wait_host_capacity(self) -> None:
        processors = os.cpu_count() or 1
        threshold = max(1.0, processors * 1.5)
        for _ in range(30):
            if os.getloadavg()[0] <= threshold:
                return
            time.sleep(2)

    def _diagnostics(self, payer: str, payee: str) -> None:
        commands = (
            [*self.compose, "ps"],
            [*self.compose, "logs", "--no-color", "--tail=200", "agentvault", f"marketpulse-{payee}",
             "facilitator", "emqx", "postgres", "anvil", f"quantstream-{payer}"],
        )
        for command in commands:
            status, output = self._try(command, 60)
            print(f"diagnostic exit={status}\n{output}", flush=True)


def _safe_id(value: str) -> bool:
    return bool(re.fullmatch(r"[A-Za-z0-9._-]+", value))
