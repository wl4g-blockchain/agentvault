# Payment signing use case

This use case exercises a real `walletd` child process. The test provisions a
master key, starts independent API/management/Unix-socket listeners, creates and
rotates a wallet through HTTP, signs through `LocalPaymentSigner`, verifies the
recoverable EOA signature, searches durable history, and inspects Prometheus
metrics. No handler or store is mocked.

Run it from the repository root:

```bash
make e2e
```

The process uses loopback-only ephemeral ports and a temporary owner-only data
directory, so no middleware or credentials are required.
