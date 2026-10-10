# 通用配置编辑器视觉验收

通用配置抽屉采用 920px 配置工作区：顶部以薄荷绿和冰蓝渐变呈现共享用途，代码区显示行号、语法高亮、提取来源和草稿状态。说明折叠收纳；专注编辑可收起顶部介绍，让代码区平滑扩大。底部保存、取消和提取操作在不同状态下保持可见。

动效包括抽屉进出场、内容分段入场、标签错峰出现、卡片展开与浮动、说明展开及箭头旋转、专注模式切换、编辑焦点描边及下划线、草稿标记、按钮扫光及按压反馈、处理中的旋转图标和流动进度，以及成功和错误反馈。动效遵循应用动画设置与系统减少动画设置，关闭后直接呈现最终状态。

## 合成预览

```sh
cargo run --locked --example provider_icons_preview -- /tmp/switchx-common-config --common-config-design
cargo run --locked --example provider_icons_preview -- /tmp/switchx-common-config-native --common-config-native
```

预览使用合成 TOML 与内存回调，不读取账户、用户数据库或 Codex 配置，不连接上游。原生预览中的保存和提取只更新内存状态。

软件渲染覆盖深浅主题、1200×820 和 1000×680 的普通编辑、说明展开、专注编辑、草稿编辑、保存处理中、提取处理中、提取完成、错误、路由管理锁定和空内容，共 40 个状态。交互断言检查键盘编辑、操作按钮可达、忙碌时禁止重复提交和关闭、管理状态禁止写入、取消恢复草稿。另捕获入场、退场、中途反向切换、重新打开、禁用动画和系统减少动画状态，以及 120 帧动画演示。

现有 Rust 保存、提取、取消和配置应用回调保持原有行为。

## 画面

- [深色](screenshots/common-config-design/dark.png)
- [浅色](screenshots/common-config-design/light.png)
- [小窗口深色](screenshots/common-config-design/dark-compact.png)
- [小窗口浅色](screenshots/common-config-design/light-compact.png)
- [专注编辑](screenshots/common-config-design/focus.png)
- [动画演示](screenshots/common-config-design/motion.gif)

## 验证

- `make check` 通过：Rust / Slint 格式、所有目标 Clippy、229 项工作区测试通过，1 项依赖本机 Codex CLI 的测试按既有设置忽略。
- `provider_icons_preview --common-config-design`：合成渲染和交互断言。
- `git diff --check`。
