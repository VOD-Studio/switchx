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

原生界面已提供深浅主题、侧栏导航和状态抽屉。未接入的页面明确显示开发中；主题和导航状态目前只保存在本次窗口实例中。

`cargo run --example keychain_probe` 在系统凭据存储中写入一次独立的合成测试条目，读取后立即删除；不读取现有账号数据。macOS 本机测试已通过。

`cargo run --example config_probe -- /tmp/switchx-models.json` 只把合成 `config.toml` 差异预览输出到 stdout。预览保留其他 provider、MCP、项目、安全设置和注释；它不写用户配置。生成的 `env_key = "SWITCHX_LOCAL_TOKEN"` 仅在启动 Codex 的环境已提供本地令牌时才可用于请求。

`cargo run --example codex_cli_probe` 会通过 npm 执行 Codex CLI 0.156.1，在独立临时 `CODEX_HOME` 中启动同一个 SwitchX 路由入口和两个本地假上游。实测两个别名分别到达对应假上游，且 `sx-ds-flash` 完成一次读取临时文件、回传工具结果、第二轮回答。运行结束会删除该临时目录；无真实 API Key、登录态或模型调用。

`cargo run --example deepseek_live_probe` 会在终端隐藏输入地读取测试 Key，调用 DeepSeek 模型发现，并通过 SwitchX 路由发送一次真实 Responses 请求，再让隔离的 Codex CLI 0.156.1 发送一次真实请求；这两次模型调用可能计费。Key 只保存在测试进程内存中，临时 `CODEX_HOME` 结束后删除。2026-09-24 实测：模型发现返回 `deepseek-flash`、`deepseek-v4-pro`；路由请求返回 HTTP 200、`completed` 和 `SWITCHX_OK`；Codex CLI 返回 `SWITCHX_CODEX_OK`。此探针仍使用合成目录元数据，未验证真实文件工具调用、取消、Desktop/IDE 或 ChatGPT 订阅认证。
