# AgentVault documentation / 文档导航

AgentVault provides policy-controlled outbound payments and evidence-based inbound settlement through HTTP SDKs.

AgentVault 通过 HTTP SDK，提供受策略约束的付款和以可信结算证据为依据的收款能力。

## Architecture / 架构

- [Architecture overview — English](architecture/overview.md)
- [架构概览 — 中文](architecture/overview_ZH.md)
- [Logical architecture](architecture/assets/agentvault-logical-architecture.svg)
- [Physical architecture](architecture/assets/agentvault—physical-architecture.svg)

The overviews own architecture, domain invariants, payment flows, custody boundaries, and deployment topology. Both language editions reference the same English diagrams.

核心架构、领域不变量、支付流程、托管边界和部署拓扑统一维护在上述概览中。中英文版本引用同一组英文架构图。

## Implementation and operations / 实现与运维

- [Application SDKs and A2A integration](../src/sdk/README.md)
- [Docker deployment](../deploy/docker/README.md)
- [Helm deployment and Secret bootstrap](../deploy/helm/agentvault/README.md)
- [MarketPulse end-to-end verification](../use-cases/marketpulse-feed-subscription/README.md)
- [MarketPulse 端到端验证与验收](../use-cases/marketpulse-feed-subscription/README_CN.md)
