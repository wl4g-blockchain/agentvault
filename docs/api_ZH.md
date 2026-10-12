# HTTP 与签名接口

默认产品 API 前缀为 `/api/v1`。配置 `server.api_token_file` 后，每个产品请求
必须携带 `Authorization: Bearer <token>`。管理端点使用独立 listener，不属于
该产品 API 前缀。

## 钱包

`POST /wallet` 请求：

```json
{"id":"payer"}
```

响应 `201` 与公开元数据。`GET /wallet` 返回
`{"items":[...],"total":1}`；`GET /wallet/{id}` 返回单个钱包；对单个钱包执行
`PUT` 会原子生成并写入新私钥，因此地址随之变化；`DELETE` 返回 `204`。

私钥导入刻意不开放 HTTP，请使用 owner-only 环境中的离线 CLI。

## 支付签名

`POST /sign`、Local 和 MQTT 适配器完全共用以下模型：

```json
{
  "version": "wallet.sign.v1",
  "request_id": "b8fc3156-5a54-41ca-934f-95a49fac3a8b",
  "client_id": "flowgent-agent-01",
  "wallet_id": "payer",
  "purpose": "x402.payment",
  "scheme": "eip712-secp256k1",
  "digest_b64": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
  "issued_at": 1791772800,
  "expires_at": 1791772830,
  "metadata": {"payment_id": "pay-001"}
}
```

成功响应包含地址、十六进制签名与签名时间。协议/策略失败仍返回同一 envelope，
但只有 `error.code`、`error.message`，不含签名，确保三种传输行为一致。

Client policy 使用最长前缀匹配：最具体的前缀必须同时授权 wallet 与 purpose，
不能回退到更宽泛的规则；请求有效期还必须满足 TTL 与时钟偏差限制。

## 历史搜索

`GET /sign` 支持精确过滤：

- `request_id`、`client_id`、`wallet_id`、`purpose`、`outcome`
- 完成时间 `from`、`to`（Unix 秒）
- `offset`、`limit`（默认 100，最大 500）

`outcome` 可为 `succeeded`、`invalid_request`、`expired`、`conflict`、
`wallet_not_found`、`failed`。结果按最新优先排序。幂等重投或冲突请求可能使同一
request ID 产生多条事件；`GET /sign/{request_id}` 返回最新一条。

## Rust 客户端端口

Agent 业务代码应依赖 `Arc<dyn IPaymentSigner>` 或对应泛型，只调用：

```rust,ignore
let response = signer.execute(request).await?;
```

同机部署使用 `LocalPaymentSigner`，broker 部署使用 `MqttPaymentSigner`；业务层
不应按传输实现分支。
