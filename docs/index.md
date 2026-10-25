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
          │ signed context / opaque scope token
          ▼
AgentVault                            支付领域授权、预算、审批、签名、结算、Ledger
          │ SDK(gRPC/MQTT) / Admin HTTP / Unix
          ▼
Flowgent / Sigbot / QuantStream       工作流、Agent、业务交付与补偿
          │
          ▼
External Facilitator / Provider / RPC 链上验证与结算事实
```

通常部署同时启用 AuthGuard 与 AgentVault：Envoy 保护 AgentVault 的业务、Admin HTTP API 与可选 gRPC Transport。AgentVault 本地校验 AuthGuard 交付的短期签名上下文；授权范围过大时只把短期 `x-authguard-scope-token` 交给 AuthGuard 内部 `ResolveScope` gRPC 解析，再取得 Principal。AgentVault 不解析用户凭据，也不重新执行 IAM 规则。部署可以显式关闭 AuthGuard；此时 AgentVault HTTP/gRPC API 按设计不要求身份，适用于已隔离的可信网络。该模式不会关闭支付领域的 Scenario 归属、Policy、预算、审批、余额和签名约束。

MQTT 不经过 HTTP Gateway。Broker 必须使用独立用户名、`client_id` 和默认拒绝的 Topic ACL；AgentVault 收到工作负载命令后仍以持久化 Client/Scenario 映射校验业务身份。Payer/Payee 工作负载只订阅自己的相关结果 Topic；审批通知只交付使用独立凭据的业务 Admin UI，SDK 通过权威 Intent 状态等待审批，不旁听管理员事件。审批决策使用 `clients/{client}/approvers/{principal}/decisions` Topic，AgentVault 从 Topic 派生审批 Principal，并再次校验 Scenario 归属及 `pay_scenario_approver` 映射；消息体无权自报审批身份。HTTP Principal 与 MQTT 身份不能互相替代。

多副本 AgentVault 使用每进程唯一 Broker Client ID 与 Lease Owner，并以同一 Consumer Group 的共享订阅消费命令，避免持久 Session 踢线或每个副本重复执行。服务端和五语言 SDK 都必须验证 SUBACK 的 Topic 数量及实际授予的 QoS 1；ACL 拒绝或 QoS 降级在启动/请求阶段立即 Fail Closed，不能伪装成业务超时。

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
| Scenario | 业务支付隔离边界 | 只属于一个 Client；创建后默认不能支付 |
| Policy | Scenario 的双向资金授权 | 只属于一个 Scenario；显式绑定一个或多个 Wallet |
| Wallet | EOA 或只读外部收款地址 | 私钥版本化加密；可被多个 Policy 共享 |
| Run | 一次逻辑业务执行 | `run_id` 是全链路唯一的业务幂等字段；在租户边界内由 `(scenario_id, run_id)` 唯一约束，不是授权主体 |

从 Policy 看，关系严格为 `1:N Wallet`；关系表允许同一 Wallet 反向被多个 Policy/Scenario 共享，这是共享企业资金池所必需。共享 Wallet 时，数据库事务强制 Wallet 级单笔和每日硬上限，防止多个 Scenario 分别合规但累计超额。锁按稳定 ID 顺序获取，避免并发预算死锁。

Payer 的授权范围只由 `scenario_id` 选择；Client 来自可信 HTTP/gRPC/Unix/MQTT 上下文，`run_id + challenge` 描述一次逻辑业务操作。调用方不能提交 `policy_id`、`wallet_id`、资产、金额或收款人投影来影响授权，后四项必须从原始 402 Challenge 解析。平台级凭据不会自动获得跨 Client 能力。余额查询和受控管理接口可以显式定位 Wallet，但它们不是签名授权入口。

`run_id` 在 SDK、HTTP/gRPC/MQTT wire、Intent、Outbox 和业务交付中保持不变，授权轮询、网络超时、重发、Verify、Settle 及交付恢复都复用它；不再存在第二个公开的业务幂等字段。同一 Scenario 下，同 `run_id` 且同请求指纹返回原结果；同 `run_id` 但不同指纹必须冲突并 Fail Closed。

可观测标识与业务标识严格分离：交易 Run 的生命周期大于单段 Trace，同一 `run_id` 可跨进程重启、人工审批和对账形成多个 `trace_id`；每次 HTTP/gRPC/MQTT/Provider 调用或重试创建新的 `span_id`，并通过 W3C `traceparent` 传递父子关系。系统禁止定义额外的独立关联标识。异步 wire 中兼容字段 `requestSpanId` 必须与 `traceparent` 的 producer Span ID 完全相同，用于响应 Topic 并由响应回显；它不是新 ID，不得持久化、参与业务唯一约束或恢复查询，只允许作为 MQTT QoS 重投递的短生命周期回复缓存键。消费响应创建新 Span，重试保持 `run_id`、递增 `attempt_no` 并取得新 Span。若外部 Provider 协议硬性要求 HTTP `Idempotency-Key`，Adapter 只把 `run_id` 映射为该协议头，不创建新身份。

OUTBOUND 时 AgentVault 把 Scenario 下所有匹配的 Policy-Wallet membership 视为候选钱包池。协议、资产、Origin、Recipient 或方向不匹配的 membership 不进入候选；Wallet 停用、缺少本地 Active Key、算法/规范地址不兼容 Challenge 网络或预算拒绝则保留为可审计的拒绝结果。系统按“可直接授权、需审批、拒绝”分组，在同组内优先选择当日已花费加预留最少的 Wallet，并以请求哈希做稳定散列决胜。选择结果写入 Intent 作为内部审计事实；幂等重放始终返回同一结果。候选数量有界，最终 membership、网络兼容性、Key Version、Policy 快照与双层预算在 SQL 锁内再次校验，因此并发选择不能绕过限额。多个匹配 Policy 是替代授权（OR）；需要叠加限制时应合并为一个 Policy，避免用重叠 Policy 表达交集语义。链上余额会在每次签名前再次 Fail Closed 检查；它不是负载均衡权重，也不会把同一个 Intent 静默改绑到另一 Wallet。

钱包池选择是授权前的确定性调度，不是分布式锁：并发请求可能在锁外看到同一用量快照，最终 SQL 条件更新仍会阻止超额，其中一个请求可以 Fail Closed，但系统不会在已创建 Intent 后静默换钱包。Catalog 分页接口只服务展示；Scenario-Wallet 归属、Offer 解析等权限事实必须使用数据库存在性/唯一性查询，禁止从“第一页里是否找到”推导授权结果。

AgentVault 自己持有链/RPC 连接；业务 Payer 只提交 `scenario_id + run_id + challenge`，不得配置私钥、Wallet ID 或链 RPC。签名前的链上余额/Gas 检查属于实时安全闸门；独立的 Wallet Balance Sync Job 默认每 3 分钟分页观察托管钱包并写入非权威 SQL 快照，只服务资金看板、预算预警和运营分析，绝不能替代签名前检查或 Ledger 结算证据。

## 3. 双向 Policy

每个 Policy 使用同一预算单位约束双向流量，但 INBOUND 与 OUTBOUND 的限额完全独立：

| 方向 | 典型业务 | 独立控制项 |
|---|---|---|
| OUTBOUND | Agent 购买 API/MCP 数据 | `max_per_payment`、`max_per_day`、审批阈值、审批追加额度、收款人/Origin 白名单 |
| INBOUND | 平台销售付费资源 | `max_per_payment`、`max_per_day`、审批阈值、审批追加额度、Offer/资产/网络范围 |

入站额度不是“收入越多越好”的装饰配置。它用于限制库存、订阅席位、独家数据、风控敞口或有上游采购成本的资源，防止并发超卖。Offer 必须绑定具备 INBOUND 权限的 Policy 和该 Policy 的 Wallet membership；调用方不能覆盖价格、资产、网络或收款地址。

一次审批只为一个 Intent 增加有限额度。Grant 绑定 `intent_id + request_hash + amount + approver + expiry + jti`，JWT 只作为 AgentVault 内部证明，数据库保存 Claims Hash 和加密恢复材料，并在预算事务内原子消费一次。审批不会修改长期 Policy，也不能突破 Wallet 硬上限。

是否触发审批只由匹配 Policy 的方向性 `approval_enabled/approval_threshold` 决定；不存在可绕过 Policy 的进程级总开关。运行时审批组件始终可处理已持久化的 Pending Intent，以保证重启恢复。

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

SDK 控制面只允许 gRPC 与 MQTT，两者同 HTTP Admin/Unix 入口复用同一个 `PaymentApi` 和 Handler 图，不在传输层复制预算或签名逻辑。gRPC 仅是 SDK 内部的有界 Envelope 传输，不形成额外公开业务 API；启用 AuthGuard 时由其 Envoy gRPC 入口认证并注入签名 Principal Context。由于网关看不到 Envelope 内部路径，gRPC 服务只允许自身仍执行 Scenario/Approver 领域授权的 SDK 支付命令，明确拒绝 `/admin/*` 与未知路由；Admin API 只能走可被 AuthGuard 逐路由判定的 HTTP 入口。`src/cmd/` 是唯一组合根，负责选择 SQL、Cache、Protocol、Provider、RPC 和 Transport adapter。业务 Handler 只依赖 Port trait。

源代码顶层严格收敛为：

```text
src/
├── adapters/     # Rust/Go/Python/TypeScript/Java SDK 与共享 gRPC Proto
├── apm/          # OTLP、Prometheus、运行时诊断、pprof 摘要
├── cache/        # 非资金权威 Memory/Redis
├── cmd/          # CLI、服务组合根、生命周期
├── config/       # 强类型 YAML 与校验
├── controller/   # HTTP、gRPC、Unix Socket、MQTT、Management
├── handler/      # 扁平的应用服务、端口实现、签名、结算、恢复任务
├── model/        # DTO、命令、结果与可复用值对象
│   └── domain/   # 与 SQL 表或权威持久化聚合对应的实体
├── protocol/     # x402、MPP adapter
├── storage/      # Port、SQL base、SQLite/PostgreSQL adapter
├── utils/        # 密钥材料、密码学、错误等公共能力
├── lib.rs
└── main.rs
```

Go、Rust、Python、TypeScript 测试统一放在组件的 `tests/` 下；Java 按其组件
约定放在 `test/` 下。测试目录镜像源码相对路径，命名为 `<module>_test.*`
（Java 使用 `<Module>Test.java`）。源码目录不再混入测试文件；结构门禁同时拒绝
缺失测试和孤儿测试。服务端 `model/domain/<domain>.rs` 只定义权威持久化实体，
`model/<domain>.rs` 只在实体无法直接复用时定义 DTO/命令/结果和值对象；
`handler/<domain>.rs` 不定义可跨边界复用的业务模型。

## 5. 持久化与一致性

SQLite 与 PostgreSQL 都从同名的 `001_init.ddl.sql`、`001_init.dml.sql` 初始化，并由 schema parity test 校验表、列、索引和版本。八个支付核心表与七个支撑表如下：

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
| 支撑 | `pay_wallet_balance` | Balance Sync Job 写入的非权威链上余额观察值 |
| 支撑 | `pay_policy_wallet` | Policy 与 Wallet membership |
| 支撑 | `pay_scenario_approver` | 四眼审批人映射 |
| 支撑 | `pay_outbox` | 崩溃可恢复的外部消息投递 |
| 支撑 | `pay_schema_version` | Forward-only schema 版本 |

普通 Catalog CRUD 复用数据库 base 宏消除 bind/executor 样板；预算预留、Grant 消费、Intent 状态转换、可信 Ledger 入账和 Outbox 写入保持显式聚合事务。这样复用不会隐藏资金锁序和条件更新。

Canonical DDL **禁止业务 Trigger、存储过程和隐式副作用**。`CHECK`、`FOREIGN KEY`、`UNIQUE`、类型和索引只承担单行或静态关系约束；需要读取其他聚合、判断状态迁移或获取锁的规则，必须由 AgentVault Storage Port 在一个显式事务内执行。PostgreSQL 使用确定性的行锁顺序和带旧版本条件的 `UPDATE`，SQLite 使用 `BEGIN IMMEDIATE` 和同一套领域校验；任何校验失败都会回滚完整调用。两种方言因此共享相同业务语义，而不是维护两套 Trigger 程序。

| 原 DDL 隐式逻辑 | 显式 AgentVault 原子调用 |
|---|---|
| Policy 预算单位、Scenario-Wallet 唯一启用策略、共享 Wallet 硬限额 | `PolicyStorage.insert/update` 锁定 Policy 与排序后的 Wallet，校验全部 membership，并在首次绑定时条件初始化预算单位 |
| Offer 的 Scenario、Wallet、方向、协议、网络、资产和单笔限额 | `OfferStorage.insert/update` 在 Policy→Wallet 锁内验证完整 INBOUND 授权范围 |
| Intent 授权范围、不可变身份、Credential 只绑定一次 | 创建时锁定 Policy 并验证方向/协议/资产/金额；迁移时比较完整不可变身份和旧 revision，再执行条件更新 |
| Ledger 精确冲正与不可变历史 | 只公开 append/reversal Port；冲正前锁定原分录并验证 Wallet、Intent、金额、资产、方向和外部 Effect 全部匹配 |
| Wallet 预算/共享限额与 Key Version 生命周期 | Wallet 事务统计已绑定策略，Key Repository 校验 Local Custody 和 Active 元数据；轮换/Compromise 使用 forward-only 条件更新并检查影响行数 |
| Event、Ledger、加密 Key Material 的 append-only 语义 | 对外 Port 不提供任意 update/delete；运行时只调用专用追加或状态迁移函数，生产数据库凭据不得暴露给业务服务 |

直接绕过 Storage Port 执行任意 SQL 不属于受支持的运行模式。生产 PostgreSQL 应由独立迁移身份拥有 Schema，AgentVault 运行身份遵循最小权限；SQLite 文件只能由单一 AgentVault 进程写入。数据库管理员始终能改写数据库，Trigger 也不能抵御管理员，因此安全边界必须是凭据隔离、审计和受控迁移，而不是隐藏在 DDL 中的业务代码。

关键一致性规则：

- Intent 以 `UNIQUE(scenario_id, run_id)` 约束逻辑执行；同 Run 同 Hash 返回原结果，同 Run 不同 Hash 冲突。Approval 只投影同一个 `run_id`，并由 `(intent_id, run_id, intent_hash)` 复合外键绑定原 Intent，不能生成审批专用幂等键。
- Outbox 以 `UNIQUE(destination, event_type, run_id)` 去重；消息重投不能生成新业务标识。
- Ledger 仅从可信 Provider/RPC 证据入账；签名或本地 Verify 不是到账。
- `UNIQUE(wallet_id, posting_key)` 对同一资金事实去重；同一交易的本金、Gas、退款使用可信 `effect_ref` 区分。
- Ledger 只追加；错误使用 Reversal 分录修正，历史行不可更新或删除。
- 外部结算与本地事务无法原子提交，先写持久恢复事件，再靠幂等 Provider 调用和 Reconciliation 收敛。
- 恢复关键证据按 `(intent_id, event_type, attempt_no)` 唯一键精确读取；分页事件流仅用于审计展示，不能决定恢复状态。
- PostgreSQL 使用行锁与 Lease；SQLite 使用 `BEGIN IMMEDIATE` 和事务 Lease。Redis Lock 不参与正确性证明。

## 6. Payer 流程

1. SDK 调用免费 MCP Tool 或付费 Tool；付费端返回 x402/MPP Challenge。
2. SDK 为逻辑业务操作只生成一个稳定 `run_id`，提交 `scenario_id + run_id + challenge`；后续轮询和重试不得更换它。
3. AgentVault 验证 Client/Scenario，并从 402 Challenge 解析协议、资产、网络、Origin、Recipient；调用方不能指定 Wallet 或 Policy。
4. AgentVault 从匹配 Policy 的 Wallet 池选择可签名 EOA；SQL 事务锁内复核 membership、Key Version，并重算 Scenario 当日使用量、未决预留和共享 Wallet 总量。
5. 在限额内直接预留；超过阈值时持久化 Approval 并经 MQTT Outbox 通知业务 Admin UI。
6. 待审批投影携带原 Intent 的 `run_id`；管理员的独立 Principal 必须用该同一 Run 提交决策，UI/SDK 不得生成 decision key。一次性 Grant 在锁内消费并完成预算预留。
7. 每次签名前重新检查 Policy、Wallet 生命周期、余额/Gas 和授权有效期；Adapter 生成 Intent-bound typed data，Signer 不接受调用方任意 Digest。
8. SDK 收到签名凭据后重试原 MCP 请求，并把标准支付响应证据回传 AgentVault。
9. RPC/Provider 确认交易和 Token Transfer 后，Intent、Event、Ledger、Outbox 在同一 SQL 事务内提交；业务交付恢复。

## 7. Payee 流程

1. Payee SDK 根据 `scenario_id + resource` 解析服务端 Offer。
2. AgentVault 按 Offer 签发唯一、带过期时间且绑定 Scenario/金额/资产/收款人的 Challenge，并预留 INBOUND 容量。
3. 外部 Agent 提交 Credential；协议 Adapter 做语义指纹、防跨 Challenge 兑换和字段一致性校验。
4. AgentVault 调用外部 Facilitator/Provider Verify/Settle，不持有付款方私钥。
5. 仅在可信结算证据匹配网络、Token、金额、Recipient 和确认数后写 Ledger 与 Receipt。
6. Payee 以 `run_id` 幂等交付资源。收款成功但业务交付失败时复用同一 `run_id` 重试交付，不重复收款。

Credential 语义指纹在同一支付方向内全局唯一：任何 INBOUND Credential
都不能跨资源或业务请求重复兑换，任何 OUTBOUND Credential 也不能重复签发。
当同一 AgentVault 部署同时服务付款方与收款方时，同一笔链上支付允许在相反
方向各记录一次，以分别形成付款钱包和收款钱包的资金事实。

## 8. Protocol 与外部结算

`PaymentProtocol` 把 wire Challenge/Credential 转换为统一领域对象，并负责协议语义指纹与签名计划。完整 Payer 能力包括 x402 v2 / EVM / USDC / exact / EIP-3009，以及 MPP 0.15.1 的 Tempo `charge`：Adapter 从权威 Challenge 构造受约束的 type-0x76 交易和 EIP-712 digest，仍由既有 Wallet/Policy/预算/审批链路签名，再生成官方 Credential；Payee 通过固定外部 Provider 的 verify/settle 获取可信链上证据。MPP session、split、fee-payer、machine-token 与 delegated-key 仍显式 Unsupported，不能伪装成 charge。协议库可用时直接封装官方 crate；业务内核不复制协议栈。AgentVault x402 Payee profile 会随 402 返回 `payment-expires-at`（规范十进制 Unix 秒）；Payer 必须原样回传这个绝对时间，使审批轮询和进程重试解析出同一个 Challenge Hash。Adapter 会同时按 x402 `maxTimeoutSeconds` 校验它，缺失、过期或超出窗口均 Fail Closed，而不是以本机当前时间重新生成过期点。

x402 Provider 只接受 v2 结构：HTTP 优先解析规范 Base64 `PAYMENT-REQUIRED`，兼容原 JSON body；头与 body 同时声明但内容不同即拒绝。MCP Tool Call 支持错误 Tool Result 中的 `structuredContent` 与文本 JSON，两者冲突同样拒绝；付款重试使用 `x402/payment`，成功证据使用 `x402/payment-response`。MPP Provider 使用官方 `mpp` crate 的 HTTP Challenge/Credential/Receipt 与 MCP PaymentError/`org.paymentauth/credential`/`org.paymentauth/receipt` 类型；仅选择允许列表中的 Tempo `charge`，非付款认证 Challenge 不会误判。`Payment-Authorization` 只有在对端 Challenge 明确要求时使用，`Authorization: Payment` 不得覆盖已有业务 Bearer。所有协议付款 `_meta` 在调用业务 Handler 前剥离，其他 `_meta` 原样保留。

Rust、Go、Python、TypeScript 的 x402 Provider 直接组合各自官方 v2 SDK；五语言 MPP Provider 同样优先组合官方/上游库模型。当前上游 Java x402 模块仍是仅本地安装的 `1.0.0-SNAPSHOT` v1（`X-PAYMENT` 与 v1 body），不能作为 v2 类型依赖，因此 Java 只保留受共享夹具约束的薄 v2 wire 投影，钱包签名、协议权威校验和结算仍由使用官方 Rust 类型的 Vault 执行；在上游提供兼容的 Java v2 类型后再替换该投影，禁止以 v1 库伪装 v2 支持。

Facilitator/Provider 始终是进程外依赖。HTTP adapter 固定 Base URL 和相对路径、禁止跨 Authority Redirect、限制响应体与超时，并校验返回的资产和结算证据。Provider 跟随所属 Payee 协议配置；共享 `chains` 注册表由运营方固定链类型与 RPC，Challenge 不能选择 Provider 或 RPC。

## 9. Cache、密钥与可观测性

`server:`、`mgmt:`、`logging:`、`storage:` 与 `cache:` 是主 YAML 的顶层通用能力；唯一业务根是 `agentvault:`。`agentvault.payer` 与 `agentvault.payee` 分别配置出站和入站协议，不能通过一个共享开关隐式同时启用。Facilitator/MPP Provider 只属于 `payee`，`agentvault.chains` 则由两个方向共享并通过 `family` 支持不同链实现。
`storage/` 只提供一个权威 SQL Port、SQLite/PostgreSQL 方言与显式聚合事务；
`cache/` 提供一致的 Memory/Redis Port。Redis 支持 standalone/cluster、环境变量
SecretRef、Cluster 单槽 Namespace、字段级 TTL、连接/操作超时和有界指数退避。
服务启动时实际打开所选 adapter；预算、审批、Intent、Settlement、幂等和 Ledger
永远回源 SQL。

本地托管 EOA 使用 AES-256-GCM envelope encryption、完整 UUID v4 Nonce 和不可变 Key Version。Wallet 元数据位于 `pay_wallet`，加密 EOA 密文位于同一 Wallet 聚合的 `wallet_key_version` 子表；这是支持安全轮换所需的规范化一对多关系，不是独立文件 Key Store。`agentvault.key_management` 只引用 KEK，绝不保存 EOA 私钥。轮换先写新版本并原子切换 Active；旧版本仅作为不可变审计与受控恢复材料保留，未签名操作不会继续使用 Retired Key。任何 API 都不能导出明文私钥。Dashboard 创建 EOA 时先申请临时 RSA-3072 公钥，浏览器固定使用 RSA-OAEP（SHA-256/MGF1-SHA-256）直接加密 32 字节 EOA 私钥。API 返回 `session_id/server_public_key/expires_at`，提交仅包含 `session_id/ciphertext`；不协商算法、不派生客户端 AES 密钥。公钥使用 base64url DER SubjectPublicKeyInfo，OAEP label 为 UTF-8 的 `agentvault.wallet-import.v2\0{session_id}\0{wallet_id}\0secp256k1`（`\0` 表示 NUL）。服务端临时私钥由 Recovery AEAD 密封后写入带 TTL 的 Cache；WalletHandler 验证身份与有效期、原子消费会话后解密 EOA，再委托 WalletKeyManager 用现有 KEK 封装加密并原子写入 SQL。过期、失败和成功的消费均不可重放，临时私钥不入库。前后端须同步升级；旧 ECDH 请求被拒绝，升级后需重新申请会话。此层加密减少 TLS 终止点和请求日志接触私钥的机会，但公钥和页面仍通过 HTTPS 分发，不能抵御可主动替换页面或公钥的攻击者。Credential/Grant 的崩溃恢复材料由独立 `agentvault.recovery_encryption` AES-256-GCM 密钥环保护，Event 和日志只保存 Hash/Reference。

APM 提供结构化日志、OTLP Trace、Prometheus/OpenMetrics、健康检查和受限运行时诊断。HTTP 入站提取 W3C `traceparent/tracestate`，gRPC metadata 进入同一 tracing middleware；AgentVault 调用外部 Facilitator/Provider 与 EVM RPC 时建立 client span 并继续注入该上下文，运营 Secret Header 无权覆盖 trace header。SDK 的 gRPC/MQTT Transport 必须创建或继承 producer Span Context，注入 W3C Trace Context，并输出可校验的 `trace_id/span_id/parent_span_id` 结构化日志；宿主业务已接入 OpenTelemetry 时，Adapter 必须桥接当前 Context，不得自建第二条身份链。MQTT 的 `requestSpanId` 只是 producer Span ID 的 wire 投影；publish、consume、response 和 retry 各自建立 Span 并传播/链接 W3C Context。Trace Context 只用于可观测性，绝不参与 Client/Scenario 授权。Grafana Dashboard 位于 `deploy/apm/grafana/`。

Management 健康端点区分存活与就绪：`/healthz` 只证明进程事件循环可服务；`/readyz` 在 `mgmt.health.probe_timeout` 内查询当前权威 SQL adapter，数据库不可用或超时即返回 503。Memory/Redis Cache 仅是可降级加速层，故其故障产生指标和告警但不篡改资金状态，也不单独摘除 Ready 实例。

## 10. SDK 与 Dashboard

`src/adapters/{rust,golang,python,typescript,java}` 使用一致的功能模块：

```text
model/        wire/domain DTO
transport/    统一契约、gRPC/MQTT 实现、超时、响应上限和脱敏
protocol/     PaymentProtocol Provider/Factory、x402 与 MPP wire 映射
payer/        PayerMiddleware + 生态 HTTP 客户端拦截器
payee/        PayeeMiddleware + 生态服务端中间件
config/       传输与双向 Middleware 强类型配置
agentvault    唯一公开 Facade，含审批查询/决策入口
```

Payer 总入口负责捕获 402、请求授权、等待签名/审批结果、校验 Credential 的 Origin
绑定、有限重试原业务 HTTP/MCP 请求并上报结算响应；Rust/Go/Python/TypeScript/Java
分别提供 Reqwest、net/http、HTTPX、Fetch、Spring 实现。Payee 总入口负责 Offer、
Challenge、Verify、Settle，并由 Axum、Gin、FastAPI/ASGI、Express、Spring 适配器
决定是否放行业务请求。业务代码只在启动时选择 gRPC 或 MQTT；SDK 不提供 HTTP
控制面 Transport，切换传输不改变业务 Handler。

五种 Payer 配置只包含 `scenario_id` 与超时、审批轮询、重试等客户端行为参数；不得出现 Wallet、Policy、金额、资产、网络、收款人或商户 Origin 配置。SDK 授权 wire 只发送操作上下文和捕获的原始 Challenge，并校验返回 Intent 中由 AgentVault 选择的 Policy/Wallet 与签名凭据绑定完整。

SDK 源码中的强类型 DTO 和契约测试是调用边界，不维护额外公开 API 描述目录。Dashboard 位于 `web/`，只使用同源 Cookie/上下文，不保存 Bearer Token。一级菜单按业务职责组织为 Payer 交易、Payee 交易、审批、余额与 Ledger、Client 身份归属、Scenario 治理、Wallet 管理和运行状态。所有可见列表默认按 10 条分页；守恒指标需要完整 Ledger 时在后台以有界批次读取，不能把当前页误当完整事实。Client 页面只读展示可信身份绑定及其 Scenario 关系，身份变更仍走受控 `console import`；Scenario 不保存外部应用引用，其 `id` 就是唯一稳定配置身份。Scenario 创建会在一个服务端事务中写入 Scenario、初始 Policy 与勾选的多个 Wallet membership；Wallet 通过单列、限高、可滚动的多选下拉选择，任一 Wallet 无效则整体回滚。独立 Wallet 页面允许创建本地 EOA，但私钥先在浏览器中使用临时 RSA 公钥加密，明文不会进入 React 状态、URL、日志和 JSON 请求；仅服务端 WalletHandler 在消费会话后短暂解密，再交给既有 KEK 入库流程。`agentvault console` 与 Dashboard 共用同一领域/存储端口：目录创建使用规范资源导入，启停和审批使用既有状态机，支付、审批、Ledger 与 Event 历史只读。Wallet 命令面固定为 `create/list/get`，后两者只序列化公开 metadata；不存在查看、导出、修改、轮换、停用或删除 EOA 私钥的 console/UI 路径，密钥创建后只能由运行时 signer 经 custody port 使用。Policy、Wallet 等目录数据保持在各自内聚的治理流程中。

## 11. 部署与真实 E2E

- `deploy/docker/`：默认 AuthGuard + Envoy + AgentVault + Dashboard；另有明确命名的开放拓扑。MQTT 默认关闭，必须显式接入认证和 ACL 完整的 Broker。
- `deploy/helm/agentvault/`：AuthGuard 为默认启用的 umbrella dependency，受保护 HTTPRoute 使用 `authguard.io/protected=true`；禁用后不创建受保护 Route，运营方必须明确暴露开放 Service。
- `use-cases/marketpulse-feed-subscription/e2e/`：从零构建、部署和分层验证的真实案例。

E2E 中 MarketPulse 暴露两个 MCP Tool：昨日新闻免费，实时新闻按次收费。QuantStream 使用 Rust、Go、Python、TypeScript、Java 五种等价后端定时采集；五种 Payer 只知道各自独立的 Client transport identity 与 Scenario，不配置 Wallet、私钥、链 RPC 或支付投影。`console import` 为五个 Payer 分别导入 Client/Scenario/Policy/MQTT 身份和两个独立充值 EOA，共十个 Wallet；每个 Case 的两次真实付款必须只落到当前语言的两个 Wallet，其余八个余额不得变化。数据库 membership/Intent、AgentVault 签名日志、十个链上余额及 SQL 快照共同作为证据。付费 Tool 分别经 MQTT 与 gRPC 请求 AgentVault 签名，随后由五种 Payee Middleware 调官方 Facilitator Verify/Settle，只在结算成功后释放数据。唯一外层入口 `runner.py` 默认以 `--e2e-sdk-scope simple` 执行 Rust/Go 2×2×2=8 个快速隔离 x402 Case；发布验收显式使用 `--e2e-sdk-scope full` 执行 5×5×2=50 个 Case。`s21` 至 `s24` 按所选 scope 验证 SDK 步骤日志、签名钱包、Payee 结算、3 分钟余额同步、Ledger/Receipt/交易绑定、Token 守恒与 Facilitator Gas。独立 `x402-solana` 场景在固定版本的真实本地 Validator 上创建 SPL Mint/ATA，由上游 x402 v2 client/facilitator 完成 verify 与唯一一次 settle，并核对 SPL 余额、交易手续费和 Lamport 守恒。独立 `mpp-tempo` 场景在固定摘要的官方 Tempo localnet 上，以官方 `mpp` Rust SDK 驱动的本地 HTTP 结算服务完成非变更 verify、type-0x76 广播、丢响应恢复和包含 pathUSD Gas 的守恒；Tempo `charge` 直接使用链 RPC，不伪造 x402 式 Facilitator，也不额外部署同义清算合约。五 SDK 的双协议/双 Transport/双角色一致性由共享契约夹具门禁。Web 场景校验同 scope 的不可变报告，再以报告派生的 API fixture 驱动真实 Chromium，截图验证每页最多 10 条的 Payer/Payee 列表、全部 Ledger 守恒、Client→Scenario 可信归属，以及 12 个候选 Wallet 的单列滚动多选下拉与双 Wallet 原子创建；它不会伪称隔离数据库是一个生产库。三条链路分别输出不可变报告，不能互相替代。

## 12. 安全边界

- AuthGuard 负责 HTTP 身份和 IAM；AgentVault 负责支付领域授权，二者不会重复实现。
- Wallet 管理、支付授权、审批和签名职责分离；审批人不能与请求人相同。
- Challenge/Credential 严格绑定 Chain、Token Contract、Decimals、Amount、Recipient、Nonce、Expiry、Domain 和业务请求。
- SDK 禁止把支付凭据跟随跨 Origin Redirect，限制响应体和超时，并在所有重试中稳定复用 `run_id`。
- 关键持久化失败一律 Fail Closed；Cache、日志、Event 或客户端回报都不能升级为资金事实。
- 停用 Client/Scenario/Policy/Wallet 只阻止新操作，不删除历史 Intent、Event、Approval 或 Ledger。

以上约束共同保证 AgentVault 是可恢复、可审计的机器支付基础设施，而不是另一个身份系统、嵌入式结算方或普通 Wallet CRUD 服务。
