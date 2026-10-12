# 架构说明

AgentVault 采用端口与适配器边界，同时保持单一可部署二进制。领域模型与服务
不依赖 Axum、Unix socket 或 MQTT。

```text
src/
├── model/           稳定的钱包与支付签名契约
├── service/         钱包 CRUD 与签名用例
├── signer/          面向外部调用方的 IPaymentSigner 适配器
├── api/             HTTP 产品 API
├── transport/       入站 Unix socket 与 MQTT 适配器
├── management.rs    健康检查与 Prometheus 管理面
├── observability/   有界指标与 OTLP trace
├── server/          组件生命周期和优雅退出
├── config/config.rs 唯一强类型配置层次
├── history.rs       持久化、有界、脱敏的审计元数据
└── store.rs         加密私钥存储
```

## 依赖方向

`model/sign.rs` 中的 `PaymentSignRequest` 是公共契约。入站适配器完成解码后只
调用 `PaymentSignService`；服务依次校验协议、授权策略、TTL、request ID 与
digest 长度，再访问加密 store。随后写入脱敏审计记录及低基数 metrics。
任何传输实现都不会接触明文私钥。

`IPaymentSigner` 是提供给 Flowgent/Sigbot 的客户端端口。
`LocalPaymentSigner` 与 `MqttPaymentSigner` 实现相同的 `execute(request)`，调用
方切换部署拓扑时无需修改支付流程代码。

## 运行时生命周期

`AgentVaultServer` 只创建一次 store、history、service 与可观测性组件，再把
启用的 HTTP API、management、local、MQTT 组件注册给 `ServerSupervisor`。
Supervisor 统一监听进程信号、广播退出、按 `server.shutdown_timeout` 等待，并
上报首个组件错误；子组件不会各自安装冲突的 signal handler。

产品 API 和管理 API 使用不同 listener，管理路由不会并入产品路由。

## 持久化与并发

- Store 持有进程独占锁，并串行化密钥变更。
- 每把私钥独立认证加密，并把公开元数据绑定为 AAD。
- 钱包轮换通过完整记录替换实现原子更新。
- request ID 在有限时间内缓存；相同重投返回缓存响应，不同数据复用同一 ID
  会被拒绝。
- 审计历史通常以 JSON Lines 追加，超过阈值后压缩；永不保存 digest 或签名。
