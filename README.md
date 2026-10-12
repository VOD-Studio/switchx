# SwitchX

<img src="assets/app-icon.png" alt="SwitchX 应用图标" width="128" height="128">

为 Codex 管理模型上游的桌面应用，使用 Rust 与 Slint 构建。

SwitchX 把 API 供应商、ChatGPT 订阅和 Grok 订阅放进同一个模型目录，通过本地路由同时发布到 Codex 的模型菜单。写入 Codex 配置前先预览，之后可随时恢复原状。

当前为预览版，以 macOS 为主要平台。

## 功能

- **连接**：兼容 Responses 的 API 上游（内置 DeepSeek、Kimi、MiniMax、小米 MiMo 预设）、ChatGPT 订阅与 Grok 订阅。
- **模型目录**：获取上游模型列表并批量添加，为每个模型设置公开 ID、显示名、上下文窗口和思考等级。
- **本地路由**：多个连接的模型同时出现在 Codex 的 `/model` 菜单中，按公开 ID 精确转发，各上游凭据互相隔离。
- **直连**：把 Codex 直接指向单个 API 上游，不需要 SwitchX 常驻。
- **账号**：管理多个 ChatGPT 与 Grok 账号，支持设备码登录，并显示官方额度。
- **备用上游**：主上游连接失败时，改用一个预先指定的 API 上游。
- **配置恢复**：原子写入 `config.toml` 并记录恢复日志。恢复时保留外部修改，冲突会明确报告。
- **请求记录**：查看最近请求的结果、耗时和实际上游，不保存正文与凭据。

## 安装

需要最新稳定版 Rust 与 Git LFS。发布路由前，SwitchX 会调用本机的 Codex CLI 检查模型目录。

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

生成的应用未签名、未公证，仅供本机使用。

## 使用

1. 在「设置」中选择 Codex 配置目录。
2. 在「连接」中添加 API 上游或订阅连接；订阅账号在「连接 → 订阅账号」中登录。
3. 在「工作台」中选择要发布的模型和默认模型。
4. 点击「预览并启用」，核对改动后「确认启用」。
5. 点击「启动 Codex」打开新会话，用 `/model` 切换模型。
6. 点击「恢复并停止」，还原启用前的配置。

已有的 Codex 会话不会切换到 SwitchX 路由，启用或恢复后请新建会话。

## 数据与凭据

| 平台    | 默认数据目录                                            |
| ------- | ------------------------------------------------------- |
| macOS   | `~/Library/Application Support/SwitchX`                 |
| Windows | `%LOCALAPPDATA%\SwitchX`                                |
| Linux   | `$XDG_DATA_HOME/switchx`，默认 `~/.local/share/switchx` |

- API Key 与本地路由令牌以明文保存在 SQLite 中，数据库及其副本都包含凭据。
- ChatGPT 与 Grok 的 OAuth 凭据分别保存在 `codex_oauth_auth.json` 与 `xai_oauth_auth.json` 中，Unix 权限为 `0600`。
- API Key 与 OAuth token 不会出现在 Codex 配置、界面列表、日志或恢复记录中。

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

`make format` 与 `make check` 需要 `slint-lsp` 1.18.1：`cargo install slint-lsp --version 1.18.1 --locked`。

测试只使用本地 mock 和合成数据，不需要账号或 API Key。手动试验配置流程时，请使用隔离的 `SWITCHX_DATA_DIR` 和 `CODEX_HOME`。`deepseek_live_probe`、`direct_live_probe` 和 `routed_live_probe` 会发起真实模型请求，可能产生费用。

```
src/                业务库：路由、存储、凭据与 Codex 配置
src/desktop/        桌面外壳：窗口回调、视图更新与后台命令
crates/switchx-ui/  独立编译的 Slint 界面
ui/                 页面、编辑器、组件与主题
tests/              集成测试与合成夹具
examples/           探针与界面预览
scripts/            macOS 打包脚本
```

## 致谢

供应商预设和图标参考了 [CC Switch](https://github.com/farion1231/cc-switch)，图标来源与许可见 [assets/providers/NOTICE.md](assets/providers/NOTICE.md)。
