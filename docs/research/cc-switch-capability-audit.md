# CC Switch 本地能力审计：SwitchX 功能基线与路由边界

审计日期：2026-09-24。用途：为 SwitchX 的 Rust + Slint 桌面方案提供功能事实，尤其是 v1 Codex 配置切换、Provider 管理及按模型跨 Provider 路由的差距。本报告不包含产品代码或完整架构设计。

## 1. 审计范围与证据等级

- 审计对象：`/Users/issuser/Developer/xfy/switchx/others/cc-switch`，独立 Git 仓库；本地 HEAD 为 `da193d4f7a6ce3710623c312245c752376c0d036`，提交时间 `2026-09-23T22:32:19+08:00`，提交说明为更新预设模型及定价。审查期间该仓库工作树干净。
- 本地版本为 **3.20.4**，由 [package.json:3](../../others/cc-switch/package.json#L3)、[Cargo.toml:3](../../others/cc-switch/src-tauri/Cargo.toml#L3)、[tauri.conf.json:4](../../others/cc-switch/src-tauri/tauri.conf.json#L4) 三处交叉确认。没有据此宣称它是远端最新发布版；本地 `git describe --tags --always` 仅返回提交短哈希。
- 已检查工作目录的祖先目录和本地源码树，未发现适用的 AGENTS.md。采用并行只读检索，分别审查路由认证、功能适用范围、存储同步及安全，再汇总为本文件。
- **实现（静态）**：存在实际调用、数据结构或读写路径；**部分**：存在功能但应用范围或协议有限；**未发现**：在所列入口和调用链内没有证据；**文档声称**：只作为待核对描述，不能代替实现。
- 没有启动 CC Switch、任何目标客户端、代理或测试；没有读取用户真实 auth.json、配置、数据库、钥匙串或会话文件。文中这些路径均来自源码定义。测试函数只作静态旁证，不能称为测试通过或在线验收。
- 主线程已负责最新 OpenAI `auth.command`、`supports_websockets`、`model_catalog_json` 官方契约和 DeepSeek 在线资料。本报告仅记录本地如何使用这些能力，不以旧适配器推断当前厂商 API 上限，也不重复做官方产品兼容性结论。

下文链接均相对本报告指向本地审计快照，标签为文件名和起始行号；跨文件同名模块以链接目标为准。

## 2. 最重要的结论

1. **已有模型映射、模型发现和 Codex 模型目录；没有发现通用的按请求模型跨 Provider 选路。** 请求虽然提取 model，但选 Provider 只传 app_type；先选择当前 Provider 或故障队列，再在该 Provider 内改写模型。证据：[handler_context.rs:113](../../others/cc-switch/src-tauri/src/proxy/handler_context.rs#L113)、[provider_router.rs:45](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L45)、[forwarder.rs:1245](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L1245)。
2. **Codex ChatGPT 官方账号并非完全不支持本地代理。** 已有原生凭据透传、托管 OAuth 账号管理、配额及模型查询；原生官方账号被明确排除出跨 Provider 故障转移。证据：[codex_config.rs:3541](../../others/cc-switch/src-tauri/src/codex_config.rs#L3541)、[provider_router.rs:15](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L15)、[commands/codex_oauth.rs:28](../../others/cc-switch/src-tauri/src/commands/codex_oauth.rs#L28)。
3. **本地快照已经包含 DeepSeek 原生 Responses 预设和专用目录。** 不能把保留的 Responses→Chat 转换路径写成“DeepSeek 只能 Chat Completions”。证据：[codexProviderPresets.ts:1375](../../others/cc-switch/src/config/codexProviderPresets.ts#L1375)、[codex_config.rs:2077](../../others/cc-switch/src-tauri/src/codex_config.rs#L2077)。
4. **“几乎全部功能”必须按应用分别验收。** README 称九个工具，源码实际有十个 AppType，额外包含 Pi；代理、MCP、Skills、会话、托盘、深链各自的支持集合不同。证据：[README_ZH.md:240](../../others/cc-switch/README_ZH.md#L240)、[app_config.rs:396](../../others/cc-switch/src-tauri/src/app_config.rs#L396)。
5. **UI 的“连通性检查”不是模型调用成功证据。** 当前实现 GET base_url，任何 HTTP 响应都算可达，不验证 API key、模型或工具调用。证据：[stream_check.rs:150](../../others/cc-switch/src-tauri/src/services/stream_check.rs#L150)、[stream_check.rs:200](../../others/cc-switch/src-tauri/src/services/stream_check.rs#L200)。
6. **安全能力不能概括为“全加密、全原子”。** Provider 配置以 JSON 存入 SQLite；托管 OAuth 是文件权限保护的 JSON；同步发送 SQL 和 ZIP；settings.json 的写入仍是直接 truncate。证据：[providers.rs:217](../../others/cc-switch/src-tauri/src/database/dao/providers.rs#L217)、[codex_oauth_auth.rs:1972](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L1972)、[sync_protocol.rs:149](../../others/cc-switch/src-tauri/src/services/sync_protocol.rs#L149)、[settings.rs:692](../../others/cc-switch/src-tauri/src/settings.rs#L692)。

## 3. 应用适用范围

“切换式”表示当前 Provider 投影到客户端配置；“累加式”表示多个 Provider 共存于原生配置，不等于 CC Switch 为其提供逐请求路由。分类依据：[app_config.rs:430](../../others/cc-switch/src-tauri/src/app_config.rs#L430)。

| 应用 | Provider 与本地配置 | CC Switch 本地代理 | MCP / Skills / Prompts | 会话与主要限制 |
| --- | --- | --- | --- | --- |
| Claude Code | 切换式，Anthropic env/settings；支持通用供应商投影 | 通用接管、热切换、故障队列、协议桥接 | 三者都有；CLAUDE.md | 会话浏览、恢复、删除；官方登录类别仍有接管限制 |
| Codex CLI / Desktop | 切换式，config.toml、受控 auth.json、生成模型目录；两端共用 Codex 应用类型 | Responses/Chat 入口、桥接及官方凭据透传 | 三者都有；AGENTS.md | 会话管理、统一历史桶及可选旧历史迁移；不是任意客户端版本保证 |
| Gemini CLI | 切换式，.env 与 settings.json 的认证配置 | 通用接管；Gemini 原生路径与认证适配 | 三者都有；GEMINI.md | 会话管理、用量导入；存在 OAuth 数据形状不等于任意 Google 登录链路均已支持 |
| Grok Build | 切换式，.grok/config.toml | 通用接管，复用 Codex adapter | 三者都有；AGENTS.md | 会话和用量导入、xAI 托管认证相关功能 |
| Claude Desktop | 独立 3P profile，Direct / Proxy 两种模式 | 独立 /claude-desktop gateway，模型菜单与别名映射 | 不实现 Desktop 自己的三者同步；UI 共享项映射到 Claude Code | 不应把共享的 Claude 会话面板认作 Desktop 原生会话支持 |
| OpenCode | 累加式，opencode.json；另有 OMO / OMO Slim 配置 | 不在通用接管集合内 | 三者都有；AGENTS.md | JSON/SQLite 会话、用量导入；原生多 Provider/多模型不是本代理按模型选路 |
| OpenClaw | 累加式，openclaw.json；默认模型、目录、agents/env/tools | 不在通用接管集合内 | MCP 明确跳过；Skills app 标志不支持；有 AGENTS.md 和专用工作区 | 会话管理；工作区、日记文件编辑另有实现 |
| Hermes Agent | 累加式，config.yaml；模型和记忆设置 | 不在通用接管集合内 | 三者都有；SOUL.md | 文件/SQLite 会话、Web UI/dashboard 入口；未列入本地会话用量同步集合 |
| Pi | 累加式，原生 models.json；独立配置模块和前端入口 | 不在通用接管集合内 | 无原生 MCP 注册表；有 Skills、AGENTS.md 及专用 prompt 模板 | 会话与用量导入；Prompt 不走通用自动投影路径 |
| MiniMax Code / MCode | 累加式，config.yaml；专用配置提交流程 | 不在通用接管集合内 | 三者都有；AGENTS.md 有 32 KiB 校验 | 会话浏览/恢复和用量导入；CC Switch 明确拒绝代删会话，要求在 MCode 内删除 |

矩阵的实现证据：

- 通用接管只有 Claude/Codex/Gemini/Grok Build：[app_config.rs:442](../../others/cc-switch/src-tauri/src/app_config.rs#L442)。Claude Desktop 独立入口：[server.rs:299](../../others/cc-switch/src-tauri/src/proxy/server.rs#L299)。`get_adapter()` 给 OpenCode 等返回适配器并不能推翻接管范围，它也被配置解析等路径复用：[providers/mod.rs:261](../../others/cc-switch/src-tauri/src/proxy/providers/mod.rs#L261)。
- MCP app 标志及实际同步分支：[app_config.rs:28](../../others/cc-switch/src-tauri/src/app_config.rs#L28)、[mcp.rs:136](../../others/cc-switch/src-tauri/src/services/mcp.rs#L136)；Skills app 标志：[app_config.rs:121](../../others/cc-switch/src-tauri/src/app_config.rs#L121)。仅存在 Skills 目录解析函数不能证明应用同步开关可用。
- Prompt 文件与不支持项：[prompt_files.rs:21](../../others/cc-switch/src-tauri/src/prompt_files.rs#L21)；Pi 单独路径：[prompt.rs:284](../../others/cc-switch/src-tauri/src/services/prompt.rs#L284)。Desktop 的 UI 共享映射：[App.tsx:181](../../others/cc-switch/src/App.tsx#L181)。
- 会话扫描九类来源：[session_manager/mod.rs:58](../../others/cc-switch/src-tauri/src/session_manager/mod.rs#L58)；MCode 删除拒绝：[session_manager/mod.rs:126](../../others/cc-switch/src-tauri/src/session_manager/mod.rs#L126)；会话用量只导入 Claude/Codex/Gemini/OpenCode/Grok/Pi/MCode：[session_usage.rs:120](../../others/cc-switch/src-tauri/src/services/session_usage.rs#L120)。
- Gemini 配置：[gemini_config.rs:18](../../others/cc-switch/src-tauri/src/gemini_config.rs#L18)、[gemini_config.rs:343](../../others/cc-switch/src-tauri/src/gemini_config.rs#L343)；OpenCode：[opencode_config.rs:58](../../others/cc-switch/src-tauri/src/opencode_config.rs#L58)；Hermes：[hermes_config.rs:101](../../others/cc-switch/src-tauri/src/hermes_config.rs#L101)；MCode：[mcode_config.rs:60](../../others/cc-switch/src-tauri/src/mcode_config.rs#L60)。

## 4. 真实功能清单与 README 对照

| 功能 | 审计结论 | 具体实现入口与范围 |
| --- | --- | --- |
| Provider 增删改查、排序、预设 | 实现；README 的 50+ 是文档口径，本次未对各应用去重计数 | [provider/mod.rs:5085](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L5085)、[provider/mod.rs:5207](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L5207)、[provider/mod.rs:5567](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L5567)、[provider/mod.rs:6926](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L6926) |
| 配置切换、live 导入及回填 | 实现；按应用处理原生格式，代理接管期间走热切换分支 | [provider/mod.rs:5712](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L5712)、[provider/live.rs:1867](../../others/cc-switch/src-tauri/src/services/provider/live.rs#L1867)、[provider/live.rs:1754](../../others/cc-switch/src-tauri/src/services/provider/live.rs#L1754) |
| 通用供应商 | 实现，投影 Claude/Codex/Gemini，保留各端专属字段；不是跨所有十个应用 | [provider/mod.rs:7482](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L7482) |
| 公共配置片段、端点维护 | 实现；公共 JSON/TOML 配置、端点增删及最后使用信息 | [commands/config.rs:307](../../others/cc-switch/src-tauri/src/commands/config.rs#L307)、[provider/mod.rs:6887](../../others/cc-switch/src-tauri/src/services/provider/mod.rs#L6887) |
| 本地代理 / 路由模式 | 实现；启动、停止、按应用接管、恢复、热切换、状态查询 | [commands/proxy.rs:22](../../others/cc-switch/src-tauri/src/commands/proxy.rs#L22)、[services/proxy.rs:1225](../../others/cc-switch/src-tauri/src/services/proxy.rs#L1225)、[services/proxy.rs:1772](../../others/cc-switch/src-tauri/src/services/proxy.rs#L1772) |
| 故障队列、熔断、健康状态 | 实现，按应用和 Provider；详见第 5 节 | [commands/failover.rs:74](../../others/cc-switch/src-tauri/src/commands/failover.rs#L74)、[commands/proxy.rs:322](../../others/cc-switch/src-tauri/src/commands/proxy.rs#L322) |
| 请求修正 / 整流 | 实现；thinking/signature/budget/media 重试标志按 Provider 隔离；不是通用语义修复器 | [forwarder.rs:463](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L463)、[forwarder.rs:325](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L325) |
| 请求覆盖、缓存与 reasoning 适配 | 实现于 Provider 元数据及各协议转换；需要按上游能力验收 | [provider.rs:401](../../others/cc-switch/src-tauri/src/provider.rs#L401)、[provider.rs:508](../../others/cc-switch/src-tauri/src/provider.rs#L508)、[transform_codex_chat.rs:353](../../others/cc-switch/src-tauri/src/proxy/providers/transform_codex_chat.rs#L353) |
| 端点测速 / 连通性检查 | 实现；网络延迟与可达性，不代表推理或鉴权成功 | [speedtest.rs:26](../../others/cc-switch/src-tauri/src/services/speedtest.rs#L26)、[stream_check.rs:200](../../others/cc-switch/src-tauri/src/services/stream_check.rs#L200) |
| MCP | 实现统一 CRUD、逐应用启停、原生格式同步、从七类客户端导入；不是一个 MCP 执行宿主 | [mcp.rs:84](../../others/cc-switch/src-tauri/src/services/mcp.rs#L84)、[mcp.rs:136](../../others/cc-switch/src-tauri/src/services/mcp.rs#L136)、[mcp.rs:550](../../others/cc-switch/src-tauri/src/services/mcp.rs#L550) |
| Skills | 实现仓库发现、GitHub/ZIP 安装、更新检测、逐应用同步、未管理项导入、卸载备份与恢复 | [commands/skill.rs:31](../../others/cc-switch/src-tauri/src/commands/skill.rs#L31)、[commands/skill.rs:121](../../others/cc-switch/src-tauri/src/commands/skill.rs#L121)、[commands/skill.rs:331](../../others/cc-switch/src-tauri/src/commands/skill.rs#L331)、[services/skill.rs:971](../../others/cc-switch/src-tauri/src/services/skill.rs#L971) |
| Skills 同步方式 | Auto 优先软链失败回退复制，亦可显式选择；独立 SSOT | [skill.rs:54](../../others/cc-switch/src-tauri/src/services/skill.rs#L54)、[skill.rs:563](../../others/cc-switch/src-tauri/src/services/skill.rs#L563) |
| Prompts | 实现 Markdown 预设、导入、激活与 live 回填/备份；各应用分别选定预设，不应解释为任意文本自动跨端等价转换 | [prompt.rs:153](../../others/cc-switch/src-tauri/src/services/prompt.rs#L153)、[commands/prompt.rs:16](../../others/cc-switch/src-tauri/src/commands/prompt.rs#L16) |
| 项目 Profile | README 主清单之外已有实现；只分 Claude、Claude Desktop、Codex 三组，保存 Provider/MCP/Skills/Prompt 选择 | [profile.rs:33](../../others/cc-switch/src-tauri/src/services/profile.rs#L33)、[profile.rs:124](../../others/cc-switch/src-tauri/src/services/profile.rs#L124)；应用时先退出该应用接管，按项收集警告：[profile.rs:365](../../others/cc-switch/src-tauri/src/services/profile.rs#L365) |
| 请求日志 / 用量仪表盘 | 实现按应用、Provider、模型统计、趋势、分页日志、详情和计价；详见第 7 节 | [commands/usage.rs:11](../../others/cc-switch/src-tauri/src/commands/usage.rs#L11)、[commands/usage.rs:104](../../others/cc-switch/src-tauri/src/commands/usage.rs#L104) |
| 余额脚本 / 订阅额度 | 实现脚本用量查询，以及 Claude/Codex 等订阅查询；与本地 Token 成本估算是不同数据源 | [provider/usage.rs:126](../../others/cc-switch/src-tauri/src/services/provider/usage.rs#L126)、[subscription.rs:354](../../others/cc-switch/src-tauri/src/services/subscription.rs#L354)、[commands/codex_oauth.rs:28](../../others/cc-switch/src-tauri/src/commands/codex_oauth.rs#L28) |
| 认证中心 | 实现 GitHub Copilot、Codex OAuth、xAI OAuth 的统一登录/账号接口；不等于所有客户端登录均由它接管 | [commands/auth.rs:14](../../others/cc-switch/src-tauri/src/commands/auth.rs#L14)、[commands/auth.rs:111](../../others/cc-switch/src-tauri/src/commands/auth.rs#L111)、[commands/auth.rs:234](../../others/cc-switch/src-tauri/src/commands/auth.rs#L234) |
| 备份 / 导入导出 | 实现完整 SQL 导入导出、数据库备份、轮换、重命名、恢复；Skills 文件与数据库备份不是同一载荷 | [commands/import_export.rs:32](../../others/cc-switch/src-tauri/src/commands/import_export.rs#L32)、[commands/import_export.rs:145](../../others/cc-switch/src-tauri/src/commands/import_export.rs#L145)、[database/backup.rs:117](../../others/cc-switch/src-tauri/src/database/backup.rs#L117) |
| 云同步 | README 的自定义云盘目录和 WebDAV 之外，已有 S3；是 SQL+Skills ZIP 快照同步，不是实时多端数据库合并 | [WebdavSyncSection.tsx:95](../../others/cc-switch/src/components/settings/WebdavSyncSection.tsx#L95)、[s3_sync.rs:38](../../others/cc-switch/src-tauri/src/services/s3_sync.rs#L38)、[sync_protocol.rs:357](../../others/cc-switch/src-tauri/src/services/sync_protocol.rs#L357) |
| 系统托盘 | Provider 快切、Auto 故障模式、用量后缀、Profile 菜单；普通 Provider 分区只有 Claude/Codex/Gemini/Grok | [tray.rs:163](../../others/cc-switch/src-tauri/src/tray.rs#L163)、[tray.rs:441](../../others/cc-switch/src-tauri/src/tray.rs#L441)、[tray.rs:558](../../others/cc-switch/src-tauri/src/tray.rs#L558)、[tray.rs:793](../../others/cc-switch/src-tauri/src/tray.rs#L793) |
| 会话 | 实现浏览搜索、消息加载、终端恢复和单项/批量删除；各端有差别，MCode 例外见矩阵 | [SessionManagerPage.tsx:244](../../others/cc-switch/src/components/sessions/SessionManagerPage.tsx#L244)、[commands/session_manager.rs:62](../../others/cc-switch/src-tauri/src/commands/session_manager.rs#L62)、[session_manager/mod.rs:100](../../others/cc-switch/src-tauri/src/session_manager/mod.rs#L100) |
| Codex 统一会话历史 | 实现 custom provider 桶投影；可选迁入旧官方会话，迁移前备份 JSONL 和 state DB，有恢复入口 | [codex_config.rs:3650](../../others/cc-switch/src-tauri/src/codex_config.rs#L3650)、[codex_history_migration.rs:192](../../others/cc-switch/src-tauri/src/codex_history_migration.rs#L192)、[commands/settings.rs:148](../../others/cc-switch/src-tauri/src/commands/settings.rs#L148) |
| 深链 | 实现 ccswitch://v1/import 的 Provider/MCP/Prompt/Skill 解析及确认 UI；远程 configUrl 尚未实现 | [deeplink/parser.rs:15](../../others/cc-switch/src-tauri/src/deeplink/parser.rs#L15)、[DeepLinkImportDialog.tsx:262](../../others/cc-switch/src/components/DeepLinkImportDialog.tsx#L262)、[deeplink/provider.rs:617](../../others/cc-switch/src-tauri/src/deeplink/provider.rs#L617) |
| OpenClaw 工作区 | 实现白名单 Agent 文件、每日记忆读写搜索、默认模型、env/tools 配置 | [commands/workspace.rs:9](../../others/cc-switch/src-tauri/src/commands/workspace.rs#L9)、[commands/workspace.rs:194](../../others/cc-switch/src-tauri/src/commands/workspace.rs#L194)、[commands/openclaw.rs:50](../../others/cc-switch/src-tauri/src/commands/openclaw.rs#L50) |
| Hermes / OpenCode 扩展 | Hermes 记忆配置、Web UI/dashboard；OpenCode OMO/OMO Slim 本地配置、启停、agents/categories | [commands/hermes.rs:59](../../others/cc-switch/src-tauri/src/commands/hermes.rs#L59)、[commands/hermes.rs:98](../../others/cc-switch/src-tauri/src/commands/hermes.rs#L98)、[commands/omo.rs:8](../../others/cc-switch/src-tauri/src/commands/omo.rs#L8)、[services/omo.rs:1068](../../others/cc-switch/src-tauri/src/services/omo.rs#L1068) |
| 出站网络代理 / 环境冲突 | 实现全局代理验证保存与运行态更新、代理探测；环境变量冲突检测、备份删除及恢复 | [commands/global_proxy.rs:35](../../others/cc-switch/src-tauri/src/commands/global_proxy.rs#L35)、[commands/global_proxy.rs:213](../../others/cc-switch/src-tauri/src/commands/global_proxy.rs#L213)、[commands/env.rs:8](../../others/cc-switch/src-tauri/src/commands/env.rs#L8) |
| 桌面系统集成 | 自动启动、应用更新、窗口配置、简中/繁中/英/日有源码；三平台声明未在本次运行验证 | [commands/settings.rs:198](../../others/cc-switch/src-tauri/src/commands/settings.rs#L198)、[commands/settings.rs:306](../../others/cc-switch/src-tauri/src/commands/settings.rs#L306)、[tauri.conf.json:12](../../others/cc-switch/src-tauri/tauri.conf.json#L12)、[i18n/index.ts:4](../../others/cc-switch/src/i18n/index.ts#L4) |

README 的主题切换、首次登录确认、签名/插件相关“小工具”还出现在 [README_ZH.md:226](../../others/cc-switch/README_ZH.md#L226)、[README_ZH.md:268](../../others/cc-switch/README_ZH.md#L268)。本次未逐项追完这些小工具的全部执行链，保留为后续 parity 核对项，不据文案认定全部实现或验收成功。UI 美观程度也未做运行截图评估。

## 5. 路由、故障转移与模型目录

### 5.1 实际选路顺序

静态调用链为：HTTP handler → RequestContext 提取 model/session → `select_providers(app_type)` → 当前 Provider 或有序故障队列 → 每个候选的模型改写与协议转换 → 上游请求 → 响应/用量处理。关键位置：[handlers.rs:187](../../others/cc-switch/src-tauri/src/proxy/handlers.rs#L187)、[handler_context.rs:132](../../others/cc-switch/src-tauri/src/proxy/handler_context.rs#L132)、[forwarder.rs:429](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L429)。

| 情形 | 实际行为 | 证据 |
| --- | --- | --- |
| 故障转移关闭 | 只返回当前 Provider；应用层故障转移超时配置旁路，重试次数置 0 | [provider_router.rs:112](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L112)、[handler_context.rs:201](../../others/cc-switch/src-tauri/src/proxy/handler_context.rs#L201) |
| 故障转移开启 | 只按 DAO 故障队列顺序尝试，当前 Provider 不会自动插在队首 | [provider_router.rs:82](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L82) |
| Provider 不健康 | 按 app_type:provider_id 管理熔断状态；筛选可用后，发送前再取 half-open 许可 | [provider_router.rs:103](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L103)、[forwarder.rs:482](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L482) |
| 单候选转发 | forwarder 跳过发送前熔断许可检查；自动队列选取阶段仍可能已过滤 open 状态，不能概括为所有单候选都忽略熔断 | [forwarder.rs:460](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L460) |
| 可重试错误 | 网络、超时及多数上游错误可换 Provider；400/405/406/413/414/415/422/501 被列为不可重试；官方 Codex 全部不可重试 | [forwarder.rs:2777](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L2777) |
| 200 后失效 | 有完整非流式响应读取、流首块预取及 Responses 语义错误识别，避免仅按 HTTP 200 认成功 | [forwarder.rs:2463](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L2463)、[forwarder.rs:3161](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L3161) |
| 已输出的流中途失败 | 报错，不在已有输出后切到另一家继续拼接 | [forwarder.rs:3201](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L3201) |
| 成功切到备用 Provider | 可更新逻辑当前目标、托盘和 UI 事件；仅在对应应用启用接管时更新 | [failover_switch.rs:81](../../others/cc-switch/src-tauri/src/proxy/failover_switch.rs#L81) |

熔断实现具备 Closed/Open/HalfOpen、连续失败阈值、半开成功阈值、冷却时间、错误率和最小样本量；不是仅有设置页面。证据：[circuit_breaker.rs:38](../../others/cc-switch/src-tauri/src/proxy/circuit_breaker.rs#L38)、[circuit_breaker.rs:157](../../others/cc-switch/src-tauri/src/proxy/circuit_breaker.rs#L157)、[circuit_breaker.rs:230](../../others/cc-switch/src-tauri/src/proxy/circuit_breaker.rs#L230)。这里没有每模型独立的健康 key；同一 Provider 下不同模型共享这条应用级 Provider 健康维度。

### 5.2 哪些“模型路由”已经存在

| 能力 | 现状及边界 | 证据 |
| --- | --- | --- |
| Claude 模型档位映射 | 按 haiku/sonnet/opus/fable、subagent/default 重写 body.model；当前 Provider 内映射 | [model_mapper.rs:69](../../others/cc-switch/src-tauri/src/proxy/model_mapper.rs#L69)、[model_mapper.rs:119](../../others/cc-switch/src-tauri/src/proxy/model_mapper.rs#L119) |
| Claude Desktop 安全模型路由 | route_id→上游 model，支持显示名和 1M 标记；不含另一 provider_id，未知模型可报错 | [provider.rs:390](../../others/cc-switch/src-tauri/src/provider.rs#L390)、[claude_desktop_config.rs:686](../../others/cc-switch/src-tauri/src/claude_desktop_config.rs#L686) |
| Grok 原生模型配置投影 | 在已选 Provider 上应用真实模型，再做可选 Chat/Anthropic 桥接 | [forwarder.rs:1260](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L1260) |
| 上游模型发现 | 请求单个 Provider 的模型端点，支持 data[].id 和 models[].slug/id；404/405 时换候选 URL，其他失败直接返回 | [model_fetch.rs:83](../../others/cc-switch/src-tauri/src/services/model_fetch.rs#L83)、[model_fetch.rs:120](../../others/cc-switch/src-tauri/src/services/model_fetch.rs#L120) |
| Codex 模型目录生成 | Provider settings.modelCatalog.models→目录条目，含显示名、上下文、输入模态、reasoning 等；生成 cc-switch-model-catalog.json 并写 model_catalog_json | [codex_config.rs:1575](../../others/cc-switch/src-tauri/src/codex_config.rs#L1575)、[codex_config.rs:1701](../../others/cc-switch/src-tauri/src/codex_config.rs#L1701)、[codex_config.rs:2421](../../others/cc-switch/src-tauri/src/codex_config.rs#L2421) |
| Codex 目录 UI 持久化 | 表单将目录行保存回 Provider modelCatalog；不是仅留在 README 的示例 | [ProviderForm.tsx:1541](../../others/cc-switch/src/components/providers/forms/ProviderForm.tsx#L1541) |
| 本地 GET /models、/v1/models | 返回当前 live 配置所引用且属于 CC Switch 的目录，缺失时返回 models 空数组；不是聚合所有 Provider 的 OpenAI data 列表 | [server.rs:322](../../others/cc-switch/src-tauri/src/proxy/server.rs#L322)、[handlers.rs:88](../../others/cc-switch/src-tauri/src/proxy/handlers.rs#L88) |
| ChatGPT Codex 模型发现 | 单独调用 ChatGPT Codex 后端 models，携带托管账号 token、workspace ID 和版本头 | [services/codex_oauth_models.rs:12](../../others/cc-switch/src-tauri/src/services/codex_oauth_models.rs#L12)、[services/codex_oauth_models.rs:41](../../others/cc-switch/src-tauri/src/services/codex_oauth_models.rs#L41) |
| models.dev 数据 | 已有定价同步；不能据此说支持自动跨 Provider 路由或完整实时能力探测 | [modelsDevAutoSync.ts:26](../../others/cc-switch/src/lib/modelsDevAutoSync.ts#L26) |

**未发现的增强空间**：在所审查的 ProviderRouter、RequestContext、forwarder 和目录生成链中，没有请求模型→多 Provider 候选规则、按模型独立优先级/故障队列/熔断、按价格/延迟动态选 Provider 或全 Provider 聚合目录的通用实现。这个结论依据正向调用链，而非只搜索一个“routing”关键词；不能把已有模型别名替换重新命名成这项新能力。

目录也不是能力探测器：通用模板会按 NativeResponses/ProxyChat/Anthropic 改造工具声明，某些厂商使用随应用打包的专用模板，可能随客户端或厂商协议变化而漂移。[codex_config.rs:1612](../../others/cc-switch/src-tauri/src/codex_config.rs#L1612)、[codex_config.rs:2299](../../others/cc-switch/src-tauri/src/codex_config.rs#L2299)。

### 5.3 DeepSeek 的本地事实

DeepSeek 当前本地预设明确写 `apiFormat: "openai_responses"`，附带模型目录；NativeResponses 分支会加载打包的 DeepSeek 专用目录，且允许其官方模板保留通用模板会裁剪的能力字段。证据：[codexProviderPresets.ts:1375](../../others/cc-switch/src/config/codexProviderPresets.ts#L1375)、[codex_config.rs:2094](../../others/cc-switch/src-tauri/src/codex_config.rs#L2094)、[codex_config.rs:2314](../../others/cc-switch/src-tauri/src/codex_config.rs#L2314)。这是**本地实现事实**，没有运行请求验证该快照内每个模型别名、上下文值或 reasoning 档位当前仍有效。Responses→Chat 历史兼容代码仍在，只能证明兼容路径存在。

## 6. Codex 官方账号与协议适配边界

### 6.1 三种凭据路径应分别看待

| 路径 | 已实现的行为 | 不应越过的结论边界 |
| --- | --- | --- |
| Codex 原生官方账号 | 识别官方账号 Provider；生成 cc-switch-official 本地路由，requires_openai_auth=true，保留客户端传入 Authorization；固定转向 ChatGPT Codex 后端 | 不是把 ChatGPT 登录转换成普通 OpenAI API key；有存储 API key 的普通官方 API 配置另行识别 |
| CC Switch 托管 Codex OAuth | Device Code 登录、账号列表/绑定、token 刷新、账号锁、独立本地账号 ID 与上游 workspace ID；提供模型及额度查询 | 管理多个账号不等于可以任意轮换正在进行的 Codex 官方会话 |
| Claude / Desktop 使用 Codex OAuth 上游 | Claude adapter 强制 Responses 格式，forwarder 从托管账号取 token 并补 ChatGPT 账号头，再桥接 Anthropic 消息和响应 | 能桥接消息不等于完整复现 Codex 客户端所有工具、插件、桌面功能或服务端语义 |

证据：官方 Provider 的分类与固定上游见 [providers/codex.rs:292](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L292)、[providers/codex.rs:959](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L959)；官方投影见 [codex_config.rs:3494](../../others/cc-switch/src-tauri/src/codex_config.rs#L3494)、[codex_config.rs:3541](../../others/cc-switch/src-tauri/src/codex_config.rs#L3541)；托管认证入口和锁见 [codex_oauth_auth.rs:388](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L388)、[codex_oauth_auth.rs:446](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L446)、[codex_oauth_auth.rs:725](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L725)；Claude OAuth 桥接见 [providers/claude.rs:35](../../others/cc-switch/src-tauri/src/proxy/providers/claude.rs#L35)、[forwarder.rs:1783](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L1783)。

关键限制：

- **官方账号禁止跨 Provider 重试。** 当前官方账号即使遗留自动故障转移配置，也保持单路由；故障队列中过期的官方项被跳过。forwarder 对官方路由的错误一律判为不可重试，防止复用其入站凭据访问另一账号卡片。[provider_router.rs:72](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L72)、[forwarder.rs:2777](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L2777)。
- **切账号可能要求新建会话/重启客户端。** 托管官方账号会检查入站 chatgpt-account-id 和会话是否匹配，不匹配报错；热切换目标不代表旧客户端已经加载新登录。[forwarder.rs:57](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L57)。
- **代理只有 HTTP/SSE 这条本地契约。** 官方代理投影明确将 supports_websockets 设为 false；直连的统一官方配置表可设 true。这里的字段存在不能推出本地实现 WebSocket 代理。[codex_config.rs:3509](../../others/cc-switch/src-tauri/src/codex_config.rs#L3509)、[codex_config.rs:3571](../../others/cc-switch/src-tauri/src/codex_config.rs#L3571)。
- **官方登录保留是选项，默认 false。** 普通第三方直接切换把 key 放入 Provider 作用域的 experimental_bearer_token；是否保留 auth.json 受开关控制，关闭时计划删除 auth 文件。接管另有保留规则，不能把两个路径混为一谈。[settings.rs:393](../../others/cc-switch/src-tauri/src/settings.rs#L393)、[codex_config.rs:3890](../../others/cc-switch/src-tauri/src/codex_config.rs#L3890)、[codex_config.rs:3932](../../others/cc-switch/src-tauri/src/codex_config.rs#L3932)。
- **存在防止第三方误用官方凭据的校验。** 缺少可用 key 却回退官方 auth、或缺少承载 key 的自定义 Provider 表会拒绝写入；不是任何手工拼接的配置都被无条件接受。[codex_config.rs:3940](../../others/cc-switch/src-tauri/src/codex_config.rs#L3940)。
- 是否允许某个第三方客户端使用账号、长期服务稳定性及官方支持承诺，不能从这些私有后端 URL、客户端版本头或测试源码推出。本地仅证明有相应实现；最新官方认证 helper 契约由主线程核实。

### 6.2 协议方向与有损之处

| 客户端输入 | 上游方向 | 本地证据与限制 |
| --- | --- | --- |
| Anthropic Messages | Anthropic 透传、OpenAI Chat、Responses、Gemini Native | [providers/claude.rs:35](../../others/cc-switch/src-tauri/src/proxy/providers/claude.rs#L35)、[handlers.rs:237](../../others/cc-switch/src-tauri/src/proxy/handlers.rs#L237)；由显式格式/托管类型决定 |
| Codex Responses | 原生 Responses、Chat Completions、Anthropic Messages | [providers/codex.rs:25](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L25)、[providers/codex.rs:168](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L168)；不是简单修改 base_url |
| Gemini 原生路径 | Gemini 原生上游、API key/OAuth 形状 | [server.rs:393](../../others/cc-switch/src-tauri/src/proxy/server.rs#L393)、[providers/gemini.rs:64](../../others/cc-switch/src-tauri/src/proxy/providers/gemini.rs#L64)；不得推定存在 Gemini→所有协议的通用转换 |

Responses→Chat 有工具名/namespace、reasoning、响应/SSE 等转换；还维护内存历史以补齐 previous_response_id + tool output，最多缓存 512 个响应，不能等同完整服务端持久 Responses 会话。[transform_codex_chat.rs:258](../../others/cc-switch/src-tauri/src/proxy/providers/transform_codex_chat.rs#L258)、[codex_chat_history.rs:10](../../others/cc-switch/src-tauri/src/proxy/providers/codex_chat_history.rs#L10)、[codex_chat_history.rs:90](../../others/cc-switch/src-tauri/src/proxy/providers/codex_chat_history.rs#L90)。

Responses→Anthropic 会丢弃不支持的 hosted tools，例如 web_search，因此目录写入路径主动禁用相应工具；不能承诺协议功能无损。[transform_codex_anthropic.rs:397](../../others/cc-switch/src-tauri/src/proxy/providers/transform_codex_anthropic.rs#L397)、[codex_config.rs:2434](../../others/cc-switch/src-tauri/src/codex_config.rs#L2434)。原生 Responses 也有 xAI namespace/schema 特例，表明“同叫 Responses”并不保证所有客户端私有字段兼容：[providers/codex.rs:209](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L209)。

本地还注册 compact、图像生成/编辑、alpha/search 等 Codex 相关 HTTP 路由；注册入口只能证明转发路径存在，不能推出所有桥接上游都支持这些端点。[server.rs:326](../../others/cc-switch/src-tauri/src/proxy/server.rs#L326)。

## 7. 日志、用量、备份和同步的真实语义

### 7.1 日志与用量

- 请求记录字段包括应用、Provider、请求模型/响应模型/计价模型、Token/cache 用量、费用、首 Token 时间、延迟、状态、错误、session 和流标志；这份记录结构不是完整请求/响应正文归档。[usage/logger.rs:64](../../others/cc-switch/src-tauri/src/proxy/usage/logger.rs#L64)。
- 代理记录与本地会话导入写入同一用量表，存在去重和代理数据替换会话记录的规则；不能把两个来源直接相加。[usage/logger.rs:123](../../others/cc-switch/src-tauri/src/proxy/usage/logger.rs#L123)、[session_usage.rs:120](../../others/cc-switch/src-tauri/src/services/session_usage.rs#L120)。
- 自定义定价、倍数、计价模型来源及 models.dev 同步属于估算账本，余额脚本及订阅额度来自不同上游接口；没有证据说明本地成本等于供应商最终账单。[commands/proxy.rs:180](../../others/cc-switch/src-tauri/src/commands/proxy.rs#L180)、[commands/proxy.rs:237](../../others/cc-switch/src-tauri/src/commands/proxy.rs#L237)、[commands/usage.rs:175](../../others/cc-switch/src-tauri/src/commands/usage.rs#L175)。
- 用量脚本通过受限 QuickJS 计算 request/extractor，JS 执行限 5 秒、内存 16 MiB、栈 256 KiB；网络请求超时另限 2–30 秒。它仍能声明实际 HTTP URL、headers/body，不应按纯静态说明文本处理。[usage_script.rs:35](../../others/cc-switch/src-tauri/src/usage_script.rs#L35)、[usage_script.rs:247](../../others/cc-switch/src-tauri/src/usage_script.rs#L247)。

### 7.2 备份与同步

- 数据库定期备份默认间隔 24 小时、保留 10 份，可配置；另有手工备份/恢复。README 的“保留最近 10 个”描述的是默认值。[settings.rs:1109](../../others/cc-switch/src-tauri/src/settings.rs#L1109)、[database/backup.rs:412](../../others/cc-switch/src-tauri/src/database/backup.rs#L412)。
- 普通导出是完整数据库 SQL；WebDAV/S3 同步是 db.sql + skills.zip + manifest，带 SHA-256、大小与版本信息；先上传载荷再上传 manifest。[database/backup.rs:117](../../others/cc-switch/src-tauri/src/database/backup.rs#L117)、[sync_protocol.rs:149](../../others/cc-switch/src-tauri/src/services/sync_protocol.rs#L149)、[webdav_sync.rs:53](../../others/cc-switch/src-tauri/src/services/webdav_sync.rs#L53)、[s3_sync.rs:38](../../others/cc-switch/src-tauri/src/services/s3_sync.rs#L38)。
- 同步排除请求日志、连通性日志、健康、live 备份、用量汇总与会话导入游标等本机表；下载保留其中明确列出的本地表。托管 OAuth 外部 JSON 不在这份 SQL+Skills 构造路径内，因此不能将其称为“完整账号登录迁移”。[database/backup.rs:85](../../others/cc-switch/src-tauri/src/database/backup.rs#L85)、[codex_oauth_auth.rs:414](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L414)。
- 下载是恢复快照：先备份/替换 Skills，再导入数据库，数据库失败时回滚 Skills；不是每条 Provider 记录的双向冲突合并。[sync_protocol.rs:357](../../others/cc-switch/src-tauri/src/services/sync_protocol.rs#L357)。
- 自动同步 worker 对数据库变更进行合并后上传；有手工下载入口，不能仅凭“云同步”称为持续自动双向合并。[webdav_auto_sync.rs:157](../../others/cc-switch/src-tauri/src/services/webdav_auto_sync.rs#L157)、[commands/webdav_sync.rs:136](../../others/cc-switch/src-tauri/src/commands/webdav_sync.rs#L136)。
- README 中 Dropbox/OneDrive/iCloud 的方案是自定义数据目录交给外部同步客户端；本次没有发现它们各自的专用账号 SDK 集成证据。WebDAV 和 S3 则有应用自己的上传/下载实现。[README_ZH.md:266](../../others/cc-switch/README_ZH.md#L266)、[commands/settings.rs:296](../../others/cc-switch/src-tauri/src/commands/settings.rs#L296)。

## 8. 安全与数据保护：已有措施和缺口

| 边界 | 已有证据 | 审计判断 |
| --- | --- | --- |
| 本地监听 | 默认 127.0.0.1:15721；监听地址可配置 | 通用 router 未挂统一入口 token 校验；不能视作可直接暴露公网的多租户代理。[types.rs:42](../../others/cc-switch/src-tauri/src/proxy/types.rs#L42)、[server.rs:291](../../others/cc-switch/src-tauri/src/proxy/server.rs#L291)、[services/proxy.rs:1955](../../others/cc-switch/src-tauri/src/services/proxy.rs#L1955) |
| Desktop gateway | 单独检查 Bearer gateway token | 这是明确实现的入口保护，不应推及全部通用端点。[handlers.rs:271](../../others/cc-switch/src-tauri/src/proxy/handlers.rs#L271) |
| 上游凭据隔离 | 通常替换入站认证头；仅官方 Codex 路径保留客户端 Authorization，固定官方上游并校验所选账号 | 是路由安全的关键，跨模型选路必须保留这种账号边界。[forwarder.rs:2082](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L2082)、[providers/codex.rs:959](../../others/cc-switch/src-tauri/src/proxy/providers/codex.rs#L959) |
| 密钥落盘 | Provider settings_config 序列化进 SQLite；托管 Codex OAuth JSON 含 refresh_token，Unix 写入权限 0600 | 所读持久化路径没有应用层加密；0600 不是加密，也不是系统钥匙串。[providers.rs:217](../../others/cc-switch/src-tauri/src/database/dao/providers.rs#L217)、[codex_oauth_auth.rs:284](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L284)、[codex_oauth_auth.rs:1993](../../others/cc-switch/src-tauri/src/proxy/providers/codex_oauth_auth.rs#L1993) |
| 导出与远端同步 | Provider 表不在同步跳过列表；SQL 文本和 Skills ZIP 原样形成上传载荷 | 可携带 Provider 的明文密钥和敏感配置；SHA-256 是完整性校验，不是保密机制。所读上传链未见端到端加密。[database/backup.rs:85](../../others/cc-switch/src-tauri/src/database/backup.rs#L85)、[sync_protocol.rs:156](../../others/cc-switch/src-tauri/src/services/sync_protocol.rs#L156) |
| 前端设置读取 | 清除返回值中的 WebDAV password 和 S3 secret_access_key | 已有脱敏，但不可外推为所有 Provider/API/错误输出均无秘密。[settings.rs:771](../../others/cc-switch/src-tauri/src/settings.rs#L771) |
| 日志脱敏 | URL 删除 userinfo/query/fragment，并按已知值替换秘密；模型发现错误体另截断脱敏 | 有明确措施，未做全仓每条日志的数据流证明。[lib.rs:179](../../others/cc-switch/src-tauri/src/lib.rs#L179)、[model_fetch.rs:166](../../others/cc-switch/src-tauri/src/services/model_fetch.rs#L166) |
| 配置原子性 | 公共 atomic_write 使用临时文件/替换，另有 private 写入 | README 不能解读为所有文件均原子；settings.json 仍直接 create/write/truncate。[config.rs:383](../../others/cc-switch/src-tauri/src/config.rs#L383)、[settings.rs:711](../../others/cc-switch/src-tauri/src/settings.rs#L711) |
| 代理退出与恢复 | 有严格备份、恢复及失败保留备份路径；Codex 还处理较新账号 token 的恢复竞态 | 这是需要复用的业务语义，不是“停止端口”即可完成；本次未做崩溃/断电验证。[services/proxy.rs:1225](../../others/cc-switch/src-tauri/src/services/proxy.rs#L1225)、[services/proxy.rs:91](../../others/cc-switch/src-tauri/src/services/proxy.rs#L91)、[services/proxy.rs:1772](../../others/cc-switch/src-tauri/src/services/proxy.rs#L1772) |
| SQL 导入 | 在临时库使用 SQLite authorizer，拒绝 Attach/Detach、虚拟表、越界 PRAGMA 等，再做模式校验 | 真实防护实现，不只检查导出文件头。[database/backup.rs:63](../../others/cc-switch/src-tauri/src/database/backup.rs#L63)、[database/backup.rs:220](../../others/cc-switch/src-tauri/src/database/backup.rs#L220) |
| Skills ZIP | 条目数、展开总量、下载量与符号链接目标限制；检查 ParentDir 等路径分量 | 已有压缩包和路径防护；安装内容本身是否可信仍与来源有关。[skill.rs:319](../../others/cc-switch/src-tauri/src/services/skill.rs#L319)、[skill.rs:4135](../../others/cc-switch/src-tauri/src/services/skill.rs#L4135) |
| 目录与会话路径 | Codex 目录读取限制所属路径及 symlink 逃逸；删除会话按来源根目录校验；工作区文件白名单 | 部分具体入口具备防护，不声称所有磁盘访问都有统一沙箱。[codex_config.rs:2539](../../others/cc-switch/src-tauri/src/codex_config.rs#L2539)、[session_manager/mod.rs:188](../../others/cc-switch/src-tauri/src/session_manager/mod.rs#L188)、[commands/workspace.rs:9](../../others/cc-switch/src-tauri/src/commands/workspace.rs#L9) |
| 深链 | 校验 scheme/version/path/resource，确认 UI 遮罩 key | 深链可含 apiKey/usageScript 等敏感内容；Provider 深链只列七个应用，未含 Desktop/Pi/MCode；configUrl 明确返回未支持。[deeplink/parser.rs:81](../../others/cc-switch/src-tauri/src/deeplink/parser.rs#L81)、[deeplink/parser.rs:132](../../others/cc-switch/src-tauri/src/deeplink/parser.rs#L132)、[deeplink/provider.rs:617](../../others/cc-switch/src-tauri/src/deeplink/provider.rs#L617) |
| 桌面包与更新 | Tauri CSP、受限 asset scope、更新公钥和更新端点有配置 | 仅证明配置和更新入口存在，未审计发行签名、依赖供应链或安装包安全。[tauri.conf.json:28](../../others/cc-switch/src-tauri/tauri.conf.json#L28)、[tauri.conf.json:62](../../others/cc-switch/src-tauri/tauri.conf.json#L62) |

## 9. 版本、许可证与 Rust + Slint 的可借鉴边界

项目根 LICENSE 为 MIT，版权标记为 Jason Young、2025；许可证文本要求在副本或实质部分保留版权和许可声明，并包含按原样提供的免责条款。[LICENSE:1](../../others/cc-switch/LICENSE#L1)。这份根许可证结论不自动覆盖依赖、图标、第三方内嵌模板或上游服务条款；本次没有逐资产核验，也未核验 Slint 的许可条件。

现有 UI 是 React/TypeScript/Tauri，不是 Slint；后端含 Rust/Tokio/Axum/SQLite。证据：[package.json:23](../../others/cc-switch/package.json#L23)、[package.json:83](../../others/cc-switch/package.json#L83)、[Cargo.toml:30](../../others/cc-switch/src-tauri/Cargo.toml#L30)、[Cargo.toml:47](../../others/cc-switch/src-tauri/Cargo.toml#L47)。业务层也非完全独立：forwarder 持有 Tauri AppHandle 并从应用状态取认证管理器，故障转移会发事件和刷新托盘。[forwarder.rs:168](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L168)、[forwarder.rs:1785](../../others/cc-switch/src-tauri/src/proxy/forwarder.rs#L1785)、[failover_switch.rs:100](../../others/cc-switch/src-tauri/src/proxy/failover_switch.rs#L100)。

因此，对于 SwitchX 的研究输入是：可以参考配置投影、恢复保护、熔断和协议边界；React 组件不能直接视作 Slint UI，Rust 服务也要先识别 Tauri 依赖。这里不要求现在抽象或移植这些模块。

## 10. 对 SwitchX v1 的有界需求输入

以下是基于审计的设计建议，不是 CC Switch 已实现事实，也不是本轮开发承诺：

1. 将 Provider、模型目录、路由规则、上游协议、账号凭据分别表达。首个新增能力应可明确描述为“请求模型 A → Provider X 的模型 B → 受约束备用 Provider Y”，并能解释命中规则；不能只给当前 Provider 加一张模型菜单。
2. Codex 直接切换、第三方代理、官方账号透传设为可区分的模式；保留原生登录和代理退出恢复的状态转换应纳入 v1 验收。官方账号不能直接进入普通 API key 故障队列。
3. 目录、定价表和 HTTP 可达性分别标注来源与更新时间。目录可见、端点返回状态、一次文本回复、工具调用和图片/流式完整性应是不同验证结果。
4. 按本报告矩阵建立后续 parity 清单：MCP/Skills/Prompts、用量/会话、备份/WebDAV/S3、Profile/托盘/深链逐项实现，不将“已支持某应用”理解为所有子功能到齐。
5. Slint 的页面组织可围绕“配置与 Provider、模型与规则、运行状态、扩展、数据与设置”；美观方案应在独立 UI 设计任务中验证，本审计不新增产品代码或界面文件。

## 11. 验证记录与未完成边界

- 只执行文件列举、rg、带行号的源码读取及 Git 只读命令；本轮唯一新增工作区文件为本报告，没有修改 others/cc-switch，没有提交。
- 已对照 README、前端入口、Tauri command 注册、服务及转发调用链。注册证据例如 [lib.rs:1491](../../others/cc-switch/src-tauri/src/lib.rs#L1491)、[lib.rs:1513](../../others/cc-switch/src-tauri/src/lib.rs#L1513)，避免把孤立函数视为已接入 UI 的完整功能。
- 对故障顺序、官方账号隔离、目录所有权等存在测试源码旁证，例如 [provider_router.rs:408](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L408)、[provider_router.rs:473](../../others/cc-switch/src-tauri/src/proxy/provider_router.rs#L473)、[codex_config.rs:8179](../../others/cc-switch/src-tauri/src/codex_config.rs#L8179)。**本轮没有执行这些测试。**
- 未完成的验证包括真实账号登录/刷新、上游模型可用性、全部协议能力矩阵、跨平台 UI/托盘、应用崩溃恢复、多设备同步竞争和逐依赖许可证检查。这些不影响上述“源码存在/缺失”的结论，但限制任何“已实测可用”的表述。
