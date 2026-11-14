# AgentVault Python adapter

Create one `AgentVault` with `GrpcVaultTransport` or `MqttVaultTransport`.
`vault.payer(config).standard()` returns an
`HttpxPayerMiddleware` and performs
the complete 402/approval/retry flow only when a `PayerRequestContext` is
supplied. `FastPayeeMiddleware` is pure ASGI middleware and can be installed on
FastAPI without payment logic in route functions. The inbound entry point is
`vault.payee(config).fast(app, routes)`.

The transport choice is startup configuration; payer and payee application
code remains identical. Run tests with
`PYTHONPATH=. python3 -m unittest -v $(find tests -type f -name '*_test.py' | sort | sed 's#/#.#g;s#\.py$##')`.
