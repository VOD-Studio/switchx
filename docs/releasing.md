# 版本与发布

## 版本规则

`Cargo.toml` 的 `[workspace.package].version` 是唯一版本来源。应用和 `switchx-ui` 通过 `version.workspace = true` 继承，`Cargo.lock`、`switchx --version`、macOS 包和发布文件名必须与它一致。Rust 工具链固定在 `rust-toolchain.toml`，依赖构建一律使用 `--locked`。

遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)：补丁版本用于兼容修复，次版本用于新功能，主版本用于不兼容变更。`0.x` 阶段需要不兼容改动时递增次版本，并在 changelog 说明迁移要求。预发布使用 `0.2.0-rc.1` 等格式，对应标签为 `v0.2.0-rc.1`。发版工具不接受 `v` 前缀或 `+build` 元数据；标签统一使用 `v` 前缀。

macOS 的 `CFBundleShortVersionString` 与 `CFBundleVersion` 使用数值版本（如 `0.2.0`），完整预发布版本写入 `SwitchXVersion`，并通过 CLI 和发布文件名标识。应用的 Bundle ID 保持 `dev.switchx.preview`。

## Changelog

按 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 维护根目录 `CHANGELOG.md`：

- 每个功能或修复及时写入 `Unreleased`，描述用户可见的行为；不要简单复制 Git 提交列表。
- 使用 `Added`、`Changed`、`Deprecated`、`Removed`、`Fixed`、`Security` 分类，只保留有内容的分类。
- 正式条目格式为 `## [0.1.0] - 2026-10-12`，按版本从新到旧排列。
- 配置、数据格式或凭据存储变化应说明影响与迁移步骤，不记录真实账号或密钥。

首次发布可以使用初始版本 `0.1.0`。此后每次发布都必须递增版本，已经发布的版本与标签不得覆盖。

## 发版步骤

发布脚本只使用 Python 标准库，需要 Python 3.11 以上。以下命令使用 `python3`；默认版本较旧时为 Make 命令添加 `PYTHON=python3.12`。

```sh
# 发布当前初始版本；之后改用 0.1.1、0.2.0 或 0.2.0-rc.1 等新版本。
make prepare-release VERSION=0.1.0
```

这会同步 workspace 与两个本地 crate 的 lockfile 版本，将 `Unreleased` 内容移到当天的版本条目，留下空的 `Unreleased`。不会更新第三方依赖、提交、创建标签或推送。先核对与整理 `CHANGELOG.md`，再执行：

```sh
make check
python3 scripts/release.py check --tag v0.1.0
git diff
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "feat: prepare release v0.1.0"
git tag -a v0.1.0 -m "SwitchX v0.1.0"
git push origin main
git push origin v0.1.0
```

后续发版将上面的版本统一替换成实际新版本。先确认准备发布的功能已经提交，且工作区干净；如存在其他改动，单独处理，不要将它们混入发版提交。

在 [Release 工作流](https://github.com/VOD-Studio/switchx/actions/workflows/release.yml) 中确认所有检查与构建成功，再到 [Releases](https://github.com/VOD-Studio/switchx/releases) 核对附件。普通分支推送、PR 和手动 CI 只运行检查与测试；版本标签推送才会构建六个平台的产物并发布。

检查任务使用 GitHub 托管的 `ubuntu-24.04` x64 runner，在 Ubuntu 24.04 容器内安装 Git LFS、Python 3.12、仓库固定的 Rust 工具链和 Slint 格式化工具，再运行 `make check`。默认情况下，普通 CI、发版前置检查和六个平台构建均使用 GitHub 托管 runner；检查任务可通过下文的仓库变量切换到自托管 runner。

## 构建矩阵与附件

| 平台 | 架构 | Rust target | 发布文件 |
| --- | --- | --- | --- |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.dmg`，包含 `SwitchX.app` |
| macOS | Intel | `x86_64-apple-darwin` | `.dmg`，包含 `SwitchX.app` |
| Windows | x64 | `x86_64-pc-windows-msvc` | `.zip`，包含 `switchx.exe` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` | `.zip`，包含 `switchx.exe` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `.AppImage` 与包含 `switchx` 的 `.tar.gz` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` | `.AppImage` 与包含 `switchx` 的 `.tar.gz` |

版本标签通过 GitHub 托管 runner 的检查后，每个平台使用 [GitHub 提供的对应架构原生 runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)，先跑 Clippy、单元和集成测试，再构建 release 二进制。包名为 `SwitchX-<version>-<target>.<extension>`，包内包含 changelog、README、供应商图标许可和记录版本/target/源提交/工作区是否有改动/Rust 工具链的 `BUILD-INFO.json`。macOS 包是 HFS+ 格式的 UDZO 压缩磁盘映像，以上文件位于卷根目录，另有指向「应用程序」的快捷方式供拖拽安装；打包后会挂载映像，确认其中的 `SwitchX.app` 签名完整。Windows 构建静态链接 CRT；Linux 包在 Ubuntu 24.04 构建，不承诺兼容更旧的 glibc。

Release 发布前会校验标签、workspace 版本、lockfile 和 changelog，要求六个平台的八个包（Linux 每种架构各两个）与各自 `.sha256` 文件齐全，核对包内版本与源提交、拒绝从有未提交改动的工作区构建的包，并生成 `SHA256SUMS`。发布 job 在 GitHub 托管的 `macos-15` runner 上用 `hdiutil` 挂载 DMG，用 Homebrew `squashfs` 提供的 `unsquashfs` 读取两个架构 AppImage 内的 `BUILD-INFO.json`，无需执行 Linux 程序。任一检查失败都会阻止发布。全部完成后先创建 draft、上传附件，再公开 Release；上传失败时 draft 可以在重跑工作流后继续。已经公开的 Release 不会被覆盖。预发布版本自动标为 prerelease，且不会成为 latest。

下载后可在 Linux 使用 `sha256sum -c SHA256SUMS`，macOS 使用 `shasum -a 256 -c SHA256SUMS`；如果只下载一个平台包，使用对应 `.sha256` 文件。Windows 可使用 PowerShell 的 `Get-FileHash <文件名> -Algorithm SHA256` 与校验文件比对。

## GitHub 设置

工作流使用 GitHub 自动提供的 `GITHUB_TOKEN`；只有发布 job 请求 `contents: write`，其他 job 为只读。无需个人访问令牌或发布密钥。仓库需启用 GitHub Actions，并允许发布 job 的写权限。actions 固定到完整提交 SHA；更新时一起核对版本注释与 SHA。

### 自托管 runner 开关

仅检查任务（Linux x64，`build.yml` 的 `check`）支持切换；六个平台构建和发布 job 始终使用 GitHub 托管 runner。在仓库 Settings → Secrets and variables → Actions → Variables 中配置：

| 变量 | 取值 | 作用 |
| --- | --- | --- |
| `USE_SELF_HOSTED_RUNNER` | `true` 启用；未设置或其他值为关闭 | 检查任务改用自托管 runner |
| `SELF_HOSTED_RUNNER_LABELS` | JSON 数组，可选，默认 `["self-hosted","linux","x64"]` | 自托管 runner 的标签 |

```sh
gh variable set USE_SELF_HOSTED_RUNNER --body true
gh variable set SELF_HOSTED_RUNNER_LABELS --body '["self-hosted","linux","x64","switchx"]'
gh variable delete USE_SELF_HOSTED_RUNNER   # 回退到 GitHub 托管 runner
```

仓库是公开的，来自 fork 的 PR 会忽略该开关，始终使用 GitHub 托管 runner，避免外部代码在自托管机器上运行。检查任务使用 `ubuntu:24.04` 容器，自托管 runner 需要 Linux x64 并安装 Docker；建议使用临时（ephemeral）runner，并在 Settings → Actions → General 中要求外部协作者的 PR 先经批准再运行工作流。若启用后没有匹配标签的在线 runner，任务会一直排队，需删除变量回退。

macOS 当前使用 ad-hoc 签名，未做 Developer ID 签名与公证，DMG 本身不签名；用户首次打开需在「系统设置 → 隐私与安全性」中允许。Windows 未做 Authenticode 签名。正式签名需要另行配置相应证书与账号；当前工作流提供可下载的原生包。

本地打包可运行以下命令（target 必须匹配本机 Rust host），结果写入 `dist/`；macOS 上生成 `.dmg`：

```sh
python3 scripts/package-release.py --target aarch64-apple-darwin
```

## Linux AppImage

在 x64 或 ARM64 Linux 原生主机上安装仓库的 Rust 工具链、Git LFS、GUI 构建依赖，以及 Python 3.11+、binutils、ImageMagick、desktop-file-utils、squashfs-tools，然后运行：

```sh
make appimage
# 或显式指定与本机 rustc host 一致的 target：
python3 scripts/package-release.py --target x86_64-unknown-linux-gnu
```

两个命令都会生成本机架构的 `.AppImage` 和原有 `.tar.gz`。使用 `CARGO_TARGET_DIR` 时，程序从 Cargo 报告的输出目录读取；工具下载缓存仍在仓库的 `target/appimage-tools/`。`make release` 仍用于 macOS bundle。

`scripts/appimage.py` 从官方发布地址下载 linuxdeploy `1-alpha-20251107-1`、appimagetool `1.9.1` 和 type2-runtime `20251108`，在下载后与每次复用缓存前核对固定 SHA-256。升级工具时应同时更新 URL 版本与两个架构的校验值。打包按 [linuxdeploy 流程](https://docs.appimage.org/packaging-guide/from-source/linuxdeploy-user-guide.html) 收集依赖，使用 `packaging/linux/` 的桌面入口与 AppRun，以及现有应用图标。工具使用 extract-and-run，CI 构建不需要 FUSE。构建完成会在隔离目录中验证版本、API Key helper、路由令牌 helper、数据库未修改和包内构建元数据，全部使用合成凭据。

发布版在 Ubuntu 24.04 构建；AppImage 不降低 glibc 最低版本要求，仍需宿主提供 Fontconfig、XKB、OpenGL 驱动及 X11/Wayland 桌面。较新发行版上的本地构建可能要求更高的 glibc，分发包应使用 CI 的基线环境。

AppImage 启动时，凭据路径解析仅在当前程序位于其 AppDir 布局内时采用运行时的 `APPIMAGE` 绝对路径，防止继承其他 AppImage 的环境变量而误指向别的程序。配置保留外部文件路径，不保存每次启动生成的临时挂载路径。移动、重命名或删除文件后需重新启用配置；手动解压安装应将整个 AppDir 保存在固定位置。
