# M3 API Key 保存检查（2026-09-29）

本文件首轮记录对应提交 `a95bd8a`；用户随后要求移除旧 API Key 迁移和兼容，当前行为与检查见文末修订。

按用户要求对齐本地 CC Switch `846de29` 的供应商认证保存方式。SQLite 升级至 v8，在 `providers.settings_config` 的 JSON `auth.OPENAI_API_KEY` 字段保存明文 API Key。数据库及其副本包含凭据，不提供数据库加密。ChatGPT OAuth 仍单独保存在私有 JSON，本地路由令牌仍使用系统凭据库。

## 行为

- 新建、替换 Key、供应商资料、模型和选项在同一事务中保存，失败回滚。编辑 Key 留空时，在事务内保留数据库当时的 Key，不回写表单打开时的旧值。
- SQLite v7 与更早资料迁移保留供应商、模型、选项、绑定和请求历史。旧 Key 先读取并成功保存，再清理同供应商 ID 的 SwitchX API 凭据条目；读取或写入失败不删除源凭据。并发编辑或已删除记录不会被旧迁移覆盖。
- 活动的直连或路由 journal 会暂缓旧 Key 迁移，旧两参数钥匙串 helper 继续可用；恢复后可迁移。无旧凭据时允许在表单补填。
- 新直连 helper 使用 `credential PROVIDER_ID ABSOLUTE_DATA_DIR`，明确绑定数据目录。Key 不进入目标 Codex 配置、列表、状态、模型目录、请求记录或恢复 journal；`ProviderRecord` 保持仅含元数据。
- 密码输入保持遮蔽；删除供应商会同时删除其 SQLite API Key 和模型映射。Unix 应用数据目录为 `0700`，数据库为 `0600`。

## 已通过

```sh
make check
sh scripts/bundle-macos.sh
cargo test --locked --test router managed_accounts
env -u SWITCHX_DATA_DIR cargo run --locked --example direct_cli_probe
env -u SWITCHX_DATA_DIR cargo run --locked --example routed_cli_probe -- --http-only
```

`make check` 的 Rust/Slint 格式检查、全部目标 Clippy 和测试通过：119 项通过，1 项需要本机 Codex CLI 的既有目录检查按默认配置忽略。macOS 调试 bundle 构建通过。

重点覆盖明文 JSON 持久化、元数据不携带 Key、原子失败回滚、旧凭据 CAS 迁移、成功保存后清理、清理失败保留已保存 Key、读取或写入失败保留旧来源、留空保留并发更新后的值，以及 malformed JSON、控制字符和 UTF-8 字节长度校验。迁移的系统凭据操作由合成 resolver 注入，不读取用户旧 Key。

API/OAuth 两项跨模块集成检查通过，验证保存到 SQLite 的 API Key 只用于自己的本地假上游，OAuth 账号固定绑定、删除后不回退；请求数据库不含 API Key 或 OAuth token，供应商数据库不含 OAuth token。

Codex CLI `0.158.0-alpha.2.1` 在临时 `CODEX_HOME` 中，通过新 helper 的显式数据目录读取合成 Key，完成直连 Responses 合成回答。子进程移除 `SWITCHX_DATA_DIR` 和继承的 API 认证环境变量，非 loopback 流量被代理限制；配置精确恢复，临时数据库清理。helper 的相对路径、local-token 多余参数和 credential 多余参数另有三项命令检查，均在读取凭据前拒绝且 stdout 为空。

`routed_cli_probe --http-only` 在隔离目录保存两条 SQLite 合成 API Key，再由生产 `RouteSession` 读取，检查相同实际模型的两个公开 ID 只到达各自 mock 上游。每个 mock 要求自己的 Bearer Key，SSE completion、两条正常完成请求元数据与 Key 不进入日志的断言均通过；端口冲突、无效目录、旧预览与恢复冲突检查复用完整探针，最后恢复配置并清理临时数据库和本地令牌。同进程仅读取此探针新建的系统本地令牌，不调用独立 CLI 的令牌 helper，不验证文件工具轮次。

## 验收边界

完整 `routed_cli_probe` 已通过预览、SQLite Key 持久化、发布与隔离配置检查，但 CLI 获取系统本地令牌的 `local-token` helper 超时，未完成 CLI 请求轮次。失败后已恢复配置、清理临时数据库和独立本地令牌；不能把此前旧版 CLI 路由成功作为本轮成功。

本轮未操作真实供应商 Key、用户数据库或用户 Codex 登录/配置，没有真实上游请求。真实旧钥匙串迁移、完整路由 CLI helper、原生表单交互和 Windows/Linux 运行仍需独立验证。

## 后续修订：移除旧 API Key 兼容（2026-09-29）

按用户后续要求删除旧 API Key 钥匙串读取、迁移、清理及旧两参数 `credential` helper。API Key 只从 SQLite 读取；旧引用仅作为历史数据库元数据保留，不参与认证。缺少 SQLite Key 时要求在表单重新输入。已有数据库资料仍可升级，不读取或删除旧系统 API 凭据。

本地路由令牌本轮仍用系统凭据库。对照本地 CC Switch `846de29`：普通 Codex 代理使用固定 `PROXY_MANAGED` 占位符，没有 SwitchX 同类独立随机访问令牌；Claude Desktop 专用 gateway 生成 `ccs-<UUID v4>`，明文保存在 SQLite `settings` 的 `claude_desktop_gateway_token` 中，并写入 profile 的 `inferenceGatewayApiKey`，请求时检查 Bearer。参见 [Codex 代理配置](https://github.com/farion1231/cc-switch/blob/846de29c13ac4d65f164db8c15dd5fd58e29f972/src-tauri/src/live/project/codex.rs#L478)、[gateway token](https://github.com/farion1231/cc-switch/blob/846de29c13ac4d65f164db8c15dd5fd58e29f972/src-tauri/src/claude_desktop_config.rs#L279) 与 [SQLite settings](https://github.com/farion1231/cc-switch/blob/846de29c13ac4d65f164db8c15dd5fd58e29f972/src-tauri/src/database/dao/settings.rs#L49)。该核对只读参考源码，没有访问用户 CC Switch 数据。

本轮 `make check` 通过，118 项通过、1 项默认忽略；macOS 调试 bundle 构建通过。新增检查验证旧引用且无 SQLite Key 时只报缺失，在两类 journal 存在时也不会迁移或改写数据库；空 Key 编辑失败保留原资料，删除供应商不访问系统凭据库。并发 Key 保留、原子保存与错误脱敏测试继续通过。

更新后的主程序通过三项命令检查：旧 `credential PROVIDER_ID` 直接报缺少数据目录，相对数据目录和 `local-token` 多余参数也在凭据访问前拒绝，stdout 均为空。重新构建探针后，隔离直连 CLI 与 `routed_cli_probe --http-only` 再次通过，所有请求仅到本地 mock；后者使用自己的临时路由令牌，检查完成与冲突恢复后清理。
