# AgentVault

面向自主 AI Agent 的企业级双向机器支付基础设施。

AgentVault 是一个 Rust 中间件，同时承载两类业务：

- Payer / OUTBOUND：购买付费 HTTP/MCP 服务，执行预算预留、超额审批、Intent 绑定签名和结算对账。
- Payee / INBOUND：按固定 Offer 对 HTTP Route/MCP Tool 收费，签发 402 Challenge，经外部 Provider 验证/结算并生成可信 Receipt。

私钥和所有资金状态只保存在 PostgreSQL 或 SQLite。私钥采用 AES-256-GCM 信封加密、完整 UUID v4 Nonce 和不可变版本；Ledger 只追加。Memory/Redis 只缓存可重建数据，不能决定预算、审批、幂等、结算或余额。

## 架构定位

```text
Flowgent / Sigbot / 第三方 Agent
        │ Rust / Go / Python / TypeScript / Java SDK
        ▼
AuthGuard + Envoy（默认集成，可显式关闭）── HTTP 身份与 IAM
        ▼
AgentVault ── Scenario / Policy / Approval / Signer / Settlement / Ledger
        ▼
外部 x402 Facilitator / MPP Provider / 固定 RPC
```

AgentVault 不实现登录、OAuth2/OIDC、会话和 IAM 策略引擎。标准 Docker/Helm 部署默认由 AuthGuard 保护所有 HTTP 支付和 Admin API；显式关闭后，HTTP 按设计完全开放。MQTT 不复用 HTTP 身份，而是使用 Broker 认证、默认拒绝的 Topic ACL 和持久化 Client/Scenario 映射。审批决策使用独立认证的管理员 Topic，AgentVault 将其中的 Client 与审批 Principal 再绑定到 SQL Scenario/Approver 映射，报文体不能自报这两种身份。

资源关系为：

```text
Client 1 ── N Scenario 1 ── N Policy 1 ── N Wallet
                                      N ── 1 共享 Wallet
```

同一 Wallet 可以被多个 Policy 共享，用于企业公共资金池；共享时必须设置 Wallet 聚合硬限额。Policy 对 INBOUND 与 OUTBOUND 分别设置单笔、每日、审批阈值和审批追加额度，收款侧同样可以防止资源超卖。

完整流程、数据表关系、一致性、安全和部署边界见 [架构白皮书](docs/index.md)。

## 仓库结构

```text
src/          Rust 服务：apm、cache、cmd、config、controller、handler、
              model、protocol、storage、utils
migrations/   PostgreSQL/SQLite 对齐的 001_init DDL/DML
src/adapters/ Rust、Go、Python、TypeScript、Java SDK 与共享传输 Proto
web/          AgentVault 运维 Dashboard
deploy/       Grafana、Docker、Helm 部署资源
use-cases/    MarketPulse/QuantStream 真实多进程支付 E2E
docs/         唯一架构白皮书入口
```

项目不维护对外 API 描述目录。多语言 SDK 的强类型 DTO 和契约测试是客户端调用边界。

## 构建与测试

```bash
make fmt-check
make lint
make test
make build
```

Makefile 会在每次 Rust 构建前检查 `target/`，超过 10 GiB 时执行 `cargo clean`。

多语言 SDK：

```bash
(cd src/adapters/rust && cargo test --locked)
(cd src/adapters/golang && go test ./...)
(cd src/adapters/python && PYTHONPATH=. python3 -m unittest -v $(find tests -type f -name '*_test.py' | sort | sed 's#/#.#g;s#\.py$##'))
(cd src/adapters/typescript && npm ci && npm test && npm run build)
(cd src/adapters/java && mvn test)
```

Dashboard：

```bash
(cd web && npm ci && npm test && npm run build)
(cd use-cases/marketpulse-feed-subscription/e2e && \
  python3 runner.py -s web --e2e-sdk-scope full \
  --matrix-evidence-run <已完成的Matrix运行目录>)
```

## 部署

默认 Docker 拓扑集成 AuthGuard 与 Envoy：

```bash
cp deploy/docker/.env.example deploy/docker/.env
${EDITOR:-vi} deploy/docker/.env
docker compose --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.yaml up -d
```

隔离可信网络可明确选择 `docker-compose.open.yaml`。Helm Chart 位于 `deploy/helm/agentvault/`，同样默认启用 AuthGuard 依赖。

仓库配置默认关闭 `agentvault.payer.enabled` 与 `agentvault.payee.enabled`。生产必须由运营方提供 SQL、Wallet KEK、按方向独立的协议/Provider、共享链注册表和可选的认证 MQTT 配置及 SecretRef。

## 真实 E2E

`use-cases/marketpulse-feed-subscription/e2e/` 从零构建两个业务系统：

- MarketPulse Payee：免费昨日新闻 MCP Tool 与付费实时新闻 Tool。
- QuantStream Payer：逻辑等价的 Rust、Go、Python、TypeScript、Java 采集服务和管理员审批 UI。

x402 SDK 矩阵使用 PostgreSQL、带认证/ACL 的 EMQX、官方 x402 Facilitator、Anvil/TestUSDC、AgentVault、Payee 和 SDK 进程，验证 402 重试、MQTT/gRPC 签名、日限额审批、一次性 Grant、EVM 结算、Receipt/Ledger/交易绑定、Facilitator 精确 Gas 与交付恢复。显式 x402 Solana 门禁让固定版本的上游 client/facilitator 访问真实本地 Validator，并验证 SPL 与 Lamport 手续费精确守恒。独立 MPP 门禁使用固定镜像摘要的官方 Tempo localnet 和由官方 MPP Rust SDK 支撑的 HTTP 结算边界，验证 Vault 签名 type-0x76 charge、verify/settle、丢响应恢复、单次广播、回执及包含 Gas 的 pathUSD 守恒。

五种 SDK 的协议 Provider 均以 x402 v2 处理 HTTP（`PAYMENT-REQUIRED`，兼容 JSON body 且冲突即拒绝）和 MCP JSON-RPC（`structuredContent`/文本 Challenge 及协议自有 `_meta` 凭据/回执）。MPP 同样通过官方 paymentauth wire 类型覆盖 HTTP 与 MCP；付款认证头不得覆盖无关的业务 Bearer。进入受保护业务 Handler 前会移除协议自有付款凭据元数据。

```bash
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s structure
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s infrastructure
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s payer
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s payee
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s full \
  --payer-language rust --payee-language rust
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s mpp-tempo
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s x402-solana
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s matrix # simple：8 组
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s matrix \
  --e2e-sdk-scope full # full：50 组
```

x402 EVM 五语言矩阵、x402 Solana 支付门禁与 MPP Tempo charge 门禁分别生成不可变报告，避免一条链路掩盖另一条链路的问题。

## License

Apache-2.0，见 [LICENSE](LICENSE)。
