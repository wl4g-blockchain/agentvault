# AgentVault Architecture Overview

[中文](overview_ZH.md) · [Payment Architecture Whitepaper](AgentVault-Payment-Architecture-Whitepaper.md)

AgentVault is the payment authorization and settlement boundary for business applications. Its Payer component selects and authorizes wallet use; its Payee component confirms collection before the application releases a resource. AuthGuard owns identity and API access, while SQL owns durable payment state.

The [whitepaper](AgentVault-Payment-Architecture-Whitepaper.md) is the architecture reference. It defines responsibilities, interfaces, financial invariants, failure handling, and implementation scope.

## Logical architecture

![AgentVault logical architecture and complete payment sequence](assets/agentvault-logical-architecture.svg)

Read the figure from top to bottom:

| Steps | Purpose |
|---|---|
| 1–3 | Shared protected administration/access path. Every SDK call to AgentVault repeats the same access checks. |
| 4–6 | Request a resource and obtain its authoritative payment Challenge. |
| 7–10 | Authorize the original Challenge, reserve budget, check live funds, and return a signed Credential. |
| 11–17 | Retry the resource request, confirm inbound settlement, commit its Ledger, and deliver the resource. |
| 18–20 | Report the payment response, independently verify the transfer, and commit outbound accounting. |

Budget reservation precedes signing. Inbound confirmation precedes business delivery. Outbound accounting requires independent evidence rather than a merchant's success claim. The [numbered-call specification](AgentVault-Payment-Architecture-Whitepaper.md#numbered-calls) defines each step and its state effect.

## Physical deployment

![AgentVault physical deployment](assets/agentvault—physical-architecture.svg)

The default Helm deployment has separate Web replicas and two combined-role backend replicas using shared PostgreSQL. Payer and Payee can run independently; neither role implies another business A2A/MCP server. External Facilitators, Providers, and chain RPC remain separate from AgentVault. See [deployment profiles](AgentVault-Payment-Architecture-Whitepaper.md#deployment).

## Architectural guarantees

- Trusted principal → Client → Scenario → Policy → Wallet membership determines authority; callers cannot select signing facts arbitrarily.
- SQL transactions enforce budgets, one-use approvals, immutable accounting, and idempotent recovery.
- Wallet private keys remain under AgentVault custody; business SDKs receive constrained payment Credentials.
- Business applications retain their task and delivery state, including retry after a confirmed payment.

[SDK integration](../../src/sdk/README.md) · [Docker](../../deploy/docker/README.md) · [Helm](../../deploy/helm/agentvault/README.md) · [End-to-end verification](../../use-cases/marketpulse-feed-subscription/README.md)
