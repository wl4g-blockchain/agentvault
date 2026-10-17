# 通用编码规范

- 领域纵切、单向依赖；结构必须高内聚低耦合且收敛干净；能力启用即有配置、契约一致、测试 cases 充分且必要、调用必须 Resilience、标准可观测。

## 1. 统一 YAML 规范

配置 typed、分层；键 `snake_case`，env `<MYAPP>__`，敏感走 Secret。

```text
config/
└── config.(go|rs|py|ts) # 唯一入口；类型/默认/env

<App>Config { server,secrets,mgmt,logging,cache,storage,<myapp>/<domain>... }
```

- 类型/YAML 同层，按“基础→领域→业务”；默认值 < YAML < env，禁未知项。

```yaml
server: { host: 0.0.0.0, port: 8080 }
mgmt: { enabled: true, host: 127.0.0.1, port: 9091,
  health: { liveness_path: /healthz, readiness_path: /readyz }, metrics: { enabled: true, path: /metrics },
  otel: { enabled: false, endpoint: "", protocol: grpc, timeout: 5s, sample_rate: 1.0 } }
logging: { mode: HUMAN, level: info } # HUMAN|JSON
storage:
  provider: Postgres # Postgres|SQLite
  postgres: { url: "...", username: "${MYAPP__STORAGE__POSTGRES__USERNAME:postgres}", password: "${MYAPP__STORAGE__POSTGRES__PASSWORD:}", pool: { max: 20, timeout: 5s } }
  sqlite: { url: "...", pool: { max: 1, timeout: 5s } }
cache:
  provider: Memory # Redis|Memory
  redis: { url: "...", username: "...", password: "...", pool: { max: 20, timeout: 3s } }
  memory: { max_capacity: 65535, default_ttl: 1h, cleanup_interval: 30s }
<myapp>: ...
```

- 标量支持 `${MYAPP__A__B:default}`。Mgmt 仅私网 health/metrics，禁写；日志附 time/level/service/component/run/trace/span 并脱敏。

## 2. 独立领域 Component

组件统一 Provider+Factory（类似 SPI）：契约唯一、按需创建、禁止直建；UT 覆盖选择、失败、并发、生命周期。

```text
[type|class] I<Component> { provider(), init(), close(), <domain_operation>() }

[type|class] <Component>Factory {
  registry: Map<Provider, I<Component>>
  get(provider) -> I<Component> { // get_or_create
    return registry.add_if_absent(provider, resolve(provider).create(Config.get().get) or fail)
  }
  close() { // destroy all registered }
}
```

## 3. Storage / Repository

```text
storage/
├── storage.(go|rs|py|ts)  # 契约/工厂/共享 SQL
├── postgres.(go|rs|py|ts) # 通用 CRUD，供业务组合
├── sqlite.(go|rs|py|ts)   # 同上
└── <domain>.(go|rs|py|ts) # 复杂事务（按需）
```

- CRUD 仅预编译 SQL/受控字段：`select/select_by/insert/update/upsert/delete_by`。Java 用 `repository/XxxRepository`、`Postgres/SQLiteXxxRepository`；方言对齐。

## 4. Cache

```text
cache/
├── cache.(go|rs|py|ts) # ICache、工厂、统一语义
├── memory.(go|rs|py|ts)
└── redis.(go|rs|py|ts)
```

- `ICache`：`get/set/delete/exists/clear/close`；实现等价，缓存非权威源。

## 5. Job（调度领域）

- `job/<domain>.(go|rs|py|ts)` 仅负责 cron/并发/锁/生命周期/观测并调用组件；未获锁跳过，非法 cron 启动失败；处理、幂等、重试归组件。

## 6. Observability / OpenLineage

- `run_id` 是通用唯一可公开幂等键，可以进日志或业务表、Span、OpenLineage Facet；OTel 禁用于约束/恢复。
- 无需重复 `correlation_id`：HTTP/gRPC 自配对；异步以 producer `span_id` 作 reply topic/`requestSpanId`，响应回显并创建新 Span。重试新建 Span，`run_id` 不变、`attempt_no` 递增。
- 服务调用传播 W3C Trace Context；关键转移定义低基数 counter/histogram/gauge，记录结果/耗时/错误。

<table>
<tr><th>字段/场景</th><th>OLTP·事务场景</th><th>OLAP·ETL场景</th></tr>
<tr><td>步骤</td><td>支付→按需审批→执行→回调→对账</td><td>UI→API→Airflow Driver DAG→N-Worker DAG→各 1 Flink Job→ODS</td></tr>
<tr><td><code>run_id</code></td><td>同笔支付固定；跨重试/重启/审批/对账</td><td>每 Worker 独立；Flink 沿用，入 ODS 供 DWD 血缘</td></tr>
<tr><td><code>trace_id</code></td><td>长事务分段恢复：<b>1-runId→N-traceId</b></td><td>连续链路覆盖多 Worker：<b>1-traceId→N-runId</b></td></tr>
<tr><td><code>span_id</code></td><td colspan="2">OTel 管理；每调用/消费/操作/重试独立</td></tr>
<tr><td><code>parent_span_id</code></td><td colspan="2">仅引用父 Span，非业务 ID</td></tr>
<tr><td>共同规则</td><td colspan="2">Run/Trace 无固定层级；重试保持 <code>run_id</code>、递增 <code>attempt_no</code>、创建新 Span。</td></tr>
</table>

## 7. 多语言客户端 SDK

```text
src/adapters/
├── config/config.(go|rs|py|ts)
├── model/{base,<domain-a>,<domain-b>}
├── <domain-a>/{<domain-a>,<ecosystem>}
├── <domain-b>/{<domain-b>,standard,<ecosystem>}
├── transport/{http,grpc,mqtt}
├── utils/
└── <facade>.(go|rs|py|ts|java)
```

- 主文件唯一状态机；Adapter 转类型；Transport 管通信/超时/Trace/错误；Facade 统一入口。

## 8. Makefile 规范

- image/E2E/release 探测可覆盖的 `IN_CN_GFW`：registry 200/401=false，否则 true；`IMAGE_SOURCE` 选 aliyun|global 并透传代理。

## 9. 单元测试

每个源码必须有同义 UT，`tests/` 镜像目录；禁内联/巨型/孤儿测试。

```text
├── src/<package>/<file>.(go|rs|py|ts)
└── tests/<package>/<file>_test.(go|rs|py|ts)
```

## 10. E2E 统一部署和 Verifier

```text
use-cases/<case-name>/e2e/
├── runner.py # 唯一外层入口
├── common/ # 配置/进程/报告/部署
├── deploy/
│   ├── <middleware>/ # kafka/postgres/redis/emqx/solana/anvil/jaeger 等
│   │   ├── docker-compose.yaml
│   │   └── helm/
│   └── mock-<external>-service[-<language>]/
│       ├── src/<module>/
│       ├── Dockerfile
│       ├── docker-compose.yaml
│       └── helm/
├── verifier/
│   ├── infra/s00_infrastructure.py
│   ├── infra/s01_structure.py
│   └── <module>/s0x_<scenario>.py
└── reports/<yyyyMMddHHmmss>/
    ├── summary.md
    └── <module>/s0x_<scenario>.md
```

- `runner.py` 统一 CLI/部署/报告/cleanup：重复/逗号 `--scenario`、`--rounds/--deployer/--timeout`、复用构建及 `cleanup/--cleanup/--cleanup-after-run`；生成时间戳报告。Verifier 仅 `verify(context)`、按领域 `sNN_`；不清理供人工验收。

## 11. Helm 模板（模块/分层）

```text
helm/<chart>/
├── {Chart.yaml,values.yaml}
├── bootstrap/{common,<provider>-secrets-setup,*.env.example}
└── templates/{_helpers.tpl,platform/,gateway/,web/,<module>/}
```

- 按模块分层；ConfigMap/Secret 独立，Deployment 用 checksum；Gateway 仅边界。
- install 前 `bootstrap/` 最小权限创建/同步 K8s/GCP/AWS/Vault Secret；失败禁安装，明文禁入 Chart/values/日志。

## 12. CI 双流水线

| CI | 触发/检出 | 行为 | 产物边界 |
|---|---|---|---|
| PR | 新建/更新/重开；head SHA；取消旧任务 | 构建 `dirty-<sha8>`；UT+E2E 复用镜像 | 仅 dirty/短期证据；禁 `latest` |
| Merged | 已合并；merge SHA；串行 | 不重复测试；构建 binary、多架构 image、Helm | 全成功才发 version/`latest`/OCI Chart并回拉 |

- 流水线 upsert 原 PR 唯一评论，覆盖全部结果及证据；marker 定位，reporter 用 `if: always()` 读取 `needs.*.result`。

## 13. 门禁

```text
结构/文件矩阵 → API/依赖方向 → format/lint → UT → 集成测试 → 配置/部署渲染 → E2E → 清理
```

- 拒绝多余源码/旧符号；写入 owner 唯一；验证乱序/幂等；代码/配置/迁移/部署/文档同步；全绿并清理。
