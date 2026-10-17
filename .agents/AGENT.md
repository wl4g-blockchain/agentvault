# 通用编码规范

- 审计相邻项目；定目录/入口/依赖/删除清单。领域纵切、依赖单向；公共能力唯一、接口仅替换点、文件一责；删旧/空/重复，`utils` 仅纯函数。

- 除明确必需项外，目录树、MQ/gRPC、缓存、存储、OTel 等常见能力仅在真实需求时存在；一旦启用，必须完整遵循对应契约、配置、测试、可观测性，禁止占位实现。

## 2. Storage / Repository

```text
storage/
├── storage.(go|rs|py|ts) # 抽象基类契约、工厂、共享 SQL
├── postgres.(go|rs|py|ts) # PostgreSQL 方言、通用(动态/宏)实现 CRUD，供业务模块继承/组合
├── sqlite.(go|rs|py|ts) # SQLite 方言、通用动态 CRUD，供业务模块继承/组合
└── <domain>.(go|rs|py|ts) # 复杂事务（按需）
```

CRUD 仅预编译 SQL/占位符/受控字段：`select(page,query)`、`select_by(id)`、`insert(entity)`、`update(entity)`、`upsert(entity)`、`delete_by(id)`；禁直拼。Java：`repository/XxxRepository`、`Postgres/SQLiteXxxRepository`；模型/迁移对齐。

## 3. Cache

```text
cache/
├── cache.(go|rs|py|ts) # ICache 接口、工厂、统一语义
├── memory.(go|rs|py|ts)
└── redis.(go|rs|py|ts)
```

`ICache`：`get/set/delete/exists/clear/close`；Cache 非权威，Memory/Redis 语义等价。

## 4. 统一 YAML 配置

配置 typed、分层、启动校验；键 `snake_case`，环境变量用应用前缀双下划线映射，敏感值走 Secret。

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

Config 镜像 YAML 校验。Mgmt 私网，仅 liveness/readiness/metrics（进程/有界依赖/Prometheus,OpenMetrics），禁写。Logging 校验 `HUMAN|JSON`/level，事件带 time/level/service/component/trace/correlation，脱敏 secret/token/credential/payload。关键流转定义低基数自有 counter/histogram/gauge，覆盖转移/结果/耗时/失败。HTTP/gRPC/MQ 注入/提取 OTel `traceId/spanId/parentId`：入口根 Span、内部/出站子 Span，记录状态/错误；异步旁路采样不阻塞。

## 5. 多语言 SDK

```text
src/
└── adapters/
    ├── config/config.(go|rs|py|ts)
    ├── model/{base,<domain-N>...}
    ├── <domain-N>/{<domain-N>,<ecosystem>}
    ├── transport/{http/,grpc/,mqtt/}
    ├── utils/
    └── <facade>.(go|rs|py|ts)
```

主文件唯一状态机；Adapter 转类型；Transport 通信/超时/关联 ID/错误；胶水仅导出。

## 6. 单元测试镜像

四语言每个主源码对应同义 UT，`tests/` 路径镜像源码：

```text
├── src/<package>/<file>.(go|rs|py|ts)
└── tests/<package>/<file>_test.(go|rs|py|ts)
```

禁内联/巨型 UT；源码变更同步，拒绝孤儿。

## 7. E2E 入口、部署和 Verifier

```text
use-cases/<case-name>/e2e/
├── runner.py # 唯一外层 Python 入口
├── common/ # 配置、模型、进程、报告、部署抽象
├── deploy/
│   ├── <middleware>/ # kafka/postgres/pgvector/redis/emqx/solana/anvil/jaeger 等
│   │   ├── docker-compose.yaml
│   │   └── helm/ # 该中间件的标准 Chart
│   └── mock-<external>-service[-<language>]/ # 强依赖外部服务或 SDK 测试变体
│       ├── src/<module>/
│       ├── Dockerfile
│       ├── docker-compose.yaml
│       └── helm/ # Mock 服务标准 Chart
├── verifier/
│   ├── infra/s00_infrastructure.py
│   ├── infra/s01_structure.py
│   └── <module>/s0x_<scenario>.py
└── reports/
    └── <yyyyMMddHHmmss>/
        ├── summary.md
        └── <module>/s0x_<scenario>.md
```

`runner.py` 唯一控制 CLI/场景/轮次/部署/报告/cleanup；支持 `--scenario`（重复/逗号）、`--rounds`、`--deployer`、`--timeout`、构建复用、`cleanup/--cleanup/--cleanup-after-run`，可列场景。它生成 ID、时间戳报告和汇总；Verifier 仅经注入上下文写结果。未开 `--cleanup-after-run` 保留全部部署供人工验收/诊断，显式 cleanup 回收。Verifier 按领域分包、`sNN_<snake_case>.py`；`s00` 基础设施，场景仅 `verify(context)`/Verifier。

## 8. Makefile 镜像源

仅 image/E2E/release 探测 `IN_CN_GFW`：registry 200/401=false，否则 true，可覆盖。校验 `IMAGE_SOURCE=aliyun|global`，选 Aliyun mirror 或 docker.io/ghcr；`image-source` 输出，代理传给构建。

## 9. 门禁

```text
结构/文件矩阵 → API/依赖方向 → format/lint → 单测
→ 集成测试 → 配置/部署渲染 → E2E → 清理
```

拒绝多余源码、旧符号；数据写入 owner 唯一；消息处理重复/延迟/乱序/幂等；代码/配置/迁移/部署/文档同步。门禁全绿、清理生成物才完成。
