# switchx

Rust + Slint 原生桌面应用。M2 直连已完成 macOS / DeepSeek 验收，M3 已实现模型目录、API、ChatGPT 与 Grok 订阅账号路由；目前仍为模型路由预览版，真实官方上游和认证生命周期尚待验收。完整范围见 `docs/SWITCHX-PLAN.md`。

原生界面现可管理上游、直连配置、模型资料、多个 ChatGPT / Grok 账号与 loopback 路由。SQLite 保存供应商配置、明文 API Key、本地路由令牌、模型资料和账号绑定；ChatGPT 与 Grok OAuth 账号分别保存在私有 JSON 文件中。API Key 保存方式按用户选择对齐 CC Switch，详见下文。仓库中的**合成目录夹具**仅用于测试，不能作为真实模型能力或发布模板；`catalog_probe` 仍只输出夹具，不读取账号、凭据或现有 Codex 配置。

```sh
cargo run
cargo test
cargo run --example catalog_probe > /tmp/switchx-models.json
CODEX_HOME="$(mktemp -d)" npx -y @openai/codex@0.156.1 \
  -c 'model_catalog_json="/tmp/switchx-models.json"' debug models
```

开发过程中，改业务库运行 `make check-lib`，改桌面入口运行 `make check-app`，改路由运行 `make test-router`；完成功能点后运行 `make check` 做完整检查。`make` 默认仍然构建应用。

UI 编译隔离、依赖调试信息及业务检查复用的验证见 [开发构建调整记录](docs/acceptance/dev-build-2026-10-08.md)。

生成 UI 由独立的 `crates/switchx-ui/` 编译，主程序、集成测试和预览继续通过 `switchx::ui` 引用。修改业务 Rust 代码可以复用 UI 编译产物；UI 构建脚本仍跟踪根目录的 `ui/` 与资源文件。开发构建中业务和 UI 保留 `line-tables-only` 调试信息，第三方依赖不生成调试信息；需要调试业务或 UI 变量时可运行 `CARGO_PROFILE_DEV_DEBUG=full cargo build --locked`。编译并发保持原有设置。生成代码及界面对照见 [构建成本调整记录](docs/acceptance/build-memory-2026-10-08.md)。

界面代码按职责组织：`ui/pages/` 放工作台、连接、账号、请求记录和设置页面，`ui/editors/` 放编辑表单与头像选择器，`ui/view-models.slint` 定义展示数据。`ui/app.slint` 保留窗口布局、导航、跨页状态、凭据草稿清理和 Rust 回调接口；页面通过属性绑定与操作回调接入。基础控件、代码编辑器和主题分别在 `components.slint`、`code-editor.slint`、`tokens.slint` 中。`make format` 与 `make format-check` 会递归处理 `ui/` 下的 Slint 文件。

macOS 窗口将标题栏融入界面，隐藏独立背景和标题文字，保留系统红黄绿按钮、圆角和窗口缩放。左侧为原生按钮预留空间；拖动左侧顶部或工具栏中间的空白区域可移动窗口。关闭仍隐藏到菜单栏，菜单栏入口可重新打开窗口。其他平台继续使用系统标题栏。

`cargo run --example window_chrome_probe` 用纯 UI 窗口检查 macOS 标题栏、原生按钮、全屏退出、关闭后重新显示和最小尺寸；不读取供应商、凭据或 Codex 配置。见 [标题栏验收与原生截图](docs/acceptance/window-titlebar-2026-10-08.md)。

### Grok 账号登录与上游路由

在「连接 → 订阅账号」添加 Grok 账号，浏览器会打开 xAI 官方设备码授权页面。支持取消、重新登录、多个账号、默认账号和失效状态；重新授权同一身份更新原账号。随后在「连接」添加 Grok 订阅连接，选择默认账号或固定账号，在工作台选择模型并预览、启用路由。保存账号和连接不会启用路由，也不会写入 Codex 的 `auth.json`。

账号卡片显示官方订阅的剩余额度、重置倒计时、具体重置时间和最近更新时间。进入账号页自动查询一次，可逐账号刷新或“刷新全部”；查询在后台进行，不发送模型请求，不改动 Codex 配置。刷新失败保留上次成功数据并标明错误；低于 10% 和用尽分别使用警示色。窗口名称按官方重置间隔估算，与 CC Switch 一致；额度快照仅保存在内存中，时间文字每 30 秒更新，账单接口不轮询。凭据自动刷新沿用原有私有账号文件，额度请求只发往固定 Grok 官方账单端点且不跟随重定向。

Grok OAuth 使用 Grok CLI 的公开客户端身份，认证端点从 `https://auth.x.ai/.well-known/openid-configuration` 发现并限制在该官方源；它不是 SwitchX 独立注册的 OAuth 客户端。能否推理、可用模型、订阅权限和额度需用真实账号验收。xAI API Key 通道与 Grok 订阅的计费分开。

OAuth 登录与凭据刷新、连接检查、模型发现和上游推理统一使用 reqwest 的自动代理机制，支持 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` 及其小写形式，并遵循 `NO_PROXY` / `no_proxy`；未设置代理环境变量时读取 macOS / Windows 系统代理。访问本地上游时应将 `127.0.0.1,localhost` 加入代理排除项。SwitchX 自身向本地路由发送的验证请求始终直连。

账号刷新凭据单独保存在 `SWITCHX_DATA_DIR/xai_oauth_auth.json`，Unix 权限为 `0600`；access token 只在内存中缓存。上游固定为 `https://api.x.ai/v1/responses`，本地路由逐请求注入保存账号的 token，丢弃客户端 OpenAI 认证与工作区头。账号绑定在发布时固定，账号不能参与自动备用切换；删除前需要解除上游引用。

新增连接先保存一个未启用的 `grok-4.5` 映射（500k 上下文，low / medium / high / xhigh）。模型设置支持使用绑定账号获取 `/models`，并手动增加或修改映射；目录列出模型不代表有推理权限。发布预览会把原配置中不在所选 Grok 模型资料允许范围内的思考档位改为该模型的默认档位；恢复时还原原值。原生 Responses 兼容层展平 namespace 函数工具、恢复返回名称、移除不支持的私有字段，并处理根部联合工具 schema；无法表示的自定义工具会明确拒绝。沿用 `shell_command` 函数工具资料，导入 freeform 工具资料后可能被拒绝。

Grok 的加密思考内容仅允许在已登记、上游 / 账号 / 模型相同的会话中回传。跨账号、跨模型或 API / ChatGPT 切换需要新会话；不支持远程 compact、`previous_response_id` 或服务器 conversation 续接。Grok OAuth 配置走现有本地令牌 helper，不依赖 Codex 的 OpenAI 登录。

SQLite 升级到 schema v11，保留原有 API Key、模型、备用映射和设置；旧配置恢复记录未处理前不会升级。本地测试不使用真实凭据：

```sh
cargo test --locked --lib xai
cargo test --locked --test router xai
cargo build --locked
cargo run --locked --example xai_cli_probe
# 可选：打开一次性的原生界面夹具，退出后删除临时目录
cargo run --locked --example xai_cli_probe -- --desktop
```

探针只调用本地模拟认证和上游，使用隔离 `CODEX_HOME`，检查工具执行、加密思考回传、配置恢复和本地令牌清理。它不证明真实 Grok 订阅可用。验收过程和原生截图见 [Grok OAuth 验收](docs/acceptance/grok-oauth-2026-10-08.md)。

### 留白工作台

2026-09-30 的原生界面采用米白 / 石墨底色与鼠尾草绿，收拢为五个导航入口：

| 页面 | 操作 |
| --- | --- |
| 工作台 | 选择模型、设默认、筛选已选项、编辑映射与备用策略、预览启用、启动 Codex、恢复停止 |
| 连接 | 管理 API、ChatGPT 与 Grok 订阅连接；在“订阅账号”中管理账号与独立的原生登录操作 |
| 活动 | 查看最近请求的结果与耗时，展开单条详情 |
| 工具箱 | 保留 MCP / Skills 的位置，明确标注尚未接入 |
| 设置 | 外观、动画、目标目录、通用配置、本地端口和配置恢复 |

API、订阅、模型、通用配置和发布预览使用侧边抽屉。图标操作有悬停说明；快速操作提供页面和添加连接入口。Esc 关闭当前抽屉，关闭后键盘焦点回到主界面。保存连接与发布配置继续分开处理，API Key 输入仍保持遮罩；关闭或离开编辑器会清空凭据草稿。

按钮、选中和焦点反馈使用 150ms 过渡，页面与请求详情使用 240ms，抽屉进出使用 340ms。设置中可以关闭动画；macOS 启动时读取系统“减少动态效果”，运行中每五秒跟随更新。外观和动画开关目前仅对本次运行生效。

原型源保存在本地 `design/quiet-workbench-prototype` 分支，正式界面继续使用 Rust + Slint。验证范围及原生截图见 [留白工作台验收](docs/acceptance/quiet-workbench-2026-09-30.md)。

在 2026-09-24 的 macOS 本机测试中，Codex CLI 0.156.1 的 `debug models` 与 app-server `model/list` 均返回 `sx-ds-flash`、`sx-oai-coding`。这证明该版本能读取夹具目录，尚未证明真实 `/model` 交互、真实模型请求、Desktop 或 IDE 兼容。

`cargo test --test router` 用本地假上游验证精确映射、SSE 首事件直达、认证头隔离、未知模型、本地鉴权，以及订阅账号的工作区绑定、401/403、续期后请求和 compact。它不使用真实供应商凭据，不证明真实上游或真实订阅账号可用。

macOS 可运行 `sh scripts/bundle-macos.sh` 生成 `target/debug/SwitchX.app`；传入 `--release` 则使用优化构建，生成 `target/release/SwitchX.app`。主窗口关闭后应驻留菜单栏，可从菜单栏重新打开或退出。两种 bundle 均未签名、未公证，仅供本机检查，不能作为发布包。

应用图标原图保存在 `assets/app-icon.png`，Slint 窗口直接引用它。macOS 打包脚本先通过 `prepare-macos-icon.swift` 为本机准备底稿：macOS 26+ 使用与原图背景匹配的完整不透明方形，避免系统给透明图案再套底板并缩小；较旧系统沿用原图。随后使用系统自带的 `sips` 和 `iconutil` 生成标准与 Retina 尺寸的 `AppIcon.icns`，放入 bundle 的 `Contents/Resources/`，供 Finder 和 Dock 使用。替换原图后重新打包即可更新应用图标。

`swift scripts/check-macos-icon.swift /absolute/path/to/SwitchX.app` 通过原生图标服务检查原图蓝紫色细节的实际缩放，防止透明底板造成二次内缩。可选第二个参数保存系统渲染截图；[尺寸修复验收](docs/acceptance/app-icon-sizing-2026-09-30.md)记录原因与前后对比。

菜单栏图标使用 `assets/tray.svg` 中的单色猫咪与开关线稿，透明背景、18pt 显示高度。macOS 将它标记为原生模板图像，由系统根据菜单栏背景和菜单选中状态着色；图标颜色不跟随 SwitchX 窗口内的主题切换。

`cargo run --example tray_icon_probe` 检查单色像素、透明背景和 macOS 模板标记，并短暂显示同一个 Slint 托盘组件。[浅色与深色图标预览](docs/acceptance/screenshots/tray-icons/preview.png)包含放大图和 18pt 实际尺寸。

M1 原生界面已提供设计 token、可通过键盘操作的按钮、基础卡片/状态控件、深浅主题、侧栏、状态抽屉和真实本地空态。启动后在平台应用数据目录创建权限受限的 SQLite 数据文件，并经后台通道读取上游列表；列表只显示目标站点，隐藏 URL 中可能携带的凭据、路径和查询参数，凭据状态不包含 API Key。读取失败时保留上次成功的列表并显示错误原因和下一步。可用绝对路径 `SWITCHX_DATA_DIR` 隔离测试数据。主题和导航只保存在本次窗口实例中；路由和未接入页面仍明确标注未启用。macOS 已人工检查深浅主题、空态、上游列表、抽屉、Esc 关闭和错误态。

M1 收口检查（2026-09-24，macOS 调试 bundle，隔离的 `SWITCHX_DATA_DIR` 与两条合成上游记录）：上游页现在有按名称筛选，输入英文可实时筛选，中文字符经无障碍文本写入后可正确筛选；无匹配时显示空态。Tab 可从输入框移到“检查凭据”；抽屉用 Esc 关闭后焦点返回“状态详情”。深浅主题下均已查看筛选布局。关闭窗口后 SwitchX 进程仍在运行。**尚未完成**输入法拼音预编辑/候选上屏和托盘菜单重新打开/退出的实际操作验证；当前 UI 自动化无法访问该菜单栏项目，不能把进程驻留等同于完整托盘验收。上述检查未读取真实凭据，也未修改 Codex 配置。

`tests/fixtures/published-models.json` 与 `tests/fixtures/routed-user-config.toml` 是合成目录和 Codex 配置的 golden fixtures，用于检查 schema 输出与无关 TOML 字段、注释的保留。

`cargo run --example local_token_probe` 在独立临时 SQLite 中保存一个合成本地令牌，通过只读连接读取，再删除令牌和临时目录；不访问真实账号或系统凭据。

`cargo run --example config_probe -- /tmp/switchx-models.json` 只把合成 `config.toml` 差异预览输出到 stdout。预览保留其他 provider、MCP、项目、安全设置和注释；它不写用户配置。生成的 `env_key = "SWITCHX_LOCAL_TOKEN"` 仅在启动 Codex 的环境已提供本地令牌时才可用于请求。

`config_transaction::PreparedSwitch` 检查目标文件未变化，先写不可变目录和恢复 journal，再原子替换 `config.toml`。`restore` 按受管字段做三方比较，保留外部新增设置；同一字段发生冲突时保留外部值和 journal。直连和路由共用目标文件锁，防止 SwitchX 实例并发写入。仅含 API 模型的原生路由通过 `auth.command` 取得本地令牌；含订阅模型的路由使用 `requires_openai_auth` 和独立本地请求头，详见下文。较早的 `config_probe`、`codex_cli_probe` 和 `deepseek_live_probe` 仍使用显式提供 `SWITCHX_LOCAL_TOKEN` 的隔离探针流程。

`cargo run --example codex_cli_probe` 会通过 npm 执行 Codex CLI 0.156.1，在独立临时 `CODEX_HOME` 中启动同一个 SwitchX 路由入口和两个本地假上游。实测两个别名分别到达对应假上游，且 `sx-ds-flash` 完成一次读取临时文件、回传工具结果、第二轮回答。运行结束会删除该临时目录；无真实 API Key、登录态或模型调用。

`cargo run --example deepseek_live_probe` 会在终端隐藏输入地读取测试 Key，调用 DeepSeek 模型发现，并通过 SwitchX 路由发送一次真实 Responses 请求，再让隔离的 Codex CLI 0.156.1 发送一次真实请求；这两次模型调用可能计费。Key 只保存在测试进程内存中，临时 `CODEX_HOME` 结束后删除。2026-09-24 实测：模型发现返回 `deepseek-flash`、`deepseek-v4-pro`；路由请求返回 HTTP 200、`completed` 和 `SWITCHX_OK`；Codex CLI 返回 `SWITCHX_CODEX_OK`。此探针仍使用合成目录元数据，未验证真实文件工具调用、取消、Desktop/IDE 或 ChatGPT 订阅认证。

`cargo run --example chatgpt_auth_probe -- --synthetic` 在独立临时 `CODEX_HOME` 中用合成 API Key 验证 CLI 0.156.1 的 `requires_openai_auth` 与独立 `x-switchx-local-token` 请求头能同时抵达本地 mock；本地 mock 不转发模型请求。2026-09-24 本机已通过；另以无效的合成 ChatGPT 形状凭据测试，也观察到 CLI 把请求送到本地 `/v1/responses`，但不代表真实账号可用。

去掉 `--synthetic` 后，探针会要求在临时目录完成一次官方 ChatGPT 浏览器登录，随后把合成模型请求送到本地 mock；正常结束时删除临时登录数据。2026-09-24 真实账号实测通过：CLI 0.156.1 完成登录，独立本地校验头与 Bearer 头抵达本地 mock，CLI 收到并完成合成回复。探针只检查 `x-openai-account-id`，本次该头未出现；不能由此断言其他账号头不存在。Codex CLI 0.156.1 支持将 ChatGPT 请求体压成 zstd；探针仅在自己的临时配置中关闭请求压缩，因此未验证默认压缩路径。产品路由器现接受有大小上限的 zstd 请求并向上游发送普通 JSON，本地假上游测试覆盖该路径；尚未让真实 Codex 默认压缩请求通过产品路由器。mock 不转发模型请求，此结果也不证明真实官方上游、续期、失效或切回。假上游测试确认选中 DeepSeek 时不会转发客户端的官方认证或账号头。

## M2 直连进度

原生“连接 → 上游连接”页面现可添加、编辑、删除 Responses 上游，保存名称、API 地址、模型 ID 和 API Key。API Key 按用户选择以明文 JSON 存在 SQLite `providers.settings_config` 的 `auth.OPENAI_API_KEY` 字段，编辑输入保持密码形式，留空保留已有 Key；列表、状态、TOML 预览和恢复 journal 不显示或保存 Key。数据库及其副本包含凭据，本版不提供数据库加密或凭据同步。旧记录迁移后保留上游资料，缺少模型 ID 或凭据时可重新输入。输入地址只允许 HTTPS 或 `127.0.0.1` HTTP，拒绝 URL 内的用户名、密码、查询参数与片段。“检查”读取上游 `/models` 并核对所选 ID；它不发送推理请求，也不证明工具调用兼容。直连生效期间暂不允许编辑或删除上游，避免已配置的客户端拿到另一上游的 Key。

SQLite 自动升级至 v10，保留原有资料；旧版数据库存在恢复日志时先完成恢复。API Key 仅从 SQLite 读取，数据库中缺少 Key 时须在编辑表单重新输入。本地路由令牌也改存 SQLite；程序不再依赖 keyring，不读取、迁移或清理旧钥匙串条目。已有旧 helper 配置须先恢复，再重新预览应用；新的 helper 使用显式绝对数据目录。合成存储、独立 helper 与本机 CLI 检查已通过，见 [本地令牌保存检查](docs/acceptance/M3-local-token-storage-2026-09-29.md)。

“配置与恢复”页面可选择绝对路径的 Codex 配置目录、读取当前配置、将当前自定义上游的**元数据**填入新建表单，以及查看 `codex login status` 报告的 ChatGPT 登录、API Key 登录或未知状态。“导入当前上游”不复制原配置中的凭据，也不读取 `auth.json`；新建上游须重新输入自己的 API Key。ChatGPT 账号另有明确的“导入当前 Codex 账号”入口，见下文。`SWITCHX_CODEX_CLI` 显式指定的 CLI 始终优先；默认先查找 `PATH` 中的 CLI，再检查 `~/.local/bin/codex`、`/opt/homebrew/bin/codex` 和 `/usr/local/bin/codex`，最后回退到 ChatGPT.app 内置 CLI。常见独立安装路径也会在 Finder 启动、`PATH` 较短时检查；自动发现只选择可执行文件，并解析为绝对路径。模型列表、登录、目录检查与启动使用同一选择规则。

添加和编辑供应商时提供以下 Codex 配置选项，界面与操作参考 `others/cc-switch`：

- **config.toml (TOML)**：API 供应商提供随表单和开关实时更新的只读预览；ChatGPT 订阅连接提供可编辑的完整非敏感 TOML，详见下文。编辑表单不写入目标 Codex 配置，也不显示 API Key。
- **启用远程压缩**：生成 `name = "OpenAI"`，允许 Codex 使用远程压缩；API 直连要求上游支持 `/responses/compact`。API 路由保留现有服务器状态隔离，不应用此远程压缩选项。
- **应用通用配置**：把已保存的非敏感 TOML 片段深合并到目标配置，保留未被覆盖的字段。订阅连接保存时，与片段相同的普通字段继续继承最新通用值，手动改成不同值的字段保留为连接覆盖；上下文窗口与压缩阈值独立保存在连接中。
- **1M 上下文窗口**：开启时写入 `model_context_window = 1000000`；关闭时同时移除该字段与压缩阈值字段。
- **压缩阈值**：默认 `900000`，开启 1M 时须为小于 `1000000` 的正整数，写入 `model_auto_compact_token_limit`。窗口与阈值控件优先于通用片段中的同名字段。
- **编辑通用配置**：首次启动、打开供应商表单或导入当前配置时，若尚未保存通用片段，会从所选 Codex 目录的 `config.toml` 自动提取并保存。已有片段保持不变；明确保存空片段后不会自动填回。源文件缺失、无通用内容或由 SwitchX 管理时跳过自动提取，后续仍可重试。
- **从编辑内容提取**：读取供应商表单当前的 TOML，提取通用部分并立即保存；取消不会撤销这次提取。编辑器中的手动修改仍须点击保存，取消只丢弃手动草稿。模型、供应商身份、凭据和 MCP 配置不进入通用片段。

保存供应商选项或通用配置后，在下次应用直连或开启路由时生效。路由的全局 Codex 选项取自默认模型所属供应商；在 Codex 菜单中切换模型不会动态更换这些全局设置。恢复时对通用字段和上下文字段同样做受管差异比较，保留外部编辑并报告冲突。`model_reasoning_effort` 改为有效思考档位或被清除时，恢复保留这一选择且不报冲突；未修改时仍恢复发布前的值。SQLite 当前自动升级至 v10：`providers.codex_options` 保存供应商选项和订阅 TOML，`app_settings` 保存通用片段与本地令牌，供应商记录保存账号绑定与 `icon_id` 头像覆盖，`providers.settings_config` 保存 API 认证配置；没有保存选项且通用片段为空的旧记录保持原有行为。

2026-09-29 已完成这六项功能的原生界面检查，以及隔离直连和路由配置的写入、逐字节恢复。覆盖范围与复跑步骤见 [供应商配置验收记录](docs/acceptance/M3-provider-config-2026-09-29.md)。

随后对齐了 CC Switch 旧版通用片段的初始化、提取与保存行为，原生检查与回归结果见 [通用配置来源验收记录](docs/acceptance/M3-common-config-source-2026-09-29.md)。

从上游列表点“直连”会预览受管字段。点“应用直连”时再检查已保存的 API Key、`/models`、模型 ID 与目标配置文件是否变化，然后以 journal 和原子替换写入 `config.toml`。管理 `model`、`model_provider`、`model_catalog_json` 和 SwitchX 新建的 provider 表，启用上述选项时还管理对应的通用字段与上下文字段；其他配置和注释保留。若待移除的 `model_catalog_json` 带注释，切换会拒绝并要求先手动移走注释，不把原始注释写进 journal。生成的 provider 使用 Codex `auth.command` 调用当前 SwitchX 程序，从本地 SQLite 取得该供应商的 Bearer token；Key 不写入目标 `config.toml` 或恢复 journal。可从页面或托盘恢复，恢复时保留外部改动并报告冲突。直连不依赖 SwitchX 常驻，但移动或删除当前 SwitchX 程序会让已生成的 helper 路径失效。切换只对新启动的目标客户端生效。

`cargo test` 使用临时目录和本地 mock 验证数据库迁移、目录检查、差异写入、恢复及冲突。隔离探针：先运行 `cargo build --bin switchx`，再运行 `cargo run --example direct_cli_probe`。它使用本机 Codex CLI、临时 `CODEX_HOME`、合成供应商凭据与本地假上游。2026-09-29 已在 CLI `0.158.0-alpha.2.1` 上核对 SQLite helper 通过显式数据目录取 Key，即使没有 `SWITCHX_DATA_DIR` 环境变量也能完成直连 Responses 合成回答，随后精确恢复配置并删除临时数据库。该 mock 结果不证明真实上游的 Responses 工具对话、取消或 Desktop/IDE 兼容。

真实直连探针 `direct_live_probe` 使用已在 SwitchX 中保存的上游和 API Key。先构建主程序，再指定绝对路径的数据目录和上游 ID：

```sh
cargo build --locked --bin switchx
cargo run --locked --example direct_live_probe -- /absolute/switchx-data PROVIDER_ID
```

它只读打开已升级到当前版本的源数据库，检查真实 `/models`，在新建的临时 `CODEX_HOME` 中通过生产直连事务写入 helper 配置，再让本机 Codex CLI 完成短回答和“读合成文件 → 回传工具结果 → 第二轮回答”。可用 `SWITCHX_CODEX_CLI` 指定 CLI，与路由目录检查使用同一选择规则。这些真实模型调用可能计费。探针不读取用户的 Codex 配置或登录文件，不输出 Key；正常结束（包括请求检查失败）会恢复临时配置并清理临时目录，恢复有冲突时保留目录供检查。原有上游记录和 API Key 由 SwitchX 管理，探针不删除。自动化测试不会执行真实请求。

2026-09-28 的 M2 验收已在 macOS 27.0 arm64、Codex CLI 0.156.1、DeepSeek `deepseek-flash` 上通过：真实模型发现、helper 直连短回答、真实文件工具与第二轮回答、原生页面写出的配置实际请求、原生上游编辑/删除、托盘重新打开/恢复/退出及中文拼音预编辑/候选上屏。中文输入法与托盘点击由用户实际操作确认；配置恢复、外部注释保留、journal 清除、进程退出与验收凭据清理由程序核对。首轮托盘组合操作未完成落盘恢复，单独补验恢复后通过，完整经过与覆盖边界见 [M2 验收记录](docs/acceptance/M2-2026-09-28.md)。本轮测试 Key 的保存副本和临时目录已清理；OpenAI 官方 API、ChatGPT 官方上游及认证生命周期、Desktop/IDE、取消和其他平台仍未在本轮验收。

## M3 首批交付：模型目录与 API 路由

ChatGPT 订阅上游默认显示 OpenAI 绿色头像；API 预设继续按准确地址显示品牌头像。API 和订阅表单均可点击头像，从 CC Switch 的 110 个内置图标中按名称或关键词搜索、选择，也可恢复默认头像。返回或“完成”保留当前表单选择，点击上游“保存”后才写入 SQLite；取消表单丢弃头像修改。图标随应用离线打包，单色图标适配深浅主题，头像 ID 不进入 Codex 配置、凭据或请求。

本地检查和同组件离屏截图见 [上游头像验收记录](docs/acceptance/M3-provider-icons-2026-09-29.md)；原生点击验收受本轮窗口工具限制尚未完成。

在“连接 → 上游连接”点击“+ 添加连接”，在同一弹窗中选择 ChatGPT 订阅、Grok 订阅、自定义 API 或官方 API 预设，再填写对应表单。官方预设带品牌图标，自动填入名称、API 地址和默认模型，再填写自己的 API Key。预设提供官网和获取 Key 的入口，所有字段仍可编辑；“返回选择”可更换连接类型或预设，并清空当前 Key、认证编辑内容和旧的探测列表。已保存的官方连接也显示对应图标，按准确 API 地址识别。模型能力继续通过独立映射填写或导入。

统一添加弹窗的本地验证与深浅主题截图见 [添加连接验收记录](docs/acceptance/M3-connection-picker-2026-10-08.md)。

连接卡片的“检查”按连接独立执行，显示实际检查步骤和已耗时；等待超过 10 秒时提示仍在等待响应。成功结果和失败原因保留在对应卡片，失败后可点“重试”。其他连接和页面操作保持可用，刷新列表保留检查状态，重新保存连接会清除旧结果。检查核对登录资料或读取模型目录，必要时沿用原有凭据续期流程，不发送推理请求。成功提示会注明实际推理或工具调用权限尚未验证。

| 官方预设 | API 地址 | 默认模型 | 核对来源 |
| --- | --- | --- | --- |
| DeepSeek | `https://api.deepseek.com` | `deepseek-flash` | [官方 Codex 指南](https://api-docs.deepseek.com/quick_start/agent_integrations/codex/) |
| Kimi | `https://api.moonshot.cn/v1` | `kimi-k3` | [官方 Codex 指南](https://platform.kimi.com/docs/guide/codex-kimi) |
| MiniMax | `https://api.minimax.cn/v1` | `MiniMax-M3` | [官方 Codex 指南](https://platform.minimax.cn/docs/token-plan/codex) |
| 小米 MiMo | `https://api.xiaomimimo.com/v1` | `mimo-v2.6-pro` | [官方 Codex 指南](https://mimo.mi.com/docs/zh-CN/tokenplan/integration/codex-configuration) |

预设参考 `others/cc-switch/src/config/codexProviderPresets.ts`，图标复用其内置 SVG；来源与 MIT 许可见 [图标说明](assets/providers/NOTICE.md)。以上地址与默认模型于 2026-09-28 核对，运行时可使用“获取模型列表”确认自己的账号目录。

“连接 → 上游连接”的添加和编辑表单提供“获取模型列表”，使用当前填写的 API 地址和 Key；编辑时 Key 留空则读取该上游在 SQLite 中已保存的 API Key。无需先填写模型 ID 或保存上游。支持 `data[].id` 和 `models[].slug/id`，结果去重排序；修改地址、Key 或切换表单后清空旧列表并丢弃在途结果。探测只读取模型列表，不发送推理请求。接口未开放或返回空列表时仍可手动填写模型 ID。

“工作台”页面支持为同一个上游添加多个独立映射，也可从连接列表的“模型”进入。每条映射保存公开 ID、菜单显示名、实际请求模型、上下文窗口、支持的思考等级和默认等级；可以独立编辑、删除、选择发布和设为默认。公开 ID 全局唯一，同一上游的实际模型不重复。上游表单的模型 ID 仅作为直连默认值，修改它不会替换其他映射。

模型表头的“全选”可选择或取消当前列表内全部可发布模型，包括滚动区域中的条目；部分选中时显示横线。“已选”视图可一键取消当前选择。缺少有效模型资料的条目不参与全选，配置受管时禁止修改选择；批量选择校验通过后一次保存，失败时保留原选择。

新增映射的菜单显示名默认为“实际请求模型/上游名称”，例如 `deepseek-flash/DeepSeek`；输入或选择其他实际模型时自动更新，手动改过的显示名会保留。编辑已有映射时保留已保存的显示名。公开模型 ID 继续使用独立标识，实际请求模型仍填写上游提供的 ID。

保存 DeepSeek、Kimi、MiniMax 或小米 MiMo 的官方 API 上游时，会自动补齐预置模型映射、上下文窗口、思考等级、默认等级、输入类型和并行工具参数。预置参考 CC Switch `da193d4`（2026-09-23），共 10 个模型：DeepSeek 2 个、Kimi 2 个、MiniMax 1 个、MiMo 5 个。新上游只默认选择其默认模型发布；其余模型可在“工作台”中自行选择。重新保存已有上游只补缺少的模型，保留已保存模型的全部资料、名称、公开 ID、发布选择和备用策略。参数可继续编辑；自定义地址使用手动配置。预置沿用 SwitchX 的普通函数工具配置，未引入供应商的特殊工具和完整指令模板；仍需通过 `/models` 与实际请求验证上游能力。

手动填写参数即可生成原生 Responses 目录，无需 JSON 文件。未填上下文时使用 128000，思考等级留空则不声明推理档位；支持 `none, minimal, low, medium, high, xhigh, max, ultra`，默认等级须在支持列表中。手动目录采用文字输入和普通函数工具配置，不推断供应商的图片、并行工具、搜索或特殊工具能力。需要完整能力与指令模板时，可导入用户指定的绝对路径 Codex 目录 JSON，精确匹配实际模型 ID，保留其他字段；导入失败保留旧资料。不同上游的同名模型各自保存资料。SQLite 自动迁移到 v8，保留旧映射、已保存凭据、发布选择和备用策略；删除上游会同时删除其 API Key 与全部映射。

使用步骤：

1. 保存 Responses 上游及 API Key，在“工作台”选择连接后点击“+”手动填写参数，或导入包含实际模型 ID 的 `models.json`。映射编辑器也提供“获取模型列表”。可参考供应商提供的目录，例如 [DeepSeek 的 Codex 接入文档](https://api-docs.deepseek.com/quick_start/agent_integrations/codex)。`/models` 的 ID 列表不包含完整能力资料。
2. 选择要发布的模型和默认模型，在“设置 → Codex 工作空间”指定目标配置目录。
3. 点击“预览并启用”。SwitchX 预留所选 loopback 端口，并让本机 Codex CLI 在独立临时目录检查完整目录，以及新启动的 app-server `model/list` 返回的可选模型。可用 `SWITCHX_CODEX_CLI` 指定 CLI 程序。
4. 在预览抽屉中点击“确认启用”。再次核对 CLI、模型与上游资料；API 上游按连接读取凭据和 `/models`，订阅连接核对目标 Codex 的登录、工作区与 CLI 内置模型。验证本地路由后发布目录和配置。
5. 在 macOS 点击“启动 Codex”。SwitchX 打开 Terminal 中的新 Codex 会话，使用刚发布的配置目录和检查时选择的 CLI，不复用旧后台服务。在新会话输入 `/model` 选择模型，也可用 `-m` 参数指定公开 ID。此启动入口目前只支持 macOS。
6. 点击“恢复并停止”，或从托盘恢复、退出。macOS Command-Q 也会先恢复路由配置；冲突时保留恢复记录和仍在运行的路由，并阻止退出。活动请求最多等待 5 秒后取消。强杀后的下次启动显示待恢复目标，不自动重启路由。

部分 Codex CLI 会复用共享后台服务；该服务已经加载的模型目录不会随 `config.toml` 修改自动刷新，因此只退出并重开 CLI 可能仍显示旧模型。SwitchX 的“启动 Codex”使用 `--no-daemon` 建立新会话，不停止或修改现有后台服务。也可在终端手动启动，确保 `CODEX_HOME` 与 SwitchX 显示的目标目录相同，并使用 `SWITCHX_CODEX_CLI` 指定的同一程序：

```sh
CODEX_HOME="/absolute/codex-home" codex --no-daemon
```

目录检查会同时核对解析结果和新服务的 `model/list`，不再只以 `debug models` 能解析 JSON 作为模型菜单通过的依据。检查不读取原有账号或调用模型；目录与菜单检查通过仍不代表真实上游的工具能力或账号权限已验证。

仅含 API 模型的原生路由使用独立 `auth.command` helper 获取本地令牌，无需手工设置终端环境变量。上游 Key 保存于 SQLite，不进入 Codex 配置、请求记录或恢复 journal；API 出站请求只使用映射上游的 Key。原生 SSE 透传，取消不重放请求；服务器状态引用始终拒绝，API 模型另拒绝加密推理与压缩状态续接。应用运行中或有恢复记录时禁止修改上游、模型和绑定。成功的 `/models` 检查和目录解析不代表真实工具能力已验证。[Codex 配置约定](https://learn.chatgpt.com/docs/config-file/config-reference)

在 Codex 中切换到本次发布的其他公开模型后，“恢复并停止”会恢复开启路由前的 `model`，原配置没有该字段时移除它。新恢复记录保存已发布模型 ID；旧记录通过原发布目录识别正常模型选择，也支持先前恢复留下的部分配置。未发布的模型、外部修改的路由配置及其他受管字段仍按冲突处理。验证见 [模型切换后恢复检查](docs/acceptance/M3-model-selection-recovery-2026-09-29.md)。

每次发布生成独立的随机本地令牌，由 64 个十六进制字符组成，以明文保存在 SQLite `app_settings` 的 `local_token:router-<32hex>` 键下。API 路由的 `local-token REF ABS_DATA_DIR` helper 只读打开明确指定的数据库，不创建或迁移数据库；令牌缺失、格式无效或数据库不可用时拒绝提供凭据。配置恢复有冲突时保留令牌、路由与 journal；配置恢复完成后停止路由，删除令牌，最后移除 journal。令牌删除失败时保留 journal 和引用，排除数据库问题后可再次恢复，重启应用后也可重试。旧版无数据目录的 helper 不兼容，须先恢复原配置再重新发布。新路径的存储、helper、失败清理和恢复重试结果见 [本地令牌保存检查](docs/acceptance/M3-local-token-storage-2026-09-29.md)。

可复跑的隔离验收：

```sh
cargo build --locked --bin switchx
cargo run --locked --example routed_cli_probe
# 只验证生产路由的本地 HTTP 请求与恢复，不运行 CLI 模型请求
cargo run --locked --example routed_cli_probe -- --http-only
# 可选：使用同一套合成上游和凭据进行原生界面检查
cargo run --locked --example routed_cli_probe -- --desktop
```

`routed_cli_probe` 使用本机 CLI、两个本地假上游、临时配置和合成供应商凭据，检查目录兼容性、同名模型映射、helper 鉴权、文件工具轮次及恢复冲突，结束后恢复并清理；恢复失败则保留目录。2026-09-28 的旧版钥匙串存储已通过 Codex CLI 0.156.1 探针与原生页面配置的实际 CLI 请求，详见 [M3 首批验收记录](docs/acceptance/M3-2026-09-28.md)。该历史结果不验证本轮 SQLite API Key 存储变更，没有真实上游调用。

本地令牌改存 SQLite 之前，2026-09-29 的 API Key 存储阶段通过了 `--http-only` 检查，但完整 CLI 在 helper 读取本地令牌时超时；该历史阶段没有完成 CLI 请求或文件工具验收，见 [原检查记录](docs/acceptance/M3-api-key-storage-2026-09-29.md)。

本地令牌改存 SQLite 后，126 项自动检查通过，当前默认 `routed_cli_probe` 在 Codex CLI `0.158.0-alpha.2.1` 中完成两模型映射和 Alpha 文件工具两轮。独立 helper、写入失败时不发布、删除失败后模拟重启重试、直连 helper 回归及 macOS bundle 构建均通过；临时目录已清理。详见 [本地令牌保存检查](docs/acceptance/M3-local-token-storage-2026-09-29.md)。真实上游、Windows/Linux 和原生界面仍未验收。

单独验证新启动器和模型菜单：

```sh
cargo run --locked --example model_menu_probe -- --desktop
```

探针打印生产启动器生成的 `.command` 路径。另开终端运行该文件，只输入 `/model`，应看到 `SwitchX Mock Alpha` 和 `SwitchX Mock Beta`。关闭测试 Codex 后在探针终端按 Ctrl-C 清理临时目录。探针使用合成目录与凭据，不读取现有账号，也不发送模型请求。2026-09-29 已通过本机 Codex CLI 0.158.0 和 0.158.0-alpha.2.1 的隔离菜单检查。

当前仍是 **模型路由预览版**：一个连接可映射多个模型，修改目录需恢复后重新发布；已支持下述单个显式 API 备用上游，不提供热更新、多候选链、熔断或费用统计。订阅路由实现与隔离测试见下文；真实官方请求、实际认证续期与失效恢复、DeepSeek ↔ 官方真实切换、同会话跨上游工具历史、远端压缩、Desktop/IDE 与其他平台仍待验收，M3 尚未完整完成。

模型探测与多模型映射的隔离验收使用本地假上游和合成凭据，不调用真实供应商：

```sh
cargo build --locked --bin switchx --example routed_cli_probe
cargo run --locked --example routed_cli_probe -- --models
# 可选：打开同一套原生界面夹具，退出后清理合成凭据与临时目录
cargo run --locked --example routed_cli_probe -- --desktop-models
```

2026-09-28 的自动检查、原生映射增删改、模拟 CLI 请求及最终复跑的钥匙串授权限制见 [模型探测与映射验收记录](docs/acceptance/M3-model-mappings-2026-09-28.md)。

### ChatGPT 订阅账号路由

「连接 → 订阅账号」的 ChatGPT 卡片按账号显示 Codex 官方额度：主、副窗口分别显示剩余比例、进度条、重置倒计时和具体时间；窗口名称以服务返回的时长为准，支持 5 小时、每周、30 天及其他时长。服务提供时附带显示 Codex Credits 余额、仍可用的额度重置次数和最早到期时间。打开页面查询一次，新登录或导入后查询，支持逐账号刷新及“刷新全部”；时间文字每 30 秒更新，不自动轮询接口。查询失败保留上次成功数据并标明错误，重新登录丢弃旧快照和未完成查询的结果。

实现参考 CC Switch 的 `wham/usage` 与 `wham/rate-limit-reset-credits`，使用所选账号的 OAuth token 和工作区，后者为附带查询，失败不影响主要额度。请求固定访问 ChatGPT 官方端点，禁止重定向并限制响应大小；不发送模型请求，不启用路由，不写入原生 Codex 登录或配置。缺失的窗口或字段不会显示成 100% 剩余；不限量、缺失、无效或为零的 Credits 余额不显示。凭据刷新只保存私有账号文件；与原生登录共用凭据且需要续期时，仍要求先显式检查并续期原生登录。额度快照仅保存在内存中。验证与截图见 [ChatGPT 额度验收记录](docs/acceptance/chatgpt-quota-2026-10-09.md)。

显式验证已有默认账号的官方账单，可运行 `cargo run --locked --example codex_quota_probe -- /absolute/switchx-data /absolute/codex-home`。探针可能续期私有账号凭据，检查原生登录、配置及归属标记未改变，只输出额度数值与字段可用性，不输出凭据或账号标识；它不验证模型推理权限。

在“连接 → 上游连接”点击“+ 添加连接”，选择“ChatGPT 订阅”，输入连接名称，选择“跟随 Codex 登录”或一个保存账号。每个订阅连接独立绑定账号，可以同时发布账号 A、B 的模型；模型资料从所选本机 Codex CLI 读取，完整保留指令与工具配置。每个连接默认选择一个模型，其余可自行发布。目录不代表账号权益，实际权限以官方请求为准。

订阅编辑器按 CC Switch 截图中的操作提供 `auth.json` 与 `config.toml` 编辑区、JSON 格式化、应用/编辑通用配置、1M 窗口与压缩阈值。仅此明确打开的认证编辑区显示原始 OAuth JSON；列表、状态、TOML 预览、日志、恢复 journal 与导出不显示 token。打开编辑器不会续期或改写 Codex 登录。保持“跟随 Codex 登录”且 JSON 内容未改变时，连接继续跟随该目录；修改后保存会把登录资料保存为私有账号，并将连接固定绑定到该账号。编辑已保存账号时不能改变用户或工作区身份，编辑期间凭据被外部更新时须重新载入。

JSON、TOML 编辑区以及通用配置和供应商配置预览提供实时语法高亮，区分字段、字符串、数字、布尔值、注释与表头，并随浅色/深色主题切换。高亮保留原文、原生光标、选择和滚动；输入法组合输入期间显示原生文本。验证和合成截图见 [代码高亮检查](docs/acceptance/M3-code-highlighting-2026-09-29.md)。

TOML 文本与 1M/阈值控件双向同步，手动填写其他正整数窗口会保留；关闭 1M 删除对应窗口与阈值。允许普通 Codex 设置和不含凭据的 MCP 配置；供应商选路、认证头、密钥和 SwitchX 管理的目录字段会被拒绝。MCP 配置仅属于该连接，不进入通用片段。保存连接保留原有模型映射与发布选择，不改写目标 `config.toml` 或 `auth.json`；配置在下次开启路由时应用，“写入 Codex 登录”仍是独立操作。

账号页统一展示 ChatGPT 与 Grok 的账号、连接绑定和额度摘要。顶部“添加账号”选择账号类型，“刷新额度”查询两类账号；两者与标签栏分割线保持间距，并与下方卡片右缘对齐。卡片以首字母头像、名称与状态标记、独立的额度区组成，额度条沿卡片宽度展开并在首次出现时生长，更新时间与“详情”“更多”并排；“详情”以键值表展开连接绑定、工作区、重置时间和数据来源，“更多”提供单账号刷新、默认选择、写入登录或重新授权、移除。授权失效、额度查询错误和临近到期提示保持可见。卡片中的“Codex 文件关联”表示本机文件关联，不代表官方登录检查通过；底部“Codex 本机登录”单独提供检查和重新登录。菜单、详情、卡片增删与登录进度沿用共享动效，并支持减少动态效果。合成截图与交互验证见 [账号页验收记录](docs/acceptance/accounts-page-2026-10-09.md)。

1. 在“设置”指定目标 `CODEX_HOME`，再进入“连接 → 订阅账号”。可通过“添加账号”中的 ChatGPT 入口进行设备码登录，或明确“导入 Codex 账号”保存当前 Codex 的完整 ChatGPT 文件登录。相同工作区与用户身份重新登录会更新原账号凭据，保留账号 ID、默认选择和连接绑定。取消登录保留已有账号。
2. 选择保存账号的连接使用固定绑定，也可创建跟随目标 Codex 登录的连接。编辑订阅连接可重命名、改绑或保存配置，保留连接 ID、公开模型 ID 和发布选择。各连接即使使用相同上游模型，也有独立公开 ID。重复导入只补充缺少的映射；ID 冲突时整批回滚。
3. 卡片“更多”中的“设为默认保存账号”只改变默认保存账号。旧连接可继续跟随默认选择，但发布时解析并固定到具体账号；固定绑定不随默认选择变化。“写入 Codex 登录”是单独的显式操作，确认影响后先恢复受管配置，随后更新目标原生登录，不改变任何连接绑定。展开底部“Codex 本机登录”后，“重新登录 Codex”通过官方浏览器更新目标目录登录，也会先恢复配置并停止路由；要将该登录保存到 SwitchX，请使用“导入 Codex 账号”。工作台底部的“切回 API 预览”在恢复后只准备已选 API 模型的预览，仍需“确认启用”才发布。
4. “预览并启用 → 确认启用”分别核对每个订阅连接的账号、工作区、官方 HTTPS 目的地和区域约束。工作区在独立的私有临时 CLI 目录中发现，准备、发布和启动不切换目标账号。目标 Codex 仍须有一套 ChatGPT 入口登录 C，可以不同于上游绑定的 A、B。旧的单连接原生登录绑定继续兼容；同时发布多个订阅连接时，每个连接都须使用保存账号。
5. 已绑定的账号不能从界面移除；先删除相关连接或改绑。受管配置或恢复日志存在时禁止改绑、设默认和移除。外部删除造成失效时要求重新选择，不自动回退。接口返回 401/403 时按连接显示错误，其他连接成功不会清除它；不自动重放或备用切换。

OAuth refresh token、ID token 和账号资料单独保存到 `SWITCHX_DATA_DIR/codex_oauth_auth.json`，采用原子写入和 Unix `0600` 权限。私有账号可保存包含 access token 的完整 `auth.json` 快照，供认证编辑器读取；兼容没有该可选字段的旧记录。运行时仍使用内存 token 缓存，完整快照不进入 SQLite、预览或恢复 journal。请求续期只同步确切属于绑定账号且未被外部改动的原生登录。已发送的续期即使遇到请求取消，也会在有限超时内验证并保存轮换结果；应用退出等待这些操作结束。排队或发送前取消不会发起续期。准备阶段如需轮换与原生登录共用的凭据，会要求先显式检查并续期。

含订阅模型的配置保留 `requires_openai_auth = true`，通过独立的 `x-switchx-local-token` 请求头校验本地访问。本地令牌仅写入受限配置与恢复 journal，不转发上游。保存账号的路由使用自己的 Bearer 与工作区，忽略入口登录 C 的认证；API 模型丢弃官方认证、工作区和协议头，注入自己的 Key。Cookie 和任意客户端头不会复制到上游。订阅请求正文与 `x-codex-routing-hint` 中的模型名同步映射为实际模型，保留 `tier`；普通模型及 `tier` 提示不会被误判为旧会话状态，其他不透明提示保持原样。

`/responses` 和 `/responses/compact` 共用 SQLite 会话保护。实际 Codex `session-id`、`thread-id` 及转发元数据经过一致性检查；根会话固定到连接、保存账号、工作区和区域。相同连接换模型允许，换连接、账号或转到 API 要求新建会话；恢复和重新发布不会清除绑定。未知会话不能携带加密上下文或上游状态建立新绑定。数据库只记录身份元数据，不保存正文、token、加密内容或完整转发元数据。API 路径拒绝官方 compact/加密输入，两条路径均拒绝 `previous_response_id` 和 `conversation`。

数据库升级到 v9，将旧的全局订阅绑定迁移到原连接，保留原 ID。v8 数据库存在直连或路由恢复日志时进入仅恢复模式，恢复成功后才迁移；凭据 helper 可只读访问 v8/v9，恢复清理令牌也不触发迁移。冲突保留日志及令牌供重试。

认证透传参考 CC Switch `da193d4` 的 [官方 provider 判断](https://github.com/farion1231/cc-switch/blob/da193d4f7a6ce3710623c312245c752376c0d036/src-tauri/src/proxy/providers/codex.rs) 与 [认证透传](https://github.com/farion1231/cc-switch/blob/da193d4f7a6ce3710623c312245c752376c0d036/src-tauri/src/proxy/forwarder.rs)；2026-09-29 的账号管理方案另参考本地 CC Switch `846de29`，按用户选择增加私有 JSON 账号保存。多账号实现目前仅以合成凭据和本地 mock 验证，见 [账号保存记录](docs/acceptance/M3-managed-accounts-2026-09-29.md) 和 [上游账号绑定记录](docs/acceptance/M3-provider-account-bindings-2026-09-29.md)；真实设备登录、多账号切换、token 轮换和 Windows/Linux 权限及凭据环境仍待验收。[Codex 认证](https://learn.chatgpt.com/docs/auth)、[app-server 账号接口](https://learn.chatgpt.com/docs/app-server)

可复跑的隔离验证：

```sh
cargo run --locked --example chatgpt_route_probe
# 可选：检查合成登录状态、恢复和 API 预览；不启用真实账号
sh scripts/bundle-macos.sh
cargo run --locked --example chatgpt_route_probe -- --desktop
# 多账号本地 mock 回归
cargo test --locked --test router managed_accounts
# A/B 绑定、入口 C、API、工具回合和恢复：真实 CLI + 全部本地假服务
cargo run --locked --example managed_accounts_route_probe
# 本地编辑界面夹具：禁用 CLI；打开输出的独立 bundle，避免在线登录/续期
cargo run --locked --example managed_accounts_desktop
```

探针使用合成 ChatGPT 形状凭据、本地工作区发现服务与两个本地假上游，核对原生 CLI 的订阅/API 文件工具轮次、四条完成记录，以及配置恢复后原生登录文件保留。桌面夹具的 API 连接没有 Key，只用于恢复与预览检查。2026-09-28 已通过 CLI `0.158.0-alpha.2.1` 的隔离探针；测试范围、界面截图与未完成的真实账户验收见 [订阅路由记录](docs/acceptance/M3-subscription-2026-09-28.md)。

### 真实路由验收探针

`routed_live_probe` 从已有 SwitchX 数据目录中明确选择一或两个 **API 模型的公开 ID**；原目录须先由 SwitchX 升级到当前 v9。模型的能力与指令模板取自已保存资料，真实运行不使用仓库的合成模板。此探针创建未登录的临时 `CODEX_HOME`，不用于订阅账号验收。

```sh
cargo build --locked --bin switchx --example routed_live_probe
cargo run --locked --example routed_live_probe -- /absolute/switchx-data PUBLIC_MODEL_ID
# 两个明确选定的 API 上游
cargo run --locked --example routed_live_probe -- /absolute/switchx-data FIRST_PUBLIC_ID SECOND_PUBLIC_ID
```

源数据库只读打开。选定供应商配置、API Key 和模型资料复制到权限受限的临时 SQLite 数据库，并在独立临时 `CODEX_HOME` 中通过生产 `RouteSession` 预览、检查 `/models`、发布目录和启用 helper 路由。本地路由令牌在该临时数据库中保存和清理。只在临时副本中启用选定模型并清除备用设置，避免验收请求进入未选定的上游；原有选择与备用设置保留。

每个模型由本机 Codex CLI 完成精确短回答和“读取随机标记文件 → 工具结果 → 第二轮回答”。回答与工具检查和 `direct_live_probe` 共用；请求记录另外核对短回答至少一条、工具轮次至少两条正常完成记录，以及公开 ID、provider ID、实际模型、目录版本、HTTP 状态和首事件计时。清除继承的 API Key、base URL 与本地令牌环境变量；不读取用户配置或登录文件，不输出 Key、原始 CLI 错误或对话正文。这些真实模型调用可能计费。

成功、请求检查失败和 Ctrl-C 均执行恢复：核对临时配置字节一致、journal 消失、本地令牌删除与端口释放，再删除临时目录。Ctrl-C 在配置事务完成后取消请求，单次 CLI 最多等待 180 秒。恢复失败或有冲突时保留目录并以失败退出；强杀无法执行清理。原有上游凭据不删除。

无需真实账户的复跑使用现有假上游探针：

```sh
cargo build --locked --bin switchx --example routed_live_probe
cargo run --locked --example routed_cli_probe -- --live-probe
```

它运行同一个真实路由探针，检查两条同名模型映射、文件工具轮次、错误回答与 Ctrl-C 清理，并逐字节核对源数据库及原凭据保留。此项使用合成凭据和本地假上游，不提供真实 DeepSeek 或官方 API 验收证据；单模型通过也不代表双上游或同会话跨上游验收完成。

## 显式备用上游

在“工作台”中点击模型行右侧的“备用上游”图标，选择一个已导入资料的上游，并确认允许把完整请求发送到该站点。保存后重新预览发布，预览列出主备名称、站点和实际模型 ID，提示费用与数据接收方可能变化。备用模型无需作为独立模型发布。选择“不使用备用上游”可清除策略；删除备用上游也会清除引用。路由生效或存在恢复记录时禁止修改。

首批策略为 **主上游 → 一个备用上游，最多各尝试一次**：

- 只在 HTTP 请求发送前建立连接失败时尝试备用。未配置备用时保持原行为；主备均连接失败返回 `no_eligible_upstream`。
- 任何超时、请求已发出后的无响应/断线、HTTP 错误（包括 400、401、403、429、5xx）、SSE 中断、客户端取消和停止路由均不会触发切换。保留上游的 `Retry-After`，关闭 HTTP 客户端的默认协议重试，两次尝试共用 120 秒总预算。后续由 Codex 自己发起的重试属于另一个请求。
- 主备必须使用相同的实际模型 ID，完整能力资料和指令模板一致；仅允许显示名、说明、排序和可见性不同。不同模型自动替换、能力降级或能力交集计算尚未提供；同名不代表真实能力已验证。
- 不跟随备用模型自己的备用设置。每个新请求仍先尝试主上游；不进行并行竞速、后台探测或会话绑定。此策略仅用于 API 连接，服务器状态引用、加密内容与压缩续接仍被拒绝；订阅连接不能作为主上游或备用上游参与自动切换。
- 开启路由前，主备都要通过已有的凭据和 `/models` 检查。此功能处理路由开启之后的连接故障，不跳过启动时的验证。预览后任一站点或模型资料发生变化，必须重新预览。

SQLite 自动升级到 v8（备用字段在 v5 引入），默认不设置备用。请求记录保存原主上游 ID、实际尝试的备用上游和固定切换原因，不保存地址、Key 或正文。独立 CLI 验收使用生产 `RouteSession` 和两个本地假上游，包含备用站点上的文件工具轮次：

```sh
cargo build --locked --bin switchx
cargo run --locked --example routed_cli_probe -- --fallback
# 只使用合成凭据和临时配置的原生页面夹具
cargo run --locked --example routed_cli_probe -- --desktop-fallback
```

2026-09-28 的 mock、隔离 CLI 和原生页面检查范围见 [备用策略验收记录](docs/acceptance/M3-fallback-2026-09-28.md)。本轮没有真实供应商调用。原生页面已启用路由，隔离 Codex CLI 从页面写出的配置完成文件工具轮次，请求页显示两条完成记录；页面恢复并退出后，原配置、journal 和临时资料均核对完成。

## 请求记录与完成状态

原生路由默认把已结束请求的元数据写入 SQLite，旧库自动迁移至 v10（请求记录在 v4 引入，v5 增加备用来源）。进入“请求与用量”或点击“刷新记录”，可查看按开始时间倒序排列的最近 100 条记录：公开模型、实际尝试的上游及模型、路由版本、请求编号、上游 HTTP 状态、响应头/首事件耗时、总耗时和固定错误原因。发生备用尝试时，另显示主上游连接失败和切换去向；最后一次失败不隐藏前一次连接失败。记录不随上游删除而删除；上游名称显示当前资料，已删除的上游显示原 ID。直连请求不经过 SwitchX，不能在此记录。

| 状态 | 判定 |
| --- | --- |
| 正常完成 | 收到完整的 `response.completed` SSE 事件，或非流式 JSON 的 `status: completed`；官方 compact 则须收到完整、有效且包含压缩结果的 `response.compaction` JSON。HTTP 200、正文中提及完成或单独的 `[DONE]` 均不足以证明完成 |
| 请求失败 | 上游连接失败、非成功 HTTP、`response.failed`、`response.incomplete`、SSE `error`、非流式错误或本地校验失败 |
| 流中断 | 完成前上游读取失败/超时、缺失终态的 EOF、路由停止，或无法安全解析的响应 |
| 用户取消 / 客户端断开 | 完成前客户端连接被释放；代理无法进一步区别主动取消和下游网络断开 |

响应头 `x-switchx-request-id` 可与本地记录对应。耗时从本地鉴权后的处理开始，到首次明确终态为止；首事件是首个完整 SSE 数据事件，并非首 token。已经确认的终态不会因随后关闭连接而被覆盖。停止路由引起的取消单独记为 `router_stopping`，不会算作用户取消，也不会重放请求。

仅观察有界内存中的响应；单个 SSE 事件或非流式 JSON 超过 2 MiB、SSE 格式无效时会中断转发并记录固定错误码。有效响应字节保持原样。请求/响应正文、上游原始错误消息、URL、Cookie、认证头和 Key 均不落盘。未通过本地鉴权、正文提取阶段被拒绝的请求和 `/models` 等其他端点不进入推理记录。请求在终态时落盘，进程强杀前尚未结束的请求可能没有记录；写入失败会保留安全诊断并在刷新页面时提示。当前无自动滚动、用量计费、记录清理或导出功能。

本地 mock、隔离 Codex CLI 与原生页面的验证范围见 [请求记录验收](docs/acceptance/M3-requests-2026-09-28.md)。本轮没有真实上游请求。
