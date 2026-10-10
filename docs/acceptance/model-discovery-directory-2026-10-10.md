# 连接设置获取模型后同步模型目录

2026-10-10，macOS、Slint 1.18.1。使用真实 Slint 界面、合成模型获取结果和临时 SQLite，未读取真实账号、修改用户 Codex 配置或调用真实上游。

已有 API 连接在“连接设置”获取模型后，切换到同一连接的“模型目录”，获取到的新模型自动加入目录草稿。按实际模型 ID 去重，已有条目的公开 ID、自定义显示名和完整元数据保留；获取结果不会删除原有条目。点击“保存模型目录”后才写入 SQLite，新条目仍需在工作台选择连接才能加入启用范围。

| 检查 | 结果 |
| --- | --- |
| 设置页到目录页 | 关闭设置页清理获取结果前，传递本次有效结果；新条目显示在目录并启用保存按钮 |
| 重复获取 | 同一实际模型只保留一条，不覆盖已保存的名称、上下文或思考等级 |
| 连接隔离 | 打开其他连接的目录，不带入本连接的获取结果 |
| 获取重试 | 新请求先清除上一轮结果，失败后不会带入旧模型 |
| 保存边界 | 获取和标签切换不写目录；显式保存后保留原模型资料，新增条目未自动选入工作台 |
| 界面 | 深浅主题、1200×820 和 1000×680 均完成软件渲染检查，条目、反馈与保存按钮可见 |

回归测试：`cargo test --locked --bin switchx settings_model_discovery_populates_directory_drafts_and_preserves_saved_models`。

完整 `make check` 通过：Rust/Slint 格式检查、Clippy `-D warnings`、227 项测试；1 项需本地 Codex CLI 的目录解析测试按原设定忽略。

设置 `SWITCHX_CONNECTION_SNAPSHOTS` 为绝对输出目录，可由上述测试导出渲染帧。验收截图位于 [screenshots/model-discovery-directory](screenshots/model-discovery-directory)，仅含合成资料。
