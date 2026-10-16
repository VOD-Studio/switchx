# SwitchX

[![CI](https://github.com/VOD-Studio/switchx/actions/workflows/ci.yml/badge.svg)](https://github.com/VOD-Studio/switchx/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/VOD-Studio/switchx?include_prereleases)](https://github.com/VOD-Studio/switchx/releases)

<img src="assets/app-icon.png" alt="SwitchX 应用图标" width="128" height="128">

为 Codex 管理模型上游的桌面应用，使用 Rust 与 Slint 构建。

SwitchX 把 API 供应商、ChatGPT 订阅和 Grok 订阅放进同一个模型目录，通过本地路由同时发布到 Codex 的模型菜单。写入 Codex 配置前先预览，之后可随时恢复原状。

当前为预览版，以 macOS 为主要平台。

## 功能

- **连接**：兼容 Responses 的 API 上游（内置 DeepSeek、Kimi、MiniMax、小米 MiMo 预设）、ChatGPT 订阅与 Grok 订阅。
- **模型目录**：获取上游模型列表并批量添加，为每个模型设置全局唯一的显示名称、上下文窗口、思考等级，以及文字／图片／音频输入能力。
- **请求定制**：添加或编辑模型时展开「请求定制」，设置 User-Agent 预设、自定义 Header 和 Body；批量添加可统一应用。API 连接编辑器也有同一张卡片，作为该连接所有模型的默认值。保存后下次发布路由生效。
- **本地路由**：多个连接的模型同时出现在 Codex 的 `/model` 菜单中，按显示名称精确转发，各上游凭据互相隔离。内部稳定 ID 和改名前的名称保留为兼容别名，不出现在模型菜单中；历史名称由原模型保留，删除模型后释放。直连目录使用上游实际模型 ID。
- **直连**：把 Codex 直接指向单个 API 上游，不需要 SwitchX 常驻。
- **账号**：管理多个 ChatGPT 与 Grok 账号，支持设备码登录，并显示官方额度。
- **备用上游**：主上游连接失败时，改用一个预先指定的 API 上游。
- **配置恢复**：原子写入 `config.toml` 并记录恢复日志。恢复时保留外部修改，冲突会明确报告。
- **请求记录**：查看最近请求的结果、耗时、实际上游与 Token 用量，不保存正文与凭据。
- **用量统计**：按今天、近 24 小时、近 7 / 14 / 30 / 90 天、全部或精确到分钟的自定义范围查看 Token、缓存命中率、请求数与估算费用，按客户端、连接和模型筛选，查看活跃时段与明细。路由请求实时计入；Codex 的直连、原生登录与其他提供方，以及 Claude Code、Gemini CLI、OpenCode、Grok Build、Pi、MiniMax Code 的用量均从本地日志或只读数据库导入。同一 Claude 回复的后续快照会补全原记录，不重复增加请求。价格按模型以美元或人民币自行填写，缓存读取、5 分钟写入与 1 小时写入分别计费；订阅连接显示等效 API 价格。

## 安装

在 [GitHub Releases](https://github.com/VOD-Studio/switchx/releases) 下载对应平台与架构的包；macOS 打开 `.dmg` 后将 `SwitchX.app` 拖到「应用程序」，Windows 运行 `switchx.exe`。Linux 提供 `.AppImage` 与 `.tar.gz`：AppImage 添加执行权限后直接运行，tar.gz 解压后运行 `./switchx`。每个版本提供 macOS、Windows、Linux 的 x64 与 ARM64 包，以及 `SHA256SUMS` 校验文件。Linux 通用包在 Ubuntu 24.04 构建，需要兼容的 glibc、Fontconfig、XKB 和 OpenGL 库，以及 X11 或 Wayland 桌面；AppImage 随包携带非系统基础依赖，仍使用宿主的图形驱动与桌面环境。

Arch Linux / Omarchy x86_64 另有原生 `.pkg.tar.zst` 包，在 Arch 环境构建。下载后用 `sudo pacman -U ./switchx-<版本>-1-x86_64.pkg.tar.zst` 安装，随后可从应用菜单或终端运行 `switchx`；程序安装在 `/usr/bin/switchx`，用户数据仍保存在原来的数据目录。卸载使用 `sudo pacman -R switchx`。从 AppImage 或源码运行切换过来时，请先恢复原有直连或路由，再从已安装的程序重新启用，使凭据 helper 使用新的固定路径。

```sh
chmod +x SwitchX-0.1.0-x86_64-unknown-linux-gnu.AppImage
./SwitchX-0.1.0-x86_64-unknown-linux-gnu.AppImage
```

请把 AppImage 保存在固定位置后再启用直连或路由，Codex 凭据配置会记录这个文件的绝对路径。移动或重命名后需重新预览并启用配置。没有可用 FUSE 的环境可以运行 `APPIMAGE_EXTRACT_AND_RUN=1 ./SwitchX-0.1.0-x86_64-unknown-linux-gnu.AppImage`，此模式每次启动会解压文件；Codex 调用凭据程序时也需要这个变量，请在同一终端中先 `export APPIMAGE_EXTRACT_AND_RUN=1`，再启动 SwitchX 和 Codex。

macOS 包采用 ad-hoc 签名，尚未使用 Developer ID 签名或 Apple 公证；Windows 包尚未进行代码签名。macOS 首次打开时如提示无法验证开发者，先关闭提示，再到「系统设置 → 隐私与安全性」点击「仍要打开」；也可以在终端运行 `xattr -dr com.apple.quarantine /Applications/SwitchX.app`。请从「应用程序」启动 SwitchX，不要直接在磁盘映像中运行：Codex 配置会记录应用路径，推出映像后路由将无法取得本地凭据。请自行在终端运行 `codex --no-daemon` 启动新的 Codex 会话。

从源码运行需要 [rust-toolchain.toml](rust-toolchain.toml) 中指定的 Rust（rustup 会自动安装）与 Git LFS。发布路由前，SwitchX 会调用本机的 Codex CLI 检查模型目录。

```sh
git lfs install
git clone https://github.com/VOD-Studio/switchx.git
cd switchx
cargo run
```

界面图片由 Git LFS 保存。克隆后如果图片仍是文本指针，构建会提示运行 `git lfs pull`；下载完成后重新运行 `cargo run`。

在 macOS 上打包：

```sh
sh scripts/bundle-macos.sh            # target/debug/SwitchX.app
sh scripts/bundle-macos.sh --release  # target/release/SwitchX.app
```

生成的应用采用 ad-hoc 签名，未使用 Developer ID 签名或公证。打包脚本也支持 `--target TRIPLE` 和 `CARGO_TARGET_DIR`。需要与发布版相同的 DMG 时，运行 `python3 scripts/package-release.py --target aarch64-apple-darwin`（target 须与本机 Rust host 一致），结果写入 `dist/`。

在 Linux 上运行 `make appimage`，会构建本机架构的 release 程序并在 `dist/` 生成 `.AppImage`、`.tar.gz` 及各自的校验文件。除 Rust、Git LFS 和原生 GUI 构建依赖外，还需 Python 3.11+、binutils、ImageMagick、`desktop-file-validate`（desktop-file-utils）和 `unsquashfs`（squashfs-tools）。首次打包会从官方 GitHub Releases 下载固定版本的 linuxdeploy、appimagetool 和 AppImage runtime，校验 SHA-256 后缓存到 `target/appimage-tools/`。完整步骤与兼容范围见 [发布指南](docs/releasing.md#linux-appimage)。

在 Arch Linux / Omarchy x86_64 上运行 `make arch-package`，会通过 `makepkg` 从源码构建，在 `dist/arch/` 生成原生包、SHA-256 校验文件以及对应的源码包、`PKGBUILD` 和 `.SRCINFO`。依赖安装、固定 Rust 工具链及验证方式见 [Arch 打包指南](docs/releasing.md#arch-linux)。

## 使用

1. 在「设置」中选择 Codex 配置目录。
2. 在「连接」中添加 API 上游或订阅连接；订阅账号在「连接 → 订阅账号」中登录。
3. 在「工作台」中选择要发布的模型和默认模型。
4. 点击「预览并启用」，核对改动后「确认启用」。
5. 在终端运行 `codex --no-daemon` 打开新会话，用 `/model` 切换模型。
6. 点击「恢复并停止」，还原启用前的配置。

已有的 Codex 会话不会切换到 SwitchX 路由，启用或恢复后请新建会话。

模型的「输入能力」可在模型编辑页或连接目录中修改：文字始终开启，图片和音频独立选择；全部关闭即为纯文本。「沿用资料」保留已存或导入的输入能力，新建且无资料时默认纯文本。批量添加时，单个模型的选择优先于统一输入能力，统一设置优先于预设或官方资料。保存后需重新预览、发布并重启 Codex；声明能力不会改变上游实际支持情况，也不保证客户端提供对应的输入入口。

模型的「请求定制」只作用于 SwitchX 转发给该上游的请求，直连与模型发现不使用它。Header 必须是字符串值的 JSON 对象，同名字段不区分大小写，其中的 `User-Agent` 优先于单独设置。Body 必须是 JSON 对象，在协议转换后递归合并；数组整体替换，`null` 作为值保留。认证、会话和 HTTP 传输头，以及 `model`、`stream`、`input`、`tools`、`tool_choice`、`background` 与会话续接字段不能覆盖。留空或清空后保存即可恢复默认行为。API 连接上的请求定制是该连接所有模型的默认值：发布路由时先应用连接设置，再合并模型自己的设置，同名 Header、Body 字段和 User-Agent 以模型为准；备用上游使用备用连接自己的默认值。

定制内容仅保存在本地 SQLite，不写入 Codex TOML、模型目录、恢复日志或请求记录。主备模型分别使用自己的定制配置；修改连接目录会保留已有模型的定制。单项定制总大小上限为 32 KiB。

## 供 AI Agent 使用（MCP）

`switchx mcp` 在标准输入输出上运行一个 [MCP](https://modelcontextprotocol.io) 服务，Codex、Claude Code 等 agent 可以直接查询 SwitchX，默认只读。

最简单的接入方式是打开应用的「工具箱」：选择客户端（Codex、Claude Code 或其他使用 `mcpServers` JSON 的客户端）并按需开启下面的权限，然后点「复制一句话」，把这句话发给 agent，它会自己运行注册命令，并在新会话中调用 `get_status` 确认可用。也可以复制注册命令或 JSON 配置手动注册。命令使用当前 SwitchX 程序的完整路径，设置了 `SWITCHX_DATA_DIR` 时一并传给 MCP 服务；工具箱还会识别当前工作空间的 Codex 是否已注册以及注册时的权限。


| 工具             | 内容                                                                     |
| ---------------- | ------------------------------------------------------------------------ |
| `get_status`     | Codex 配置是否由 SwitchX 托管（路由 / 直连）、连接与模型数量、最近请求和今日用量 |
| `list_providers` | 连接的名称、类型、服务地址、默认模型、凭据是否已配置与账号绑定           |
| `list_models`    | 模型映射：公开模型 ID、实际上游模型、上下文窗口、推理档位、输入能力、启用状态与备用连接 |
| `get_usage`      | 按预设或自定义时间段的 Token 用量、估算费用、按连接 / 模型分布与明细     |
| `list_requests`  | 最近经路由的请求：状态、耗时、错误原因、备用切换与 Token 数              |
| `list_prices`    | 已填写的模型价格（每百万 Token），以及各模型的累计请求与 Token 数        |
| `get_app_status` | 正在运行的应用的实时状态：本地路由是否运行、目标 Codex 配置的当前模式与模型 |

读取时以只读方式打开现有数据库，不会创建或迁移数据；任何工具都不接收也不返回 API Key、OAuth token、本地路由令牌或凭据引用。首次安装或升级后请先打开一次应用完成数据库初始化。路由进程是否在运行、账号额度等实时状态仍需在应用中查看。

```sh
codex mcp add switchx -- /Applications/SwitchX.app/Contents/MacOS/switchx mcp
```

```sh
claude mcp add --scope user switchx -- /Applications/SwitchX.app/Contents/MacOS/switchx mcp
```

用 `switchx mcp --allow-writes` 启动时，agent 还可以修改模型和价格：

| 工具                 | 作用                                                   |
| -------------------- | ------------------------------------------------------ |
| `save_model`         | 添加或编辑模型名称、实际模型、上下文、思考等级、输入能力、导入资料与请求定制；省略字段保留原值 |
| `delete_model`       | 删除模型映射；不删除连接或凭据 |
| `set_models_enabled` | 启用或停用已保存的模型映射                             |
| `set_model_fallback` | 设置或清除模型的备用 API 连接                          |
| `set_price`          | 设置模型每百万 Token 的输入、缓存输入与输出价格        |
| `delete_price`       | 删除模型价格                                           |

例如先调用 `list_models` 获取 `public_id`，再调用 `save_model`：

```json
{"public_id": "sx-example", "input_modalities": ["text", "image"]}
```

改为 `["text"]` 即关闭多模态；加入 `"audio"` 可声明音频输入。省略 `input_modalities` 保留当前资料。`request_overrides` 省略时保留，传对象整体替换，传 `null` 清空；工具只返回是否配置，不返回自定义 Header 或 Body 内容。

写入与应用内操作使用相同的校验。路由或直连启用期间，模型相关修改会被拒绝，需先在应用中恢复原配置；价格只影响费用估算，随时可改。修改在下次发布路由后才影响 Codex。正在运行的 SwitchX 会在约 2 秒内自动刷新界面。连接、API Key 和账号仍只能在应用中管理。

```sh
codex mcp add switchx -- /Applications/SwitchX.app/Contents/MacOS/switchx mcp --allow-writes
```

加上 `--allow-control`（可与 `--allow-writes` 同时使用）后，agent 还可以让正在运行的 SwitchX 应用改写 Codex 配置：

| 工具             | 作用                                                     |
| ---------------- | -------------------------------------------------------- |
| `preview_route`  | 准备模型路由发布，返回将写入的变更与 `preview_id`        |
| `apply_route`    | 按预览启用模型路由                                       |
| `preview_direct` | 准备让 Codex 直连一个 API 连接，返回变更与 `preview_id`  |
| `apply_direct`   | 按预览启用直连                                           |
| `cancel_preview` | 放弃尚未应用的预览                                       |
| `restore_config` | 恢复原配置并停止本地路由                                 |

这些操作由应用执行，与在界面中操作相同，结果也会显示在应用里；需要应用正在运行。应用必须使用 agent 自己刚生成的预览：每个 `preview_id` 只能用一次，用户在应用中的任何其他操作都会使它失效。路由端口、默认模型和 Codex 目录省略时使用应用中当前的选择。应用启动后只在本机回环地址监听，并把端口和随机令牌写入数据目录中仅所有者可读的 `control-endpoint.json`，退出时删除；令牌不会出现在工具输出、日志或界面中。在「工具箱」中关闭「允许 agent 控制 SwitchX」会立即停止监听并删除该文件，此后控制工具和 `get_app_status` 都会被拒绝；这个选择保存在本地数据库中，重启后保持。

Linux 包使用 `/usr/bin/switchx` 或 tar.gz 解压出的 `switchx`，Windows 使用 `switchx.exe` 的完整路径。设置 `SWITCHX_DATA_DIR` 时，MCP 服务读取同一数据目录。

## 数据与凭据

| 平台    | 默认数据目录                                            |
| ------- | ------------------------------------------------------- |
| macOS   | `~/Library/Application Support/SwitchX`                 |
| Windows | `%LOCALAPPDATA%\SwitchX`                                |
| Linux   | `$XDG_DATA_HOME/switchx`，默认 `~/.local/share/switchx` |

- API Key 与本地路由令牌以明文保存在 SQLite 中，数据库及其副本都包含凭据。
- ChatGPT 与 Grok 的 OAuth 凭据分别保存在 `codex_oauth_auth.json` 与 `xai_oauth_auth.json` 中，Unix 权限为 `0600`。
- API Key 与 OAuth token 不会出现在 Codex 配置、界面列表、日志或恢复记录中。
- 用量统计读取所选 Codex 目录下的 `sessions`、`archived_sessions`，以及 Claude 配置目录下的 `projects`（含子 agent 和 Workflow 日志）。Claude 目录默认使用绝对路径的 `CLAUDE_CONFIG_DIR`，否则使用 `~/.claude`；可在「用量 → 数据来源」修改目录并分别关闭自动扫描。只解析计数、模型、消息/会话 ID 和时间，不解析或保存提示词、回复与凭据。
- 会话日志在启动及之后每分钟增量同步，未完成的尾行留到下次读取；文件替换或截断会重新读取，并按消息 ID 去重。「同步全部日志」和「重新导入」会手动读取全部已配置来源，包括已关闭自动扫描的来源；重新导入前会备份数据库（备份同样包含凭据）。本机没有保留的日志无法补回；这不是 Claude 网页聊天或订阅剩余额度查询。
- 客户端筛选覆盖 Codex、Claude Code、Gemini CLI、OpenCode、Grok Build、Pi、MiniMax Code，以及未识别客户端。路由记录能与日志的响应 ID 精确匹配时，会合并用量并保留客户端归属和上报费用；其余不从模型、时间或供应商猜测关联。各客户端的日志显示为独立会话分组，不自动绑定 SwitchX 连接或账号。
- 费用按当前填写的价格估算；默认写入倍率参考 [Claude 缓存计费说明](https://platform.claude.com/docs/en/build-with-claude/prompt-caching#pricing)。缓存写入留空时，5 分钟默认按输入价的 1.25 倍、1 小时按 2 倍，可分别覆盖；缓存读取留空仍按输入价。旧定价升级后保留原来的写入价格，避免改变已有估算。没有 TTL 分项的历史缓存写入按 5 分钟计；写入总数包含 1 小时分项，统计不会重复相加。

| 环境变量            | 作用                                                                       |
| ------------------- | -------------------------------------------------------------------------- |
| `SWITCHX_DATA_DIR`  | 改用指定的数据目录，须为绝对路径                                           |
| `SWITCHX_CODEX_CLI` | 指定 Codex CLI；默认依次查找 `PATH`、常见安装位置和 ChatGPT.app 内置的 CLI |
| `HTTPS_PROXY` 等    | 出站代理，遵循 `NO_PROXY`；未设置时使用系统代理                            |

五种附加来源可在「用量 → 数据来源」分别设置绝对路径和自动同步开关：

| 客户端 | 默认来源 | 同步口径 |
| --- | --- | --- |
| Gemini CLI | `~/.gemini/tmp/*/chats/session-*.json(l)` | 兼容 JSON 与 JSONL 更新、检查点和迁移残留；思考 Token 计入输出；会话回退不撤销已发生的用量 |
| OpenCode | `~/.local/share/opencode/opencode.db` | 只读访问 SQLite 及 WAL，优先 V2；包括完成或失败的压缩记录，未完成消息待后续写入补齐 |
| Grok Build | `~/.grok/{sessions,archived_sessions}/…/updates.jsonl` | 按 `turn_completed` 的每轮、每模型独立总量入账；请求数使用 `modelCalls`（缺失时记 1），不做相邻轮差分 |
| Pi | `~/.pi/agent/sessions` | 包含助手、工具结果、压缩和分支摘要用量；复制或分支中的相同条目去重 |
| MiniMax Code | `~/.minimax/v2/sqlite/runtime-state.sqlite` | 只读访问已提交用量流水，以会话 ID 与流水 ID 去重；变化时校验已有流水的费用修订 |

- OpenCode 支持绝对路径的 `OPENCODE_DB` 和 `XDG_DATA_HOME`；Pi 支持 `PI_CODING_AGENT_SESSION_DIR`、`PI_CODING_AGENT_DIR`；MiniMax Code 支持 `MINIMAX_DATA_DIR`。应用中的自定义来源路径优先。Pi 若在自身设置里配置了其他会话目录，可在这里指定相同位置。
- JSONL 只处理完整新增行，保存非正文的解析状态与边界指纹；JSON 文件和 SQLite 在源数据变化时检查用量快照，按稳定 ID 插入或补全。源数据库不会迁移或改写，原有导入记录在来源文件删除后仍保留，直到显式重建。
- 客户端完整上报的美元费用优先于本地估价。MiniMax Code 的显式零费用保留为零；OpenCode/Pi 的零值可能只是缺少定价，因此回退本地价格。Grok 的 `costIsPartial` 不作为完整费用；无完整金额且无模型价格时显示未定价。费用修订不会增加请求数。
- Gemini、OpenCode、MiniMax Code 的独立思考/推理 Token 会合并进输出；Grok 的推理已包含在输出中，不重复相加。缓存与普通输入统一成现有用量页的口径。
- 无共同响应 ID 的路由请求与按轮聚合日志无法可靠逐条对应，不按相似 Token 数或邻近时间猜测去重。若手动将额外客户端接到 SwitchX 路由，应选择适合的记录来源，避免把两种口径相加。

## 开发

| 命令               | 用途                                         |
| ------------------ | -------------------------------------------- |
| `make check-lib`   | 检查业务库                                   |
| `make check-app`   | 检查桌面程序                                 |
| `make test-router` | 运行路由集成测试                             |
| `make arch-package` | 在 Arch Linux x86_64 构建 pacman 包 |
| `make check`       | 检查格式（含 Slint）、运行 Clippy 和全部测试 |
| `make release-check` | 检查 workspace、lockfile 和 changelog 版本 |
| `make prepare-release VERSION=0.1.0` | 更新版本并整理待发布 changelog |

`make format` 与 `make check` 需要 `slint-lsp` 1.18.1：`cargo install slint-lsp --version 1.18.1 --locked`。
版本与发布脚本需要 Python 3.11 以上；如果系统默认 Python 较旧，可使用 `make check PYTHON=python3.12`，其他 Make 命令也接受 `PYTHON`。

每次分支推送、PR 和手动运行 CI 都会在 GitHub 托管 Linux x64 runner 的 Ubuntu 24.04 容器中检查版本、Rust/Slint 格式，运行 Clippy 与全部测试。只有推送 `vX.Y.Z` 标签时，才会在 GitHub 托管的 macOS、Windows、Linux x64 与 ARM64 原生 runner 上检查、测试并构建 release 包，同时在 Arch 容器内构建并验证 x86_64 pacman 包，再自动创建包含全部构建的 GitHub Release。版本规则、changelog 与发版步骤见 [发布指南](docs/releasing.md)。

测试只使用本地 mock 和合成数据，不需要账号或 API Key。手动试验配置流程时，请使用隔离的 `SWITCHX_DATA_DIR` 和 `CODEX_HOME`。`deepseek_live_probe`、`direct_live_probe` 和 `routed_live_probe` 会发起真实模型请求，可能产生费用。

```
src/                业务库：路由、存储、凭据与 Codex 配置
src/desktop/        桌面外壳：窗口回调、视图更新与后台命令
crates/switchx-ui/  独立编译的 Slint 界面
ui/                 页面、编辑器、组件与主题
tests/              集成测试与合成夹具
examples/           探针与界面预览
scripts/            版本管理、多平台发布与 macOS 打包脚本
.github/            推送 CI 与标签发布工作流
```

## 致谢

SwitchX 采用 [MIT 许可证](LICENSE)。第三方资源保留各自的许可声明。

供应商预设和图标参考了 [CC Switch](https://github.com/farion1231/cc-switch)，图标来源与许可见 [assets/providers/NOTICE.md](assets/providers/NOTICE.md)。
