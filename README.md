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

M1 原生界面已提供设计 token、可通过键盘操作的按钮、基础卡片/状态控件、深浅主题、侧栏、状态抽屉和真实本地空态。启动后在平台应用数据目录创建权限受限的 SQLite 元数据文件，并经后台通道读取上游列表；列表只显示目标站点，隐藏 URL 中可能携带的凭据、路径和查询参数。只有点击“检查凭据”才读取已保存的钥匙串引用，界面只接收状态、不接收密钥。读取失败时保留上次成功的列表并显示错误原因和下一步。可用绝对路径 `SWITCHX_DATA_DIR` 隔离测试数据。主题和导航只保存在本次窗口实例中；路由、Codex 配置和未接入页面仍明确标注未启用。macOS 已人工检查深浅主题、空态、上游列表、抽屉、Esc 关闭和错误态。

M1 收口检查（2026-09-24，macOS 调试 bundle，隔离的 `SWITCHX_DATA_DIR` 与两条合成上游记录）：上游页现在有按名称筛选，输入英文可实时筛选，中文字符经无障碍文本写入后可正确筛选；无匹配时显示空态。Tab 可从输入框移到“检查凭据”；抽屉用 Esc 关闭后焦点返回“状态详情”。深浅主题下均已查看筛选布局。关闭窗口后 SwitchX 进程仍在运行。**尚未完成**输入法拼音预编辑/候选上屏和托盘菜单重新打开/退出的实际操作验证；当前 UI 自动化无法访问该菜单栏项目，不能把进程驻留等同于完整托盘验收。上述检查未读取真实凭据，也未修改 Codex 配置。

`tests/fixtures/published-models.json` 与 `tests/fixtures/routed-user-config.toml` 是合成目录和 Codex 配置的 golden fixtures，用于检查 schema 输出与无关 TOML 字段、注释的保留。

`cargo run --example keychain_probe` 在系统凭据存储中写入一次独立的合成测试条目，读取后立即删除；不读取现有账号数据。macOS 本机测试已通过。

`cargo run --example config_probe -- /tmp/switchx-models.json` 只把合成 `config.toml` 差异预览输出到 stdout。预览保留其他 provider、MCP、项目、安全设置和注释；它不写用户配置。生成的 `env_key = "SWITCHX_LOCAL_TOKEN"` 仅在启动 Codex 的环境已提供本地令牌时才可用于请求。

`config_transaction::PreparedSwitch` 已实现配置切换的独立事务核心：检查目标文件未变化、先写不可变目录和恢复 journal、再原子替换 `config.toml`。`restore` 按受管字段做三方比较，保留外部新增设置；同一字段发生冲突时保留外部值和 journal。自动化测试只在临时目录运行。界面尚未调用该模块；应用必须先验证路由器、本地令牌和目标客户端，再允许切换真实配置。当前 `env_key` 方案仍要求 Codex 启动环境提供 `SWITCHX_LOCAL_TOKEN`。

`cargo run --example codex_cli_probe` 会通过 npm 执行 Codex CLI 0.156.1，在独立临时 `CODEX_HOME` 中启动同一个 SwitchX 路由入口和两个本地假上游。实测两个别名分别到达对应假上游，且 `sx-ds-flash` 完成一次读取临时文件、回传工具结果、第二轮回答。运行结束会删除该临时目录；无真实 API Key、登录态或模型调用。

`cargo run --example deepseek_live_probe` 会在终端隐藏输入地读取测试 Key，调用 DeepSeek 模型发现，并通过 SwitchX 路由发送一次真实 Responses 请求，再让隔离的 Codex CLI 0.156.1 发送一次真实请求；这两次模型调用可能计费。Key 只保存在测试进程内存中，临时 `CODEX_HOME` 结束后删除。2026-09-24 实测：模型发现返回 `deepseek-flash`、`deepseek-v4-pro`；路由请求返回 HTTP 200、`completed` 和 `SWITCHX_OK`；Codex CLI 返回 `SWITCHX_CODEX_OK`。此探针仍使用合成目录元数据，未验证真实文件工具调用、取消、Desktop/IDE 或 ChatGPT 订阅认证。

`cargo run --example chatgpt_auth_probe -- --synthetic` 在独立临时 `CODEX_HOME` 中用合成 API Key 验证 CLI 0.156.1 的 `requires_openai_auth` 与独立 `x-switchx-local-token` 请求头能同时抵达本地 mock；本地 mock 不转发模型请求。2026-09-24 本机已通过；另以无效的合成 ChatGPT 形状凭据测试，也观察到 CLI 把请求送到本地 `/v1/responses`，但不代表真实账号可用。

去掉 `--synthetic` 后，探针会要求在临时目录完成一次官方 ChatGPT 浏览器登录，随后把合成模型请求送到本地 mock；正常结束时删除临时登录数据。2026-09-24 真实账号实测通过：CLI 0.156.1 完成登录，独立本地校验头与 Bearer 头抵达本地 mock，CLI 收到并完成合成回复。探针只检查 `x-openai-account-id`，本次该头未出现；不能由此断言其他账号头不存在。Codex CLI 0.156.1 支持将 ChatGPT 请求体压成 zstd；探针仅在自己的临时配置中关闭请求压缩，因此未验证默认压缩路径。产品路由器现接受有大小上限的 zstd 请求并向上游发送普通 JSON，本地假上游测试覆盖该路径；尚未让真实 Codex 默认压缩请求通过产品路由器。mock 不转发模型请求，此结果也不证明真实官方上游、续期、失效或切回。假上游测试确认选中 DeepSeek 时不会转发客户端的官方认证或账号头。
