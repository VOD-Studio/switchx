# 留白工作台原生界面验收

日期：2026-09-30。采用设计预览中的“留白工作台”，实现于 Rust + Slint；设计原型另存本地 `design/quiet-workbench-prototype` 分支（`ecf3e5c`）。

## 实现范围

- 导航收拢为工作台、连接、活动、工具箱、设置。订阅账号移到连接页的独立标签。
- 工作台使用紧凑模型行，支持已选筛选、默认模型、映射编辑、备用策略；发布、启动与恢复放在列表下方。
- API、订阅、模型、通用配置、头像选择、发布预览、直连预览和状态详情共用侧边抽屉。保留现有业务回调和配置事务。
- 请求列表显示简短耗时；展开后显示响应头、首事件、HTTP 状态、请求标识及安全错误说明。
- 按钮/勾选/焦点使用 150ms 反馈，页面和详情展开使用 240ms，抽屉使用 340ms。关闭动画或检测到 macOS 减少动态效果时，过渡时长为零。系统偏好在启动时读取，运行中每五秒更新。
- macOS 原生“导航 → 快速操作”菜单注册 ⌘K；Esc 关闭抽屉，关闭后焦点回到主界面。图标按钮提供悬停说明。
- 直连、路由运行、待恢复与未启用有独立状态。直连时明确标记模型菜单尚未发布，并提供查看直连配置入口。
- 关闭或离开编辑器清空凭据草稿；嵌套头像选择仍保留正在编辑的表单。保存订阅不切换 Codex 原生登录。
- 工具箱仍明确标注 MCP / Skills 尚未接入；本轮没有实现新的工具管理能力。主题和动画偏好仍仅保留在本次运行中。

## 自动检查

`make check` 通过，包括 Rust/Slint 格式检查、`cargo clippy --locked --all-targets -- -D warnings` 和 `cargo test --locked`：

| 测试层 | 结果 |
| --- | --- |
| 库单元测试 | 147 通过，1 忽略 |
| 桌面入口与展示逻辑 | 10 通过 |
| 本地 mock 路由集成 | 10 通过 |

共 167 项通过。忽略项是默认不运行的本机 Codex 预置目录检查；本轮另通过原生发布流程检查了隔离目录与模型菜单。

原有 `provider_avatar_picker_preserves_form_drafts_and_defaults` 测试补充了离开连接页清空 API/OAuth 草稿、直连/路由状态区分和减少动态效果的断言。

## 原生交互检查

使用 `managed_accounts_desktop` 创建独立应用身份、绝对数据目录和 `CODEX_HOME`，只含合成账号、连接和请求记录。该夹具的 CLI 为 `/usr/bin/false`，没有使用其在线登录操作。

已检查深浅主题、五页导航、连接/账号标签、模型编辑表单、API 模型实际保存、默认选择、取消选择、已选筛选、请求详情展开、设置、错误反馈、抽屉取消与 Esc、⌘K。订阅模型保存遇到夹具禁用 CLI 的预期错误时，草稿保留，取消仍正常；API 映射通过生产保存逻辑写入隔离 SQLite 并更新列表。

两个账号夹具退出前均确认合成 `config.toml` 未改变，随后删除其目录和临时应用。

## 原生发布、直连与恢复

使用 `routed_cli_probe --desktop-models` 的两个 loopback 假上游、三个模型映射和临时配置，应用二进制放入独立的临时 bundle。没有使用实际用户的数据目录。

1. 原生“预览并启用”调用 Codex CLI **0.159.0**，核对三个可选模型；预览长路径和摘要完整显示。
2. “确认启用”后，界面显示路由运行、启动入口可用，连接与模型编辑禁用。
3. 通过生产 `local-token REF ABS_DATA_DIR` helper 在内存中取得临时令牌；HTTP 客户端仅向本机路由发出合成请求。缺少会话标识的请求得到 `400 session_required`；携带独立 UUID 的 `session-id` 后，Mock Beta 返回 **HTTP 200**，并观察到 `response.completed`。令牌未输出到日志、截图或文件。
4. 活动页显示实际的本地成功/失败记录，展开成功请求可查看耗时和安全元数据。
5. 点击“恢复并停止”后，恢复日志和本地令牌移除，18731 端口释放，编辑入口重新可用。
6. 第二轮通过 API 直连预览、应用、查看和恢复，确认工作台显示“直连已启用”，不会误报待恢复或已发布模型菜单。
7. 再次启用路由后使用 **Command-Q** 退出。夹具确认原配置恢复、无路由日志，并清理配置、数据库与合成凭据。

两轮路由夹具、两轮账号夹具及额外创建的临时 route bundle 已清理，18731 端口已释放。上述结果是**原生 UI + 隔离 CLI 目录检查 + 本地假上游 HTTP 验证**，不代表真实供应商请求、真实 OAuth 生命周期或 CLI 模型工具回合验收。

## 尺寸与截图

复用 `provider_icons_preview` 的生产 Slint 组件及软件渲染器，替换依赖旧按钮坐标的主题切换，并增加九模型工作台、活动、设置的尺寸检查：

```sh
cargo run --locked --example provider_icons_preview -- /absolute/output/directory --layout-only
```

检查 1200×820、1000×680 两种尺寸的深浅主题。另执行未带 `--layout-only` 的完整预览，检查既有编辑表单、头像搜索与全部 110 个头像的两种主题及 Retina 渲染。列表在较小窗口内滚动，主要操作保持可见。软件渲染器的菜单条与 macOS 原生菜单位置不同，这些图片证明布局结果，不代替 Windows/Linux 原生验收。

| 截图 | 来源 |
| --- | --- |
| [浅色工作台](screenshots/quiet-workbench/native-workbench-light.png) | macOS 原生窗口，隔离假上游 |
| [深色工作台](screenshots/quiet-workbench/native-workbench-dark.png) | macOS 原生窗口，隔离假上游 |
| [发布预览](screenshots/quiet-workbench/native-route-preview.png) | macOS 原生窗口，实际目录检查摘要 |
| [请求详情](screenshots/quiet-workbench/native-request-detail.png) | macOS 原生窗口，本地假上游请求 |
| [1000×680 浅色布局](screenshots/quiet-workbench/software-workbench-light-1000x680.png) | 生产 Slint 组件的软件渲染，九模型合成数据 |
| [1200×820 深色布局](screenshots/quiet-workbench/software-workbench-dark-1200x820.png) | 生产 Slint 组件的软件渲染，九模型合成数据 |
| [1000×680 连接页](screenshots/quiet-workbench/software-connections-light-1000x680.png) | 生产 Slint 组件的软件渲染，保留本地凭据检查入口 |
| [1000×680 编辑抽屉](screenshots/quiet-workbench/software-editor-light-1000x680.png) | 生产 Slint 组件的软件渲染，长表单通过滚动访问 |

本机调试 bundle 由 `sh scripts/bundle-macos.sh` 生成于 `target/debug/SwitchX.app`。本轮只做本地提交和构建，没有推送、发布或真实上游调用。
