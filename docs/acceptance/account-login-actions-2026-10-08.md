# 订阅账号与当前 Codex 登录操作区分

日期：2026-10-08。

## 界面调整

- “Codex 原生登录”改为“重新登录当前 Codex”，与“检查 Codex 登录”放在“当前 Codex 登录”状态旁。
- 添加、导入账号仍位于保存账号列表上方，说明它们只保存账号，设为默认也不会切换当前 Codex 登录。
- 当前登录区域说明重新登录和写入 Codex 会先恢复配置、停止路由，并要求重新发布和启动新会话；上游绑定保持不变。
- “切回 API 预览”单独列出，说明先恢复并停止路由，预览仍需确认启用。

本次保留既有回调、登录互斥和凭据处理逻辑。

## 验证

工作区同时有另一批 Rust/Slint 功能改动，因此在当前提交的独立副本中只应用本次修改，并使用独立构建目录验证；没有读取真实账号或用户 Codex 配置。

- `make check` 通过：Rust/Slint 格式检查、全目标 Clippy、176 项测试通过，1 项需要本机 Codex CLI 的目录检查按默认设置忽略。
- 最后收回新增页面高度、补充滚动截图后，重新通过 `make format-check`、`cargo clippy --locked --example provider_icons_preview -- -D warnings` 和布局预览。
- `cargo run --locked --example provider_icons_preview -- /absolute/output/directory --layout-only` 使用生产 Slint 组件和两个合成账号，覆盖 1200×820、1000×680 的深浅主题，并通过实际滚轮事件检查页面底部操作。

以下为软件渲染截图，证明本次布局与滚动结果，不代表真实官方登录或 macOS 原生点击验收。

| 尺寸与主题 | 截图 |
| --- | --- |
| 1200×820 浅色 | [完整账号页面](screenshots/account-login-actions/accounts-light-1200x820.png) |
| 1200×820 深色 | [完整账号页面](screenshots/account-login-actions/accounts-dark-1200x820.png) |
| 1000×680 浅色 | [滚动到底部操作](screenshots/account-login-actions/accounts-actions-light-1000x680.png) |
| 1000×680 深色 | [滚动到底部操作](screenshots/account-login-actions/accounts-actions-dark-1000x680.png) |
