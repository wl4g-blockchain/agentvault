# 通用编码规范

[英文版本](AGENTS.md)

- 1.必须严格遵循以下各节规定的文件与目录结构。

- 2.库优先：实现领域功能，必须优先使用官方库或社区成熟库，仅当库缺失时才需要自研, 协议级和核心流程须直接复用库里的结构体和逻辑；确需扩展时，必须通过继承或组合/嵌入，避免重复定义同语义属性;

- 3.领域纵切、单向依赖、高内聚低耦合且收敛；能力启用有配置/契约/测试/弹性/可观测;

- 4.任何 src 下正式源码文件(`rs|go|py|java|ts`)的代码量**禁止超过 800 行**(不含注释), 否则说明你生成的代码质量不合格、必须重做精炼收敛且又合理拆分, 禁止不满足如下目录结构规范随意拆分, 每个源码文件命名必须合理规范(如果有合理拆分的某模块的内部源码文件也必须体现层级关系, 否则认为是**不合规的拆分必须删除**), **禁止以减弱功能为代价来满足规范**;

## 0. 多子模块全局结构规范

```text
<src-dir-as-(rust|java|typescript)-source-root>|<pkg-dir-as-golang-source-root>|<app-dir-as-python-source-root>/
    ├── [sdk/{rust/,golang/,python/,java/}]  # 多语言客户端 SDK 库(如何需要, 否则删除这个目录)。
    ├── common/<src|pkg|app>/{[audit-log/,error/,macro/,mem-prof,pprof/,telemetry/],...}
    ├── utils/<src|pkg|app>/
    ├── types/<src|pkg|app>/
    ├── config/<src|pkg|app>/
    └── core/<src|pkg|app>/
        ├── context/
        ├── mgmt/
        ├── cache/
        ├── storage/{postgres.(rs|go|py|ts),sqlite.(rs|go|py|ts),[mongo.(rs|go|py|ts),timescaledb.(rs|go|py|ts)]}
        ├── modules/<domain>/{route/,handler/,storage/}  # 每个源码文件代码量不应超过 500 行(不含注释), 否则说明代码质量不合格
        └── ...
    ├── <module>/<src|pkg|app>/
    ├── cmd/<src|pkg|app>/{[bin/<myapp>.(rs|go|py|ts)],cmd/{console.(rs|go|py|ts),...}}
    └── web/<src|pkg|app>/
        ├── deploy/{Dockerfile,nginx.conf}
        ├── public/
        ├── <src|pkg|app>/
        ├── tests/
        └── ...
use-cases/<case-name>/e2e/
    ├── common/{context.py,<domain>/*.py,}  # 针对当前 E2E case 的通用或 util 相关脚本 (禁止任何非 py 文件)
    ├── config/{<myapp>/{init_0x.(ddl|dml).sql,init.import.yaml,values-override.yaml},<domain>/<file>.(json|yaml|conf|toml|env|gz)}  # 不同领域的初始化/运行时配置文件
    ├── deploy/
    │   ├── deployer/{base.py,docker.py,kubernetes.py}  # 2种模式部署器(含对应 XxxCleaner 卸载清理程序)
    │   ├── <middleware>/{docker-compose.yaml,helm/}
    │   └── mock-<external>-service[-<language>]/{(src|pkg|app)/{<package>/,Dockerfile,docker-compose.yaml,helm/}}
    ├── verifier/{infra/s00_infra.py,infra/s01_structure.py,<module>/sNN_<scenario>.py,web/sNN_<scenario>.py}  # 其中 web/ 下一定是使用 playwright 真实截图测试 UI
    ├── reports/<yyyyMMddHHmmss>/{summary.md,<module>/s0x_<scenario>.md}
    ├── README.md
    ├── README_CN.md
    ├── requirements.txt  # 统一且唯一的依赖定义。
    └── runner.py  # 此 E2E 的唯一执行入口。
Makefile  # 非 Java 项目必须提供。
benches/  # Rust、Go、Python 项目必须提供。
...
```

- 0.1 依赖顺序默认从顶层依次向下;
- 0.2 类似如正则或GNU命令行风格, 即其中`<>`表示必须有; `[]`表示可选; `|`表示选其一; `,`表多个; `{}`表示指定目录层;
- 0.3 `runner.py` 统一 CLI/部署/报告/cleanup，支持多 `--scenario`、rounds/deployer/timeout、复用构建及按需清理；生成时间戳报告。Verifier 仅 `verify(context)`、按领域 `sNN_`；默认保留环境供验收;

## 1. 唯一 YAML 配置规范

- 配置 typed、分层；键 `snake_case`，env `<MYAPP>__`，敏感走 Secret。

```text
config/
└── config.(go|rs|py|ts) # 唯一入口；类型/默认/env

<App>Config { server,mgmt,logging,cache,storage,secrets,<myapp>/<domain>... }
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

## 2. 领域组件规范

- 任何独立的领域组件(即子模块)**必须**实现为统一的 Provider SPI 体系结构; 严格结构模版代码请参考如下 `<domain>/src/`

```text
src/
├── common/
│   ├── context/
│   │   ├── src/
│   │   │   ├── lib.rs                   # 公共模块/结构导出
│   │   │   └── lifecycle/
│   │   │       ├── application.rs       # ApplicationContext 统一注册、获取和生命周期管理
│   │   │       └── provider.rs          # IProvider 顶级接口定义
│   ├── audit-log/src/
│   ├── mem-prof/src/
│   ├── ...
├── <domain>/src/
│   ├── lib.rs                           # 独立领域模块/结构导出
│   ├── <domain>.rs                      # I<Domain>: IProvider 独立领域接口定义(继承)
│   └── <domain>/
│       ├── <spec_impl_name_1>.rs        # <SpecImplName1><Domain>
│       └── <spec_impl_name_2>.rs        # <SpecImplName2><Domain>
│       └── <spec_impl_name_N>/
│           └── <meaningful_name>.rs     # 可选：仅复杂内部逻辑才拆分
└── ingestor/src/                        # ↓↓↓ 示例：如下为 ingestion 领域模块
    ├── lib.rs
    ├── ingestor.rs                      # ITestIngestor: IProvider 继承定义
    └── ingestor/
        ├── web.rs                       # WebTestIngestor: ITestIngestor 领域实现
        └── twitter.rs                   # TwitterTestIngestor: ITestIngestor 领域实现
        └── twitter/
            └── twitter_client.rs        # 使用不同的第三方库连接 Twitter 的内部逻辑
```

- 使用示例

```rust
let ctx = ApplicationContext::global(); // 所有领域模块共享同一进程级上下文
ctx.register::<dyn ITestIngestor, _>("web-default", web_ingestor_instantiator)?;
ctx.register::<dyn ITestIngestor, _>("twitter-default", twitter_ingestor_instantiator)?;
ctx.init().await?; // 启动上下文，实例仍在首次 get 时初始化
let web = ctx.get::<dyn ITestIngestor>("web-default").await?;
let twitter = ctx.get::<dyn ITestIngestor>("twitter-default").await?;
let page = web.ingest("https://example.test/news/42", "Market update");
let tweet = twitter.ingest("tweet-42", "Latest update");

// 先停止业务接入，并等待相关任务结束。
ctx.close_for::<dyn ITestIngestor>("web-default").await?; // 可选：单独关闭一个实例
ctx.shutdown().await?; // 整体停机，关闭其余所有注册实例
```

- 上述数据摄取示例使用 `ITestIngestor`、`WebTestIngestor` 和 `TwitterTestIngestor`。领域接口必须继承 `IProvider`；注册的异步构造器返回 `Result<Arc<dyn ITestIngestor>>`。
- 各领域模块通过 `ApplicationContext::global()` 获取相同上下文，由应用入口统一负责注册、`init` 和 `shutdown`。`global()` 不会启动上下文，也不会在停机后重新创建上下文。`ApplicationContext::new()` 用于隔离测试等明确需要独立上下文的场景。
- 注册表按领域接口类型和 `instance_id` 两层索引，不额外传入实现名, 同一领域的不同实现使用不同实例 ID;
- 同一对象暴露多个领域接口时，使用 `register_with::<Target, Source>(instance_id, projection)` 共享生命周期;

- **重要： 如上模版示例针对 Go/Rust/Python 任何主源码文件里，都禁止直接定义函数，而必须先定义合理的结构体再在结构体里面定义函数;**

## 3. 存储与仓储规范

```text
storage/
├── storage.(go|rs|py|ts)      # IGenericRepository 通用基础 CRUD 操作接口函数定义、契约/工厂/共享 SQL 拼接
├── postgres.(go|rs|py|ts)     # PostgresRepository impl IGenericRepository: 适配 Postgres 的通用基础 CRUD 实现，供业务子类重写/组合
├── sqlite.(go|rs|py|ts)       # 同上
└── <domain>/                  # 具体领域模块
    ├── postgres.(go|rs|py|ts) # <Domain>PostgresRepository impl PostgresRepository 基类, 必须优先使用基类 CRUD 函数, 且不应超过 250 行
    └── sqlite.(go|rs|py|ts)   # 同上
```

- `IGenericRepository` 里必须有 CRUD 通用函数(反射/宏动态生成函数)：`select(entity,page)/select_by(id)/insert(entity)/update(entity)/upsert(entity)/delete_by(id)`;

## 4. 缓存规范

```text
cache/
├── cache.(go|rs|py|ts)        # ICache 通用缓存操作接口函数定义、契约/工厂, 包括不限于如 get/set/delete/exists/clear/close
├── redis.(go|rs|py|ts)        # RedisCache 适配 Redis signle/cluster 的通用 cache 操作实现类
└── memory.(go|rs|py|ts)       # 同上
```

## 5. 作业与调度

- `job/<domain>.(go|rs|py|ts)` 仅负责 cron/并发/锁/生命周期/观测并调用组件；未获锁跳过，非法 cron 启动失败；处理、幂等、重试归组件。

## 6. 可观测性与 OpenLineage 规范

- `run_id` 是通用唯一可公开幂等键，可以进日志或业务表、Span、OpenLineage Facet；OTel 禁用于约束/恢复。
- 禁止另建独立关联标识。HTTP/gRPC 自配对；异步 wire 的 `requestSpanId` 必须等于 W3C `traceparent` 中的 producer `span_id`，响应原样回显并创建 consumer Span；不得持久化或参与业务约束，仅可作为 MQ QoS 重投递的短生命周期回复缓存键。重试新建 Span，`run_id` 不变、`attempt_no` 递增。
- 服务调用传播 W3C Trace Context；关键转移定义低基数 counter/histogram/gauge，记录结果/耗时/错误。

| 字段/场景 | OLTP·事务场景 | OLAP·ETL场景 |
|---|---|---|
| 步骤 | 支付→按需审批→执行→回调→对账 | UI→API→Airflow Driver DAG→N-Worker DAG→各 1 Flink Job→ODS |
| `run_id` | 同笔支付固定；跨重试/重启/审批/对账 | 每 Worker 独立；Flink 沿用，入 ODS 供 DWD 血缘 |
| `trace_id` | 长事务分段恢复：**1-runId→N-traceId** | 连续链路覆盖多 Worker：**1-traceId→N-runId** |
| `span_id` | OTel 管理；每调用/消费/操作/重试独立 | 同左 |
| `parent_span_id` | 仅引用父 Span，非业务 ID | 同左 |
| 共同规则 | Run/Trace 无固定层级；重试保持 `run_id`、递增 `attempt_no`、创建新 Span | 同左 |

## 7. 多语言客户端 SDK (如果需要, 否则删除)

```text
src/sdk/
├── config/config.(go|rs|py|ts)
├── model/{base,<domain-a>,<domain-b>}
├── <domain-a>/{<domain-a>,[ecosystem]}
├── <domain-b>/{<domain-b>,[standard],[ecosystem]}
├── transport/{http,grpc,mqtt}
├── utils/
└── <facade>.(go|rs|py|ts|java)
```

- 主文件唯一状态机；Adapter 转类型；Transport 管通信/超时/Trace/错误；Facade 统一入口。

## 8. Makefile 规范

- image/E2E/release 探测可覆盖的 `IN_CN_GFW`：registry 200/401=false，否则 true；`IMAGE_SOURCE` 选 aliyun|global 并透传代理。

## 9. 单元测试规范

- 源码须有同义 UT，`tests/` 镜像目录；禁内联/巨型/孤儿测试。

```text
├── src/<package>/<file>.(go|rs|py|ts)
└── tests/<package>/<file>_test.(go|rs|py|ts)
```

## 10. 模块化、分层 Helm Chart 规范

```text
helm/<chart>/
├── {Chart.yaml,values.yaml}
├── bootstrap/{common,setup-secret-<provider>.sh,*.env.example}
└── templates/{_helpers.tpl,platform/,gateway/,web/,<module>/}
```

- 按模块分层；ConfigMap/Secret 独立，Deployment 用 checksum；Gateway 仅边界。
- install 前 `bootstrap/` 最小权限创建/同步 K8s/GCP/AWS/Vault Secret；失败禁安装，明文禁入 Chart/values/日志。

## 11. CI 流水线规范

| CI | 触发/检出 | 行为 | 产物边界 |
|---|---|---|---|
| PR | 新建/更新/重开；head SHA；取消旧任务 | 构建 `dirty-<sha8>`；UT+E2E 复用镜像 | 仅 dirty/短期证据；禁 `latest` |
| Merged | 已合并；merge SHA；串行 | 不重复测试；构建 binary、多架构 image、Helm | 全成功才发 version/`latest`/OCI Chart并回拉 |

- 流水线 upsert 原 PR 唯一评论，覆盖全部结果及证据；marker 定位，reporter 用 `if: always()` 读取 `needs.*.result`。

## 12. 质量门禁规范

```text
结构/文件矩阵 → API/依赖方向 → format/lint → UT → 集成测试 → 配置/部署渲染 → E2E → 清理
```

- 拒绝多余源码/旧符号；写入 owner 唯一；验证乱序/幂等；代码/配置/迁移/部署/文档同步；全绿并清理。
