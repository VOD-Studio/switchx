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

仓库目前的 `0.1.0` 尚未发布，初始功能记录在 `Unreleased`，因此首次可以准备 `0.1.0`。此后每次发布都必须递增版本，已经发布的版本与标签不得覆盖。

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

检查任务使用 `vod-omarchy-docker-01` 对应的 `self-hosted`、`Linux`、`X64`、`docker`、`compose`、`omarchy` 标签，在 Ubuntu 24.04 容器内安装 Git LFS、Python 3.12、仓库固定的 Rust 工具链和 Slint 格式化工具，再运行 `make check`。该 runner 所在组需允许本仓库使用，并且 runner 需能运行 Docker 容器。

## 构建矩阵与附件

| 平台 | 架构 | Rust target | 发布文件 |
| --- | --- | --- | --- |
| macOS | Apple Silicon | `aarch64-apple-darwin` | `.zip`，包含 `SwitchX.app` |
| macOS | Intel | `x86_64-apple-darwin` | `.zip`，包含 `SwitchX.app` |
| Windows | x64 | `x86_64-pc-windows-msvc` | `.zip`，包含 `switchx.exe` |
| Windows | ARM64 | `aarch64-pc-windows-msvc` | `.zip`，包含 `switchx.exe` |
| Linux | x64 | `x86_64-unknown-linux-gnu` | `.tar.gz`，包含 `switchx` |
| Linux | ARM64 | `aarch64-unknown-linux-gnu` | `.tar.gz`，包含 `switchx` |

版本标签通过自托管 runner 的检查后，每个平台使用 [GitHub 提供的对应架构原生 runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)，先跑 Clippy、单元和集成测试，再构建 release 二进制。包名为 `SwitchX-<version>-<target>.<extension>`，包内包含 changelog、README、供应商图标许可和记录版本/target/源提交/工作区是否有改动/Rust 工具链的 `BUILD-INFO.json`。Windows 构建静态链接 CRT；Linux 包在 Ubuntu 24.04 构建，不承诺兼容更旧的 glibc。

Release 发布前会校验标签、workspace 版本、lockfile 和 changelog，要求六个平台包与各自 `.sha256` 文件齐全，核对包内版本与源提交、拒绝从有未提交改动的工作区构建的包，并生成 `SHA256SUMS`。任一检查失败都会阻止发布。全部完成后先创建 draft、上传附件，再公开 Release；上传失败时 draft 可以在重跑工作流后继续。已经公开的 Release 不会被覆盖。预发布版本自动标为 prerelease，且不会成为 latest。

下载后可在 Linux 使用 `sha256sum -c SHA256SUMS`，macOS 使用 `shasum -a 256 -c SHA256SUMS`；如果只下载一个平台包，使用对应 `.sha256` 文件。Windows 可使用 PowerShell 的 `Get-FileHash <文件名> -Algorithm SHA256` 与校验文件比对。

## GitHub 设置

工作流使用 GitHub 自动提供的 `GITHUB_TOKEN`；只有发布 job 请求 `contents: write`，其他 job 为只读。无需个人访问令牌或发布密钥。仓库需启用 GitHub Actions，并允许发布 job 的写权限。actions 固定到完整提交 SHA；更新时一起核对版本注释与 SHA。

macOS 当前使用 ad-hoc 签名，未做 Developer ID 签名与公证，Windows 未做 Authenticode 签名。正式签名需要另行配置相应证书与账号；当前工作流提供可下载的原生包。

本地打包可运行以下命令（target 必须匹配本机 Rust host），结果写入 `dist/`：

```sh
python3 scripts/package-release.py --target aarch64-apple-darwin
```
