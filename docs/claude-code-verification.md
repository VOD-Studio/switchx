# Claude Code 直连与模型列表验证

日期：2026-10-16。范围：M1；macOS arm64，Claude Code 2.1.301。

## 已实现

- 独立工作空间的连接创建、编辑、删除和用户配置导入；保存不写入客户端配置。
- helper、API Key、Bearer、本机官方登录四种认证选择。
- 获取和手动添加模型，编辑名称、说明、`behavesAs`，勾选、排序、默认模型和角色映射。
- CLI 路径与版本识别、能力门槛提示、直连预览和确认、恢复预览和冲突保留。
- 明确指定工作空间的 MCP 读取、模型编辑、预览与一次性应用；独立的托盘恢复入口。

模型发现按 [Anthropic SDK 的 URL 拼接规则](https://github.com/anthropics/anthropic-sdk-typescript/blob/main/src/client.ts) 在原样根地址后追加 `/v1/models`；不会把网关路径误当成 OpenAI 的 `/v1` 基址。

本次没有实现 Claude 请求路由、聚合、协议转换、云平台认证和官方多账号管理。

## 原生界面

以下为实际 Slint 界面的合成资料截图。回归覆盖 1000×680 和 1400×900、浅色和深色；检查模型编辑与保存按钮位于窗口内，以及退出编辑和导航后清空凭据。

| 页面 | 浅色 | 深色 |
| --- | --- | --- |
| 紧凑窗口连接页 | [查看](previews/claude-direct/connections-1000-light.png) | [查看](previews/claude-direct/connections-1000-dark.png) |
| 紧凑窗口模型编辑 | [查看](previews/claude-direct/models-1000-light.png) | [查看](previews/claude-direct/models-1000-dark.png) |
| 大窗口连接页 | [查看](previews/claude-direct/connections-1400-light.png) | [查看](previews/claude-direct/connections-1400-dark.png) |
| 大窗口模型编辑 | [查看](previews/claude-direct/models-1400-light.png) | [查看](previews/claude-direct/models-1400-dark.png) |

## 真实 CLI 菜单

配置由 `examples/claude_direct_probe.rs` 调用实际应用服务生成，使用临时资料、临时 `CLAUDE_CONFIG_DIR` 与合成 API Key。探针进程通过 macOS `sandbox-exec` 禁止联网，限制写入到临时目录，并拒绝读取正常的 Claude 配置与用户钥匙串目录；未覆盖系统 HOME。

在隔离目录中跳过首次安装引导后，启动 CLI 时只加载用户设置，禁用外部 MCP 配置。操作仅为 `/model`、当前会话模型选择与 `/exit`；没有发送模型请求。

| 操作 | 观察结果 |
| --- | --- |
| 替换菜单 | `Default` 后依次为 `SwitchX First`、`SwitchX Second`；各自显示合成说明，顺序与预览一致 |
| 设置默认模型 | 启动时显示 `SwitchX Second`，对应实际 ID `switchx-probe/second` |
| 选择自定义 ID | 选择第一项并使用 `s`，CLI 确认 `Set model to SwitchX First for this session only`；当前会话显示第一项 |
| 追加菜单 | 应用追加策略后重新打开菜单，内置项保留；自定义两项依次位于第 6、7 项 |
| 已运行会话 | 写入后首次立即打开菜单仍是旧值，随后重新打开显示更新后的菜单；当前会话模型保持第一项 |
| 恢复 | CLI 正常退出后，通过应用服务恢复；原本不存在的 `settings.json` 被移除，恢复成功 |

CLI 对没有 `behavesAs` 的合成第二项提示模型未知，并采用默认上下文假设；这是客户端对手动 ID 的提示，不代表真实上游接受该 ID。第一项的 `behavesAs` 被 CLI 接受，但本次没有验证真实模型能力或工具调用。

### 复现配置流程

以下命令只生成或恢复合成配置，不启动 CLI，不发起网络请求。目录需为专用的绝对路径，保留以便检查恢复记录。

```sh
cargo run --example claude_direct_probe -- /absolute/probe-directory replace
cargo run --example claude_direct_probe -- /absolute/probe-directory append
cargo run --example claude_direct_probe -- /absolute/probe-directory restore
```

若交互验证 CLI，使用该目录下的 `claude` 作为 `CLAUDE_CONFIG_DIR`，并维持网络隔离；不要将这些合成设置写入日常目录。

## 自动化覆盖

- JSON 严格解析、重复字段拒绝、无关字节和顺序保留、错误信息不回显配置内容。
- 工作空间隔离、连接版本校验、空凭据保留、旧 Codex 查询隔离、托管期间的编辑限制。
- helper 命令引用、私有文件权限、认证字段切换、官方登录文件不受影响；另用实际 SwitchX 二进制核对新版数据库的 helper 读取，输出与合成凭据一致，标准错误为空。
- 外部修改保留、预览过期拒绝、受管字段冲突、显式保留外部值、写入前后崩溃恢复。
- 默认模型独立于菜单；显式空菜单导入与应用；官方登录的独立角色映射。
- 模型发现使用本机模拟上游，核对所选认证头，失败响应不泄露正文。
- MCP 的客户端作用域、版本冲突、凭据字段拒绝、保存不应用、托管锁；预览 ID 单次使用。
- 原生模型编辑回调、排序和默认项、输入清理、双主题与两种窗口大小。

验证结果：

- `make check` 通过：380 项 Rust 测试通过，2 项既有 CLI 探针默认忽略；脚本测试 41 项中 2 项因缺少平台工具跳过。
- 最后关于菜单预览和工作空间页面记忆的修正，另通过 `cargo fmt --all -- --check`、`cargo clippy --locked --workspace --all-targets -- -D warnings`、`cargo test --locked --lib claude` 和 `cargo test --locked --bin switchx desktop::`。
- 本地验证日志位于 `target/claude-final-check.log` 与 `target/claude-last-changes-check.log`；日志不纳入版本控制。

上述 CLI 结果来自隔离的真实客户端，其余服务测试使用合成目录与模拟上游，不包含真实付费请求或正式账号登录验收。
