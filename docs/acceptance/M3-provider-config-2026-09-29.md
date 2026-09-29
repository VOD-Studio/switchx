# M3 供应商 Codex 配置验收

2026-09-29，macOS arm64、Slint 1.18.1，本机调试 bundle。原生检查使用 `routed_cli_probe --desktop` 创建的两个本地假上游、合成凭据，以及临时绝对路径的 `SWITCHX_DATA_DIR` 和 `CODEX_HOME`。目录检查使用本地 Codex CLI `0.158.0-alpha.2.1`。

本轮覆盖供应商 TOML 预览、远程压缩选项、通用配置、1M 上下文与压缩阈值，以及直连和路由的配置写入、恢复。没有真实上游模型或 compact 请求，也没有真实订阅账号验收。

## 检查结果

| 检查 | 证据与结果 |
| --- | --- |
| 自动检查 | `make check` 通过：80 个库测试、2 个主程序测试、7 个路由集成测试，共 89 项；1 项已有的仅 CLI 目录测试保持 ignored。Rust/Slint 格式检查与 Clippy 通过 |
| 原生构建 | `sh scripts/bundle-macos.sh` 成功生成本机调试 bundle |
| 供应商预览 | 新建、编辑和切换预设时，TOML 预览实时更新且只读；启用远程压缩后 provider 名称为 `OpenAI` |
| 上下文与阈值 | 开启 1M 写出 `model_context_window = 1000000`；设置阈值 `850000` 后预览正确，`0` 被拒绝 |
| 通用配置提取与取消 | 从当前配置提取只更新草稿；取消后重新打开，已保存片段仍为空 |
| 通用配置验证与保存 | 无效 TOML 显示错误并保留草稿；保存 `model_reasoning_effort = "high"` 和 `[features]` 中的 `memories = true` 后，供应商预览合并这些字段；关闭“应用通用配置”后预览不再包含它们 |
| 选项持久化 | 保存供应商并重新打开编辑器，远程压缩、通用配置开关、1M 开关与 `850000` 阈值均完整保留 |
| 直连写入 | 原生应用直连后，临时 `config.toml` 包含预览的上下文、阈值与通用字段，provider 名称为 `OpenAI`，没有 API Key 明文 |
| 直连恢复 | 恢复后配置与原文件逐字节一致，`direct-journal.json` 清除 |
| 路由预览与写入 | 默认 Alpha 模型的原生发布预览经本地 CLI 核对两个模型目录；开启路由后写入同样的上下文、阈值与通用字段，provider 名称为 `SwitchX Router`，API 路由不启用远程压缩 |
| 路由恢复与退出 | 恢复后配置与原文件逐字节一致，切换 journal 清除，loopback 端口 `18731` 释放；夹具正常退出并清理合成凭据与临时目录 |
| 通用配置操作区 | 初版按钮曾超出可视区域；改为固定底部操作区后，原生确认提取、取消、保存按钮均可见并可操作 |

直连与路由检查证明本次选项能经生产配置事务写入和恢复；目录检查证明该本地 CLI 能加载两个合成模型。远程压缩选项写出 `name = "OpenAI"`，仍须由真实 API 直连上游支持 `/responses/compact`。API 路由保留服务器状态隔离，此选项不会启用 API 路由的远程压缩。

## 复跑

```sh
make check
sh scripts/bundle-macos.sh
cargo run --locked --example routed_cli_probe -- --desktop
```

原生夹具仅使用合成凭据和隔离目录。对真实上游的模型请求、压缩、订阅认证生命周期，以及 Desktop/IDE 与其他平台的兼容性，仍需另行验收。
