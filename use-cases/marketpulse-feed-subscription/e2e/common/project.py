from pathlib import Path


E2E_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = E2E_ROOT.parents[2]
CONFIG_ROOT = E2E_ROOT / "config"
DEPLOY_ROOT = E2E_ROOT / "deploy"
REPORT_ROOT = E2E_ROOT / "reports"


DEPLOY_COMPONENTS = (
    "anvil",
    "emqx",
    "evm",
    "facilitator",
    "mock-mpp-tempo-service",
    "jaeger",
    "mock-payee-marketpulse-golang-service",
    "mock-payee-marketpulse-java-service",
    "mock-payee-marketpulse-python-service",
    "mock-payee-marketpulse-rust-service",
    "mock-payee-marketpulse-typescript-service",
    "mock-payer-quantstream-service-golang",
    "mock-payer-quantstream-service-java",
    "mock-payer-quantstream-service-python",
    "mock-payer-quantstream-service-rust",
    "mock-payer-quantstream-service-typescript",
    "mock-payer-quantstream-ui",
    "postgres",
    "solana",
    "tempo",
)
