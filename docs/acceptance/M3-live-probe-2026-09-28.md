# M3 真实路由探针准备验收

## 范围

2026-09-28，macOS arm64、本机 Codex CLI `0.158.0-alpha.2.1`。本批新增 `routed_live_probe`，真实请求需要明确指定已保存的公开模型 ID。下面的运行证据使用现有探针提供的两个本地假上游、合成钥匙串条目及临时目录，没有真实供应商调用或计费。

## 实现

- 只读打开当前 v5 数据库；不会创建源数据库、升级旧库或写入原记录。原有发布选择与备用设置保留。
- 明确选择一或两个公开模型，仅在临时副本中启用它们并清除备用。复用原模型能力资料、凭据引用和生产 `RouteSession`，不重新保存或删除原有上游 Key。
- 与 `direct_live_probe` 共用真实 CLI 短回答、文件工具和精确第二轮回答检查。使用同一本机 CLI 选择规则，支持 `SWITCHX_CODEX_CLI`。
- 每次 CLI 检查后核对新请求记录；短回答至少一条、文件工具轮次至少两条正常完成记录，去向、公开 ID、实际模型、目录版本和计时必须匹配。
- 成功、请求检查失败、超时或 Ctrl-C 后走同一恢复流程。Ctrl-C 等待配置事务完成再取消请求；恢复失败保留目录并退出失败。强杀不保证清理。

## 实测结果

| 检查 | 结果 |
| --- | --- |
| 两个公开 ID | 两个上游使用同一实际模型 `shared-model`；每个公开 ID 都完成精确回答、读取随机文件、工具结果与第二轮回答 |
| 实际请求记录 | 每个模型短回答一条、工具轮次两条，共六条正常完成记录；映射、HTTP 200、目录版本和首事件计时匹配 |
| 凭据隔离 | 两个假上游分别验证自己的合成 Bearer Key，并拒绝本地令牌头与客户端账号头 |
| 原发布/备用设置 | 源模型均未发布，其中 Alpha 保存了备用引用；临时运行选择两者、清除备用，源数据库运行后逐字节保持不变 |
| 错误回答 | 假上游返回完成事件和错误答案，探针报告失败；配置、journal、本地令牌、端口和目录均完成清理 |
| Ctrl-C | 单模型请求在上游等待期间发出 SIGINT；探针报告中断，并完成相同的恢复与清理 |
| 原凭据保留 | 成功、失败和中断运行后，源上游凭据仍可读取且内容一致；最后由原假上游夹具删除其合成凭据 |
| 只读保护 | 自动化验证源库禁止增改删、不创建不存在的数据库、不迁移旧版本，读取后的数据库字节一致 |

全目标自动化共 53 项通过；Clippy、Rust/Slint 格式检查与差异检查通过。运行后未残留本批路由探针的临时目录。

## 复跑

```sh
cargo build --locked --bin switchx --example routed_live_probe
cargo run --locked --example routed_cli_probe -- --live-probe
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
make format-check
```

真实运行入口：

```sh
cargo run --locked --example routed_live_probe -- /absolute/switchx-data PUBLIC_MODEL_ID
cargo run --locked --example routed_live_probe -- /absolute/switchx-data FIRST_PUBLIC_ID SECOND_PUBLIC_ID
```

## 未验收

真实 DeepSeek 调用等待选定的数据目录与公开模型 ID。官方 API、ChatGPT 订阅路径、同会话跨上游工具历史和 Desktop/IDE 兼容均没有新增实测证据，M3 仍是 API 路由预览版。
