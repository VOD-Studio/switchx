# switchx

Rust + Slint 原生桌面应用。M2 直连已完成 macOS / DeepSeek 验收，当前推进 M3 的模型目录与 API 路由预览版；完整范围见 `docs/SWITCHX-PLAN.md`。

原生界面现可管理上游、直连配置、模型资料与 loopback 路由。SQLite 保存元数据和凭据引用，系统凭据存储保存上游 Key 与独立的本地路由令牌。仓库中的**合成目录夹具**仅用于测试，不能作为真实模型能力或发布模板；`catalog_probe` 仍只输出夹具，不读取账号、凭据或现有 Codex 配置。

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

M1 原生界面已提供设计 token、可通过键盘操作的按钮、基础卡片/状态控件、深浅主题、侧栏、状态抽屉和真实本地空态。启动后在平台应用数据目录创建权限受限的 SQLite 元数据文件，并经后台通道读取上游列表；列表只显示目标站点，隐藏 URL 中可能携带的凭据、路径和查询参数。只有点击“检查凭据”才读取已保存的钥匙串引用，界面只接收状态、不接收密钥。读取失败时保留上次成功的列表并显示错误原因和下一步。可用绝对路径 `SWITCHX_DATA_DIR` 隔离测试数据。主题和导航只保存在本次窗口实例中；路由和未接入页面仍明确标注未启用。macOS 已人工检查深浅主题、空态、上游列表、抽屉、Esc 关闭和错误态。

M1 收口检查（2026-09-24，macOS 调试 bundle，隔离的 `SWITCHX_DATA_DIR` 与两条合成上游记录）：上游页现在有按名称筛选，输入英文可实时筛选，中文字符经无障碍文本写入后可正确筛选；无匹配时显示空态。Tab 可从输入框移到“检查凭据”；抽屉用 Esc 关闭后焦点返回“状态详情”。深浅主题下均已查看筛选布局。关闭窗口后 SwitchX 进程仍在运行。**尚未完成**输入法拼音预编辑/候选上屏和托盘菜单重新打开/退出的实际操作验证；当前 UI 自动化无法访问该菜单栏项目，不能把进程驻留等同于完整托盘验收。上述检查未读取真实凭据，也未修改 Codex 配置。

`tests/fixtures/published-models.json` 与 `tests/fixtures/routed-user-config.toml` 是合成目录和 Codex 配置的 golden fixtures，用于检查 schema 输出与无关 TOML 字段、注释的保留。

`cargo run --example keychain_probe` 在系统凭据存储中写入一次独立的合成测试条目，读取后立即删除；不读取现有账号数据。macOS 本机测试已通过。

`cargo run --example config_probe -- /tmp/switchx-models.json` 只把合成 `config.toml` 差异预览输出到 stdout。预览保留其他 provider、MCP、项目、安全设置和注释；它不写用户配置。生成的 `env_key = "SWITCHX_LOCAL_TOKEN"` 仅在启动 Codex 的环境已提供本地令牌时才可用于请求。

`config_transaction::PreparedSwitch` 检查目标文件未变化，先写不可变目录和恢复 journal，再原子替换 `config.toml`。`restore` 按受管字段做三方比较，保留外部新增设置；同一字段发生冲突时保留外部值和 journal。直连和路由共用目标文件锁，防止 SwitchX 实例并发写入。原生路由通过 `auth.command` 取得本地令牌；较早的 `config_probe`、`codex_cli_probe` 和 `deepseek_live_probe` 仍使用显式提供 `SWITCHX_LOCAL_TOKEN` 的隔离探针流程。

`cargo run --example codex_cli_probe` 会通过 npm 执行 Codex CLI 0.156.1，在独立临时 `CODEX_HOME` 中启动同一个 SwitchX 路由入口和两个本地假上游。实测两个别名分别到达对应假上游，且 `sx-ds-flash` 完成一次读取临时文件、回传工具结果、第二轮回答。运行结束会删除该临时目录；无真实 API Key、登录态或模型调用。

`cargo run --example deepseek_live_probe` 会在终端隐藏输入地读取测试 Key，调用 DeepSeek 模型发现，并通过 SwitchX 路由发送一次真实 Responses 请求，再让隔离的 Codex CLI 0.156.1 发送一次真实请求；这两次模型调用可能计费。Key 只保存在测试进程内存中，临时 `CODEX_HOME` 结束后删除。2026-09-24 实测：模型发现返回 `deepseek-flash`、`deepseek-v4-pro`；路由请求返回 HTTP 200、`completed` 和 `SWITCHX_OK`；Codex CLI 返回 `SWITCHX_CODEX_OK`。此探针仍使用合成目录元数据，未验证真实文件工具调用、取消、Desktop/IDE 或 ChatGPT 订阅认证。

`cargo run --example chatgpt_auth_probe -- --synthetic` 在独立临时 `CODEX_HOME` 中用合成 API Key 验证 CLI 0.156.1 的 `requires_openai_auth` 与独立 `x-switchx-local-token` 请求头能同时抵达本地 mock；本地 mock 不转发模型请求。2026-09-24 本机已通过；另以无效的合成 ChatGPT 形状凭据测试，也观察到 CLI 把请求送到本地 `/v1/responses`，但不代表真实账号可用。

去掉 `--synthetic` 后，探针会要求在临时目录完成一次官方 ChatGPT 浏览器登录，随后把合成模型请求送到本地 mock；正常结束时删除临时登录数据。2026-09-24 真实账号实测通过：CLI 0.156.1 完成登录，独立本地校验头与 Bearer 头抵达本地 mock，CLI 收到并完成合成回复。探针只检查 `x-openai-account-id`，本次该头未出现；不能由此断言其他账号头不存在。Codex CLI 0.156.1 支持将 ChatGPT 请求体压成 zstd；探针仅在自己的临时配置中关闭请求压缩，因此未验证默认压缩路径。产品路由器现接受有大小上限的 zstd 请求并向上游发送普通 JSON，本地假上游测试覆盖该路径；尚未让真实 Codex 默认压缩请求通过产品路由器。mock 不转发模型请求，此结果也不证明真实官方上游、续期、失效或切回。假上游测试确认选中 DeepSeek 时不会转发客户端的官方认证或账号头。

## M2 直连进度

原生“上游供应商”页面现可添加、编辑、删除 Responses 上游，保存名称、API 地址、模型 ID 和系统钥匙串凭据。已有 SQLite v1 上游资料迁移到 v2 后保留原记录，缺少模型 ID 的旧记录需编辑补齐。输入地址只允许 HTTPS 或 `127.0.0.1` HTTP，拒绝 URL 内的用户名、密码、查询参数与片段。“检查”读取上游 `/models` 并核对所选 ID；它不发送推理请求，也不证明工具调用兼容。直连生效期间暂不允许编辑或删除上游，避免已配置的客户端拿到另一上游的 Key。

“配置与恢复”页面可选择绝对路径的 Codex 配置目录、读取当前配置、将当前自定义上游的**元数据**填入新建表单，以及查看 `codex login status` 报告的 ChatGPT 登录、API Key 登录或未知状态。导入不复制原配置中的凭据，也不读取 `auth.json`；新建上游须重新输入自己的 API Key。`SWITCHX_CODEX_CLI` 可指向要检查的 CLI；默认优先使用本机 ChatGPT.app 内的 CLI。

从上游列表点“直连”会预览受管字段。点“应用直连”时再检查钥匙串、`/models`、模型 ID 与目标配置文件是否变化，然后以 journal 和原子替换写入 `config.toml`。只管理 `model`、`model_provider`、`model_catalog_json` 和 SwitchX 新建的 provider 表；其他配置和注释保留。若待移除的 `model_catalog_json` 带注释，切换会拒绝并要求先手动移走注释，不把原始注释写进 journal。生成的 provider 使用 Codex `auth.command` 调用当前 SwitchX 程序，从系统钥匙串读取 Bearer token；配置与 SQLite 均不保存明文 Key。可从页面或托盘恢复，恢复时保留外部改动并报告冲突。直连不依赖 SwitchX 常驻，但移动或删除当前 SwitchX 程序会让已生成的 helper 路径失效。切换只对新启动的目标客户端生效。

`cargo test` 使用临时目录和本地 mock 验证数据库迁移、目录检查、差异写入、恢复及冲突。隔离探针：先运行 `cargo build --bin switchx`，再运行 `cargo run --example direct_cli_probe`。它在临时 `CODEX_HOME`、合成钥匙串条目与本地假上游中启动 Codex CLI 0.156.1；2026-09-24 已验证 CLI 通过 helper 取凭据、发送直连 Responses 请求并完成合成回答，结束后恢复配置并删除测试凭据。该 mock 探针不使用真实供应商 Key，也不证明真实上游的 Responses 工具对话、取消或 Desktop/IDE 兼容。

真实直连探针 `direct_live_probe` 使用已在 SwitchX 中保存的上游和系统钥匙串引用。先构建主程序，再指定绝对路径的数据目录和上游 ID：

```sh
cargo build --locked --bin switchx
cargo run --locked --example direct_live_probe -- /absolute/switchx-data PROVIDER_ID
```

它检查真实 `/models`，在新建的临时 `CODEX_HOME` 中通过生产直连事务写入 helper 配置，再让 Codex CLI 0.156.1 完成短回答和“读合成文件 → 回传工具结果 → 第二轮回答”。这些真实模型调用可能计费。探针不读取用户的 Codex 配置或登录文件，不输出 Key；正常结束（包括请求检查失败）会恢复临时配置并清理临时目录，恢复有冲突时保留目录供检查。原有上游记录与钥匙串条目由 SwitchX 管理，探针不删除。自动化测试不会执行真实请求。

2026-09-28 的 M2 验收已在 macOS 27.0 arm64、Codex CLI 0.156.1、DeepSeek `deepseek-flash` 上通过：真实模型发现、helper 直连短回答、真实文件工具与第二轮回答、原生页面写出的配置实际请求、原生上游编辑/删除、托盘重新打开/恢复/退出及中文拼音预编辑/候选上屏。中文输入法与托盘点击由用户实际操作确认；配置恢复、外部注释保留、journal 清除、进程退出与验收凭据清理由程序核对。首轮托盘组合操作未完成落盘恢复，单独补验恢复后通过，完整经过与覆盖边界见 [M2 验收记录](docs/acceptance/M2-2026-09-28.md)。本轮测试 Key 的保存副本和临时目录已清理；OpenAI 官方 API、ChatGPT 官方上游及认证生命周期、Desktop/IDE、取消和其他平台仍未在本轮验收。

## M3 首批交付：模型目录与 API 路由

“模型路由”页面可以为每个已保存的上游连接导入一个模型，设置稳定的公开 ID 和显示名、选择是否发布、指定默认模型。模型资料来自用户指定的绝对路径 Codex 目录 JSON；只选择与该连接实际模型 ID 精确匹配的条目，保留能力和指令模板。不同上游的同名模型各自保存资料。导入失败保留旧资料，上游模型 ID 改变后要求重新导入。SQLite v1/v2 自动迁移到 v3，删除上游会同时删除其模型资料。

使用步骤：

1. 保存 Responses 上游及 API Key，在“模型路由 → 模型资料”导入包含此模型的 `models.json`。可参考供应商提供的目录，例如 [DeepSeek 的 Codex 接入文档](https://api-docs.deepseek.com/quick_start/agent_integrations/codex)。`/models` 的 ID 列表不包含完整能力资料。
2. 选择要发布的模型和默认模型，在“配置与恢复”指定目标 Codex 配置目录。
3. 点击“预览发布”。SwitchX 预留所选 loopback 端口，并让本机 Codex CLI 在独立临时目录实际解析完整目录。可用 `SWITCHX_CODEX_CLI` 指定 CLI 程序。
4. 点击“开启路由”。再次核对 CLI、模型与上游资料，读取所选上游的系统凭据，检查 `/models`，验证本地路由后发布目录和配置。重启目标 Codex，在模型菜单或 `-m` 参数选择公开 ID。
5. 点击“恢复并停止”，或从托盘恢复、退出。macOS Command-Q 也会先恢复路由配置；冲突时保留恢复记录和仍在运行的路由，并阻止退出。活动请求最多等待 5 秒后取消。强杀后的下次启动显示待恢复目标，不自动重启路由。

原生路由使用独立 `auth.command` helper 获取本地令牌，无需手工设置终端环境变量。上游 Key 不进入 Codex 配置、SQLite 或恢复记录；出站请求只使用映射上游的 Key。原生 SSE 透传，取消不重放请求；暂时拒绝服务器状态引用、加密推理与压缩状态续接。应用运行中或有恢复记录时禁止修改上游、模型和绑定。成功的 `/models` 检查和目录解析不代表真实工具能力已验证。[Codex 配置约定](https://learn.chatgpt.com/docs/config-file/config-reference)

可复跑的隔离验收：

```sh
cargo build --locked --bin switchx
cargo run --locked --example routed_cli_probe
# 可选：使用同一套合成上游和凭据进行原生界面检查
cargo run --locked --example routed_cli_probe -- --desktop
```

`routed_cli_probe` 使用本机 CLI、两个本地假上游、临时配置和独立的合成钥匙串条目，检查目录兼容性、同名模型映射、helper 鉴权、文件工具轮次及恢复冲突，结束后恢复并清理；恢复失败则保留目录。2026-09-28 已通过 Codex CLI 0.156.1 的探针与原生页面配置的实际 CLI 请求，详见 [M3 首批验收记录](docs/acceptance/M3-2026-09-28.md)。本轮没有真实上游调用。

当前仍是 **API 路由预览版**：每个连接一个模型，修改目录需恢复后重新发布；已支持下述单个显式备用上游，不提供热更新、多候选链、熔断或费用统计。ChatGPT 订阅认证路由、DeepSeek ↔ 官方真实路由切换、同会话跨上游工具历史、远端压缩、Desktop/IDE 与其他平台仍待验收，M3 尚未完整完成。

## 显式备用上游

在“模型路由”中点击模型的“备用”，选择一个已导入资料的上游，并确认允许把完整请求发送到该站点。保存后重新预览发布，预览列出主备名称、站点和实际模型 ID，提示费用与数据接收方可能变化。备用模型无需作为独立模型发布。选择“不使用备用上游”可清除策略；删除备用上游也会清除引用。路由生效或存在恢复记录时禁止修改。

首批策略为 **主上游 → 一个备用上游，最多各尝试一次**：

- 只在 HTTP 请求发送前建立连接失败时尝试备用。未配置备用时保持原行为；主备均连接失败返回 `no_eligible_upstream`。
- 任何超时、请求已发出后的无响应/断线、HTTP 错误（包括 400、401、403、429、5xx）、SSE 中断、客户端取消和停止路由均不会触发切换。保留上游的 `Retry-After`，关闭 HTTP 客户端的默认协议重试，两次尝试共用 120 秒总预算。后续由 Codex 自己发起的重试属于另一个请求。
- 主备必须使用相同的实际模型 ID，完整能力资料和指令模板一致；仅允许显示名、说明、排序和可见性不同。不同模型自动替换、能力降级或能力交集计算尚未提供；同名不代表真实能力已验证。
- 不跟随备用模型自己的备用设置。每个新请求仍先尝试主上游；不进行并行竞速、后台探测或会话绑定。服务器状态引用、加密内容与压缩续接仍被拒绝。
- 开启路由前，主备都要通过已有的凭据和 `/models` 检查。此功能处理路由开启之后的连接故障，不跳过启动时的验证。预览后任一站点或模型资料发生变化，必须重新预览。

SQLite 自动升级到 v5，默认不设置备用。请求记录保存原主上游 ID、实际尝试的备用上游和固定切换原因，不保存地址、Key 或正文。独立 CLI 验收使用生产 `RouteSession` 和两个本地假上游，包含备用站点上的文件工具轮次：

```sh
cargo build --locked --bin switchx
cargo run --locked --example routed_cli_probe -- --fallback
# 只使用合成凭据和临时配置的原生页面夹具
cargo run --locked --example routed_cli_probe -- --desktop-fallback
```

2026-09-28 的 mock、隔离 CLI 和原生页面检查范围见 [备用策略验收记录](docs/acceptance/M3-fallback-2026-09-28.md)。本轮没有真实供应商调用。原生页面已启用路由，隔离 Codex CLI 从页面写出的配置完成文件工具轮次，请求页显示两条完成记录；页面恢复并退出后，原配置、journal 和临时资料均核对完成。

## 请求记录与完成状态

原生路由默认把已结束请求的元数据写入 SQLite，旧库自动迁移至 v5（请求记录在 v4 引入，v5 增加备用来源）。进入“请求与用量”或点击“刷新记录”，可查看按开始时间倒序排列的最近 100 条记录：公开模型、实际尝试的上游及模型、路由版本、请求编号、上游 HTTP 状态、响应头/首事件耗时、总耗时和固定错误原因。发生备用尝试时，另显示主上游连接失败和切换去向；最后一次失败不隐藏前一次连接失败。记录不随上游删除而删除；上游名称显示当前资料，已删除的上游显示原 ID。直连请求不经过 SwitchX，不能在此记录。

| 状态 | 判定 |
| --- | --- |
| 正常完成 | 收到完整的 `response.completed` SSE 事件，或非流式 JSON 的 `status: completed`；HTTP 200、正文中提及完成或单独的 `[DONE]` 均不足以证明完成 |
| 请求失败 | 上游连接失败、非成功 HTTP、`response.failed`、`response.incomplete`、SSE `error`、非流式错误或本地校验失败 |
| 流中断 | 完成前上游读取失败/超时、缺失终态的 EOF、路由停止，或无法安全解析的响应 |
| 用户取消 / 客户端断开 | 完成前客户端连接被释放；代理无法进一步区别主动取消和下游网络断开 |

响应头 `x-switchx-request-id` 可与本地记录对应。耗时从本地鉴权后的处理开始，到首次明确终态为止；首事件是首个完整 SSE 数据事件，并非首 token。已经确认的终态不会因随后关闭连接而被覆盖。停止路由引起的取消单独记为 `router_stopping`，不会算作用户取消，也不会重放请求。

仅观察有界内存中的响应；单个 SSE 事件或非流式 JSON 超过 2 MiB、SSE 格式无效时会中断转发并记录固定错误码。有效响应字节保持原样。请求/响应正文、上游原始错误消息、URL、Cookie、认证头和 Key 均不落盘。未通过本地鉴权、正文提取阶段被拒绝的请求和 `/models` 等其他端点不进入推理记录。请求在终态时落盘，进程强杀前尚未结束的请求可能没有记录；写入失败会保留安全诊断并在刷新页面时提示。当前无自动滚动、用量计费、记录清理或导出功能。

本地 mock、隔离 Codex CLI 与原生页面的验证范围见 [请求记录验收](docs/acceptance/M3-requests-2026-09-28.md)。本轮没有真实上游请求。
