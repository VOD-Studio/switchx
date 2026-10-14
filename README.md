# SwitchX

[![CI](https://github.com/VOD-Studio/switchx/actions/workflows/ci.yml/badge.svg)](https://github.com/VOD-Studio/switchx/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/VOD-Studio/switchx?include_prereleases)](https://github.com/VOD-Studio/switchx/releases)

<img src="assets/app-icon.png" alt="SwitchX 应用图标" width="128" height="128">

为 Codex 管理模型上游的桌面应用，使用 Rust 与 Slint 构建。

SwitchX 把 API 供应商、ChatGPT 订阅和 Grok 订阅放进同一个模型目录，通过本地路由同时发布到 Codex 的模型菜单。写入 Codex 配置前先预览，之后可随时恢复原状。

当前为预览版，以 macOS 为主要平台。

## 功能

- **连接**：兼容 Responses 的 API 上游（内置 DeepSeek、Kimi、MiniMax、小米 MiMo 预设）、ChatGPT 订阅与 Grok 订阅。
- **模型目录**：获取上游模型列表并批量添加，为每个模型设置全局唯一的显示名称、上下文窗口和思考等级。
- **请求定制**：添加或编辑模型时展开「请求定制」，设置 User-Agent 预设、自定义 Header 和 Body；批量添加可统一应用。API 连接编辑器也有同一张卡片，作为该连接所有模型的默认值。保存后下次发布路由生效。
- **本地路由**：多个连接的模型同时出现在 Codex 的 `/model` 菜单中，按显示名称精确转发，各上游凭据互相隔离。内部稳定 ID 和改名前的名称保留为兼容别名，不出现在模型菜单中；历史名称由原模型保留，删除模型后释放。直连目录使用上游实际模型 ID。
- **直连**：把 Codex 直接指向单个 API 上游，不需要 SwitchX 常驻。
- **账号**：管理多个 ChatGPT 与 Grok 账号，支持设备码登录，并显示官方额度。
- **备用上游**：主上游连接失败时，改用一个预先指定的 API 上游。
- **配置恢复**：原子写入 `config.toml` 并记录恢复日志。恢复时保留外部修改，冲突会明确报告。
- **请求记录**：查看最近请求的结果、耗时、实际上游与 Token 用量，不保存正文与凭据。
- **用量统计**：按今天、7 天、30 天、90 天或全部查看 Token、缓存命中率、请求数与估算费用，按连接和模型交叉筛选，查看活跃时段与明细。路由请求实时计入；直连、原生 Codex 登录和其他提供方的用量从 Codex 会话日志增量导入。价格按模型以美元或人民币自行填写，订阅连接显示等效 API 价格。

## 安装

在 [GitHub Releases](https://github.com/VOD-Studio/switchx/releases) 下载对应平台与架构的包；macOS 打开 `.dmg` 后将 `SwitchX.app` 拖到「应用程序」，Windows 运行 `switchx.exe`，Linux 运行 `./switchx`。每个版本提供 macOS、Windows、Linux 的 x64 与 ARM64 包，以及 `SHA256SUMS` 校验文件。Linux 包在 Ubuntu 24.04 构建，需要兼容的 glibc、Fontconfig、XKB 和 OpenGL 库，以及 X11 或 Wayland 桌面。

macOS 包采用 ad-hoc 签名，尚未使用 Developer ID 签名或 Apple 公证；Windows 包尚未进行代码签名。macOS 首次打开时如提示无法验证开发者，先关闭提示，再到「系统设置 → 隐私与安全性」点击「仍要打开」；也可以在终端运行 `xattr -dr com.apple.quarantine /Applications/SwitchX.app`。请从「应用程序」启动 SwitchX，不要直接在磁盘映像中运行：Codex 配置会记录应用路径，推出映像后路由将无法取得本地凭据。应用内「启动 Codex」目前仅支持 macOS；其他平台请自行在终端启动新的 Codex 会话。

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

## 使用

1. 在「设置」中选择 Codex 配置目录。
2. 在「连接」中添加 API 上游或订阅连接；订阅账号在「连接 → 订阅账号」中登录。
3. 在「工作台」中选择要发布的模型和默认模型。
4. 点击「预览并启用」，核对改动后「确认启用」。
5. 点击「启动 Codex」打开新会话，用 `/model` 切换模型。
6. 点击「恢复并停止」，还原启用前的配置。

已有的 Codex 会话不会切换到 SwitchX 路由，启用或恢复后请新建会话。

模型的「请求定制」只作用于 SwitchX 转发给该上游的请求，直连与模型发现不使用它。Header 必须是字符串值的 JSON 对象，同名字段不区分大小写，其中的 `User-Agent` 优先于单独设置。Body 必须是 JSON 对象，在协议转换后递归合并；数组整体替换，`null` 作为值保留。认证、会话和 HTTP 传输头，以及 `model`、`stream`、`input`、`tools`、`tool_choice`、`background` 与会话续接字段不能覆盖。留空或清空后保存即可恢复默认行为。API 连接上的请求定制是该连接所有模型的默认值：发布路由时先应用连接设置，再合并模型自己的设置，同名 Header、Body 字段和 User-Agent 以模型为准；备用上游使用备用连接自己的默认值。

定制内容仅保存在本地 SQLite，不写入 Codex TOML、模型目录、恢复日志或请求记录。主备模型分别使用自己的定制配置；修改连接目录会保留已有模型的定制。单项定制总大小上限为 32 KiB。

## 数据与凭据

| 平台    | 默认数据目录                                            |
| ------- | ------------------------------------------------------- |
| macOS   | `~/Library/Application Support/SwitchX`                 |
| Windows | `%LOCALAPPDATA%\SwitchX`                                |
| Linux   | `$XDG_DATA_HOME/switchx`，默认 `~/.local/share/switchx` |

- API Key 与本地路由令牌以明文保存在 SQLite 中，数据库及其副本都包含凭据。
- ChatGPT 与 Grok 的 OAuth 凭据分别保存在 `codex_oauth_auth.json` 与 `xai_oauth_auth.json` 中，Unix 权限为 `0600`。
- API Key 与 OAuth token 不会出现在 Codex 配置、界面列表、日志或恢复记录中。
- 用量统计读取 `CODEX_HOME/sessions` 与 `archived_sessions` 下的会话日志时，只解析用量相关记录，保存 Token 数、模型 ID、会话 ID 与时间；提示词、回复、工作目录和凭据不会读取或保存。可在「用量 → 数据来源」中关闭扫描；重新导入前会在数据目录备份数据库（备份同样包含凭据）。

| 环境变量            | 作用                                                                       |
| ------------------- | -------------------------------------------------------------------------- |
| `SWITCHX_DATA_DIR`  | 改用指定的数据目录，须为绝对路径                                           |
| `SWITCHX_CODEX_CLI` | 指定 Codex CLI；默认依次查找 `PATH`、常见安装位置和 ChatGPT.app 内置的 CLI |
| `HTTPS_PROXY` 等    | 出站代理，遵循 `NO_PROXY`；未设置时使用系统代理                            |

## 开发

| 命令               | 用途                                         |
| ------------------ | -------------------------------------------- |
| `make check-lib`   | 检查业务库                                   |
| `make check-app`   | 检查桌面程序                                 |
| `make test-router` | 运行路由集成测试                             |
| `make check`       | 检查格式（含 Slint）、运行 Clippy 和全部测试 |
| `make release-check` | 检查 workspace、lockfile 和 changelog 版本 |
| `make prepare-release VERSION=0.1.0` | 更新版本并整理待发布 changelog |

`make format` 与 `make check` 需要 `slint-lsp` 1.18.1：`cargo install slint-lsp --version 1.18.1 --locked`。
版本与发布脚本需要 Python 3.11 以上；如果系统默认 Python 较旧，可使用 `make check PYTHON=python3.12`，其他 Make 命令也接受 `PYTHON`。

每次分支推送、PR 和手动运行 CI 都会在 GitHub 托管 Linux x64 runner 的 Ubuntu 24.04 容器中检查版本、Rust/Slint 格式，运行 Clippy 与全部测试。只有推送 `vX.Y.Z` 标签时，才会在 GitHub 托管的 macOS、Windows、Linux x64 与 ARM64 原生 runner 上检查、测试并构建 release 包，再自动创建包含全部构建的 GitHub Release。版本规则、changelog 与发版步骤见 [发布指南](docs/releasing.md)。

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

供应商预设和图标参考了 [CC Switch](https://github.com/farion1231/cc-switch)，图标来源与许可见 [assets/providers/NOTICE.md](assets/providers/NOTICE.md)。
