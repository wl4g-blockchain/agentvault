# AgentVault Dashboard

The dashboard is a same-origin operations client for AgentVault. It never asks
for, persists, or attaches bearer tokens. In the normal deployment AuthGuard
and Envoy establish the principal before forwarding `/api/v1`; in explicitly
disabled-auth deployments the same browser client works unchanged.

```bash
npm ci
npm run test
npm run build
```

The Vite development server proxies `/api/v1` to `AGENTVAULT_API_TARGET` and
the same-origin `/agentvault-management` health prefix to
`AGENTVAULT_MANAGEMENT_TARGET`. Production routing is owned by the deployment
gateway; the dashboard Nginx image only serves static assets.

Operational lists request and render 10 rows per page. Ledger conservation is
computed from a separately bounded, complete fact read rather than the visible
page. Client governance is read-only and shows the trusted identity binding
beside its associated Scenarios; Scenario IDs are the only configuration
identity and no external-application reference is stored. Scenario Wallet
membership uses a single-column, height-bounded,
scrolling multi-select dropdown so large Wallet catalogs cannot widen the form.

Wallet import uses a temporary RSA-3072 public key and fixed RSA-OAEP/SHA-256.
The shared `src/security/e2ee.ts` utility only encrypts bytes with a public key.
Its only application caller is the Wallet creation submit handler; listing,
navigation, and field input do not acquire sessions or invoke encryption.
The backend consumes the cached private key once, then encrypts the imported EOA
under the existing Wallet KEK before storing it in SQL. Frontend and backend must
be upgraded together; old ECDH sessions cannot be reused.

With Node.js 22.6+ installed, run the Web Crypto → Rust → SQLite contract test
from the repository root (CI also runs it explicitly):

```bash
cargo test --locked --lib handler::wallet_handler::tests::browser_public_key_encryption_reaches_kek_encrypted_sql -- --ignored --exact
```

The existing MarketPulse runner also exercises Wallet creation in real Chromium,
including independent RSA decryption and failure paths, using a controlled API
fixture. From `use-cases/marketpulse-feed-subscription/e2e`:

```bash
python3 -m pip install -r requirements-web.txt
python3 -m playwright install chromium
python3 runner.py -s web --matrix-evidence-run <completed-matrix-run>
```
