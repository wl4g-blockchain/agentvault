# AgentVault 架构白皮书

> Enterprise AI Agent Payment Infrastructure · Architecture White Paper

## 摘要

AgentVault 是面向 AI Agent 的双向机器支付中间件。它把业务平台中的工作流身份、付款策略、收款报价、私钥签名、一次性审批、外部结算、不可变资金流水和恢复任务组合为一个可审计的支付边界。Flowgent、Sigbot 或其他业务应用只通过多语言 SDK 发起操作，不直接持有付款私钥，也不把“客户端声称支付成功”当作到账事实。

系统支持两类对称业务：

- Payer / OUTBOUND：业务 Agent 收到 HTTP/MCP `402 Payment Required` 后，由 AgentVault 校验 Scenario、Policy、Wallet、余额和预算，再生成与 Intent 绑定的协议签名。
- Payee / INBOUND：业务平台按 Offer 对 HTTP Route 或 MCP Tool 收费，由 AgentVault 签发 Challenge、验证 Credential、调用外部 Facilitator/Provider 结算，并在可信证据确认后入账。

AgentVault 不实现 OAuth2/OIDC、登录、会话或 IAM 策略引擎；不内嵌 Facilitator；不把 Redis 作为资金权威；不提供任意摘要签名；不使用文件保存钱包或支付状态。

## 1. 系统定位与 AuthGuard 关系

三个系统的职责按观察视角组合，而不是互相复制：

```text
Human / Agent / Business UI
          │
          ▼
AuthGuard + Envoy Gateway             HTTP 身份、登录、会话、IAM、路由授权
          │ signed x-authguard-context
          ▼
AgentVault                            支付领域授权、预算、审批、签名、结算、Ledger
          │ SDK / HTTP / gRPC / Unix / MQTT
          ▼
Flowgent / Sigbot / QuantStream       工作流、Agent、业务交付与补偿
          │
          ▼
External Facilitator / Provider / RPC 链上验证与结算事实
```

通常部署同时启用 AuthGuard 与 AgentVault：Envoy 保护 AgentVault 的业务和 Admin HTTP API，AgentVault 只校验 AuthGuard 交付的短期签名上下文并取得 Principal，不解析用户凭据，也不重新执行 IAM 规则。部署可以显式关闭 AuthGuard；此时 AgentVault HTTP API 按设计不要求身份，适用于已隔离的可信网络。该模式不会关闭支付领域的 Scenario 归属、Policy、预算、审批、余额和签名约束。

MQTT 不经过 HTTP Gateway。Broker 必须使用独立用户名、`client_id` 和默认拒绝的 Topic ACL；AgentVault 收到工作负载命令后仍以持久化 Client/Scenario 映射校验业务身份。审批决策使用独立管理员 Broker 凭据与 `clients/{client}/approvers/{principal}/decisions` Topic，AgentVault 从 Topic 派生审批 Principal，并再次校验 Scenario 归属及 `pay_scenario_approver` 映射；消息体无权自报审批身份。HTTP Principal 与 MQTT 身份不能互相替代。

## 2. 资源与授权模型

```text
PaymentClient 1 ── N PaymentScenario 1 ── N PaymentPolicy
                                                │
                                                N
                                                │ pay_policy_wallet
                                                N
                                                │
                                             Wallet
```

| 资源 | 语义 | 关键不变量 |
|---|---|---|
| Client | 注册的业务应用 | `auth_principal` 唯一绑定可信上游 Principal |
| Scenario | Workflow/AgentFlow 的支付隔离边界 | 只属于一个 Client；创建后默认不能支付 |
| Policy | Scenario 的双向资金授权 | 只属于一个 Scenario；显式绑定一个或多个 Wallet |
| Wallet | EOA 或只读外部收款地址 | 私钥版本化加密；可被多个 Policy 共享 |
| Run | 一次工作流执行 | 只用于追踪和幂等，不是授权主体 |

从 Policy 看，关系严格为 `1:N Wallet`；关系表允许同一 Wallet 反向被多个 Policy/Scenario 共享，这是共享企业资金池所必需。共享 Wallet 时，数据库事务强制 Wallet 级单笔和每日硬上限，防止多个 Scenario 分别合规但累计超额。锁按稳定 ID 顺序获取，避免并发预算死锁。

请求体只携带 `scenario_id`。服务端从可信传输上下文解析 Client，再验证 Scenario 所属关系；平台级凭据不会自动获得跨 Client 能力。

## 3. 双向 Policy

每个 Policy 使用同一预算单位约束双向流量，但 INBOUND 与 OUTBOUND 的限额完全独立：

| 方向 | 典型业务 | 独立控制项 |
|---|---|---|
| OUTBOUND | Agent 购买 API/MCP 数据 | `max_per_payment`、`max_per_day`、审批阈值、审批追加额度、收款人/Origin 白名单 |
| INBOUND | 平台销售付费资源 | `max_per_payment`、`max_per_day`、审批阈值、审批追加额度、Offer/资产/网络范围 |

入站额度不是“收入越多越好”的装饰配置。它用于限制库存、订阅席位、独家数据、风控敞口或有上游采购成本的资源，防止并发超卖。Offer 必须绑定具备 INBOUND 权限的 Policy 和该 Policy 的 Wallet membership；调用方不能覆盖价格、资产、网络或收款地址。

一次审批只为一个 Intent 增加有限额度。Grant 绑定 `intent_id + request_hash + amount + approver + expiry + jti`，JWT 只作为 AgentVault 内部证明，数据库保存 Claims Hash 和加密恢复材料，并在预算事务内原子消费一次。审批不会修改长期 Policy，也不能突破 Wallet 硬上限。

## 4. 逻辑架构

```text
Controllers                       Application / Domain                  Adapters
┌────────────────────┐            ┌──────────────────────────┐          ┌─────────────────────┐
│ HTTP + AuthGuard   │───────────▶│ Scenario / Policy        │─────────▶│ PostgreSQL / SQLite │
│ Unix Socket        │            │ Offer / Approval         │          │ Memory / Redis      │
│ MQTT + broker ACL  │            │ Signing / Settlement     │          │ x402 / MPP          │
│ Dashboard          │            │ Reconciliation / Outbox  │          │ Provider / EVM RPC  │
└────────────────────┘            └──────────────────────────┘          └─────────────────────┘
```

HTTP、gRPC、Unix Socket 与 MQTT 复用同一个 `PaymentApi` 和 Handler 图，不在传输层复制预算或签名逻辑。gRPC 仅是 SDK 内部的有界 Envelope 传输，不形成额外公开业务 API；启用 AuthGuard 时由其 Envoy gRPC 入口认证并注入签名 Principal Context。`src/cmd/` 是唯一组合根，负责选择 SQL、Cache、Protocol、Provider、RPC 和 Transport adapter。业务 Handler 只依赖 Port trait。

源代码顶层严格收敛为：

```text
src/
├── adapters/     # Rust/Go/Python/TypeScript/Java SDK 与共享 gRPC Proto
├── apm/          # OTLP、Prometheus、运行时诊断、pprof 摘要
├── cache/        # 非资金权威 Memory/Redis
├── cmd/          # CLI、服务组合根、生命周期
├── config/       # 强类型 YAML 与校验
├── controller/   # HTTP、gRPC、Unix Socket、MQTT、Management
├── handler/      # 应用服务、签名、结算、恢复任务
├── model/        # 协议无关领域模型
├── protocol/     # x402、MPP adapter
├── storage/      # Port、SQL base、SQLite/PostgreSQL adapter
├── utils/        # 密钥材料、密码学、错误等公共能力
├── lib.rs
└── main.rs
```

Go、Rust、Python、TypeScript（以及保持同矩阵的 Java）测试统一放在组件的
`tests/` 下，目录镜像源码相对路径，命名为 `<module>_test.*`（Java 使用
`<Module>Test.java`）。源码目录不再混入测试文件；结构门禁同时拒绝缺失测试和
孤儿测试。

## 5. 持久化与一致性

SQLite 与 PostgreSQL 都从同名的 `001_init.ddl.sql`、`001_init.dml.sql` 初始化，并由 schema parity test 校验表、列、索引和版本。八个支付核心表与六个支撑表如下：

| 分类 | 表 | 职责 |
|---|---|---|
| 核心 | `pay_client` | 业务应用和可信 Principal 映射 |
| 核心 | `pay_scenario` | Client 下的支付隔离边界 |
| 核心 | `pay_policy` | 双向额度、白名单和审批规则 |
| 核心 | `pay_offer` | Payee 固定资源报价 |
| 核心 | `pay_intent` | 双向支付当前状态、预算预留和恢复游标 |
| 核心 | `pay_event` | 不可变操作事件 |
| 核心 | `pay_approval` | 一次性审批请求和 Grant 绑定 |
| 核心 | `pay_ledger` | 已确认资金事实 |
| 支撑 | `pay_wallet`、`wallet_key_version` | Wallet 元数据、硬限额和加密密钥版本 |
| 支撑 | `pay_policy_wallet` | Policy 与 Wallet membership |
| 支撑 | `pay_scenario_approver` | 四眼审批人映射 |
| 支撑 | `pay_outbox` | 崩溃可恢复的外部消息投递 |
| 支撑 | `pay_schema_version` | Forward-only schema 版本 |

普通 Catalog CRUD 复用数据库 base 宏消除 bind/executor 样板；预算预留、Grant 消费、Intent 状态转换、可信 Ledger 入账和 Outbox 写入保持显式聚合事务。这样复用不会隐藏资金锁序和条件更新。

关键一致性规则：

- `UNIQUE(scenario_id, idempotency_key)`；同 Key 同 Hash 返回原结果，同 Key 不同 Hash 冲突。
- Ledger 仅从可信 Provider/RPC 证据入账；签名或本地 Verify 不是到账。
- `UNIQUE(wallet_id, posting_key)` 对同一资金事实去重；同一交易的本金、Gas、退款使用可信 `effect_ref` 区分。
- Ledger 只追加；错误使用 Reversal 分录修正，历史行不可更新或删除。
- 外部结算与本地事务无法原子提交，先写持久恢复事件，再靠幂等 Provider 调用和 Reconciliation 收敛。
- PostgreSQL 使用行锁与 Lease；SQLite 使用 `BEGIN IMMEDIATE` 和事务 Lease。Redis Lock 不参与正确性证明。

## 6. Payer 流程

1. SDK 调用免费 MCP Tool 或付费 Tool；付费端返回 x402/MPP Challenge。
2. SDK 用首次生成的稳定幂等键提交 `scenario_id + run_id + challenge`。
3. AgentVault 验证 Client/Scenario、协议、资产、网络、Origin、Recipient 和 Wallet membership。
4. SQL 事务重算 Scenario 当日使用量、未决预留和共享 Wallet 总量。
5. 在限额内直接预留；超过阈值时持久化 Approval 并经 MQTT Outbox 通知业务 Admin UI。
6. 管理员的独立 Principal 提交决策；一次性 Grant 在锁内消费并完成预算预留。
7. 每次签名前重新检查 Policy、Wallet 生命周期、余额/Gas 和授权有效期；Adapter 生成 Intent-bound typed data，Signer 不接受调用方任意 Digest。
8. SDK 收到签名凭据后重试原 MCP 请求，并把标准支付响应证据回传 AgentVault。
9. RPC/Provider 确认交易和 Token Transfer 后，Intent、Event、Ledger、Outbox 在同一 SQL 事务内提交；Workflow 恢复。

## 7. Payee 流程

1. Payee SDK 根据 `scenario_id + resource` 解析服务端 Offer。
2. AgentVault 按 Offer 签发唯一、带过期时间且绑定 Scenario/金额/资产/收款人的 Challenge，并预留 INBOUND 容量。
3. 外部 Agent 提交 Credential；协议 Adapter 做语义指纹、防跨 Challenge 兑换和字段一致性校验。
4. AgentVault 调用外部 Facilitator/Provider Verify/Settle，不持有付款方私钥。
5. 仅在可信结算证据匹配网络、Token、金额、Recipient 和确认数后写 Ledger 与 Receipt。
6. Payee 以 Intent/业务执行键幂等交付资源。收款成功但业务交付失败时重试交付，不重复收款。

Credential 语义指纹在同一支付方向内全局唯一：任何 INBOUND Credential
都不能跨资源或业务请求重复兑换，任何 OUTBOUND Credential 也不能重复签发。
当同一 AgentVault 部署同时服务付款方与收款方时，同一笔链上支付允许在相反
方向各记录一次，以分别形成付款钱包和收款钱包的资金事实。

## 8. Protocol 与外部结算

`PaymentProtocol` 把 wire Challenge/Credential 转换为统一领域对象，并负责协议语义指纹与签名计划。当前优先级为 x402 v2 / EVM / USDC / exact / EIP-3009，以及 MPP / Tempo Charge。协议库可用时直接封装官方 crate；业务内核不复制协议栈。AgentVault x402 Payee profile 会随 402 返回 `payment-expires-at`（规范十进制 Unix 秒）；Payer 必须原样回传这个绝对时间，使审批轮询和进程重试解析出同一个 Challenge Hash。Adapter 会同时按 x402 `maxTimeoutSeconds` 校验它，缺失、过期或超出窗口均 Fail Closed，而不是以本机当前时间重新生成过期点。

Facilitator/Provider 始终是进程外依赖。HTTP adapter 固定 Base URL 和相对路径、禁止跨 Authority Redirect、限制响应体与超时，并校验返回的资产和结算证据。Provider 跟随所属 Payee 协议配置；共享 `chains` 注册表由运营方固定链类型与 RPC，Challenge 不能选择 Provider 或 RPC。

## 9. Cache、密钥与可观测性

`server:`、`mgmt:`、`logging:`、`storage:` 与 `cache:` 是主 YAML 的顶层通用能力；唯一业务根是 `agentvault:`。`agentvault.payer` 与 `agentvault.payee` 分别配置出站和入站协议，不能通过一个共享开关隐式同时启用。Facilitator/MPP Provider 只属于 `payee`，`agentvault.chains` 则由两个方向共享并通过 `family` 支持不同链实现。
`storage/` 只提供一个权威 SQL Port、SQLite/PostgreSQL 方言与显式聚合事务；
`cache/` 提供一致的 Memory/Redis Port。Redis 支持 standalone/cluster、环境变量
SecretRef、Cluster 单槽 Namespace、字段级 TTL、连接/操作超时和有界指数退避。
服务启动时实际打开所选 adapter；预算、审批、Intent、Settlement、幂等和 Ledger
永远回源 SQL。

本地托管 EOA 使用 AES-256-GCM envelope encryption、完整 UUID v4 Nonce 和不可变 Key Version。Wallet 元数据位于 `pay_wallet`，加密 EOA 密文位于同一 Wallet 聚合的 `wallet_key_version` 子表；这是支持安全轮换所需的规范化一对多关系，不是独立文件 Key Store。`agentvault.key_management` 只引用 KEK，绝不保存 EOA 私钥。轮换先写新版本并原子切换 Active；旧版本仅作为不可变审计与受控恢复材料保留，未签名操作不会继续使用 Retired Key。任何 API 都不能导出明文私钥。Credential/Grant 的崩溃恢复材料由独立 `agentvault.recovery_encryption` AES-256-GCM 密钥环保护，Event 和日志只保存 Hash/Reference。

APM 提供结构化日志、OTLP Trace、Prometheus/OpenMetrics、健康检查和受限运行时诊断。Grafana Dashboard 位于 `deploy/apm/grafana/`。

## 10. SDK 与 Dashboard

`src/adapters/{rust,golang,python,typescript,java}` 使用一致的功能模块：

```text
model/        wire/domain DTO
transport/    HTTP、MQTT、超时、响应上限和脱敏
               以及可切换的 gRPC 内部传输
payer/        PayerMiddleware + 生态 HTTP 客户端拦截器
payee/        PayeeMiddleware + 生态服务端中间件
config/       传输与双向 Middleware 强类型配置
agentvault    唯一公开 Facade，含审批查询/决策入口
```

Payer 总入口负责捕获 402、请求授权、等待签名/审批结果、校验 Credential 的 Origin
绑定、有限重试原业务 HTTP/MCP 请求并上报结算响应；Rust/Go/Python/TypeScript/Java
分别提供 Reqwest、net/http、HTTPX、Fetch、Spring 实现。Payee 总入口负责 Offer、
Challenge、Verify、Settle，并由 Axum、Gin、FastAPI/ASGI、Express、Spring 适配器
决定是否放行业务请求。业务代码只在启动时选择 gRPC、MQTT 或受 AuthGuard/Envoy
保护的 HTTP Transport，切换传输不改变业务 Handler。

SDK 源码中的强类型 DTO 和契约测试是调用边界，不维护额外公开 API 描述目录。Dashboard 位于 `web/`，只使用同源 Cookie/上下文，不保存 Bearer Token；它可读取 Client、Scenario、Policy、Wallet、Offer、Payment、Approval、Balance 和运行状态。资源创建和密钥注入继续走受控 Provisioning Manifest，前端不能接触私钥。

## 11. 部署与真实 E2E

- `deploy/docker/`：默认 AuthGuard + Envoy + AgentVault + Dashboard；另有明确命名的开放拓扑。MQTT 默认关闭，必须显式接入认证和 ACL 完整的 Broker。
- `deploy/helm/agentvault/`：AuthGuard 为默认启用的 umbrella dependency，受保护 HTTPRoute 使用 `authguard.io/protected=true`；禁用后不创建受保护 Route，运营方必须明确暴露开放 Service。
- `use-cases/marketpulse-feed-subscription/e2e/`：从零构建、部署和分层验证的真实案例。

E2E 中 MarketPulse 暴露两个 MCP Tool：昨日新闻免费，实时新闻按次收费。QuantStream 使用 Rust、Go、Python、TypeScript、Java 五种等价后端定时采集；付费 Tool 经 x402、MQTT 签名、超额审批 UI、官方 Facilitator、Anvil TestUSDC、PostgreSQL Ledger 和失败后恢复完成闭环。Payee profile 启动 Facilitator + Anvil + Solana；Payer dependency profile 只启动链节点。当前资金断言覆盖 EVM/x402，Solana 在该案例中验证真实节点健康，不宣称已完成 Solana 支付。

## 12. 安全边界

- AuthGuard 负责 HTTP 身份和 IAM；AgentVault 负责支付领域授权，二者不会重复实现。
- Wallet 管理、支付授权、审批和签名职责分离；审批人不能与请求人相同。
- Challenge/Credential 严格绑定 Chain、Token Contract、Decimals、Amount、Recipient、Nonce、Expiry、Domain 和业务请求。
- SDK 禁止把支付凭据跟随跨 Origin Redirect，限制响应体和超时，并稳定复用幂等键。
- 关键持久化失败一律 Fail Closed；Cache、日志、Event 或客户端回报都不能升级为资金事实。
- 停用 Client/Scenario/Policy/Wallet 只阻止新操作，不删除历史 Intent、Event、Approval 或 Ledger。

以上约束共同保证 AgentVault 是可恢复、可审计的机器支付基础设施，而不是另一个身份系统、嵌入式结算方或普通 Wallet CRUD 服务。
