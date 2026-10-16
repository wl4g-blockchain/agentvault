# 通用编码规范

- 审计相邻项目；领域纵切、单向依赖、能力唯一、文件一责；删旧/空/重复，`utils`仅纯函数。

- MQ/gRPC/缓存/存储/OTel按需；启用即有契约、配置、测试、观测。

## 1. 统一 YAML 规范

配置 typed、分层、校验；键 `snake_case`，env `<MYAPP>__`，敏感走 Secret。

```text
config/
├── config.(go|rs|py|ts)    # 加载；根/嵌套类型
└── constants.(go|rs|py|ts) # 默认/env 前缀

<App>Config { server,secrets,mgmt,logging,cache,storage,<myapp>/<domain>... }
```

- `config/config.xx` 唯一加载；`<Section>Config`/`Properties` 与 YAML 同名同层。字段“基础→领域→业务”同 YAML 序；默认值 < YAML < `APP__...`，禁未知项。

```yaml
server: { host: 0.0.0.0, port: 8080 }
mgmt:
  enabled: true
  host: 127.0.0.1
  port: 9091
  context_path: /
  health: { liveness_path: /healthz, readiness_path: /readyz }
  metrics: { enabled: true, path: /metrics }
  otel: { enabled: false, endpoint: "", protocol: grpc, timeout: 5s, sample_rate: 1.0 }
logging: { mode: HUMAN, level: info } # HUMAN|JSON
storage:
  provider: Postgres # Postgres|SQLite
  postgres:
    url: "${MYAPP__STORAGE__POSTGRES__URL:postgres://db/app}"
    username: "${MYAPP__STORAGE__POSTGRES__USERNAME:postgres}"
    password: "${MYAPP__STORAGE__POSTGRES__PASSWORD:}"
    pool: { max_connections: 20, acquire_timeout: 5s }
  sqlite:
    url: "${MYAPP__STORAGE__SQLITE__URL:sqlite://app.db}"
    pool: { max_connections: 1, acquire_timeout: 5s }
cache:
  provider: Memory # Redis|Memory
  redis:
    url: "${MYAPP__CACHE__REDIS__URL:redis://cache}"
    username: "${MYAPP__CACHE__REDIS__USERNAME:default}"
    password: "${MYAPP__CACHE__REDIS__PASSWORD:}"
    pool: { max_connections: 20, acquire_timeout: 3s }
  memory:
    max_capacity: 65535
    default_ttl: 1h
    cleanup_interval: 30s
<myapp>: ...
```

- Mgmt 私网，仅 liveness/readiness/metrics（进程、有界依赖、OpenMetrics），禁写。
- Metrics 低基数 counter/histogram/gauge 覆盖关键转移、结果、耗时、失败。
- HTTP/gRPC/MQ 注入/提取 OTel `traceId/spanId/parentId`：入口根、内部/出站子 Span，记录状态/错误。
- Logging 校验 mode/level，附 time/level/service/component/trace/correlation，脱敏。

## 2. 独立领域 Component

- 组件用 Provider+Factory（类似 SPI）：契约唯一、实现可替换，禁直建；UT 覆盖选择/失败/幂等/生命周期。

```text
[type|class] I<Component> { provider(), init(), close(), <domain_operation>() }

[type|class] <Component>Factory {
  registry: Map<Provider, I<Component>>
  get(provider) -> I<Component> { // get_or_create
    return registry.add_if_absent(provider, resolve(provider).create(Config.get().get) or fail) // create component on-demand
  }
  close() { // destroy all registered }
}
```

## 3. Storage / Repository

```text
storage/
├── storage.(go|rs|py|ts) # 契约、工厂、共享 SQL
├── postgres.(go|rs|py|ts) # PostgreSQL 通用 CRUD，供业务组合
├── sqlite.(go|rs|py|ts) # SQLite 通用 CRUD，供业务组合
└── <domain>.(go|rs|py|ts) # 复杂事务（按需）
```

- CRUD 仅预编译 SQL/占位符/受控字段：`select/select_by/insert/update/upsert/delete_by`，禁拼接。Java：`repository/XxxRepository`、`Postgres/SQLiteXxxRepository`；模型/迁移对齐。

## 4. Cache

```text
cache/
├── cache.(go|rs|py|ts) # ICache 接口、工厂、统一语义
├── memory.(go|rs|py|ts)
└── redis.(go|rs|py|ts)
```

- `ICache`：`get/set/delete/exists/clear/close`；Memory/Redis 语义等价。

## 5. Job（调度领域）

- `job/<domain>.(go|rs|py|ts)` 仅做 cron/并发/锁/生命周期/观测并调用组件，禁领域逻辑；未获锁跳过，cron 无效失败/告警；处理/幂等/重试归组件。

## 6. 多语言客户端 SDK

```text
src/
└── adapters/
    ├── config/config.(go|rs|py|ts)
    ├── model/{base,<domain-a>,<domain-b>}
    ├── <domain-a>/{<domain-a>,<ecosystem>}
    ├── <domain-b>/{<domain-b>,standard,<ecosystem>}
    ├── transport/{http,grpc,mqtt}
    ├── utils/
    └── <facade>.(go|rs|py|ts)
```

主文件唯一状态机；Adapter 转类型；Transport 通信/超时/关联 ID/错误；胶水仅导出。

## 7. Makefile 规范

- image/E2E/release 探测 `IN_CN_GFW`（可覆盖）：registry 200/401=false，否则 true；`IMAGE_SOURCE` 选 aliyun|global（Aliyun/docker.io/ghcr）并传代理。

## 8. 单元测试

- 每个主源码须有同义 UT，`tests/` 路径镜像源码：

```text
├── src/<package>/<file>.(go|rs|py|ts)
└── tests/<package>/<file>_test.(go|rs|py|ts)
```

- 禁内联/巨型 UT；源码变更同步，拒绝孤儿。

## 9. E2E 统一部署和 Verifier

```text
use-cases/<case-name>/e2e/
├── runner.py # 唯一外层 Python 入口
├── common/ # 配置、进程、报告
├── deploy/
│   ├── <middleware>/ # kafka/postgres/pgvector/redis/emqx/solana/anvil/jaeger 等
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
└── reports/
    └── <yyyyMMddHHmmss>/
        ├── summary.md
        └── <module>/s0x_<scenario>.md
```

- `runner.py` 统一 CLI/部署/报告/cleanup：重复/逗号 `--scenario`、`--rounds/--deployer/--timeout`、复用构建及 `cleanup/--cleanup/--cleanup-after-run`；生成时间戳报告。Verifier 仅 `verify(context)`、按领域 `sNN_`；不清理供人工验收。

## 10. Helm 模板（模块/分层）

```text
helm/<chart>/{Chart.yaml,values.yaml}
├── bootstrap/{common,<provider>-secrets-setup,*.env.example}
└── templates/{_helpers.tpl,platform/{service-account,configmap,secret}.yaml,
    gateway/{gateway,route,security-policy,configmap}.yaml,
    web/{deployment,service,route,configmap,secret,network-policy}.yaml,
    <module>/{deployment,service,route,configmap,secret,network-policy,service-monitor}.yaml}
```

按模块/层分目录，禁平铺；ConfigMap/Secret独立，Deployment用checksum；Gateway仅边界/策略，web/模块自有资源。

Helm 前 `bootstrap/`：校验输入，最小权限创建/同步 K8s/GCP/AWS/Vault Secret，仅写非敏感 values 引用；失败禁install，明文禁入Chart/values/日志。

## 11. 门禁

```text
结构/文件矩阵 → API/依赖方向 → format/lint → 单测 → 集成测试 → 配置/部署渲染 → E2E → 清理
```

- 拒绝多余源码/旧符号；数据写入owner唯一；消息处理重复/延迟/乱序/幂等；代码/配置/迁移/部署/文档同步；全绿且清理生成物才完成。
