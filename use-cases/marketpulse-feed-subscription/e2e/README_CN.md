# MarketPulse 情报订阅真实 E2E

这是 AgentVault 从零构建、部署并进行进程级验收的业务项目，不是把内存
Mock 包装成 E2E。它模拟两个完整系统：

- **MarketPulse** 是卖方数据平台，分别以 Rust/Axum、Go/Gin、
  Python/FastAPI、Java/Spring Boot、TypeScript/Express 实现五套逻辑等价的
  独立服务。每套都保持 `main/Application → server → SDK PayeeMiddleware + MCP
  → handler/feed → storage/feed` 边界，并在 SQLite 中初始化恰好 100 条真实
  语义的新闻数据。`route/feed`（Java 为 `FeedController`）只承载 `/healthz`
  和 `/metrics` 运维接口，收费 MCP Listener 由 server 统一拥有。MCP Endpoint 提供免费的
  `marketpulse.yesterday_news` 与每次 1 USDC 的
  `marketpulse.realtime_news`。收费 Tool 从服务端 Offer 生成 x402 v2 402，
  由 AgentVault 调外部官方 Facilitator 验证/结算，只在确认结算后交付数据。
- **QuantStream** 是买方量化采集平台。Go、Rust、Python、Java、TypeScript
  五个逻辑等价的独立后端定时调用两个 Tool；SDK 检测 402，并按 Case 配置通过 MQTT 或 gRPC 请求
  AgentVault 对 Intent 绑定报文签名，收到 Credential 后重试同一 MCP 操作，
  上报官方付款响应并持久化采集结果。五个服务分别拥有独立 Client、Scenario、
  Policy、MQTT 身份和两个已充值 EOA；业务进程仍只配置 Scenario，不配置 Wallet、
  Policy 或支付投影，也不持有 EOA 私钥。AgentVault 只能从该语言 Scenario 的双成员
  Wallet 池自动选择可签名 EOA。两次真实收费采集必须分别落到两个不同 Wallet，
  证明的是服务端自动选择，而不是把固定 Wallet ID 隐藏到别处。

Node 管理 UI 属于 QuantStream，而非 AgentVault。它订阅持久审批通知，重连后
从权威 API 恢复 Pending Approval，展示精确 Intent/金额并发布管理员决策。
UI 自身 Token 是业务层二级校验，与 HTTP IAM 相互独立。

## 支付拓扑

```text
QuantStream（Go/Rust/Python/Java/TS）
  ├─ 免费 MCP ───────────────────────────────────┐
  └─ 收费 MCP → 402                              │
       └─ AgentVault SDK → MQTT / gRPC authorize │
            └─ Client/Scenario → Policy/Wallet   │
               → 余额/预算/白名单复查             │
               ├─ 通过 → EOA 签名                │
               └─ 超额 → MQTT 审批 ──────────────┼→ QuantStream Admin UI
                    → 内部一次性 JWT Grant        │
                    → 原子消费/复查/签名           │
       ← Credential，重试完全相同的 MCP 请求      │
                         MarketPulse 卖方 ←───────┘
                           → 官方 Facilitator → Anvil/TestUSDC
                           → Receipt + 双向 Ledger
```

直连 E2E 设置 `server.authguard.enabled=false`，因为这里验收支付和 MQTT 边界，
不验收 IAM 产品；HTTP 只在一次性 Loopback Network Namespace 内显式全开放。
根目录的标准 Docker/Helm 部署默认启用 AuthGuard/Envoy。MQTT 不经过 AuthGuard，
其身份边界由 EMQX 凭据、默认拒绝 ACL 与 AgentVault 持久化 Topic 身份映射共同
组成。工作负载 Topic 派生 `client_id`；审批决策使用独立的
`clients/{client}/approvers/{principal}/decisions` Topic，EMQX 将精确 Topic 绑定
管理员账号，AgentVault 再以 Topic 派生的 Principal 校验 Scenario Approver 表，
报文体不能覆盖任一身份。

## 双向额度与审批

Provisioning Manifest 明确表达 `Client 1:N Scenario`、`Scenario 1:N Policy`、
`Policy 1:N Wallet membership`。生产环境允许多个 Policy 共享 Wallet；本矩阵则
刻意使用五套隔离 Payer 身份和十个不同 EOA，以便直接发现 SDK 之间的越权串用。
AgentVault 在同等授权结果中优先选择当日使用量最低的钱包，稳定散列负责并列决胜。
Wallet 总硬限额与固定事务锁顺序防止累计超额。

- QuantStream OUTBOUND 正常日额度：1 USDC；
- 第二笔付款进入 `PendingApproval`；
- 单次审批可增加额度上限：50 USDC；
- 每个付款 Wallet 当日硬限额：51 USDC；
- 两个 OUTBOUND Intent 必须各选择一个池成员，Anvil 上两个 EOA 必须各扣 1 USDC；
- MarketPulse INBOUND 收款/销售日限额：10 USDC，用于防止超卖。

审批 Grant 绑定 Intent ID、不可变 Request Hash、金额、审批人、有效期和 Nonce。
JWT 只在 AgentVault 内部使用；Approval 表保存 `jti`、Claims Hash 和版本化 AEAD
密文。Grant 消费、额度预留、余额复查与进入签名状态在同一 SQL 事务中完成。

## 真实依赖与 Profile

所有依赖均位于 `deploy/`，并参考 Flowgent 的 Anvil、Solana、Facilitator 部署：

- `payer`：Anvil + Solana，不启动 Facilitator；
- `payee`：Anvil + Solana + 官方 x402 Facilitator；
- `infra`：PostgreSQL、EMQX、Jaeger；
- `apps`：AgentVault、审批 UI、只读审计器；
- `payee-{go,rust,python,java,typescript}`：选择一个收款后端；
- `ingestor-{go,rust,python,java,typescript}`：选择一个付款后端。

完整资金验收使用 x402 v2 / EVM / EIP-3009 与编译后的 TestUSDC。Solana 是
真实 Validator 健康依赖，但本用例不虚报 Solana 支付覆盖。Broker、RPC、
Facilitator 均非 Mock。MarketPulse 还会以规范十进制 Unix 秒返回 AgentVault
x402 Profile 的 `payment-expires-at`；审批轮询始终复用这个绝对值，因此同一份
官方 x402 Body 在所有重试中映射到同一个持久 Challenge Hash。

## 目录结构

```text
marketpulse-feed-subscription/e2e/
├── deploy/
│   ├── anvil/  solana/  facilitator/
│   ├── mock-payee-marketpulse-{golang,rust,python,java,typescript}-service/
│   │   └── {main/Application,server,route/feed,handler/feed,storage/feed}
│   ├── mock-payer-quantstream-ingest-ui/src/
│   ├── mock-payer-quantstream-ingest-service-golang/
│   │   ├── main.go
│   │   └── pkg/{route,handler,job,agent}/
│   ├── mock-payer-quantstream-ingest-service-rust/
│   │   └── src/{lib.rs,main.rs,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-python/
│   │   └── app/{main.py,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-java/
│   │   └── src/io/agentvault/e2e/{Application.java,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-typescript/
│   │   └── src/{main.ts,route/,handler/,job/,agent/}
│   └── */tests/           # 与源码相对路径一致的单元测试
├── common/contract/       # Provisioning 与 MQTT 契约
├── verifier/              # 结构、业务与资金证据断言
├── reports/               # 分层验收报告
├── docker-compose.yaml
└── runner.py              # 仅依赖 Python 标准库的编排器
```

五语言后端刻意维持一致的组合方式：极简 `main/Application`、仅含运维接口的
`route/ingestion → handler/ingestion`，以及独立的定时采集链路
`job/ingestion_job → agent/mcp`。
`MCPHelper` 分别使用 mark3labs/mcp-go、rmcp、官方 Python MCP 包、官方 Java MCP
SDK 与官方 TypeScript MCP SDK。每个 Job 都有两个
独立 Cron 定时函数：每日调用免费的昨日新闻 Tool，高频调用收费的实时新闻 Tool；
MCP Service 注册这两个外部 Tool 并接入对应语言的 Payer Middleware，框架胶水中
不再实现任何支付逻辑。

## 分层运行

```bash
cd use-cases/marketpulse-feed-subscription/e2e

python3 runner.py --list
python3 runner.py -s structure
python3 runner.py -s infrastructure
python3 runner.py -s payer
python3 runner.py -s payee
python3 runner.py -s full --payer-language rust --payee-language rust
python3 runner.py -s full --payer-language go --payee-language java
python3 runner.py -s matrix --matrix-workers 2
python3 runner.py -s matrix --resume-run <yyyyMMddHHmmss> --skip-build --cleanup-after-run
python3 runner.py cleanup
```

`runner.py` 是唯一外层编排入口，统一负责构建、部署、健康检查、场景选择、诊断和
清理；它会先校验结构，并写入不可覆盖的
`reports/<yyyyMMddHHmmss>/<module>/` 分层证据及该次运行的 `summary.md`。部署默认
保留以便人工验收和诊断；需要自动回收时显式加 `--cleanup-after-run`，之后回收则
运行 `runner.py cleanup`。

## Full Flow 必须提供的证据

每次 Full Flow 明确选择一组付款/收款语言和一种签名传输；`matrix` 默认以两个
有界 Worker 执行 5 个 Payer × 5 个 Payee × MQTT/gRPC = 50 个相互隔离的组合：
一个 Worker 专属 MQTT，另一个专属 gRPC。两个 Worker 使用不同 Compose Project、
端口段、运行时配置、数据库卷与链状态；`--matrix-workers 1` 可显式串行运行。

每个 Case 只有在五份详细验证全部通过后才写入绑定 Case 身份的 Checkpoint。宿主机或
Runner 中断后，`--resume-run` 会校验既有 Checkpoint，拒绝身份不匹配或损坏的证据，
并只执行同一不可变报告目录中尚未完成的 Case。

1. 免费 MCP 真实交付，收费 Tool 首次返回真正的 x402 v2 402；
2. 两个稳定逻辑操作，且交付重试不会二次授权或更换 Credential；
3. MQTT/gRPC 签名请求真实到达 AgentVault；MQTT 审批通知、UI 决策、一个精确金额
   的已消费 Grant，并自动恢复同一 Intent；
4. 两条 OUTBOUND 与两条 INBOUND Settled Intent、四条已确认 Ledger Payment、
   绑定 Receipt 及 Settlement/Reconciliation Event；
5. 两条 MarketPulse 持久交付和两条 QuantStream 持久采集记录；
6. 当前 Payer 的两个 EOA 各精确减少 1 USDC，其余八个 Payer EOA 保持不变，
   Gateway/Payee 精确增加 2 USDC，总量守恒，且 AgentVault 对十个钱包的定时余额
   快照最终全部与链上值一致。

手续费/退款/充值/转账/冲正矩阵、MPP/Tempo 结算与 Solana 支付不在本用例中
冒充已完成；它们需要各自真实 Provider Fixture 与独立验收场景。
