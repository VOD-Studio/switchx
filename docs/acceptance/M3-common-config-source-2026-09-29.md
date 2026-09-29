# M3 通用配置来源与 CC Switch 对齐

2026-09-29，macOS arm64、Slint 1.18.1，本机调试 bundle。本次对齐截图对应的 CC Switch 旧版通用片段行为：首次自动提取、加载已保存快照、从供应商表单提取并立即保存。

原生检查使用 `routed_cli_probe --desktop` 的本地假上游、合成凭据、临时绝对路径 `SWITCHX_DATA_DIR` 和 `CODEX_HOME`。未使用真实账号或真实上游请求。

| 检查 | 结果 |
| --- | --- |
| 首次初始化 | 启动后 SQLite 自动保存 `approval_policy = "never"`，原配置中的模型、注释与合成 MCP 不进入通用片段；源文件不变 |
| 提取来源 | 在供应商表单开启 1M 并设阈值 `850000`，提取后 SQLite 立即包含这两个字段；磁盘 `config.toml` 中仍没有它们，证明来源是当前表单 |
| 提取与取消 | 未点击保存便核对数据库，提取结果已经持久化；随后修改手动草稿并取消，重新打开仍显示提取结果 |
| 手动保存和清空 | 手动编辑仍须保存；保存空片段后重新打开，已保存空值保持，不会从磁盘自动填回 |
| 无效预览 | 1M 阈值改为 `0` 后，提取被拒绝，之前保存的 `model_reasoning_effort = "high"` 不变 |
| 操作提示 | 最新 bundle 的提取成功与阈值错误均直接显示在顶部，无须滚动查找 |
| 退出与清理 | 夹具正常退出，源配置逐字节保持原样，没有切换 journal，合成凭据与临时目录清理完成 |

`make check` 通过：86 个库测试、2 个主程序测试、7 个路由集成测试，共 95 项通过；1 项已有的仅 CLI 目录测试保持 ignored。Rust/Slint 格式检查与 Clippy 通过，`sh scripts/bundle-macos.sh` 构建成功。

新增自动测试还覆盖：源缺失后可重试、仅敏感字段产生的空表不占用首次初始化、已有片段与显式空值不被覆盖、受管配置或活跃 journal 跳过初始化、无效 TOML 不改存量、条件插入不覆盖已有保存值，以及从预览排除凭据 helper、凭据引用与敏感环境变量。初始化不读取或修改 `auth.json`。

```sh
make check
sh scripts/bundle-macos.sh
SWITCHX_DESKTOP_BINARY="$PWD/target/debug/SwitchX.app/Contents/MacOS/switchx" \
  cargo run --locked --example routed_cli_probe -- --desktop
```

本记录覆盖通用片段来源与保存行为；真实供应商压缩、订阅认证、Desktop/IDE 与其他平台仍不属于本轮验收。
