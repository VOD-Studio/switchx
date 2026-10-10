# 侧栏品牌区调整

2026-10-10，使用真实 Slint 界面和合成数据渲染。

删除侧栏的「Codex / 本机工作空间」说明块及其下方留白，导航整体上移 80px。用户提供的透明猫咪 PNG 原样保存为 `assets/logo.png`；品牌区采用 36×36px 图标、8px 间距和 19px / 600 字重的 `switchx`，通过水平布局的交叉轴对齐使图标与文字居中。

## 渲染与交互检查

浅色、深色均检查 1200×820 和 1000×680，覆盖普通窗口及 macOS 标题栏覆盖模式。图标无底板，文字无裁切；标题栏模式使品牌和导航整体下移 18px，避免占用原生窗口按钮的位置。前后渲染对比确认右侧内容像素一致。

现有侧栏预览的点击坐标随导航上移更新。普通模式和标题栏模式均通过五个导航入口、动画拉伸与落点、中途切换目标及减少动态效果检查。

| 尺寸 | 浅色 | 深色 |
| --- | --- | --- |
| 1200×820 | [截图](screenshots/sidebar-brand/settings-light-1200x820.png) | [截图](screenshots/sidebar-brand/settings-dark-1200x820.png) |
| 1000×680 | [截图](screenshots/sidebar-brand/settings-light-1000x680.png) | [截图](screenshots/sidebar-brand/settings-dark-1000x680.png) |

截图展示标题栏覆盖模式的布局，不包含系统绘制的窗口按钮。预览未访问账号、凭据、Codex 配置或真实上游。

`make check` 通过：Rust / Slint 格式、所有目标的 Clippy、229 项测试；另有 1 项需要本地 Codex CLI 的测试按原配置忽略。`git diff --check` 通过，`assets/logo.png` 与用户提供的 PNG 逐字节一致。

复现命令：

```sh
cargo run --locked --example provider_icons_preview -- /tmp/switchx-sidebar-after --layout-only
cargo run --locked --example provider_icons_preview -- /tmp/switchx-sidebar-overlay --layout-only --native-titlebar-overlay
cargo run --locked --example provider_icons_preview -- /tmp/switchx-sidebar-motion-standard --sidebar-motion
cargo run --locked --example provider_icons_preview -- /tmp/switchx-sidebar-motion --sidebar-motion --native-titlebar-overlay
```
