# 协议样本（fixture）

这些不是手写的期望值，而是 **2026-11 从真实 host 抓下来的响应**，用于
`dsh-protocol` 的反序列化测试与 wire 形态回归。

## 采集条件

| 项 | 值 |
| --- | --- |
| dsh 版本 | `@deepseek-ai/dsh@0.2.2-rc.3`（见 DECISIONS #18） |
| 启动 | `--profile dsh-client-rs --from-default-profile web --no-open --port 0` |
| `DSH_HOME` | 隔离的空目录（未复用用户环境） |
| 凭据 | 仅 `DEEPSEEK_API_KEY` 环境变量兜底（故 catalog 非空） |
| 认证 | `GET /?token=…` 换到的 cookie（浏览器路径，payload version 1） |

## 各文件

| 文件 | 端点 | 内容要点 |
| --- | --- | --- |
| `model-catalog.json` | `session/modelCatalog` | `default`、`routableProviders`、`groups[].models[].reasoning.efforts[]`、`failures`（本样本为空数组） |
| `permission-catalog.json` | `permissionPresets/catalog` | 三个预设 `read-only` / `workspace-write` / `danger-full-access`，`defaultPreset` = `workspace-write` |
| `session-list.json` | `session/list` | 空 `items`（新 `$DSH_HOME` 没有会话） |
| `error-arguments-invalid.json` | `session/events` | 故意漏参，记录失败 envelope 的形状 |
| `follow-stream.jsonl` | WS `session/follow` | 一次真实 prompt 的 13 行帧：1 个 snapshot（records 已裁成 `{type,seq}`）、9 个 `assistant-stream`、3 个代表性 event |

## 采集方式

```bash
URL=$(grep -o 'http://127.0.0.1:[0-9]*/?token=[^ ]*' host.log | head -1)
BASE=${URL%%/\?*}
curl -s -c jar.txt -o /dev/null "$URL"
curl -s -b jar.txt -H 'Content-Type: application/json' \
  -d '{"type":"client-request","rpcId":"fx","method":"session/list","payload":{"args":{"_request":{}}}}' \
  "$BASE/api/session/list"
```

## 注意

- 样本里**没有**会话数据，也没有任何凭据。重新采集时别把带 token 的 URL 写进文件。
- `follow-stream.jsonl` 是走 WebSocket 抓的：`GET /connect?token=…&interactions=answer`
  换 v2 cookie，连 `ws://<host>:<port>/api/remote.mux`，发 `{"type":"open",…}`。
  它消耗了一次真实模型调用（prompt 是「只回复两个字：收到」）。
