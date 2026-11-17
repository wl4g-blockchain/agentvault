# AgentVault 架构概览

[English](overview.md) · [文档导航](../index.md)

AgentVault 是面向 AI Agent 的双向支付基础设施。业务应用内嵌 Payer 或 Payee SDK，通过标准 HTTP 调用 AgentVault。AgentVault 负责支付领域授权、钱包托管、审批、结算验证、不可变会计记录与崩溃恢复；业务应用继续负责工作流、资源、任务状态和交付。

本文描述已经实现的服务与部署边界。协议接入契约归 [SDK 文档](../../src/sdk/README.md) 维护；中英文概览引用同一组英文架构图。

## 1. 逻辑架构与控制边界

![AgentVault 逻辑架构：UI、AuthGuard、业务 SDK、支付服务、外部结算与基础设施](assets/agentvault-logical-architecture.svg)

### 主要调用时序

图中使用连续数字编号，字母后缀表示并行的客户端或路由分支；响应和重试复用原连接。

| 步骤 | 调用 |
|---|---|
| 1 | 业务客户端请求资源；需要付款时，资源服务返回支付 Challenge。 |
| 2a–2c | UI、Payer SDK、Payee SDK 分别经 Envoy Gateway 提交请求。 |
| 3 | Gateway 调用 AuthGuard 完成身份和访问检查。 |
| 4 | Gateway 将 protected API 请求路由至 AgentVault；物理图中 4a 提供 Web 静态资源，4b 路由后端 API。 |
| 5 | AgentVault 检查实时链上余额/Gas，独立观察转账证据。 |
| 6 | Payee 调用外部 Facilitator/Provider 验证并结算 Credential。 |
| 7 | Provider 执行或确认链上结算。 |
| 8 | AgentVault 读取或原子写入权威支付状态与已确认 Ledger；授权、审批、恢复复用此事务边界。 |

| 控制边界 | 核心职责 |
|---|---|
| Browser / 应用 UI | Payer Transactions、Payee Transactions、Approvals、Balances & Ledger、Clients、Scenarios、Wallets。历史只读，审批和支持的配置写入复用领域 API。 |
| AuthGuard | 身份与访问检查。一个 Envoy Gateway 统一路由 Web 和已注册 protected APIs，一个粗粒度 AuthGuard 组件代表身份服务。 |
| AgentVault | Payer 授权/签名、Payee 定价/结算、共享预算、权威状态和恢复。 |
| 业务应用 | 内嵌 Payer/Payee SDK，保留自己的资源交付和任务状态。 |
| 外部结算 | Facilitator、Provider 和已配置链 RPC 在 AgentVault 外提供可信证据。 |

Gateway 转发可信 Principal 上下文；AgentVault 检查 Client/Scenario 归属和支付策略，不重复实现身份管理。隔离可信网络可以显式关闭 AuthGuard，但支付领域校验仍然有效。

## 2. 核心职责

| 组件 | 职责 |
|---|---|
| [Payer](../../src/payer/) | 验证 Scenario/Policy、选择 Wallet、预留预算、独立审批、受约束签名和付款证据确认。 |
| [Payee](../../src/payee/) | 解析 Price、签发 Challenge、验证 Credential、调用外部结算，确认到账后允许业务交付。 |
| [共享领域/状态](../../src/core/) | Client/Scenario/Policy 归属、钱包托管与 Key Version、Intent/Approval/Ledger 事务和幂等恢复。 |
| [协议适配](../../src/protocol/) | 映射 x402/MPP 语义，取得可信链上证据。 |

[Payer/Payee SDK](../../src/sdk/) 位于业务应用进程内，不是额外服务；[CLI](../../src/cmd/) 装配后端角色，[Web](../../web/) 通过受保护 HTTP 入口管理支付操作。

## 3. 资源、授权与预算

归属关系为 **Client → Scenario → Policy → Wallet membership**。一个 Client 拥有多个 Scenario，一个 Scenario 拥有多个 Policy，一个 Policy 授权一个或多个 Wallet；显式 membership 允许共享钱包。

| 资源 | 语义与不变量 |
|---|---|
| Client | 注册应用；`auth_principal` 绑定可信 Principal。 |
| Scenario | 单一 Client 下的支付隔离边界，注册完成不等于获得付款权限。 |
| Policy | 协议、网络、资产、Origin/Recipient 限制、双向预算与审批规则。 |
| Wallet | 本地托管 EOA 或外部收款地址；共享钱包始终受聚合硬限额约束。 |
| Price | 绑定 Scenario、INBOUND Policy 及合规收款 Wallet 的权威报价。 |
| Intent | 支付持久状态、已选授权事实、预留及恢复进度。 |
| Approval | 独立管理员对已有 Intent 和业务 Run 的审批。 |
| Ledger | 已确认资金 Effect，只能追加或通过显式 Reversal 冲正。 |

`run_id` 标识一次逻辑操作，不是授权身份。`UNIQUE(scenario_id, run_id)` 对同请求指纹返回原结果，对同 Run 的不同请求拒绝冲突。轮询、审批、付款重试、证据上报、恢复和交付均保留该 Run。重试递增 `attempt_no`，Trace 可随尝试或重启变化，但不产生另一个业务关联标识。

Payer 只提交 `scenario_id + run_id + 原始 Challenge`。可信 Principal 决定 Client 归属；调用方不能为签名选择 Policy、Wallet、金额、资产、网络或收款人，这些事实来自 Challenge 与持久化配置。

匹配的 Policy–Wallet membership 构成有界钱包池。系统先区分可直接授权、需审批和拒绝，再优先选择当日已花费加预留最少的 Wallet，以请求哈希稳定决胜。多个 Policy 表达替代授权，而非限制交集。SQL 锁内再次检查 membership、Key Version、Policy 快照以及 Scenario/共享 Wallet 双层预算；Intent 建立后不能通过重试静默换钱包。授权使用 SQL membership/存在性查询，目录分页只服务展示。

同一预算单位下，INBOUND/OUTBOUND 限额独立，入站限额约束库存或业务敞口。一次性 Grant 只为一个 Intent 增加有限额度，不改写长期 Policy、不突破 Wallet 硬上限，请求人不能审批自身付款。Grant 绑定 Intent、请求哈希、金额、审批人和过期时间，在预留事务内原子消费。审批 JWT 是内部 Intent-bound 证明，不是登录 Token；持久状态保留 Claims Hash 和受保护的恢复材料。

## 4. 支付与恢复流程

### 出站：授权并付款

1. Payer SDK 从 HTTP、MCP 或受支持的 A2A 资源取得支付 Challenge。
2. SDK 固定一个 `run_id`，将原始 Challenge 与 Scenario 提交 AgentVault。
3. Payer 验证归属和 Challenge，选择合规 Wallet，在 SQL 中预留方向性预算；必要时持久化 Approval，等待有权限的独立审批人。
4. 每次签名前重新检查生命周期、授权、Key Version、实时链上余额及 Gas。Protocol 生成 Intent-bound 签名计划，不接受任意 Digest。
5. SDK 用 Credential 重试同一资源操作，再上报协议支付响应。
6. Provider/RPC 的可信证据确认转账后，Intent、Event、Ledger 和 Outbox 原子提交。签名或商户回执不单独代表到账。

### 入站：到账后交付

1. 资源 Handler 内嵌 Payee SDK，解析权威 Price。
2. Payee 签发唯一、可过期、绑定 Scenario/资源/金额/资产/收款人的 Challenge，预留 INBOUND 容量。
3. 检查 Credential 语义与重放风险，调用外部 Facilitator/Provider Verify/Settle。
4. 只有网络、Token、精度、金额、收款人、交易 Effect 与确认数匹配的证据，才能产生持久结算和 Receipt。
5. 应用用既有 `run_id` 与任务/交付存储释放资源；付款后交付失败只重试交付，不重复收费。

Credential 指纹在同一支付方向内唯一，不能跨资源或请求兑换。同一部署服务付款方和收款方时，可为同一外部支付分别记录一次 OUTBOUND 与 INBOUND 事实，表达各钱包的会计视角。

外部结算与 SQL 无法共同原子提交。持久恢复证据、幂等 Provider 调用、SQL Lease 和 Reconciliation 收敛不确定结果。恢复读取精确 `(intent_id, event_type, attempt_no)` 证据，不依赖分页审计视图。SQL Outbox 按目标、事件类型和 `run_id` 去重，每个进程有唯一 Lease Owner，恢复任务竞争 SQL Lease，而非 Broker 订阅；不需要业务消息 Broker。默认 CLI 运行 Reconciliation、审批过期和余额同步，不启动推送通知 Dispatcher；Outbox 投递通过 `PaymentEventDispatcher` 与 `PaymentEventSink` 显式扩展，SDK 和管理员可以通过 HTTP 查询支付/审批状态。

## 5. 持久化与资金一致性

PostgreSQL 或 SQLite 是支付权威。[迁移文件](../../migrations/) 在事务内初始化 canonical schema 和版本历史；启动不会重放初始化，也不接受未知版本链。

| 表 | 职责 |
|---|---|
| `pay_client`、`pay_scenario`、`pay_policy`、`pay_price` | 身份归属、授权配置与报价。 |
| `pay_wallet`、`wallet_key_version`、`pay_policy_wallet` | 钱包元数据、不可变加密版本、硬限额及 membership。 |
| `pay_scenario_approver`、`pay_approval` | 审批人、Intent-bound 决策及一次性 Grant。 |
| `pay_intent`、`pay_event` | 支付状态和只追加的操作/恢复证据。 |
| `pay_ledger` | 已确认 Effect、去重与精确冲正。 |
| `pay_outbox` | 与聚合状态共同提交的持久投递/恢复记录。 |
| `pay_wallet_balance` | 面向运营的非权威链上观察值。 |
| `pay_schema_version` | Forward-only schema 历史。 |

PostgreSQL 使用有序行锁、条件更新与 SQL Lease；SQLite 使用 `BEGIN IMMEDIATE` 和事务 Lease，只有一个 AgentVault 写进程。共享 PostgreSQL 支持多副本，Redis Lock 不参与正确性证明。

预留、Grant 消费、状态迁移、Key Version 变更和入账由领域事务显式完成。DDL 约束静态关系与唯一性，不用业务 Trigger/存储过程隐藏状态迁移。领域 Port 限定资金追加和冲正，凭据隔离与受控迁移构成运维边界，不支持任意直接 SQL 写入。

Ledger 用 `(wallet_id, posting_key)` 去重，可信 `effect_ref` 区分同一交易的本金、Gas、退款等 Effect；历史行不能修改或删除。关键持久化失败一律 Fail Closed，日志、缓存及客户端声明不能升级为资金事实。

## 6. 协议与应用集成

| 绑定 | 已实现范围 |
|---|---|
| x402 v2 / EVM | Exact Token 支付及 EIP-3009 Intent-bound 授权。 |
| x402 v2 / Solana | 已配置链、Fee Payer 与回执验证的 SPL Token exact-payment profile。 |
| MPP / Tempo | 受约束 type-0x76 签名与外部结算 Provider 支持 `charge`。 |
| HTTP / MCP | Challenge、Credential、付款重试及各协议自己的 Receipt。 |
| A2A | 显式启用、非流式 A2A 0.3 JSON-RPC `message/send`，遵循各协议自己的 metadata/extension 契约。 |

MPP session、split、费用代付、machine-token、delegated-key 仍不支持。自动 A2A 支付不声明支持流式、A2A 1.0 或嵌套 AP2；不支持或互相冲突的 wire 表达直接拒绝。

Provider 和链 RPC 来自运营方配置，不能由 Challenge 选择。适配器固定 Authority，限制响应、超时和重定向；x402 绝对过期时间在审批和重试中保留，回执上报只用于寻找证据。SDK 保留业务认证，禁止跨 Origin 转发支付 Credential，并在调用 Handler 前剥离支付 metadata。

AgentVault 不部署业务 A2A/MCP Server；这些服务归接入应用，其内嵌 Payee SDK 决定是否放行资源，出站客户端内嵌 Payer SDK。语言适配器与 wire 示例见 [SDK 契约](../../src/sdk/README.md#a2a-automatic-payments)。

## 7. 托管、配置与管理操作

钱包采用 AES-256-GCM envelope encryption 和不可变 SQL Key Version。`agentvault.key_management` 引用 KEK，不保存 EOA 私钥；只有 Payer 打开托管签名，Payee 使用公开元数据。Retirement/Compromise 不删除历史，也不允许未签名操作使用 Retired Key。

浏览器导入支持 secp256k1 EOA，先申请短期、绑定身份的 RSA-3072 公钥会话。Web Crypto 按固定 RSA-OAEP/SHA-256 参数和 Wallet/Session-bound label 加密 32 字节私钥。服务端原子消费会话，短暂解密，再交给既有 KEK 流程入库；临时私钥由 Recovery 加密后保存在 TTL Cache，不是持久钱包密钥。明文不进入浏览器状态、URL、日志或 JSON 请求。HTTPS 仍保证页面与公钥分发真实性。

Dashboard/Console 共用领域 Port。资金历史与 Client 身份绑定在 UI 中只读，身份/目录注册由受控导入负责；Scenario、初始 Policy、多 Wallet membership 原子创建。Wallet UI/Console 提供创建和公开元数据读取，不提供私钥导出或任意密钥生命周期操作。Price 写入检查 revision、保留不可变归属和创建身份，不得删除历史引用。

配置优先级为默认值 < 原生 YAML < `AGENTVAULT__...` 环境变量，Payer/Payee 分别启用。Secret 通过引用提供，不嵌入 SDK 配置；部署细节见 [Helm 指南](../../deploy/helm/agentvault/README.md)。

## 8. 物理部署与运行

![AgentVault 物理架构：共享 Gateway、独立 Web、多副本组合角色后端、私有 Management 和独立基础设施](assets/agentvault—physical-architecture.svg)

图示为默认 Helm 拓扑：独立 Web 副本和多个**同时启用两种角色**的后端副本。Payer/Payee 是模块或进程角色，不是两个必需 Deployment。`agentvault serve --role payer` 与 `--role payee` 支持独立进程并保留相同 HTTP 契约；Compose 默认一个组合后端。

| 组件 | 部署行为 |
|---|---|
| Envoy Gateway / AuthGuard | 共享受保护入口；生产配置边缘 TLS，Compose 开发入口为 HTTP `:8080`。 |
| Web | 独立生产 Nginx 镜像，内部 `8080`；浏览器经 Gateway 使用同源 API。 |
| 后端 | 默认 Helm 部署两个组合 Payer/Payee 副本，业务和 Admin API 使用 `8080`。 |
| PostgreSQL | 后端副本共用的权威数据库，持久保存加密钱包密钥、支付状态和 Ledger；SQLite 是单进程替代。 |
| Facilitator / Provider / 链节点 | 独立运行的结算服务，由运营方配置，不由不可信 Challenge 选择。 |

缓存、Secret 配置、健康检查和遥测属于配套部署细节，配置及外部端点见 [Docker](../../deploy/docker/README.md) 和 [Helm](../../deploy/helm/agentvault/README.md) 指南，不再单独展开架构区域。

## 9. 验证与相关文档

[MarketPulse 用例](../../use-cases/marketpulse-feed-subscription/README_CN.md) 覆盖五种 Payer 和五种 Payee：完整 HTTP 矩阵为 25 个隔离组合，Rust/Go 简化范围为 4 个。Case 核对钱包、SQL Intent/Ledger/Receipt、链上余额、结算 Effect、Gas 和 Trace 父子关系；Solana、Tempo、A2A 场景各自提供证据，不能用单一场景替代所有 profile。

Docker/Kubernetes CI 还执行 `live-wallet`：Chromium 加载**生产 Web 容器**，经过 Nginx 访问真实 API，不拦截浏览器请求，检查付款/收款、筛选、余额、审批、Client 归属、加密钱包、Scenario 原子创建，并保存截图和 SQL 断言。可选 `web` 场景使用不可变报告派生的 fixture 验证大数据量展示和分页，不宣称它是真实生产数据库测试。

- [SDK 与 A2A 绑定](../../src/sdk/README.md)
- [Docker 部署](../../deploy/docker/README.md)
- [Helm 部署与 Secret bootstrap](../../deploy/helm/agentvault/README.md)
- [端到端场景与证据](../../use-cases/marketpulse-feed-subscription/README_CN.md)

图底部只解释领域缩写；托管章节中的 EOA 指外部账户。
