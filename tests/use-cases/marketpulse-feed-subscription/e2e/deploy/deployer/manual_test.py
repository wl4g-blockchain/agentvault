from __future__ import annotations
import copy
import json
import subprocess
from contextlib import contextmanager, nullcontext, redirect_stdout
import io
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import traceback
import unittest
from unittest.mock import Mock, patch
ROOT=Path(__file__).resolve().parents[6]
sys.path.insert(0,str(ROOT / "use-cases/marketpulse-feed-subscription/e2e"))

from deploy.deployer.manual import ManualAcceptanceDeployer
from verifier.payment import s29_manual

class ManualLifecycleTest(unittest.TestCase):
    def test_failure_injection_option_defaults_false_and_reaches_verification_context(self):
        import runner
        from common.context import VerificationContext
        defaults=VerificationContext("unit",Path("reports"),Path("deploy"),"manual-payment",1,
                                     "rust","rust","grpc","kubernetes",60,True,False)
        self.assertFalse(defaults.fail_after_settlement)
        for enabled in (False,True):
            arguments=["runner.py","run","--deployer","kubernetes","-s","manual-payment"]
            if enabled: arguments.append("--fail-after-settlement")
            parsed=runner.parser().parse_args(arguments[1:])
            self.assertIs(parsed.fail_after_settlement,enabled)
            contexts=[]
            progress=SimpleNamespace(step=lambda *args,**kwargs:nullcontext())
            with tempfile.TemporaryDirectory() as directory, patch.object(sys,"argv",arguments), \
                 patch.object(runner.ProgressLogger,"configured",return_value=progress), \
                 patch.object(runner,"create_run_root",return_value=("unit",Path(directory))), \
                 patch.object(runner,"ReportBook"), \
                 patch.object(runner,"verifier",return_value=lambda context:contexts.append(context) or "unit fixture"), \
                 redirect_stdout(io.StringIO()):
                runner.main()
            self.assertEqual(len(contexts),1)
            self.assertIs(contexts[0].fail_after_settlement,enabled)

    def test_only_explicit_failure_injection_reaches_each_payer_job(self):
        from common.payer_identity import LANGUAGES
        for language in LANGUAGES:
            for enabled in (False,True):
                port=object.__new__(ManualAcceptanceDeployer)
                port.context=SimpleNamespace(fail_after_settlement=enabled)
                port._image_values=Mock(return_value={})
                port._payer_chart=Mock(return_value=Path("payer-chart"))
                port._template=Mock(return_value=[{"kind":"Job","metadata":{},"spec":{}}])
                port._payer_job(language,"rust","evm","a2a","grpc")
                settings=port._template.call_args.args[3]
                self.assertEqual(settings["env.QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT"],"once" if enabled else "")
                self.assertEqual(settings["env.MARKETPULSE_MESSAGE_PROTOCOL"],"a2a")

    def test_deploy_action_never_triggers_an_automatic_payment(self):
        context=SimpleNamespace(deployer="kubernetes",scenario="manual")
        port=Mock(); port.deploy.return_value={"profiles":["evm","solana","tempo"]}
        with patch.object(s29_manual,"ManualAcceptanceDeployer",return_value=port): result=s29_manual.verify(context)
        port.deploy.assert_called_once_with(); port.pay.assert_not_called(); port.acceptance_matrix.assert_not_called()
        self.assertIn("No payer payment was triggered",result)

    def test_manual_verifier_rejects_non_kubernetes_before_constructing_deployer(self):
        with patch.object(s29_manual,"ManualAcceptanceDeployer") as port:
            with self.assertRaisesRegex(ValueError,"kubernetes"): s29_manual.verify(SimpleNamespace(deployer="docker"))
            port.assert_not_called()

    def test_suspended_payer_is_an_explicit_cron_template_without_immediate_job(self):
        port=object.__new__(ManualAcceptanceDeployer)
        port._payer_job=Mock(return_value={"metadata":{"labels":{"purpose":"e2e"}},"spec":{"template":{"spec":{"containers":[]}}}})
        documents=[]; port._apply_document=documents.append
        port._suspended_payer("go")
        self.assertEqual(len(documents),1)
        cron=documents[0]
        self.assertEqual(cron["kind"],"CronJob")
        self.assertTrue(cron["spec"]["suspend"])
        self.assertEqual(cron["spec"]["concurrencyPolicy"],"Forbid")
        self.assertEqual(cron["metadata"]["name"],"quantstream-golang")

    def retained_state(self):
        from common.payer_identity import LANGUAGES
        from deploy.deployer.manual_profiles import ManualProfiles
        return {"run_id":"initial-deploy", "solana":{"mint":"unchanged-mint"}, "tempo_funding":{},
                "profiles":list(ManualProfiles.NAMES), "languages":list(LANGUAGES),
                "images":{"agentvault":"localhost/vault:frozen", "admin-ui":"localhost/ui:frozen",
                    **{f"{role}-{language}":f"localhost/{role}-{language}:frozen"
                       for role in ("payer","payee") for language in LANGUAGES}}}

    def retained_port(self, directory):
        port=object.__new__(ManualAcceptanceDeployer)
        port.state_path=Path(directory)/"manual-public.json"
        port.context=SimpleNamespace(report_root=Path(directory)/"report")
        for name in ("cleanup","_bootstrap_manifests","_install_dependencies","_configure_vault","_install_agentvault_manual","pay"):
            setattr(port,name,Mock(side_effect=AssertionError("service resume must not reset, import, or pay")))
        for name in ("_preload_local_dependencies","_ensure_cluster_image","_apply_business_claims","_deploy_payee","_suspended_payer","_install_admin_ui"):
            setattr(port,name,Mock())
        return port

    def test_service_resume_preserves_chain_state_and_deploys_only_explicit_templates(self):
        from common.payer_identity import LANGUAGES
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            initial=self.retained_state()
            port.state_path.write_text(json.dumps(initial))
            result=port.services()
            self.assertTrue(result["services_ready"])
            self.assertEqual(result["solana"],initial["solana"])
            self.assertEqual(port.images,initial["images"])
            self.assertEqual(port._deploy_payee.call_count,15)
            self.assertEqual(port._suspended_payer.call_count,5)
            self.assertEqual(port._ensure_cluster_image.call_count,len(LANGUAGES))
            port._install_admin_ui.assert_called_once_with("rust")
            for name in ("cleanup","_bootstrap_manifests","_install_dependencies","_configure_vault","_install_agentvault_manual","pay"):
                getattr(port,name).assert_not_called()
            self.assertEqual(json.loads(port.state_path.read_text()),result)

    def test_service_rollout_failure_retains_original_chain_identity_for_retry(self):
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            initial=self.retained_state()
            port.state_path.write_text(json.dumps(initial))
            port._deploy_payee.side_effect=RuntimeError("PVC image unavailable")
            with self.assertRaisesRegex(RuntimeError,"PVC image"):
                port.services()
            saved=json.loads(port.state_path.read_text())
            self.assertEqual(saved["solana"],initial["solana"])
            self.assertEqual(saved["images"],initial["images"])
            self.assertFalse(saved["services_ready"])
            port.cleanup.assert_not_called()

    def test_service_resume_never_initializes_missing_or_invalid_state(self):
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            with self.assertRaisesRegex(RuntimeError,"never initializes chains"):
                port.services()
            invalid={**self.retained_state(),"profiles":["evm"]}
            port.state_path.write_text(json.dumps(invalid))
            with self.assertRaisesRegex(RuntimeError,"incompatible"):
                port.services()
            port._preload_local_dependencies.assert_not_called()
            port.cleanup.assert_not_called()

    def test_manual_services_verifier_never_dispatches_deployment_or_payment(self):
        context=SimpleNamespace(deployer="kubernetes",scenario="manual-services")
        port=Mock(); port.services.return_value={"services_ready":True}
        with patch.object(s29_manual,"ManualAcceptanceDeployer",return_value=port):
            result=s29_manual.verify(context)
        port.services.assert_called_once_with()
        port.deploy.assert_not_called(); port.pay.assert_not_called(); port.acceptance_matrix.assert_not_called()
        self.assertIn("financial history were preserved",result)

    def test_initial_deploy_persists_chain_catalog_before_application_build_can_fail(self):
        @contextmanager
        def forwards(*args):
            yield 12345
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            port.context.run_id="deploy-before-apps"
            port.images=self.retained_state()["images"]
            port.docker=["docker"]
            port.environment={}
            port.manual_secrets={}
            chart=Path(directory)/"tempo-chart"
            chart.mkdir()
            (chart/"values.yaml").write_text('args: [node, --dev, --dev.block-time, 1ms]\nimage: {repository: tempo, tag: pinned}\n')
            port._component_chart=Mock(return_value=chart)
            port.cleanup=Mock()
            for name in ("_apply_namespaces","_bootstrap_manifests","_install_dependencies","_bootstrap_emqx",
                         "_helm_install","_wait_tempo_faucet_window","_apply_config_map","_install_provider","_configure_vault"):
                setattr(port,name,Mock())
            port._runtime_forwards=forwards
            port._forward_service=forwards
            port._provider_image=Mock(return_value="localhost/provider:frozen")
            port._build_matrix_images=Mock(side_effect=RuntimeError("application build unavailable"))
            fixture=Mock()
            fixture.solana.return_value=({"mint":"persist-before-apps"},{"SOLANA_FACILITATOR_KEYPAIR":"kept-only-in-memory"})
            fixture.fund_tempo.return_value={"funded":True}
            with patch("deploy.deployer.manual.ManualChainFixture",return_value=fixture), patch("deploy.deployer.manual.execute",return_value=SimpleNamespace(returncode=1)), patch("deploy.deployer.manual.ManualProfiles.resources",return_value="public resource catalog"):
                with self.assertRaisesRegex(RuntimeError,"application build"):
                    port.deploy()
            saved=json.loads(port.state_path.read_text())
            self.assertEqual(saved["solana"]["mint"],"persist-before-apps")
            self.assertEqual(saved["images"],port.images)
            self.assertFalse(saved["services_ready"])
            self.assertNotIn("kept-only-in-memory",port.state_path.read_text())
            port._install_agentvault_manual.assert_not_called()

    def payment_port(self, directory):
        port=object.__new__(ManualAcceptanceDeployer)
        port.context=SimpleNamespace(report_root=Path(directory),payer_language="rust",payee_language="go",
                                     payment_profile="evm",message_protocol="a2a",transport="grpc",fail_after_settlement=False)
        port.images={"payer-rust":"localhost/payer:frozen"}
        port._ensure_cluster_image=Mock()
        port._payer_job=Mock(return_value={"metadata":{},"spec":{}})
        port._apply_document=Mock()
        port._wait_job=Mock(return_value="event=manual.payer.completed run_id=run-public\n")
        port._job_logs=Mock(return_value='event=failed run_id=run-public\n{"credential":"sensitive-fixture-value"}\n')
        return port

    def test_failed_payment_preserves_before_snapshot_and_public_identity_before_wait(self):
        with tempfile.TemporaryDirectory() as directory:
            port=self.payment_port(directory)
            port.context.fail_after_settlement=True
            evidence=Mock(); evidence.run_ids.return_value={"previous-run"}; evidence.balances.return_value={"recipient":123}
            def wait(*args,**kwargs):
                report=next((Path(directory)/"payment/manual").iterdir())
                self.assertTrue((report/"request.json").is_file())
                self.assertTrue((report/"before.json").is_file())
                raise RuntimeError("authorization: sensitive-fixture-value")
            port._wait_job.side_effect=wait
            with patch("deploy.deployer.manual.ManualPaymentEvidence",return_value=evidence):
                with self.assertRaisesRegex(RuntimeError,"wait_job"):
                    ManualAcceptanceDeployer.pay(port)
            report=next((Path(directory)/"payment/manual").iterdir())
            failure=json.loads((report/"failure.json").read_text())
            self.assertEqual(failure["phase"],"wait_job")
            self.assertEqual(failure["payer"],"rust")
            self.assertEqual(failure["error_type"],"RuntimeError")
            self.assertTrue(failure["fail_after_settlement"])
            self.assertTrue(json.loads((report/"request.json").read_text())["fail_after_settlement"])
            self.assertNotIn("sensitive-fixture-value",(report/"failure.json").read_text())
            self.assertNotIn("sensitive-fixture-value",(report/"payer.log").read_text())
            self.assertIn("run-public",(report/"payer.log").read_text())
            self.assertFalse((report/"evidence.json").exists())
            evidence.verify.assert_not_called()

    def test_financial_verification_failure_is_retained_and_never_reports_success(self):
        with tempfile.TemporaryDirectory() as directory:
            port=self.payment_port(directory)
            evidence=Mock(); evidence.run_ids.return_value=set(); evidence.balances.return_value={"recipient":0}
            evidence.verify.side_effect=RuntimeError("wallet isolation mismatch")
            with patch("deploy.deployer.manual.ManualPaymentEvidence",return_value=evidence):
                with self.assertRaisesRegex(RuntimeError,"verify_financial_evidence"):
                    ManualAcceptanceDeployer.pay(port)
            report=next((Path(directory)/"payment/manual").iterdir())
            failure=json.loads((report/"failure.json").read_text())
            self.assertEqual(failure["phase"],"verify_financial_evidence")
            self.assertEqual(failure["status"],"failed")
            self.assertTrue((report/"before.json").exists())
            self.assertFalse((report/"evidence.json").exists())

    def test_raw_credential_failure_cannot_escape_through_outer_traceback(self):
        with tempfile.TemporaryDirectory() as directory:
            port=self.payment_port(directory)
            evidence=Mock(); evidence.run_ids.return_value=set(); evidence.balances.return_value={"recipient":0}
            port._wait_job.side_effect=RuntimeError("Credential: sensitive-fixture-value")
            with patch("deploy.deployer.manual.ManualPaymentEvidence",return_value=evidence):
                try:
                    ManualAcceptanceDeployer.pay(port)
                except RuntimeError as error:
                    self.assertNotIn("sensitive-fixture-value","".join(traceback.format_exception(error)))
                    self.assertIn("wait_job",str(error))
                    self.assertTrue(error.__suppress_context__)
                    self.assertIsNone(error.__cause__)
                else:
                    self.fail("a failed Job must not be reported as verified")
            report=next((Path(directory)/"payment/manual").iterdir())
            self.assertNotIn("sensitive-fixture-value",(report/"failure.json").read_text())
            self.assertNotIn("sensitive-fixture-value",(report/"payer.log").read_text())

    def test_manual_cleanup_defers_pvc_until_helm_pods_have_stopped(self):
        from deploy.deployer.kubernetes import (
            KubernetesE2EDeployer, AGENTVAULT_NAMESPACE, INFRA_NAMESPACE, PAYMENT_NAMESPACE,
            PAYEE_NAMESPACE, PAYER_NAMESPACE,
        )
        port=object.__new__(ManualAcceptanceDeployer)
        order=[]
        port._kubectl=Mock(side_effect=lambda *args,**kwargs: order.append(args))
        with patch.object(KubernetesE2EDeployer,"cleanup",side_effect=lambda: order.append("base") or "cleaned"):
            self.assertEqual(port.cleanup(),"cleaned")
        self.assertEqual(order[-1],"base")
        commands=order[:-1]
        self.assertEqual(len(commands),5)
        self.assertEqual({args[args.index("-n")+1] for args in commands},
                         {AGENTVAULT_NAMESPACE,INFRA_NAMESPACE,PAYMENT_NAMESPACE,PAYEE_NAMESPACE,PAYER_NAMESPACE})
        for args in commands:
            self.assertEqual(args[1],"deployment,service,cronjob,job")
            self.assertEqual(args[args.index("-l")+1],"agentvault.io/manual-acceptance=true")
            self.assertNotIn("pvc",args[1])

    def test_retained_payer_log_is_bounded_and_redacts_full_sensitive_lines(self):
        raw='{"authorization":"private"}\n{"signature":"private"}\n'+('public event\n'*20000)
        short=ManualAcceptanceDeployer._bounded_public_logs('{"authorization":"private"}\n{"signature":"private"}\npublic event')
        self.assertNotIn("private",short)
        self.assertIn("[redacted sensitive log line]",short)
        safe=ManualAcceptanceDeployer._bounded_public_logs(raw)
        self.assertLessEqual(len(safe.encode()),131072)
        self.assertNotIn("private",safe)
        self.assertIn("public event",safe)
        unicode_log=ManualAcceptanceDeployer._bounded_public_logs("中"*60000)
        self.assertLessEqual(len(unicode_log.encode("utf-8")),131072)

    def test_only_manual_anvil_install_uses_persistent_automine_without_mutating_chart(self):
        import yaml
        from deploy.deployer.kubernetes import KubernetesE2EDeployer, PAYMENT_NAMESPACE, INFRA_NAMESPACE
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            port._apply_document=Mock()
            chart=Path(directory)/"anvil-chart"
            chart.mkdir()
            original='args: [--host=0.0.0.0, --chain-id=31337]\n'
            (chart/"values.yaml").write_text(original)
            with patch.object(KubernetesE2EDeployer,"_helm_install",return_value="installed") as base:
                result=ManualAcceptanceDeployer._helm_install(port,"anvil",chart,PAYMENT_NAMESPACE,wait=False)
                override=base.call_args.kwargs["values"][-1]
                configured=yaml.safe_load(override.read_text())
                self.assertEqual(configured["args"], ["--host=0.0.0.0","--chain-id=31337",
                    "--state=/var/lib/anvil/state.json","--state-interval=1","--preserve-historical-states"])
                self.assertFalse(any(arg.startswith(("--block-time", "--prune-history", "--transaction-block-keeper"))
                                     for arg in configured["args"]))
                self.assertEqual(configured["persistence"],
                    {"enabled":True,"existingClaim":"manual-anvil-state","mountPath":"/var/lib/anvil"})
                self.assertEqual(configured["resources"]["limits"]["memory"],"2Gi")
                claim=port._apply_document.call_args.args[0]
                self.assertEqual(claim["metadata"]["name"],"manual-anvil-state")
                self.assertEqual(claim["metadata"]["labels"]["agentvault.io/e2e-business-evidence"],"true")
                self.assertEqual((chart/"values.yaml").read_text(),original)
                self.assertEqual(result,"installed")
                ManualAcceptanceDeployer._helm_install(port,"postgres",chart,INFRA_NAMESPACE)
                self.assertEqual(base.call_args.kwargs["values"],())

    def test_manual_anvil_chart_renders_a_single_writer_and_durable_state(self):
        import yaml
        from deploy.deployer.kubernetes import KubernetesE2EDeployer, PAYMENT_NAMESPACE
        chart=ROOT/"use-cases/marketpulse-feed-subscription/e2e/deploy/anvil/helm"
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory);port._apply_document=Mock()
            with patch.object(KubernetesE2EDeployer,"_helm_install") as base:
                port._helm_install("anvil",chart,PAYMENT_NAMESPACE)
            command=["helm","template","anvil",str(chart),"--namespace",PAYMENT_NAMESPACE]
            for manual in (False,True):
                extra=["--values",str(base.call_args.kwargs["values"][-1])] if manual else []
                docs=list(yaml.safe_load_all(subprocess.check_output([*command,*extra],text=True)))
                deployment=next(d for d in docs if d and d["kind"]=="Deployment")
                spec=deployment["spec"];pod=spec["template"]["spec"];app=pod["containers"][0]
                if manual:
                    self.assertEqual(spec["strategy"]["type"],"Recreate")
                    self.assertEqual(pod["volumes"],[{"name":"data","persistentVolumeClaim":{"claimName":"manual-anvil-state"}}])
                    self.assertEqual(app["volumeMounts"],[{"name":"data","mountPath":"/var/lib/anvil"}])
                    self.assertIn("--state-interval=1",app["args"])
                    self.assertEqual(app["resources"]["limits"]["memory"],"2Gi")
                else:
                    self.assertNotIn("volumes",pod)
                    self.assertNotIn("strategy",spec)
                    self.assertFalse(any(arg.startswith("--state") for arg in app["args"]))

    def test_manual_solana_restarts_reuse_one_claim_without_ledger_reset(self):
        import yaml
        from deploy.deployer.kubernetes import (
            KubernetesE2EDeployer, PAYMENT_NAMESPACE, BUSINESS_EVIDENCE_LABEL, BUSINESS_EVIDENCE_SELECTOR,
        )
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            port._apply_document=Mock()
            chart=Path(directory)/"solana-chart"
            chart.mkdir()
            original='args: [--rpc-port=8899, --ledger=/tmp/solana-ledger, --reset, -r, --reset=true]\n'
            (chart/"values.yaml").write_text(original)
            with patch.object(KubernetesE2EDeployer,"_helm_install",return_value="installed") as base:
                for _ in range(2):
                    result=ManualAcceptanceDeployer._helm_install(port,"solana",chart,PAYMENT_NAMESPACE,wait=False)
                    self.assertEqual(result,"installed")
                    self.assertFalse(base.call_args.kwargs["wait"])
                    override=yaml.safe_load(base.call_args.kwargs["values"][-1].read_text())
                    self.assertEqual(override["args"],["--rpc-port=8899","--ledger=/tmp/solana-ledger","--log"])
                    self.assertEqual(override["env"],{"RUST_LOG":"warn"})
                    self.assertEqual(override["persistence"],{"enabled":True,"existingClaim":"manual-solana-ledger"})
                    self.assertEqual(override["resources"]["limits"]["cpu"],"500m")
            claims=[call.args[0] for call in port._apply_document.call_args_list]
            self.assertEqual(len(claims),2)
            self.assertEqual(claims[0],claims[1])
            self.assertEqual(claims[0]["kind"],"PersistentVolumeClaim")
            self.assertEqual(claims[0]["metadata"],{
                "name":"manual-solana-ledger","namespace":PAYMENT_NAMESPACE,
                "labels":{"app.kubernetes.io/part-of":"agentvault-e2e",BUSINESS_EVIDENCE_LABEL:"true"}})
            selector=dict(part.split("=",1) for part in BUSINESS_EVIDENCE_SELECTOR.split(","))
            self.assertTrue(all(claims[0]["metadata"]["labels"].get(key)==value for key,value in selector.items()))
            self.assertEqual(claims[0]["spec"]["accessModes"],["ReadWriteOnce"])
            self.assertEqual((chart/"values.yaml").read_text(),original)
            port.cleanup.assert_not_called()
            port.pay.assert_not_called()

    def test_solana_manual_override_does_not_affect_other_namespace(self):
        from deploy.deployer.kubernetes import KubernetesE2EDeployer, INFRA_NAMESPACE
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            port._apply_document=Mock()
            with patch.object(KubernetesE2EDeployer,"_helm_install",return_value="installed") as base:
                ManualAcceptanceDeployer._helm_install(port,"solana",Path(directory),INFRA_NAMESPACE)
                self.assertEqual(base.call_args.kwargs["values"],())
            port._apply_document.assert_not_called()

    def test_solana_manual_chart_renders_pvc_single_writer_and_resume_args(self):
        import yaml
        from deploy.deployer.kubernetes import KubernetesE2EDeployer, PAYMENT_NAMESPACE
        chart=ROOT/"use-cases/marketpulse-feed-subscription/e2e/deploy/solana/helm"
        baseline=(chart/"values.yaml").read_text()
        with tempfile.TemporaryDirectory() as directory:
            port=self.retained_port(directory)
            port._apply_document=Mock()
            with patch.object(KubernetesE2EDeployer,"_helm_install") as base:
                ManualAcceptanceDeployer._helm_install(port,"solana",chart,PAYMENT_NAMESPACE)
            command=["helm","template","solana",str(chart),"--namespace",PAYMENT_NAMESPACE]
            for manual in (False,True):
                extra=["--values",str(base.call_args.kwargs["values"][-1])] if manual else []
                rendered=subprocess.check_output([*command,*extra],text=True)
                deployment=next(d for d in yaml.safe_load_all(rendered) if d and d["kind"]=="Deployment")
                app=deployment["spec"]["template"]["spec"]["containers"][0]
                data=next(v for v in deployment["spec"]["template"]["spec"]["volumes"] if v["name"]=="data")
                if manual:
                    self.assertNotIn("--reset",app["args"])
                    self.assertIn("--ledger=/tmp/solana-ledger",app["args"])
                    self.assertEqual(app["args"].count("--log"),1)
                    self.assertIn({"name":"RUST_LOG","value":"warn"},app["env"])
                    self.assertEqual(data["persistentVolumeClaim"]["claimName"],"manual-solana-ledger")
                    self.assertNotIn("emptyDir",data)
                    self.assertEqual(deployment["spec"]["strategy"]["type"],"Recreate")
                    self.assertEqual(deployment["spec"]["replicas"],1)
                    self.assertEqual(app["resources"]["limits"]["cpu"],"500m")
                    self.assertEqual(app["volumeMounts"],[{"name":"data","mountPath":"/tmp/solana-ledger"}])
                else:
                    self.assertIn("--reset",app["args"])
                    self.assertIn("emptyDir",data)
            self.assertEqual((chart/"values.yaml").read_text(),baseline)

    def test_payee_startup_probe_defers_liveness_without_changing_health_checks(self):
        port=object.__new__(ManualAcceptanceDeployer)
        port.images={"payee-java":"verified-runtime"}
        port._ensure_cluster_image=Mock();port._image_values=Mock(return_value={})
        port._payee_chart=Mock(return_value=Path("java-chart"));port._kubectl=Mock()
        app={"name":"marketpulse-java","ports":[{"containerPort":8081}],
             "livenessProbe":{"httpGet":{"path":"/healthz","port":8081},"periodSeconds":5}}
        proxy={"name":"envoy","ports":[{"containerPort":9901}]}
        document={"kind":"Deployment","metadata":{},"spec":{"template":{"spec":{"containers":[app,proxy]}}}}
        port._template=Mock(return_value=[document]);port._apply_document=Mock()
        ManualAcceptanceDeployer._deploy_payee(port,"java","tempo")
        self.assertEqual(app["startupProbe"]["failureThreshold"]*app["startupProbe"]["periodSeconds"],180)
        self.assertEqual(app["livenessProbe"]["periodSeconds"],5)
        self.assertNotIn("startupProbe",proxy)

class ManualImagePinTest(unittest.TestCase):
    def port(self):
        port=object.__new__(ManualAcceptanceDeployer)
        port.environment={}
        port.images={"payer-go":"localhost/manual-payer:local", "payee-go":"localhost/manual-payee:local"}
        port._run=Mock(return_value="")
        return port

    def test_only_explicit_manual_payer_images_are_pinned_after_verified_import(self):
        from deploy.deployer.kubernetes import KubernetesE2EDeployer
        port=self.port();port._pin_payer_image=Mock()
        with patch.object(KubernetesE2EDeployer,"_ensure_cluster_image") as ensure:
            for reference in (port.images["payer-go"],port.images["payee-go"],"docker.io/library/busybox:1"):
                port._ensure_cluster_image(reference)
        self.assertEqual(ensure.call_count,3)
        port._pin_payer_image.assert_called_once_with(port.images["payer-go"])

    def test_native_label_is_not_treated_as_success_until_cri_observes_pinned(self):
        port=self.port();reference=port.images["payer-go"]
        statuses=[SimpleNamespace(returncode=0,stdout=json.dumps({"status":{"pinned":False}})),
                  SimpleNamespace(returncode=0,stdout=json.dumps({"status":{"pinned":True}}))]
        with patch("deploy.deployer.manual.execute",side_effect=statuses) as inspect, \
             patch("deploy.deployer.manual.time.sleep") as sleep:
            port._pin_payer_image(reference)
        labels=[call.args[0] for call in port._run.call_args_list if "label" in call.args[0]]
        self.assertEqual(labels,[["sudo","-n","k3s","ctr","-n","k8s.io","images","label",reference,
            "io.cri-containerd.pinned=pinned","io.agentvault.manual-image-pin=true"]])
        self.assertEqual(inspect.call_count,2)
        sleep.assert_called_once_with(.25)
        filters=[call.args[0][-1] for call in port._run.call_args_list if "list" in call.args[0]]
        self.assertIn('labels."io.cri-containerd.pinned"=="pinned"',filters)
        self.assertIn('labels."io.agentvault.manual-image-pin"=="true"',filters)

    def test_operator_owned_pin_is_preserved_without_claiming_cleanup_ownership(self):
        port=self.port();reference=port.images["payer-go"]
        port._image_refs_with_label=Mock(side_effect=[{reference},set()])
        with patch("deploy.deployer.manual.execute",return_value=SimpleNamespace(returncode=0,stdout='{"status":{"pinned":true}}')):
            port._pin_payer_image(reference)
        port._run.assert_not_called()

    def test_runtime_without_cri_pin_support_is_rejected(self):
        port=self.port()
        with patch("deploy.deployer.manual.execute",return_value=SimpleNamespace(returncode=0,stdout='{"status":{"pinned":false}}')) as inspect, \
             patch("deploy.deployer.manual.time.sleep"):
            with self.assertRaisesRegex(RuntimeError,"did not expose.*pinned"):
                port._pin_payer_image(port.images["payer-go"])
        self.assertEqual(inspect.call_count,20)

    def test_cleanup_entrypoint_uses_single_factory_with_or_without_manual_state(self):
        import runner
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for manual in (False,True):
                if manual:(root/"manual-public.json").write_text('{"public":true}')
                ordinary=Mock()
                with patch("deploy.deployer.manual.RUNTIME_ROOT",root), \
                     patch.object(runner,"create_deployer",return_value=ordinary) as factory, \
                     patch("deploy.deployer.manual.ManualAcceptanceDeployer") as manual_factory:
                    runner.cleanup(60,"kubernetes",comprehensive=False,
                        progress=SimpleNamespace(step=lambda *args,**kwargs:nullcontext()))
                factory.assert_called_once();ordinary.cleanup.assert_called_once()
                manual_factory.assert_not_called();ordinary.cleanup_images.assert_not_called()
