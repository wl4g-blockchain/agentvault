from pathlib import Path


E2E_ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = E2E_ROOT.parents[2]
DEPLOY_ROOT = E2E_ROOT / "deploy"
REPORT_ROOT = E2E_ROOT / "reports"
