"""Long-lived Kubernetes acceptance stack and explicit, independently auditable payments."""
from __future__ import annotations

import copy
import json
import re
import time
from pathlib import Path

import yaml

from common.payer_identity import LANGUAGES
from common.project import E2E_ROOT
from common.process import execute
from .kubernetes import (
    AGENTVAULT_CHART, AGENTVAULT_NAMESPACE, AGENTVAULT_VALUES, BUSINESS_EVIDENCE_LABEL,
    INFRA_NAMESPACE, PAYMENT_NAMESPACE, PAYEE_NAMESPACE, PAYER_NAMESPACE, KubernetesE2EDeployer,
    MANUAL_IMAGE_PIN, MANUAL_IMAGE_PIN_OWNER,
)
from .manual_fixture import ManualChainFixture
from .manual_profiles import ManualProfiles
from .manual_evidence import ManualPaymentEvidence
from .docker import RUNTIME_ROOT


class ManualAcceptanceDeployer(KubernetesE2EDeployer):
    IMAGE_PIN = MANUAL_IMAGE_PIN
    IMAGE_PIN_OWNER = MANUAL_IMAGE_PIN_OWNER

    def __init__(self, context):
        super().__init__(context)
        self.manual_secrets = {}
        self.state_path = RUNTIME_ROOT / "manual-public.json"

    def _apply_document(self, document):
        document.setdefault("metadata", {}).setdefault("labels", {})["agentvault.io/manual-acceptance"] = "true"
        return super()._apply_document(document)

    def cleanup(self):
        for namespace in (AGENTVAULT_NAMESPACE, INFRA_NAMESPACE, PAYMENT_NAMESPACE, PAYEE_NAMESPACE, PAYER_NAMESPACE):
            self._kubectl("delete", "deployment,service,cronjob,job", "-n", namespace,
                          "-l", "agentvault.io/manual-acceptance=true", "--ignore-not-found", "--wait=true",
                          "--timeout=90s", timeout=100)
        return super().cleanup()

    def _ensure_cluster_image(self, reference):
        super()._ensure_cluster_image(reference)
        retained = {self.images.get(f"payer-{language}") for language in LANGUAGES}
        if reference in retained:
            self._pin_payer_image(reference)

    def _image_refs_with_label(self, key, value):
        return set(self._run(["sudo", "-n", "k3s", "ctr", "-n", "k8s.io", "images", "list", "--quiet",
                              f'labels."{key}"=="{value}"']).splitlines())

    def _pin_payer_image(self, reference):
        pinned = reference in self._image_refs_with_label(self.IMAGE_PIN, "pinned")
        owned = reference in self._image_refs_with_label(self.IMAGE_PIN_OWNER, "true")
        # Preserve pins set by an operator or another component. Only our own
        # label gives cleanup permission to remove this reference's pin.
        if not pinned or owned:
            self._run(["sudo", "-n", "k3s", "ctr", "-n", "k8s.io", "images", "label", reference,
                       f"{self.IMAGE_PIN}=pinned", f"{self.IMAGE_PIN_OWNER}=true"])
        for _ in range(20):
            status = execute(["sudo", "-n", "k3s", "crictl", "inspecti", reference],
                             E2E_ROOT, self.environment, 10)
            if status.returncode == 0 and json.loads(status.stdout).get("status", {}).get("pinned") is True:
                return
            time.sleep(0.25)
        raise RuntimeError(f"containerd did not expose the retained payer image as pinned: {reference}")

    def _apply_secret(self, namespace, name, values):
        if namespace == AGENTVAULT_NAMESPACE and name == "agentvault-secrets":
            values = {**values, **self.manual_secrets}
        super()._apply_secret(namespace, name, values)

    def deploy(self) -> dict:
        self.cleanup()
        self._apply_namespaces()
        self._bootstrap_manifests()
        self._preload_local_dependencies()
        self._install_dependencies("payee", include_solana=True)
        with self._runtime_forwards(("emqx-mqtt", "emqx-dashboard")):
            self._bootstrap_emqx()
        tempo = yaml.safe_load((self._component_chart("tempo") / "values.yaml").read_text())
        tempo["args"][3] = "100ms"
        source = tempo["image"]["repository"] + ":" + tempo["image"]["tag"]
        local = execute([*self.docker, "image", "inspect", source], E2E_ROOT, self.environment, 30)
        if local.returncode == 0:
            alias = "localhost/tempo-manual-e2e:local"
            self._run([*self.docker, "tag", source, alias])
            self._ensure_cluster_image(alias)
            tempo["image"].update(repository="localhost/tempo-manual-e2e", tag="local", pullPolicy="Never")
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        tempo_path = self.state_path.parent / "manual-tempo-values.yaml"
        tempo_path.write_text(yaml.safe_dump(tempo))
        self._helm_install("tempo", self._component_chart("tempo"), PAYMENT_NAMESPACE, values=(tempo_path,))
        with self._forward_service(PAYMENT_NAMESPACE, "tempo", 8545) as port:
            self._wait_tempo_faucet_window(f"http://127.0.0.1:{port}")
        solana, keys = ManualChainFixture(self).solana()
        self.manual_secrets = {k: v for k, v in keys.items() if k.startswith("HELPER_SECRET_")}
        self.manual_secrets["HELPER_SECRET_MANUAL_MPP_CHALLENGE"] = "manual-e2e-mpp-challenge-key-32-bytes-only"
        self._bootstrap_manifests()
        self._apply_config_map(AGENTVAULT_NAMESPACE, "agentvault-e2e-resources",
                               {"resources.yaml": ManualProfiles.resources(solana)})
        self._install_provider(solana, keys["SOLANA_FACILITATOR_KEYPAIR"])
        funding = ManualChainFixture(self).fund_tempo(solana)
        self._configure_vault(solana)
        state = {"run_id": self.context.run_id, "solana": solana, "tempo_funding": funding,
                 "profiles": list(ManualProfiles.NAMES), "languages": list(LANGUAGES),
                 "images": self.images, "provider_image": self._provider_image(), "services_ready": False}
        # Persist public chain identities before any application rollout. A PVC or
        # image failure can then resume services without resetting funded chains.
        self._record_state(state)
        self._build_matrix_images(LANGUAGES)
        self._install_agentvault_manual()
        return self.services(state)

    def _record_state(self, state: dict) -> None:
        self.state_path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.state_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(state, indent=2) + "\n")
        temporary.replace(self.state_path)
        target = self.context.report_root / "payment/manual"
        target.mkdir(parents=True, exist_ok=True)
        (target / "deployment.json").write_text(json.dumps(state, indent=2) + "\n")

    def services(self, state: dict | None = None) -> dict:
        if state is None:
            if not self.state_path.is_file():
                raise RuntimeError("manual-services requires existing manual-public.json; it never initializes chains")
            state = json.loads(self.state_path.read_text())
        if state.get("profiles") != list(ManualProfiles.NAMES) or state.get("languages") != list(LANGUAGES):
            raise RuntimeError("manual-services public state has an incompatible profile/language catalog")
        images = state.get("images", {})
        required = {"agentvault", "admin-ui", *(f"{role}-{language}" for role in ("payer", "payee") for language in LANGUAGES)}
        if not required.issubset(images):
            raise RuntimeError("manual-services public state omits a required frozen application image")
        self.images = dict(images)
        state = {**state, "services_ready": False}
        self._record_state(state)
        self._preload_local_dependencies()
        for language in LANGUAGES:
            self._ensure_cluster_image(self.images[f"payer-{language}"])
            self._apply_business_claims(language, language)
            for profile in ManualProfiles.NAMES:
                self._deploy_payee(language, profile)
            self._suspended_payer(language)
        self._install_admin_ui("rust")
        state["services_ready"] = True
        self._record_state(state)
        return state

    def _helm_install(self, release, chart, namespace, *, values=(), **options):
        if release == "anvil" and namespace == PAYMENT_NAMESPACE:
            # Automine avoids unbounded idle blocks. Each payment preflight
            # advances local Anvil time once before credentials are issued.
            source = yaml.safe_load((chart / "values.yaml").read_text())
            claim = "manual-anvil-state"
            self._apply_document({"apiVersion": "v1", "kind": "PersistentVolumeClaim", "metadata": {
                "name": claim, "namespace": PAYMENT_NAMESPACE,
                "labels": {"app.kubernetes.io/part-of": "agentvault-e2e", BUSINESS_EVIDENCE_LABEL: "true"}},
                "spec": {"accessModes": ["ReadWriteOnce"],
                         "resources": {"requests": {"storage": "4Gi"}}}})
            args = [*source["args"], "--state=/var/lib/anvil/state.json", "--state-interval=1",
                    "--preserve-historical-states"]
            self.state_path.parent.mkdir(parents=True, exist_ok=True)
            override = self.state_path.parent / "manual-anvil-values.yaml"
            override.write_text(yaml.safe_dump({"args": args,
                "persistence": {"enabled": True, "existingClaim": claim, "mountPath": "/var/lib/anvil"},
                "resources": {"limits": {"memory": "2Gi"}}}))
            values = (*values, override)
        if release == "solana" and namespace == PAYMENT_NAMESPACE:
            # Manual initialization already explicitly cleans the previous stack.
            # Ordinary container/Pod restarts must resume the same funded ledger.
            source = yaml.safe_load((chart / "values.yaml").read_text())
            claim = "manual-solana-ledger"
            self._apply_document({"apiVersion": "v1", "kind": "PersistentVolumeClaim", "metadata": {
                "name": claim, "namespace": PAYMENT_NAMESPACE,
                "labels": {"app.kubernetes.io/part-of": "agentvault-e2e", BUSINESS_EVIDENCE_LABEL: "true"}},
                "spec": {"accessModes": ["ReadWriteOnce"],
                         "resources": {"requests": {"storage": "8Gi"}}}})
            self.state_path.parent.mkdir(parents=True, exist_ok=True)
            override = self.state_path.parent / "manual-solana-values.yaml"
            override.write_text(yaml.safe_dump({
                "args": [arg for arg in source["args"] if arg.split("=", 1)[0] not in {"--reset", "-r", "--log"}]
                        + ["--log"],
                "env": {"RUST_LOG": "warn"},
                "persistence": {"enabled": True, "existingClaim": claim},
                "resources": {"limits": {"cpu": "500m"}},
            }))
            values = (*values, override)
        return super()._helm_install(release, chart, namespace, values=values, **options)

    def _provider_image(self):
        return self.environment.get("E2E_MANUAL_PROVIDER_IMAGE", "localhost/manual-payment-provider:local")

    def _preload_local_dependencies(self):
        references = {
            "registry.cn-shenzhen.aliyuncs.com/wl4g/envoyproxy_envoy:distroless-v1.36.4",
            "docker.io/rancher/mirrored-library-busybox:1.37.0",
        }
        for component in ("postgres", "emqx", "jaeger", "anvil", "solana", "facilitator"):
            image = yaml.safe_load((self._component_chart(component) / "values.yaml").read_text())["image"]
            references.add(image["repository"] + ":" + image["tag"])
        for reference in sorted(references):
            local = execute([*self.docker, "image", "inspect", reference], E2E_ROOT, self.environment, 30)
            if local.returncode == 0:
                self._ensure_cluster_image(reference)
        self._retain_volume_helper_image()

    def _retain_volume_helper_image(self):
        # CronJobs intentionally remain suspended. Keep the tiny provisioner
        # image referenced so image GC cannot break their first PVC consumer.
        image = "docker.io/rancher/mirrored-library-busybox:1.37.0"
        self._ensure_cluster_image(image)
        labels = {"app.kubernetes.io/name": "manual-volume-helper", "app.kubernetes.io/part-of": "agentvault-e2e"}
        self._apply_document({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {
            "name": "manual-volume-helper", "namespace": INFRA_NAMESPACE, "labels": labels},
            "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {
                "metadata": {"labels": labels}, "spec": {"containers": [{"name": "helper", "image": image,
                    "imagePullPolicy": "Never", "command": ["/bin/sleep", "2147483647"],
                    "resources": {"requests": {"cpu": "1m", "memory": "2Mi"},
                                  "limits": {"cpu": "10m", "memory": "16Mi"}}}]}}}})

    def _await_image(self, image):
        deadline = time.monotonic() + self.context.timeout_seconds
        while time.monotonic() < deadline:
            result = execute([*self.docker, "image", "inspect", image], E2E_ROOT, self.environment, 30)
            if result.returncode == 0:
                return
            time.sleep(5)
        raise RuntimeError(f"manual acceptance source image not built: {image}")

    def _install_provider(self, solana, keypair):
        image = self._provider_image()
        self._await_image(image)
        self._ensure_cluster_image(image)
        self._apply_secret(PAYMENT_NAMESPACE, "manual-provider-secrets", {
            "SOLANA_FACILITATOR_KEYPAIR": keypair,
            "TEMPO_DEV_PRIVATE_KEY": self.environment.get("E2E_FACILITATOR_PRIVATE_KEY",
                "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"),
        })
        environment = {
            "SOLANA_RPC_URL": "http://solana-validator:8899",
            "SOLANA_CHAIN_REFERENCE": solana["network"].split(":", 1)[1],
            "EVM_FACILITATOR_URL": "http://x402-facilitator:8085",
            "TEMPO_RPC_URL": "http://tempo:8545", "TEMPO_RECIPIENT": solana["tempo_recipient"],
            "TOKIO_WORKER_THREADS": "2",
        }
        labels = {"app.kubernetes.io/part-of": "agentvault-e2e", "app.kubernetes.io/name": "manual-payment-provider"}
        self._apply_document({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {
            "name": "manual-payment-provider", "namespace": PAYMENT_NAMESPACE, "labels": labels},
            "spec": {"replicas": 1, "selector": {"matchLabels": labels}, "template": {"metadata": {"labels": labels},
                "spec": {"containers": [{"name": "provider", "image": image, "imagePullPolicy": "Never",
                    "env": [{"name": k, "value": v} for k, v in environment.items()],
                    "envFrom": [{"secretRef": {"name": "manual-provider-secrets"}}],
                    "ports": [{"containerPort": 8087}],
                    "readinessProbe": {"httpGet": {"path": "/healthz", "port": 8087}, "periodSeconds": 3},
                    "resources": {"requests": {"cpu": "20m", "memory": "64Mi"},
                                  "limits": {"cpu": "500m", "memory": "512Mi"}}}]}}}})
        self._apply_document({"apiVersion": "v1", "kind": "Service", "metadata": {
            "name": "manual-payment-provider", "namespace": PAYMENT_NAMESPACE, "labels": labels},
            "spec": {"selector": labels, "ports": [{"port": 8087, "targetPort": 8087}]}})
        self._kubectl("rollout", "status", "deployment/manual-payment-provider", "-n", PAYMENT_NAMESPACE,
                      "--timeout=180s", timeout=190)

    def _configure_vault(self, solana):
        values = yaml.safe_load(AGENTVAULT_VALUES.read_text())
        for container in values["agentvault"]["backend"].get("extraContainers", []):
            container["imagePullPolicy"] = "Never"
        config = yaml.safe_load(values["agentvault"]["agentvault-config"])
        domain = config["agentvault"]
        asset = ManualProfiles.asset("solana", solana)
        xasset = {"network": asset["network"], "contract": asset["token_contract"],
                  "symbol": "USDC", "decimals": 6, "fee_payer": solana["fee_payer"]}
        masset = {"network": "eip155:1337", "currency": ManualProfiles.TEMPO_TOKEN,
                  "symbol": "pathUSD", "token_contract": ManualProfiles.TEMPO_TOKEN, "decimals": 6}
        for role in ("payer", "payee"):
            domain[role]["x402"]["assets"].append(xasset)
            domain[role]["mpp"].update(enabled=True, assets=[masset])
        domain["payee"]["x402"]["facilitator"]["base_url"] = "http://127.0.0.1:8087/x402/"
        domain["payee"]["x402"]["facilitator"]["timeout"] = "60s"
        domain["payee"]["mpp"].update(realm="agentvault-manual", challenge_secret={
            "file": "", "env": "HELPER_SECRET_MANUAL_MPP_CHALLENGE"})
        domain["payee"]["mpp"]["provider"].update(id="manual-tempo", base_url="http://127.0.0.1:8087/",
                                                              timeout="60s", allow_insecure_loopback=True)
        for network, family, port in ((asset["network"], "solana", 8899), ("eip155:1337", "evm", 8546)):
            chain = copy.deepcopy(domain["chains"]["eip155:31337"])
            chain.update(family=family)
            if network == "eip155:1337":
                # Tempo charges gas in TIP-20, with no native-token balance requirement.
                chain["minimum_native_balance"] = "0"
            chain["rpc"]["endpoint"] = f"http://127.0.0.1:{port}"
            chain["rpc"]["timeout"] = "30s"
            domain["chains"][network] = chain
        domain["wallet_balance_sync"]["interval"] = "15s"
        domain["reconciliation"]["interval"] = "10s"
        self._extend_vault_bridge()
        values["agentvault"]["agentvault-config"] = yaml.safe_dump(config, sort_keys=False)
        self.manual_values = self.state_path.parent / "manual-values.yaml"
        self.manual_values.parent.mkdir(parents=True, exist_ok=True)
        self.manual_values.write_text(yaml.safe_dump(values, sort_keys=False))

    def _extend_vault_bridge(self):
        raw = self._kubectl("get", "configmap", "agentvault-local-envoy-cm", "-n", AGENTVAULT_NAMESPACE, "-o", "json")
        config = yaml.safe_load(json.loads(raw)["data"]["envoy.yaml"])
        resources = config["static_resources"]
        for name, port, service, target in (("solana", 8899, "solana-validator", 8899),
                                            ("tempo", 8546, "tempo", 8545),
                                            ("manual-provider", 8087, "manual-payment-provider", 8087)):
            listener = copy.deepcopy(resources["listeners"][0])
            listener["name"] = name
            listener["address"]["socket_address"]["port_value"] = port
            proxy = listener["filter_chains"][0]["filters"][0]["typed_config"]
            proxy.update(stat_prefix=name, cluster=name)
            cluster = copy.deepcopy(resources["clusters"][0])
            cluster["name"] = name
            cluster["load_assignment"]["cluster_name"] = name
            endpoint = cluster["load_assignment"]["endpoints"][0]["lb_endpoints"][0]["endpoint"]["address"]["socket_address"]
            endpoint.update(address=f"{service}.{PAYMENT_NAMESPACE}.svc.cluster.local", port_value=target)
            resources["listeners"].append(listener)
            resources["clusters"].append(cluster)
        self._apply_config_map(AGENTVAULT_NAMESPACE, "agentvault-local-envoy-cm", {"envoy.yaml": yaml.safe_dump(config)})

    def _install_agentvault_manual(self):
        self._helm_install("agentvault", AGENTVAULT_CHART, AGENTVAULT_NAMESPACE,
                           values=(self.manual_values,), set_strings=self._image_values("agentvault", "agentvault.backend.image"))

    def _template(self, chart: Path, name: str, namespace: str, settings: dict) -> list[dict]:
        args = ["template", name, str(chart), "--namespace", namespace]
        for path in self._shared_overrides_for_chart(chart):
            args += ["--values", str(path)]
        for key, value in settings.items():
            args += ["--set-string", f"{key}={value}"]
        documents = [d for d in yaml.safe_load_all(self._helm(*args)) if d]
        for document in documents:
            spec = document.get("spec", {}).get("template", {}).get("spec", {})
            for container in [*spec.get("containers", []), *spec.get("initContainers", [])]:
                if "envoyproxy_envoy" in container.get("image", ""):
                    container["imagePullPolicy"] = "Never"
        return documents

    def _deploy_payee(self, language, profile):
        self._ensure_cluster_image(self.images[f"payee-{language}"])
        name = ManualProfiles.service(language, profile)
        suffix = ManualProfiles.suffix(profile)
        self._apply_document({"apiVersion": "v1", "kind": "PersistentVolumeClaim", "metadata": {
            "name": name, "namespace": PAYEE_NAMESPACE, "labels": {"app.kubernetes.io/part-of": "agentvault-e2e",
                BUSINESS_EVIDENCE_LABEL: "true"}},
            "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "64Mi"}}}})
        settings = {**self._image_values(f"payee-{language}"), "env.AGENTVAULT_TRANSPORT": "grpc",
            "persistence.existingClaim": name,
            "env.MARKETPULSE_SCENARIO_ID": f"scenario-marketpulse-feed{suffix}",
            "env.MARKETPULSE_PRICE_ID": f"price-{language}-{profile}-mcp",
            "env.MARKETPULSE_A2A_PRICE_ID": f"price-{language}-{profile}-a2a",
            "env.MARKETPULSE_PAYMENT_PROTOCOL": "mpp" if profile == "tempo" else "x402",
            "env.MARKETPULSE_RESOURCE_URL": ManualProfiles.origin(language, profile) + "/mcp",
            "env.MARKETPULSE_A2A_RESOURCE_URL": ManualProfiles.origin(language, profile) + "/a2a",
            "env.MARKETPULSE_ORIGIN": ManualProfiles.origin(language, profile)}
        for document in self._template(self._payee_chart(language), name, PAYEE_NAMESPACE, settings):
            if document["kind"] in {"Deployment", "Service", "ConfigMap"}:
                document["metadata"]["name"] = name
            document["metadata"].setdefault("labels", {})["app.kubernetes.io/part-of"] = "agentvault-e2e"
            if document["kind"] == "Deployment":
                document["spec"]["template"]["spec"].setdefault("securityContext", {})["fsGroup"] = 65532
                for container in document["spec"]["template"]["spec"]["containers"]:
                    if any(port.get("containerPort") == 8081 for port in container.get("ports", [])):
                        container["startupProbe"] = {"httpGet": {"path": "/healthz", "port": 8081},
                                                     "periodSeconds": 2, "timeoutSeconds": 2, "failureThreshold": 90}
            self._apply_document(document)
        self._kubectl("rollout", "status", f"deployment/{name}", "-n", PAYEE_NAMESPACE, "--timeout=180s", timeout=190)

    def _payer_job(self, payer, payee, profile, message, transport):
        environment = ManualProfiles.payer_environment(payer, payee, profile, message, transport)
        environment["QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT"] = "once" if self.context.fail_after_settlement else ""
        settings = {**self._image_values(f"payer-{payer}"),
                    "persistence.existingClaim": self._business_claim("payer", payer),
                    **{f"env.{key}": value for key, value in environment.items()}}
        documents = self._template(self._payer_chart(payer), self._payer_name(payer), PAYER_NAMESPACE, settings)
        job = next(d for d in documents if d["kind"] == "Job")
        job["metadata"].setdefault("labels", {}).update({"app.kubernetes.io/part-of": "agentvault-e2e",
                                                       BUSINESS_EVIDENCE_LABEL: "true"})
        return job

    def _suspended_payer(self, language):
        job = self._payer_job(language, "rust", "evm", "mcp", "grpc")
        self._apply_document({"apiVersion": "batch/v1", "kind": "CronJob", "metadata": {
            "name": self._payer_name(language), "namespace": PAYER_NAMESPACE, "labels": job["metadata"]["labels"]},
            "spec": {"schedule": "0 0 1 1 *", "suspend": True, "concurrencyPolicy": "Forbid",
                     "jobTemplate": {"spec": job["spec"]}}})

    def pay(self, payer=None, payee=None) -> dict:
        payer, payee = payer or self.context.payer_language, payee or self.context.payee_language
        profile, message = self.context.payment_profile, self.context.message_protocol
        name = f"manual-{payer}-{payee}-{profile}-{message}-{time.time_ns() % 1000000000}"
        directory = self.context.report_root / "payment/manual" / name
        directory.mkdir(parents=True, exist_ok=True)
        identity = {"job": name, "payer": payer, "payee": payee, "profile": profile,
                    "message": message, "transport": self.context.transport,
                    "fail_after_settlement": self.context.fail_after_settlement}
        (directory / "request.json").write_text(json.dumps(identity, indent=2) + "\n")
        phase = "prepare"
        try:
            self._ensure_cluster_image(self.images[f"payer-{payer}"])
            evidence = ManualPaymentEvidence(self, profile, payer, payee, message)
            evidence.validate_clock()
            before_runs, before = evidence.run_ids(), evidence.balances()
            (directory / "before.json").write_text(json.dumps({
                **identity, "known_run_ids": sorted(before_runs), "balances": before,
            }, indent=2) + "\n")
            job = self._payer_job(payer, payee, profile, message, self.context.transport)
            job["metadata"]["name"] = name
            job["spec"]["activeDeadlineSeconds"] = 960
            phase = "create_job"
            self._apply_document(job)
            phase = "wait_job"
            logs = self._wait_job(name, timeout_seconds=960)
            (directory / "payer.log").write_text(self._bounded_public_logs(logs))
            phase = "verify_financial_evidence"
            financial = evidence.verify(before_runs, before)
            (directory / "evidence.json").write_text(json.dumps(financial, indent=2) + "\n")
        except Exception as error:
            try:
                logs = self._bounded_public_logs(self._job_logs(name))
            except Exception:
                logs = "payer logs unavailable; inspect the retained Job\n"
            (directory / "payer.log").write_text(logs)
            # An exception can contain raw upstream responses. Persist its type,
            # never that response body or a rendered Pod/Secret specification.
            (directory / "failure.json").write_text(json.dumps({
                **identity, "status": "failed", "phase": phase, "error_type": type(error).__name__,
                "message": "Inspect the retained Job, bounded payer.log, and before.json; no financial result is assumed.",
            }, indent=2) + "\n")
            raise RuntimeError(
                f"manual payment {name} failed during {phase} ({type(error).__name__}); "
                f"inspect {directory / 'failure.json'} and bounded payer.log"
            ) from None
        return {**identity, "status": "verified", "run_id": financial["run_id"],
                "transaction": financial["transaction"]}

    @staticmethod
    def _bounded_public_logs(logs: str) -> str:
        sensitive = re.compile(
            r"authorization|payment-signature|payment-authorization|private[_ -]?key|password|"
            r"secret|sealed_credential|credential[\"']?\s*[:=]|signature[\"']?\s*[:=]|"
            r"signedTransaction|signed_transaction|signed_tx|serializedTransaction|0x[0-9a-f]{129,}|"
            r"access[_ -]?token|refresh[_ -]?token",
            re.IGNORECASE,
        )
        lines = ["[redacted sensitive log line]" if sensitive.search(line) else line for line in logs.splitlines()]
        return ("\n".join(lines) + "\n").encode("utf-8")[-131072:].decode("utf-8", errors="ignore")

    def acceptance_matrix(self) -> list[dict]:
        results = []
        for payer in LANGUAGES:
            for payee in LANGUAGES:
                result = self.pay(payer, payee)
                results.append(result)
                path = self.context.report_root / "payment/manual/matrix.json"
                path.write_text(json.dumps(results, indent=2) + "\n")
        return results
