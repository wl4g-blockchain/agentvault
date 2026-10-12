# HTTP and signer API

The default product base path is `/api/v1`. When `server.api_token_file` is
configured, every product request must include `Authorization: Bearer <token>`.
Management endpoints use their separate listener and are not under this base
path.

## Wallets

`POST /wallet` accepts:

```json
{"id":"payer"}
```

It returns `201` and public metadata. `GET /wallet` returns
`{"items":[...],"total":1}`. `GET /wallet/{id}` returns one item. `PUT` on the
item atomically generates and stores a replacement key, changing its address.
`DELETE` returns `204`.

Private-key import is intentionally not an HTTP operation; use the owner-only
offline CLI command.

## Payment signing

`POST /sign` and both transport adapters use exactly this request model:

```json
{
  "version": "wallet.sign.v1",
  "request_id": "b8fc3156-5a54-41ca-934f-95a49fac3a8b",
  "client_id": "flowgent-agent-01",
  "wallet_id": "payer",
  "purpose": "x402.payment",
  "scheme": "eip712-secp256k1",
  "digest_b64": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  "issued_at": 1791772800,
  "expires_at": 1791772830,
  "metadata": {"payment_id": "pay-001"}
}
```

Success returns address, hex signature, and signing time. Protocol or policy
failure returns the same envelope with `error.code` and `error.message` and no
signature. This keeps MQTT and local request/response behavior deterministic.

Client policy uses longest-prefix matching. The most-specific matching client
prefix must authorize both wallet and purpose; it cannot fall back to a broader
prefix. Request validity must fit the configured TTL and clock-skew bounds.

## History search

`GET /sign` supports exact-match filters:

- `request_id`, `client_id`, `wallet_id`, `purpose`, `outcome`
- `from` and `to` completion timestamps (Unix seconds)
- `offset` and `limit` (`limit` defaults to 100 and is capped at 500)

Outcomes are `succeeded`, `invalid_request`, `expired`, `conflict`,
`wallet_not_found`, and `failed`. Results are newest first. A request ID may
have multiple events due to idempotent redelivery or a conflicting attempt;
`GET /sign/{request_id}` returns the latest event.

## Rust client port

Agent code should accept `Arc<dyn IPaymentSigner>` (or a generic implementing
the trait) and call:

```rust,ignore
let response = signer.execute(request).await?;
```

Use `LocalPaymentSigner` for a colocated daemon and `MqttPaymentSigner` for a
broker-connected deployment. Business code should not branch on the adapter.
