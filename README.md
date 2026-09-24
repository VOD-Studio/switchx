# switchx

Rust + Slint 原生桌面应用，按 `docs/SWITCHX-PLAN.md` 的 M0 闸门逐步实现。

当前有应用骨架、**合成目录夹具**、独立的 loopback 路由模块、SQLite 上游元数据存储和系统凭据存储适配。界面尚未启动路由；`catalog_probe` 不读取账号、凭据或现有 Codex 配置，也不会写入 `CODEX_HOME`。夹具的能力字段只用于验证目录 schema，不能作为真实模型能力或发布模板。SQLite 只存凭据引用；界面与路由尚未接入实际凭据。

```sh
cargo run
cargo test
cargo run --example catalog_probe > /tmp/switchx-models.json
CODEX_HOME="$(mktemp -d)" npx -y @openai/codex@0.156.1 \
  -c 'model_catalog_json="/tmp/switchx-models.json"' debug models
```

在 2026-09-24 的 macOS 本机测试中，Codex CLI 0.156.1 的 `debug models` 与 app-server `model/list` 均返回 `sx-ds-flash`、`sx-oai-coding`。这证明该版本能读取夹具目录，尚未证明真实 `/model` 交互、真实模型请求、Desktop 或 IDE 兼容。

`cargo test --test router` 用两个本地假上游验证精确映射、SSE 首事件直达、认证头隔离、未知模型和本地鉴权。它不使用真实供应商凭据，不证明实际工具对话、故障切换或 ChatGPT 订阅认证。

macOS 可运行 `sh scripts/bundle-macos.sh` 生成仅供本机交互检查的 `target/debug/SwitchX.app`。主窗口关闭后应驻留菜单栏，可从菜单栏重新打开或退出。此调试 bundle 未签名、未公证，不能作为发布包。

`cargo run --example keychain_probe` 在系统凭据存储中写入一次独立的合成测试条目，读取后立即删除；不读取现有账号数据。macOS 本机测试已通过。
