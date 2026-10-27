# 自主支付人工验收

本页命令从仓库根目录执行。`manual` 保留验收环境；`manual-payment` 发起一笔真实购买；`manual-matrix` 交叉调用五语言服务。后两项不重建链或清空前一笔记录。付款的 `run_id` 串联 SDK、Vault、商户、账本和回执；签名、私钥、完整 Credential 不应打印到日志。

当前环境有三条本地链、15 个商户服务和 5 个 suspended payer 模板。就绪情况、原生 Challenge、单笔实链购买与完整交叉矩阵是不同证据；本手册列的是可执行命令，成功范围只看对应本轮报告，不能据此推定全部组合已成功。

## 1. 协议与验收证据

| 层次 | 观察位置 | 预期 |
| --- | --- | --- |
| 商户调用 | QuantStream → MarketPulse | MCP `tools/call` 或 A2A 非流式 `message/send` |
| 签名控制面 | SDK → AgentVault | gRPC 或 MQTT，与业务 HTTP 是两条链路 |
| x402 HTTP | Header | `PAYMENT-REQUIRED` → `PAYMENT-SIGNATURE` → `PAYMENT-RESPONSE` |
| x402 MCP | ToolResult / request `_meta` | HTTP 200、`isError:true`、x402 v2 `structuredContent`；不要求 HTTP 402 或 `PAYMENT-REQUIRED` 头 |
| x402 A2A | Task / Message metadata | `input-required`、`x402.payment.required` → `x402.payment.payload` → `x402.payment.receipts` |
| MPP HTTP | Header | `WWW-Authenticate: Payment …` → `Authorization: Payment …` 或挑战指定的 `Payment-Authorization` → `Payment-Receipt` |
| MPP MCP/A2A | JSON-RPC 消息 | `-32042`、`error.data.httpStatus=402`、`challenges`，Credential/Receipt 在 `org.paymentauth/*` 元数据 |
| x402/EVM | Anvil | EIP-3009、E2EUSDC ERC-20、Facilitator 支付 Gas |
| x402/Solana | Solana validator | 标准 SPL mint、ATA、Facilitator 支付 Lamport 手续费 |
| MPP/Tempo | Tempo localnet | 官方 Tempo `charge`、pathUSD TIP-20，付款者付 Token Gas |

MCP/A2A 的支付信息在消息中，不能只抓 HTTP Header 判断协议是否生效。MPP 当前实现 Tempo `charge`，不是把 EVM EIP-3009 报文换成 MPP Header。

保留环境的 15 个 payee 当前统一使用 gRPC 控制面；`manual-payment/manual-matrix --transport` 选择 payer 控制面。因此本页 300 组合表示三链 × 两消息协议 × 两 payer 控制面 × 25 语言组合，不表示两端控制面的全部交叉组合。旧隔离矩阵的 `--transport` 则同时配置该案例两端，按其报告解释。

## 2. 启动与检查

```bash
cd /home/agent/agentvault
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
export E2E_DIR="$PWD/use-cases/marketpulse-feed-subscription/e2e"
export VAULT_NS=e2e-agentvault-system
export INFRA_NS=e2e-agentvault-infra-system
export PAYMENT_NS=e2e-agentvault-payment-system
export PAYEE_NS=e2e-agentvault-mock-payee
export PAYER_NS=e2e-agentvault-mock-payer
kubectl() {
  sudo env KUBECONFIG="$KUBECONFIG" /usr/local/bin/kubectl "$@"
}
```

已经部署时先检查，避免再次初始化链。已有全部所需镜像时可用 `--skip-build`。本机 kubeconfig 是 root 的 0600 文件，以上函数通过 sudo 调用实际 `/usr/local/bin/kubectl`，本页后续 `kubectl` 命令均使用该函数；每个新终端先执行这组 export 和函数定义。不要把 kubeconfig 设为全局可读。

只有明确需要从零重建时才执行下条 `manual`：它会清理本 E2E 的旧环境、链状态和业务数据，部署后保留新环境，但不发起业务购买。下例复用已经完成构建的本轮镜像；需要从源码构建时将 `--skip-build` 改为 `--reuse-build`，后者仍会执行构建，两参数互斥。

```bash
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual \
  --skip-build --timeout 1800
```

链、Vault 和 Token 已就绪，仅服务 rollout 因镜像/PVC 时序失败时，使用以下恢复入口。它读取既有公开 fixture，冻结原镜像引用，只恢复 15 个商户、5 个 suspended payer 模板及 UI；不 cleanup、不导入 Vault、不重建链、不发起付款。部署阶段在安装应用前已原子保存公开链身份，失败后仍可恢复。

```bash
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-services \
  --skip-build --timeout 1800
```

若缺少原 `manual-public.json`，恢复入口会拒绝；不要改用 `manual` 来掩盖缺失状态，也不要手工生成假 mint/余额。先从当前公开链和数据库事实恢复部署记录。

本机验收的 Rust payer/payee 和辅助 Provider 使用当前源码的 debug 构建产物，镜像采用匹配的 glibc runtime；这轮不冒充优化 release 构建。宿主二进制要求 glibc 2.38，本机验收运行镜像为 Debian 13 / glibc 2.41；源码默认 Dockerfile 的容器内 release 构建仍保留配对的编译/运行基础镜像。源码及实际镜像摘要见本轮报告。

```bash
kubectl get node
kubectl get pods -A -o wide | rg 'e2e-agentvault-'
kubectl -n "$PAYEE_NS" get deployment,svc,pvc
kubectl -n "$PAYER_NS" get cronjob,job,svc,pvc
kubectl -n "$PAYMENT_NS" get deployment,svc
```

应看到三条链、Vault、Postgres、EMQX、外部支付 Provider 和 15 个 MarketPulse 服务。五个 payer CronJob 保持 suspended，供人工创建一次性 Job，不会后台持续花钱。`ErrImageNeverPull`、`Init:ImagePullBackOff`、`CrashLoopBackOff` 都不算就绪。

`manual` 对五个 payer 镜像使用 containerd 的 `io.cri-containerd.pinned=pinned`，并等待 CRI 确认 `status.pinned=true`，避免 suspended 模板尚未使用的镜像被 Kubelet GC 回收。清理入口只按本组件的 `io.agentvault.manual-image-pin=true` 标记解除 pin，即使公开部署状态文件丢失也可释放；已有的外部 pin 保持原归属。该机制作用于 Kubelet GC；显式 `ctr images prune --all` 仍可能删除镜像。参见 [containerd CRI 实现](https://github.com/containerd/containerd/blob/v2.2.3/internal/cri/store/image/image.go) 和 [维护者说明](https://github.com/containerd/containerd/discussions/12156)。本轮公开核对结果保存在 `payment/manual/challenges/runtime-image-pins.json`。

| runner 语言 | Payee Service 命名 | Payer 模板命名 |
| --- | --- | --- |
| `rust` | `marketpulse-rust-<profile>` | `quantstream-rust` |
| `go` | `marketpulse-golang-<profile>` | `quantstream-golang` |
| `python` | `marketpulse-python-<profile>` | `quantstream-python` |
| `typescript` | `marketpulse-typescript-<profile>` | `quantstream-typescript` |
| `java` | `marketpulse-java-<profile>` | `quantstream-java` |

profile 是 `evm`、`solana`、`tempo`。各服务均有 `/mcp` 和 `/a2a`。不要把已经 Complete 的一次性 payer Job 当作未部署；真正需要长期 Running 的是商户、Vault 和基础设施。

## 3. Port-forward

各开一个终端，前台运行，只绑定 `127.0.0.1`，Ctrl-C 结束：

```bash
kubectl -n e2e-agentvault-system port-forward svc/agentvault 18080:8080
```

```bash
kubectl -n e2e-agentvault-system port-forward svc/agentvault-management 19091:9091
```

```bash
kubectl -n e2e-agentvault-mock-payer port-forward svc/quantstream-ui 18086:8086
```

```bash
kubectl -n e2e-agentvault-mock-payee port-forward svc/marketpulse-rust-evm 18081:8081
```

```bash
kubectl -n e2e-agentvault-payment-system port-forward svc/anvil 18545:8545
```

```bash
kubectl -n e2e-agentvault-payment-system port-forward svc/solana-validator 18899:8899
```

```bash
kubectl -n e2e-agentvault-payment-system port-forward svc/manual-payment-provider 18087:8087
```

```bash
curl -fsS http://127.0.0.1:19091/healthz
curl -fsS http://127.0.0.1:19091/readyz
curl -fsS http://127.0.0.1:19091/metrics -o /tmp/manual-mgmt.prom
head -n 20 /tmp/manual-mgmt.prom
curl -fsS http://127.0.0.1:18086/healthz
# 未提供 UI 登录凭证时，审批列表预期 401。
curl -i http://127.0.0.1:18086/api/approvals
curl -fsS http://127.0.0.1:18081/healthz
curl -fsS http://127.0.0.1:18087/healthz
# 管理端口不接受业务/支付写入：预期 404。
curl -i http://127.0.0.1:19091/api/v1/payments
```

## 4. 不付款，先看 Challenge

MCP 请求与业务示例一致，先调用免费 Tool，再调用收费 Tool。每次探测使用新 run，避免持久化环境中重复使用已过期 Challenge。不同官方 MCP server 可能要求先 `initialize` 并携带返回的 `Mcp-Session-Id`；SDK 会自动完成握手。

```bash
export FREE_PROBE_RUN="manual-free-$(date +%s%N)"
export PAID_PROBE_RUN="manual-challenge-$(date +%s%N)"
curl -sS -i -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' -H "X-Run-Id: $FREE_PROBE_RUN" \
  --data '{"jsonrpc":"2.0","id":"free","method":"tools/call","params":{"name":"marketpulse.yesterday_news","arguments":{}}}' \
  http://127.0.0.1:18081/mcp

curl -sS -D /tmp/manual-paid.headers -o /tmp/manual-paid.body \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
  -H "X-Run-Id: $PAID_PROBE_RUN" \
  --data '{"jsonrpc":"2.0","id":"paid","method":"tools/call","params":{"name":"marketpulse.realtime_news","arguments":{}}}' \
  http://127.0.0.1:18081/mcp
cat /tmp/manual-paid.headers
cat /tmp/manual-paid.body
```

EVM x402 MCP 应含 `isError:true`、`x402Version:2`、`accepts[].network=eip155:31337`、amount `1000000`；原生 MCP 模式不混入 HTTP `PAYMENT-REQUIRED`。`payment-expires-at` 为稳定的绝对过期时间。Challenge 不等于付款，数据库可能记录 inbound created Intent，但不会有成功转账和 payment Ledger。

A2A 先通过五 SDK 真实 Job 发起，SDK 会激活所需扩展并在挑战后沿用商户返回的 Task/Context。检查 wire 定义可执行：

```bash
python3 -m json.tool src/adapters/tests/payment_protocol_a2a.json
```

不要把此 JSON 夹具中的假交易哈希作为实链回执；它只解释字段。

也可直接观察一次不带凭证的原生 A2A Challenge，不触发 payer 签名：

```bash
export A2A_PROBE_RUN="manual-a2a-$(date +%s%N)"
curl -sS -i -H 'Content-Type: application/json' \
  -H "X-Run-Id: $A2A_PROBE_RUN" \
  -H 'X-A2A-Extensions: https://github.com/google-agentic-commerce/a2a-x402/blob/main/spec/v0.2' \
  --data '{"jsonrpc":"2.0","id":"manual-a2a-probe","method":"message/send","params":{"message":{"kind":"message","role":"user","messageId":"manual-message-1","parts":[{"kind":"text","text":"marketpulse.realtime_news"}]}}}' \
  http://127.0.0.1:18081/a2a
```

对 EVM/Solana 应返回 x402 的 Task payment metadata；初次请求不预填不存在的 `taskId`。要看 MPP 时，把商户 port-forward 改成 `marketpulse-rust-tempo`，同一 JSON-RPC 返回 MPP Challenge；付款重试必须使用 SDK 维护的消息和 Task 绑定。

## 5. 发起实际购买

以下每条生成独立 Job 和新 run，并保留日志及业务数据库。先执行第 6、7 节记录付款前状态：

```bash
# Python payer → Java payee，MCP/x402/Anvil，Vault gRPC 控制面。
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-payment \
  --payment-profile evm --message-protocol mcp \
  --payer-language python --payee-language java --transport grpc --timeout 900

# Go payer → Rust payee，A2A/x402/Solana，Vault MQTT 控制面。
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-payment \
  --payment-profile solana --message-protocol a2a \
  --payer-language go --payee-language rust --transport mqtt --timeout 900

# TypeScript payer → Python payee，A2A/MPP/Tempo。
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-payment \
  --payment-profile tempo --message-protocol a2a \
  --payer-language typescript --payee-language python --transport grpc --timeout 900
```

`--transport` 只选择 SDK→Vault 的内部控制面；`--message-protocol` 才选择付款业务调用的 MCP/A2A。Payer 只知道 Client/Scenario，不接受人工指定 Wallet 或私钥。Vault 从对应 Policy 的两个成员中选择可签名钱包。

显式添加 `--fail-after-settlement` 可验收已有的结算后交付恢复流程；默认关闭。该选项把 payer 的 `QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT` 设为 `once`，并在 `request.json` 记录选择，不增加支付状态机。下例预期首次结算后的业务交付返回 503，SDK 沿同一 run 重试后成功，仍只有一笔链上交易和两条方向 payment Ledger；以该次日志和 RPC/账本证据核实，不能只凭退出码判断恢复成功。

```bash
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-payment \
  --payment-profile evm --message-protocol mcp --transport grpc \
  --payer-language python --payee-language java --fail-after-settlement --timeout 900
```

付款命令成功只会在核对两条方向 Intent、两条 payment Ledger、实际交易回执和精确余额/Gas 后标记 `verified`。失败时仍保留 `payment/manual/<Job>/request.json`、付款前 `before.json`、有界脱敏 `payer.log` 和 `failure.json`；失败阶段及 Job 身份可复核，不会把异常转换成成功。准备阶段若尚未取得余额快照，`before.json` 可能不存在。

列出实际 Job，并跟随其业务容器日志；Go 在容器名中写作 `golang`：

```bash
kubectl -n "$PAYER_NS" get job --sort-by=.metadata.creationTimestamp
export JOB='<刚创建的 Job 名>'
export PAYER_CONTAINER=quantstream-python
kubectl -n "$PAYER_NS" logs -f "job/$JOB" -c "$PAYER_CONTAINER"
kubectl -n "$PAYER_NS" wait "job/$JOB" --for=condition=complete --timeout=900s
```

手动从 suspended CronJob 创建 Job 的基础命令是 `kubectl -n "$PAYER_NS" create job --from=cronjob/quantstream-rust manual-rust-$(date +%s)`；模板有默认目标，跨语言、链、协议切换优先使用统一 runner，避免忘记 Scenario、目标 URL 或消息类型。

## 6. Vault、Provider、数据库

各开一个终端，在发起付款前跟随日志：

```bash
kubectl -n "$VAULT_NS" logs -f deployment/agentvault -c agentvault --since=10m | \
  rg --line-buffered 'wallet.selected|wallet.signed|payment_required|signing_requested|authorize|settle|reconcil|approval|run.id|run_id'
```

```bash
kubectl -n "$PAYEE_NS" logs -f deployment/marketpulse-java-evm -c marketpulse-java --since=10m
```

```bash
kubectl -n "$PAYMENT_NS" logs -f deployment/manual-payment-provider --since=10m
```

```bash
kubectl -n "$PAYMENT_NS" logs -f deployment/x402-facilitator --since=10m
```

预期 Vault 出现 `agentvault.wallet.selected`、`agentvault.wallet.signed`，包括 run、Scenario、Policy、Wallet 和 Intent。MPP Provider 出现 `manual.mpp.verify`、`manual.mpp.broadcast`、`manual.mpp.settle`；verify 是只读，广播只发生在 settle。Solana 出现 `manual.x402.verify/settle`；Anvil x402 转交独立上游 Facilitator。

SDK 自身还记录 `payment_required`、`signing_requested`、`signed`；A2A payer 完成摘要 `manual.payer.completed` 含 run、Task、Intent 和回执检查结果。付款后业务数据位于 `Task.artifacts[0].parts[0].data`。免费昨日新闻仍走官方 MCP 调用，`--message-protocol a2a` 切换的是收费实时新闻。

实际签名入口是 `POST /api/v1/payments/authorize`：服务内先授权、选钱包、按需审批，再调用托管签名组件，返回绑定 Intent 的 Credential。不存在额外的公开 `/sign` 路由。gRPC/MQTT 封装的是同一业务请求，SDK 随后把 Credential 写回相应 MCP/A2A 消息重投，并向 `/payments/{intent_id}/outbound-response` 上报回执。

```bash
curl -fsS http://127.0.0.1:19091/metrics | \
  rg 'agentvault_(payment_operations|wallet_selections|wallet_balance_sync)'
export INTENT_ID='<日志或数据库中的 Intent ID>'
export SCENARIO_ID='<该 Intent 的 scenario_id>'
curl -fsS --get --data-urlencode "scenario_id=$SCENARIO_ID" \
  "http://127.0.0.1:18080/api/v1/payments/$INTENT_ID" | python3 -m json.tool
curl -fsS --get --data-urlencode "scenario_id=$SCENARIO_ID" \
  "http://127.0.0.1:18080/api/v1/payments/$INTENT_ID/receipt" | python3 -m json.tool
```

这两条查询必须提供 `scenario_id`，省略时返回 400。本轮专用 E2E 配置已实测无需额外 HTTP 认证头；UI 审批入口仍保留独立登录校验。

通过 Postgres Pod 已有密码执行只读 SQL，不在终端读取密码：

```bash
kubectl -n "$INFRA_NS" exec -i deployment/postgres -- sh -ec \
  'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -U agentvault -d agentvault -v ON_ERROR_STOP=1' <<'SQL'
SELECT s.id AS scenario_id,p.id AS policy_id,pw.wallet_id,w.address,w.enabled
FROM pay_scenario s JOIN pay_policy p ON p.scenario_id=s.id
JOIN pay_policy_wallet pw ON pw.policy_id=p.id JOIN pay_wallet w ON w.id=pw.wallet_id
ORDER BY s.id,p.id,pw.wallet_id;
SELECT id,scenario_id,run_id,direction,protocol,status,wallet_id,amount::text,
       settled_amount::text,tx_hash,created_at,settled_at
FROM pay_intent ORDER BY created_at DESC,id LIMIT 30;
SELECT i.run_id,e.intent_id,e.event_type,e.attempt_no,e.tx_hash,e.created_at
FROM pay_event e JOIN pay_intent i ON i.id=e.intent_id
ORDER BY e.created_at DESC,e.id LIMIT 50;
SELECT id,run_id,status,requested_amount::text,approved_amount::text,consumed_at
FROM pay_approval ORDER BY created_at DESC LIMIT 20;
SELECT i.run_id,l.intent_id,l.direction,l.wallet_id,l.entry_type,l.amount::text,l.tx_hash
FROM pay_ledger l JOIN pay_intent i ON i.id=l.intent_id
ORDER BY l.effective_at DESC,l.id LIMIT 30;
SELECT wallet_id,network,asset,asset_available::text,native_available::text,observed_at
FROM pay_wallet_balance ORDER BY wallet_id,network;
SQL
```

选中的钱包必须属于该语言、该 profile 的 Scenario Pool。每笔成功交易的 OUTBOUND/INBOUND Intent 各一条，金额同为 `1000000`，tx_hash 相同，payment Ledger 各追加一次。不要查询或导出 `wallet_key_version`、sealed credential、JWT 明文。

```bash
kubectl -n "$INFRA_NS" exec -i deployment/postgres -- sh -ec \
  'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -U agentvault -d agentvault -v ON_ERROR_STOP=1' <<'SQL'
SELECT tx_hash,count(*) AS postings,
       sum(CASE WHEN direction='outbound' THEN amount ELSE 0 END)::text AS outbound,
       sum(CASE WHEN direction='inbound' THEN amount ELSE 0 END)::text AS inbound
FROM pay_ledger WHERE entry_type='payment' GROUP BY tx_hash ORDER BY tx_hash;
SELECT scenario_id,run_id,count(*) FROM pay_intent GROUP BY scenario_id,run_id HAVING count(*)<>1;
SQL
```

已完成的一笔双向支付 postings=2、outbound=inbound；后一查询应为空。Ledger 不允许 UPDATE/DELETE。

### 6.1 人工审批、拒绝和一次性消费

人工环境正常日额度足够交叉矩阵，不会每笔都审批。要单独验收审批，先暂停其他购买；当前 policy 管理 API 只提供查询，下面是在专用 E2E 数据库中显式变更一条 Policy 的审批阈值，不修改 Intent/账本/余额，不伪造支付结果。

以 Rust/EVM 为例，把单笔审批阈值从夹具的 `2000000` 暂改为 `1000000`。事务内锁定、读取 revision、按 revision 更新；记下返回的新 revision，恢复时做第二次 CAS：

```bash
kubectl -n "$INFRA_NS" exec -i deployment/postgres -- sh -ec \
  'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -U agentvault -d agentvault -v ON_ERROR_STOP=1' <<'SQL'
BEGIN;
SELECT id,updated_at AS expected_revision,outbound_approval_threshold AS original_threshold
FROM pay_policy WHERE id='policy-quantstream-rust-market-data' AND enabled FOR UPDATE \gset
\echo original_threshold=:original_threshold expected_revision=:expected_revision
UPDATE pay_policy SET outbound_approval_threshold=1000000,
  updated_at=GREATEST(updated_at+1,extract(epoch FROM clock_timestamp())::bigint)
WHERE id=:'id' AND updated_at=:'expected_revision' AND outbound_approval_enabled
RETURNING updated_at AS changed_revision \gset
\if :{?changed_revision}
COMMIT;
\echo changed_revision=:changed_revision
\else
ROLLBACK;
\quit 2
\endif
SQL
```

Solana/Tempo 使用对应 policy ID 的 `-solana` / `-tempo` 后缀。随后把 UI 切换到同一语言和 profile；`PROFILE_SUFFIX` 在 EVM 为空：

```bash
export PAYER_LANGUAGE=rust
export PROFILE_SUFFIX=''
kubectl -n "$PAYER_NS" set env deployment/quantstream-ui \
  QUANTSTREAM_CLIENT_ID="client-quantstream-${PAYER_LANGUAGE}" \
  QUANTSTREAM_SCENARIO_ID="scenario-quantstream-${PAYER_LANGUAGE}-market-ingestion${PROFILE_SUFFIX}" \
  QUANTSTREAM_APPROVER_PRINCIPAL="principal-quantstream-${PAYER_LANGUAGE}-admin" \
  AGENTVAULT_MQTT_APPROVER_USERNAME="quantstream-${PAYER_LANGUAGE}-approver"
kubectl -n "$PAYER_NS" rollout status deployment/quantstream-ui --timeout=120s
```

重新打开 UI port-forward，在另一终端运行 Rust payer 的 `manual-payment`；原终端会等待审批。Token 仅存本地变量，不回显：

```bash
set +x
UI_TOKEN="$(kubectl -n "$PAYER_NS" get secret marketpulse-e2e-secrets \
  -o jsonpath='{.data.admin-ui-token}' | base64 -d)"
printf 'header = "Authorization: Bearer %s"\n' "$UI_TOKEN" | \
  curl --config - -fsS http://127.0.0.1:18086/api/approvals | python3 -m json.tool
```

确认返回的 scenario、run、requestedAmount=1000000 对应当前购买，然后填入该 approvalId：

```bash
export APPROVAL_ID='<队列中的 approvalId>'
printf 'header = "Authorization: Bearer %s"\n' "$UI_TOKEN" | \
  curl --config - -fsS -H 'Content-Type: application/json' --data '{"decision":"approve"}' \
    "http://127.0.0.1:18086/api/approvals/$APPROVAL_ID/decision"
unset UI_TOKEN
```

批准后原 Job 恢复同一 Intent，`pay_approval.status=consumed`、consumed_at 非空，交易和两边 Ledger 各一次。另建一个新购买可测 `decision:reject`，预期无该 run 的成功交易/payment Ledger，Job 以业务失败结束。

完成后用刚才返回的 changed_revision 恢复原阈值；CAS 冲突时重新检查，不能覆盖他人的修改：

```bash
export CHANGED_REVISION='<刚才返回的 changed_revision 整数>'
kubectl -n "$INFRA_NS" exec -i deployment/postgres -- sh -ec \
  'PGPASSWORD="$POSTGRES_PASSWORD" psql -X -U agentvault -d agentvault -v ON_ERROR_STOP=1 -v rev="$1"' \
  manual-policy-restore "$CHANGED_REVISION" <<'SQL'
BEGIN;
UPDATE pay_policy SET outbound_approval_threshold=2000000,updated_at=updated_at+1
WHERE id='policy-quantstream-rust-market-data' AND updated_at=:'rev'
  AND outbound_approval_threshold=1000000
RETURNING id AS restored_id \gset
\if :{?restored_id}
COMMIT;
\else
ROLLBACK;
\quit 2
\endif
SQL
```

若原值不是 2000000，应恢复实际 original_threshold；不要覆盖自定义配置。不要为触发审批直接更改 pay_approval 状态或清空 Ledger。

## 7. Anvil 独立回执和余额

`manual` 的 Anvil 使用默认 automine，只在有交易或显式预检时出块，空闲期间不持续生成空块。此前每秒出块方案在长时间空闲时持续增长内存，最终触发 512Mi OOM 并丢失临时链状态；该失败证据已保留，不能把重建后的链当作旧链历史的延续。

人工 profile 使用 `manual-anvil-state` PVC（4Gi、ReadWriteOnce）、Recreate 单实例、2Gi 内存上限，以及 `--state=/var/lib/anvil/state.json --state-interval=1 --preserve-historical-states`，保留账户、合约、交易、回执和区块头。普通容器重启会载入同一 checkpoint；最近一次 checkpoint 之后、尚未落盘的交易仍可能在异常退出时丢失，因此它不是生产数据库的同步持久化承诺。不要给此配置叠加 `--prune-history`，该参数会关闭磁盘历史状态持久化。显式 `manual`/`cleanup` 仍会删除本 E2E 的 PVC。[Anvil 官方参数](https://getfoundry.sh/anvil/reference/anvil/) · [本版本序列化状态定义](https://github.com/foundry-rs/foundry/blob/4072e48705af9d93e3c0f6e29e93b5e9a40caed8/crates/anvil/src/eth/backend/db.rs)

`manual-payment`/`manual-matrix` 会先核实节点确为 Anvil、chain ID 为 31337，再显式挖一块刷新本地链时间，随后仍执行 60 秒时钟偏差检查。直接调用 SDK 或手工 `kubectl create job --from=cronjob/...` 会绕过这项准备，长时间空闲后可能因 EIP-3009 的 `validAfter` 比最新区块时间晚而拒绝；此时先执行下面的本地预检命令，再创建 Job。不要用它调整 Solana、Tempo 或任何外部链。

```bash
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast rpc --rpc-url http://127.0.0.1:8545 web3_clientVersion
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast rpc --rpc-url http://127.0.0.1:8545 eth_chainId
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast rpc --rpc-url http://127.0.0.1:8545 evm_mine
```

独立的临时节点验证会在周期 checkpoint 后 SIGKILL，确认 RPC 关闭并换新进程再加载：真实交易、回执、区块头、余额、合约 code/storage 均一致，8 秒空闲期没有新增区块。公开报告只保存状态文件大小/摘要和 RPC 对比，不保存状态文件内容；这份辅助验证不等于业务购买成功证明，实际购买仍以该次 Job 的财务证据为准。

使用 Anvil Pod 的 Foundry，无需本机安装 cast：

```bash
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast chain-id --rpc-url http://127.0.0.1:8545
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast code --rpc-url http://127.0.0.1:8545 0x5FbDB2315678afecb367f032d93F642f64180aa3
export PAYER_ADDRESS='<第 6 节选中的 pay_wallet.address>'
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast call --rpc-url http://127.0.0.1:8545 \
    0x5FbDB2315678afecb367f032d93F642f64180aa3 'balanceOf(address)(uint256)' "$PAYER_ADDRESS"
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast call --rpc-url http://127.0.0.1:8545 \
    0x5FbDB2315678afecb367f032d93F642f64180aa3 'balanceOf(address)(uint256)' \
    0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc
kubectl -n "$PAYMENT_NS" exec deployment/anvil -- \
  cast balance --rpc-url http://127.0.0.1:8545 0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266
```

付款前后执行相同命令：payer Token 减 `1000000`，payee 增同额，其他钱包不变。Gas 由 Facilitator 付。数据库 tx_hash 用 RPC 独立查询：

```bash
export TX_HASH='<pay_intent.tx_hash>'
curl -fsS -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getTransactionReceipt\",\"params\":[\"$TX_HASH\"]}" \
  http://127.0.0.1:18545 > /tmp/manual-evm-receipt.json
python3 - <<'PY'
import json
r=json.load(open('/tmp/manual-evm-receipt.json'))['result']
assert r and int(r['status'],16)==1
print('tx=',r['transactionHash'],'from=',r['from'],'to=',r['to'])
print('gas_fee_wei=',int(r['gasUsed'],16)*int(r['effectiveGasPrice'],16))
print('logs=',json.dumps(r['logs'],indent=2))
PY
```

回执必须来自 Facilitator 发往 E2EUSDC 的成功交易，包含 ERC-20 Transfer。区间内 Facilitator 原生币减量等于所有支付交易 Gas 之和；fixture 部署和 mint 的 Gas 不计入支付区间。

## 8. Solana 的 E2EUSDC 是什么

这里使用标准 SPL Token Program 下的 6 位 mint，程序 ID 为 `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA`。没有自研 Solana Token 合约，因此不应伪造一份 E2EUSDC.rs。对应实现为：`deploy/deployer/manual_fixture.py` 中 `ManualChainFixture.solana()`；自动门禁的初始化也在 `docker.py/kubernetes.py::x402_solana`。

Solana 的官方模型是程序提供代币指令、mint 标识一种代币、Token Account 保存某个 owner 的余额；官方文档也提供 `spl-token create-token/create-account/mint` 的流程。[Solana Token 模型](https://solana.com/docs/tokens)、[SPL Token 参考](https://www.solana-program.com/docs/token)。

Classic SPL mint 不自动写入 name/symbol；这里的 E2EUSDC/USDC 是夹具资产标识，链上唯一身份是本轮 mint 地址，并不表示 Circle 发行的 USDC，也不表示部署了同名自定义程序。

初始化真实执行：生成密钥 → 创建 mint → 创建 payer/payee ATA → mint 1000 Token 到每个付款 ATA；私钥只进入 Secret/Vault。人工部署报告保存公开 mint、ATA、fee-payer 和 network。显式清理并重新初始化链后地址可能变化，读取本轮报告，不使用旧地址。

manual 模式把 `/tmp/solana-ledger` 挂载到独立的 `manual-solana-ledger` PVC（8Gi、ReadWriteOnce），启动参数移除 `--reset/-r`，Deployment 使用 Recreate，避免两个 validator 同时打开同一账本。普通容器重启或 Pod 重建会加载同一账本、mint、账户和交易历史；这与显式执行 `manual`/`cleanup` 删除旧环境不同。普通隔离 E2E 仍使用临时卷和 reset，不适合代替 manual 保留环境。当前 CPU 上限为 1500m，原因与停启方法见第 11 节。

manual 的后续部署使用 `RUST_LOG=warn` 和 v3.1.14 官方布尔参数 `--log`，让应用日志保留在 stderr，由 Kubelet 轮转；不要写成 `--log=-`。`--quiet` 只隐藏 dashboard，仍会在 ledger 目录增长日志。该设置不删除交易账本，也不改变确认规则。

本机 local-path/hostPath 的 8Gi PVC 是容量声明，并非目录硬配额；Anvil 的 4Gi 声明同样不能保证主机不满盘。Solana 当前官方默认保留 10000 shreds，RocksDB 的 `max_total_wal_size=4GiB` 是触发 flush 的软阈值，不能理解为整个 ledger 的硬上限。持续监控主机余量，闲置时按第 11.1 节 scale 到 0，保留账本。

```bash
export SOLANA_WALLET=go-primary
export PUBLIC_FIXTURE="$E2E_DIR/reports/.runtime/manual-public.json"
export MINT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["solana"]["mint"])' "$PUBLIC_FIXTURE")"
export PAYER_ATA="$(python3 -c 'import json,os,sys; print(json.load(open(sys.argv[1]))["solana"]["accounts"][os.environ["SOLANA_WALLET"]])' "$PUBLIC_FIXTURE")"
export RECIPIENT_ATA="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["solana"]["accounts"]["recipient"])' "$PUBLIC_FIXTURE")"
export SOLANA_SIGNATURE='<pay_intent.tx_hash>'
curl -fsS -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getAccountInfo\",\"params\":[\"$MINT\",{\"encoding\":\"jsonParsed\"}]}" \
  http://127.0.0.1:18899 | python3 -m json.tool
for account in "$PAYER_ATA" "$RECIPIENT_ATA"; do
  curl -fsS -H 'Content-Type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTokenAccountBalance\",\"params\":[\"$account\"]}" \
    http://127.0.0.1:18899 | python3 -m json.tool
done
curl -fsS -H 'Content-Type: application/json' \
  --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"getTransaction\",\"params\":[\"$SOLANA_SIGNATURE\",{\"encoding\":\"jsonParsed\",\"maxSupportedTransactionVersion\":0}]}" \
  http://127.0.0.1:18899 | python3 -m json.tool
```

mint owner 应是上述标准 Token Program，parsed decimals=6。交易 `meta.err=null`，双方 Token 增减精确相反，fee-payer Lamport 差额等于 `meta.fee`。RPC 返回 HTTP 200 本身不能证明转账成功。

`SOLANA_WALLET` 应改为数据库本笔选中的钱包，例如 `go-primary` 或 `go-secondary`；不据示例固定认定实际扣款成员。上述 JSON 只包含公开地址和部署引用，不读取 Secret。

## 9. MPP/Tempo 与重复结算

Provider 的 `/verify` 不广播，`/settle` 只广播通过官方库验证的 type-0x76 signed transaction。固定 chain=1337、token=pathUSD、recipient=本轮收款地址；不支持 session、split 或 fee-payer delegation。

Provider 以已签交易的 hash 查询链上 receipt，已确认时重复 settle 返回相同交易。响应丢失时 Vault 先走只读 verify 恢复；不因为超时就生成一笔新支付。观察 `manual.mpp.broadcast` 次数、pay_intent.tx_hash、payment Ledger 和链上回执，而非要求日志输出完整签名。

原独立核心门禁仍可运行，但会替换当前人工环境，完成后需重新部署 manual：

```bash
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s mpp-tempo --timeout 1800
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s x402-solana --timeout 1800
```

## 10. 五语言交叉与证据范围

一组 profile、消息协议、控制面执行全部 25 个 payer/payee 组合，串行运行并保留所有记录：

```bash
python3 "$E2E_DIR/runner.py" run --deployer kubernetes -s manual-matrix \
  --payment-profile evm --message-protocol mcp --transport grpc --timeout 1800
```

逐项切换 `evm/solana/tempo`、`mcp/a2a`、`grpc/mqtt` 可覆盖 3×2×2×25=300 个购买组合。不要把命令列表当成执行结果，已完成范围以该轮报告、Job、两端 Intent、Ledger 和链上回执为准。预算/余额不足会按真实策略进入审批或失败，不会被测试代码直接改成成功。

历史 `matrix --e2e-sdk-scope full` 是五语言×五语言×两 Transport=50 个隔离的 x402/EVM/MCP Case；每 Case 两笔购买。它每次重置独立 Case 数据和链；不能与 manual 并发使用同一组 Namespace。

本轮真实报文定位后的修复包括：

- Java MPP 曾把原生 base64url `opaque` 当作 Map，导致挑战解析拒绝；现使用官方解析保留 `opaqueRaw`，避免重编码破坏 HMAC。
- Python、Go 的 MPP 转换曾混淆 MCP 展开的 `challenge.request` 对象与 HTTP echo 的 base64url 字符串。双向转换现复用各自官方模型，保留原生 opaque 和挑战指定的 header；Python 对无法无损投影的非规范 request 编码拒绝处理，避免静默改变绑定。MCP 与 A2A 共用这层转换。
- Java WebFlux 的 `Mono.fromCallable` 曾把未收到支付挑战时的 null 授权结果变成空流，丢弃免费 MCP 响应。现用 Optional 承载授权结果，免费响应原样返回，并增加本机真实 HTTP 回归。
- Rust A2A 曾让对象插入顺序影响请求哈希，同语义重投被核心绑定校验拒绝；现递归排序 JSON 对象后计算哈希，保留字段、Task/Context 和大整数精度。
- 核心读取持久化验证与回执时，曾只查 `attempt_no=0`，漏掉后续对账尝试记录。现按 Intent、事件类型读取最新尝试，再读取已持久化的验证事实或回执；不会新增广播或改写 Ledger 来补回执。
- Python 凭证重投曾保留原 body 的 Content-Length，TypeScript 曾漏捕获官方 MCP Transport 直接写入的响应，造成截断或丢失原生支付回执。现分别按当前 body 重建 HTTP framing、在 Node write/end 提交前捕获并注入已验证回执，包含真实 socket 回归。
- Java 的三个 HTTP 适配器现在共用原生消息 carrier，并以 Spring 7 / Servlet 6.1 实际依赖完成回归，防止只修某个客户端后其余客户端继续发送不同的支付 wire。

相关回归已增加，核心绑定校验保持严格。部署器测试覆盖重复安装复用同一 Anvil/Solana PVC、无 reset 参数、namespace 边界及真实 Helm 渲染；配置和生命周期契约测试不能替代实际链重启和实付验收。测试数量与已执行的购买组合以该轮报告为准。

旧失败日志会保留，用于定位和比较。修复后的成功结果必须来自包含修复的实际镜像及其新报告；Pod Ready、单独 SDK 单测或原生 Challenge 正确都不能替代实链购买验收。

## 11. 资源、保存与清理

```bash
kubectl top pod -n "$PAYMENT_NS" --containers
kubectl top pod -n "$VAULT_NS" --containers
kubectl get node -o jsonpath='{range .items[*]}{.metadata.name}{" "}{range .status.conditions[?(@.type=="DiskPressure")]}{.status}{end}{"\n"}{end}'
df -h / /dev/shm
free -h
vmstat 1 5
```

本地 Solana validator 自己产块，不是同步主网。减少 RPC 轮询不能消除 PoH/产块基础开销；改变 ticks/slot 会改变确认时间。CPU 上限、线程和 slot 参数以当前部署配置及固定版本支持项为准。调优后仍需验证 slot 前进、有效 blockhash、真实转账及确认耗时，不能关闭确认来“降低 CPU”。

长期验证否定了最初的 300m 方案：虽然 mint/ATA/转账可以执行，运行约一小时后链时钟落后十余分钟，导致付款方独立对账拒绝过旧回执。没有放宽时间校验；通过 Kubernetes 1.35 的 `/resize` 原地增配追赶，Pod UID、重启次数和链账户保持不变，随后限制为 1500m 并持续采样 slot/blockTime。Helm 当前上限为 `1500m`，Docker 默认 `E2E_SOLANA_CPUS=1.50`。这是保证本版本计时正确的资源边界，不能声称已经把长期稳定消耗降到 300m。

[v3.1.14 官方交易调度源码](https://github.com/anza-xyz/agave/blob/v3.1.14/core/src/banking_stage/transaction_scheduler/receive_and_buffer.rs#L148)在单验证者担任 leader 时持续轮询；主要开销来自交易调度而非主网同步。该版本 test-validator 未提供空闲休眠/调度器切换或 `--hashes-per-tick` CLI，不能靠降低 RPC 同步频率消除忙循环。需要更低持续占用时，应另行验证上游修复版本，避免通过改 slot、warp 或放宽回执校验掩盖问题。

每个 `manual-payment`/`manual-matrix` 案例创建付款 Job 前，都检查最新确认区块与宿主时钟偏差不超过 60 秒；过旧或超前会在花费前拒绝。本地 Anvil 人工部署使用 automine，由付款预检显式挖一块刷新时间；直接 SDK 或 CronJob 调用前也需执行第 7 节的本地预检。

### 11.1 闲时停链，保留账本

不需要随时购买时，可停止 validator 来消除它的空闲 CPU 消耗。先结束所有 Solana 购买、矩阵与待处理结算，确认部署已使用上述 PVC 且无 reset 参数；旧 emptyDir 环境不能用此方法保留链。以下检查只读取公开的卷和启动参数，失败时不要继续缩容：

```bash
kubectl -n "$PAYMENT_NS" get deployment solana-validator -o json | python3 -c '
import json, sys
d = json.load(sys.stdin)["spec"]["template"]["spec"]
c = next(c for c in d["containers"] if c["name"] == "solana-validator")
v = next(v for v in d["volumes"] if v["name"] == "data")
assert v.get("persistentVolumeClaim", {}).get("claimName") == "manual-solana-ledger", "not a retained PVC"
assert not any(a.split("=", 1)[0] in ("--reset", "-r") for a in c.get("args", [])), "reset must be absent"
print("retained ledger configuration verified")
'
```

检查通过后停止产块；PVC 不删除，五个 payer CronJob 继续保持 suspended。停链期间 Solana 付款不可用。

```bash
kubectl -n "$PAYMENT_NS" scale deployment/solana-validator --replicas=0
kubectl -n "$PAYMENT_NS" wait --for=delete pod \
  -l app.kubernetes.io/name=solana-validator --timeout=120s
kubectl -n "$PAYMENT_NS" get pvc manual-solana-ledger
```

需要验收时恢复一份实例，等待服务启动。重新启动第 3 节的 Solana port-forward；旧 Pod 对应的 port-forward 不会自动恢复。

```bash
kubectl -n "$PAYMENT_NS" scale deployment/solana-validator --replicas=1
kubectl -n "$PAYMENT_NS" rollout status deployment/solana-validator --timeout=180s
```

链停止时不会推进时钟，长时间停链可能需要较长追赶时间。恢复后先做只读 clock preflight，等区块时间与宿主时钟偏差不超过 60 秒，再发起购买。等待期间不要改 slot、warp 或绕过校验；runner 还会在创建付款 Job 前再次检查时钟。

```bash
python3 - <<'PY'
import json, time, urllib.request

def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    request = urllib.request.Request("http://127.0.0.1:18899", body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=10) as response:
        result = json.load(response)
    assert "error" not in result, result.get("error")
    return result["result"]

slot = rpc("getSlot", [{"commitment": "confirmed"}])
block_time = rpc("getBlockTime", [slot])
observed = int(time.time())
assert type(block_time) is int and abs(observed - block_time) <= 60, "chain clock not ready; wait before paying"
print("slot=", slot, "block_time=", block_time, "skew_seconds=", observed - block_time)
PY
```

保存 `reports/<时间戳>/`，以及所选 Job、Vault、Payee、Provider 的脱敏日志。活跃 SQLite 使用 backup 产生一致快照；只复制主文件会漏掉 WAL，不是可靠验收证据。不要导出完整 Secret 或原始 Credential。

人工验收结束后，明确执行以下范围化清理；查看过程中不要使用 `--cleanup-after-run`：

```bash
python3 "$E2E_DIR/runner.py" cleanup --deployer kubernetes
```
