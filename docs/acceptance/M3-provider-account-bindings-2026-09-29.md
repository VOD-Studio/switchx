# M3 上游账号绑定检查（2026-09-29）

本批为多个 ChatGPT 订阅连接分别绑定保存账号；API Key 继续随 API 上游保存。环境为 macOS 27.0 arm64，本机内置 Codex CLI `0.158.0-alpha.2.1`。自动检查使用合成 JWT、临时目录和 loopback 服务，原生界面仅操作合成账号的本地资料。本批没有真实官方登录或模型调用。

## 交付行为

- SQLite v9 保存明确的供应商类型与账号绑定。旧连接、公开模型 ID 和默认模型保持；旧全局绑定迁移到原订阅连接。新连接固定选择保存账号，可重命名和改绑，不改变映射、发布选择或 Codex 入口登录。
- 同名上游模型在不同连接下生成独立公开 ID。新 ID 最多 64 字符；长模型名缩短并加入摘要，已有 ID 原样保留。重复导入只补充缺少的映射，冲突时整批回滚。
- A/B 在各自私有临时 CLI 目录中发现官方工作区及区域，目标入口 C 可以与它们不同。发布核对账号解析快照；旧 `Default` 绑定在每次发布时解析并冻结，变化后须重新预览。
- 每个连接独立使用认证、工作区和错误状态。API 路径使用自己的 Key；入口认证和本地令牌均不进入 API 上游。401/403 与续期失败不触发请求重放或账号备用。
- SQLite 会话表将根 `session-id` 固定到连接、解析后的保存账号、工作区、origin 和区域。实际 session/thread 头及元数据要求一致；相同连接换模型允许，跨连接或改绑要求新会话。普通请求与 compact 共用保护，恢复、重启和 API-only 重发布保留绑定。未知加密/压缩输入、turn state 与上游状态不能建立新绑定；不支持的 `previous_response_id`/`conversation` 在认证及绑定前拒绝。
- 已发送的 OAuth 续期由有界模块任务验证并保存轮换结果，取消等待者不丢弃它；发送前或排队取消不发请求。退出先恢复并停止接收，再等待在途任务；恢复冲突保留可用路由。即使恢复日志被外部移除，退出也先关闭新请求入口再排空。
- 被固定或默认绑定引用的账号不能移除。界面显示引用数量与连接名称，工作线程再次核对，覆盖共用账号及过时界面状态。
- v8 存在直连或路由 journal 时仅允许恢复，完成后才迁移。credential/local-token helper 只读支持 v8/v9，不创建或迁移；恢复清理令牌使用不迁移的连接。冲突和清理失败保留令牌及 journal 供重试。

## 自动检查

```sh
RUSTC_WRAPPER= make format-check
RUSTC_WRAPPER= cargo clippy --locked --all-targets -- -D warnings
RUSTC_WRAPPER= cargo test --locked
RUSTC_WRAPPER= cargo test --locked app::tests::preset_catalog_is_accepted_by_local_codex -- --ignored
RUSTC_WRAPPER= sh scripts/bundle-macos.sh
RUSTC_WRAPPER= cargo run --locked --example managed_accounts_route_probe
```

Rust 与 Slint 格式检查通过。自动测试 142 项通过（lib 124、binary 8、router 10），默认忽略的本机 CLI 目录检查另行通过。全部目标 Clippy 无警告，macOS 调试 bundle 构建通过。取消发现工作区测试的就绪文件改为原子发布，修复并行运行时读取半写文件的夹具竞态。

重点回归覆盖 v8 迁移与恢复冲突、只读 helper、默认解析、身份和权限、原生文件竞争、取消后的轮换保存、各连接错误隔离、会话重启持久化、compact、跨 API 拒绝，以及未知上游状态不能注册会话。

隔离真实 CLI 探针通过生产 `prepare/apply` 发布 A/B/API，使用真实本机 app-server 发现各账号工作区，并以独立入口 C 启动 CLI。A、B、API 各完成文件工具与返回结果两轮；A resume 保留根 session 和映射。A 根会话转入 B/API 返回 409，未到达任何上游。恢复后销毁原 RouteSession，创建新实例发布 API-only，旧 A 根仍被拒绝；新的 API 会话完成工具回合。整个流程保持 C 登录文件与原配置，最终恢复、等待在途工作并清理临时资料。OAuth、工作区发现和模型请求全部为合成凭据及 loopback 服务。

## 原生界面

```sh
RUSTC_WRAPPER= cargo run --locked --example managed_accounts_desktop
```

打开命令输出的 `SwitchX Account Fixture.app`。夹具采用独立 bundle 标识与绝对数据/配置目录，禁用 Codex CLI；它用于本地编辑，不提供网络隔离，不应点击在线登录或续期。退出夹具窗口后，在运行命令的终端按 Enter 清理。

本次完成：

1. 页面同时显示合成 API 与 A/B 两个订阅连接，各自固定绑定账号，同名模型有两个不同公开 ID。[独立绑定截图](screenshots/provider-account-bindings/01-independent-bindings.png)
2. 编辑 B 的名称并改绑到 A，保存成功；两个连接均显示 A 的绑定。SQLite 中仍为两个公开模型、同一上游 slug；入口 B 的 `auth.json` 与 `config.toml` 逐字节保持。
3. A 显示两个引用，移除对话框列出两个连接并禁用确认。[共享引用保护截图](screenshots/provider-account-bindings/02-shared-reference-delete-blocked.png)
4. 将 A 设为默认，固定绑定保持，入口登录仍为 B；退出后登录与配置文件再次逐字节核对，进程退出，临时 bundle 和数据清理。[默认与入口独立截图](screenshots/provider-account-bindings/03-default-entry-separate.png)

首轮界面启动误命中系统缓存的既有 SwitchX，触发实际数据库的 v9 元数据迁移；窗口未执行编辑。关闭后，在无占用、无 journal、无会话记录条件下撤销该迁移至 v8；撤销前后既有表记录摘要一致，数据库完整性检查通过，临时安全副本已清理。后续使用独立 bundle，核对实际数据库仍为 v8。本批没有据此宣称实际账号或官方上游已验收。

## 验收边界

真实 A/B 双账号登录、官方模型权限、实际 refresh token 轮换、Desktop/IDE、真实并发客户端，以及 Windows/Linux 仍待独立验收。OAuth JSON 与 SQLite 的凭据依赖文件权限保护，不提供文件加密或跨设备同步。上述 mock 与合成界面结果不能替代真实上游证据。
