# Changelog

所有用户可见的变更记录在这里，格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。尚未发布的变更放在 `Unreleased`，发版时整理成带版本号与日期的条目。

## [Unreleased]

### Added

- 管理 Responses API 连接、ChatGPT 与 Grok 订阅账号及额度。
- 统一模型目录，按唯一显示名称路由，支持直连与备用 API 上游。
- 预览 Codex 配置、记录受管改动并恢复，查看不含正文与凭据的请求记录。
- 支持浅色与深色主题、搜索模型、批量添加模型以及 macOS 菜单栏。
- 统一应用与 UI crate 的版本号，提供 `switchx --version` 和发版准备命令。
- 推送与 PR 自动检查并构建 macOS、Windows、Linux 的 x64 与 ARM64 包。
- 推送版本标签后自动创建 GitHub Release，附上 changelog、六个平台包和 SHA-256 校验文件。

### Fixed

- 修正 Windows 平台分支的 Clippy 警告，以及测试对本机 Codex 和固定 Python 路径的依赖，使六个平台的 CI 可在干净环境中运行。
- 修正四个示例探针中同类的 Windows Clippy 警告，保留 Unix 临时目录的私有权限。
- 思考等级选择器测试按可访问性标签点击实际控件，避免字体改变弹窗位置时点击错误；元素调试信息仅用于测试构建。
- 修正仅在 Unix 运行的直连集成测试的导入作用域，避免 Windows 的未使用导入阻止 CI。
- 账号存储锁在操作结束时显式解锁，避免克隆或继承的文件句柄造成占用误报。
