# M3 本地路由令牌 SQLite 保存检查

2026-09-29，macOS arm64。检查使用临时 SQLite、独立 `CODEX_HOME`、合成凭据和本地假上游；没有读取或修改真实账号、用户配置或旧系统凭据，没有调用真实供应商。

## 实现范围

- 本地路由令牌由随机生成的 64 个十六进制字符组成，明文保存到 `app_settings`，键为 `local_token:router-<32hex>`。供应商 API Key 仍保存到 SQLite，ChatGPT OAuth 仍保存到私有 JSON。
- API 路由配置生成 `local-token REF ABS_DATA_DIR` 三参数 helper。helper 只读指定的当前版本数据库，不创建或迁移数据库，不从环境变量选择其他数据目录。无效参数、符号链接、缺失或无效令牌均拒绝提供凭据。
- 移除 keyring 依赖和旧 helper 兼容，不读取、迁移或删除旧系统凭据。旧配置须先恢复，再重新发布。
- 配置恢复有冲突时保留路由、令牌与 journal。配置三方恢复完成后暂保留 journal，停止路由并删除令牌，最后幂等清理 journal。令牌删除失败时保留引用，可在当前应用或重启后重试；活动路由核对原数据目录与 journal 引用，避免清理其他库或令牌。

## 自动检查与隔离运行

| 检查 | 实际结果 |
|---|---|
| `make check` | 126 项通过：111 项 lib、6 项 main、9 项 router；1 项既有 Codex CLI 目录检查默认忽略。Rust/Slint 格式检查和 all-targets clippy 通过。最终探针修改后再次执行 `make format-check lint`，通过。 |
| 存储和 helper 单元检查 | 覆盖令牌引用和长度校验、命名空间隔离、持久化与幂等删除；helper 使用明确指定的只读数据库，拒绝旧参数、相对路径、多余参数、坏引用、缺失令牌和符号链接，不创建或升级数据库。 |
| 配置与恢复单元检查 | 有原配置和原配置不存在两种情况均可先恢复并保留 journal，再幂等完成清理；恢复冲突保留令牌，触发器拒绝删除时保留引用，解除拒绝后重试只删除对应令牌。 |
| `cargo run --locked --example local_token_probe` | 合成令牌保存、只读读取、删除与临时目录清理通过。 |
| 独立进程 helper | 6 项通过：正确读取显式数据库，即使 `SWITCHX_DATA_DIR` 指向错误目录；旧两参数、相对路径、多余参数、坏引用和缺失引用均失败。失败时 stdout 为空，stderr 不含令牌；源 SQLite 字节不变，临时目录已删除。 |
| `cargo build --locked --example routed_cli_probe` 后执行 `env -u SWITCHX_DATA_DIR target/debug/examples/routed_cli_probe` | exit 0，Codex CLI `0.158.0-alpha.2.1` 从新 helper 取得 SQLite 令牌；两个公开模型分别到达正确假上游，Alpha 完成读取合成文件、工具结果与第二轮回答。 |
| 路由令牌写入失败 | 临时数据库触发器拒绝 INSERT 后，不发布配置、journal 或令牌，路由停止且端口释放。 |
| 活动路由令牌删除失败与重试 | 触发器拒绝 DELETE 后，服务器停止、配置恢复，journal 和令牌保留；新建 `RouteSession` 模拟应用重启后可再次恢复，清除令牌和 journal。探针临时目录已删除，结束后再次确认不存在。 |
| `env -u SWITCHX_DATA_DIR cargo run --locked --example direct_cli_probe` | exit 0，独立 Codex CLI 通过 SwitchX `credential` helper 完成本地 mock Responses，验证共享 helper 的直连回归。 |
| `sh scripts/bundle-macos.sh` | 成功生成 `target/debug/SwitchX.app`；此项是构建检查，未执行原生界面验收。 |

## 验收边界

上述结果证明本机合成场景中的 SQLite 令牌、独立 helper、CLI 请求、失败清理和恢复重试。真实上游、Windows/Linux 运行及原生界面操作仍未验收。历史钥匙串路径的检查记录保留，不作为当前存储方式的证据。
