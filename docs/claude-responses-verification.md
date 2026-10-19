# Claude Messages → OpenAI Responses 验证

日期：2026-10-19。范围：M3 的 Responses 适配；Chat Completions、Gemini、云平台认证和自动备用队列仍在后续阶段。

## 使用方式与范围

连接编辑器选择 **OpenAI Responses**，填写 API 根地址和 Bearer Token，保存后进入「路由」预览发布。连接卡片的「设置路由」会选中此连接并打开单连接路由设置，也可手动选择聚合。旧连接缺省使用 Anthropic Messages；协议随连接设置保存，不根据模型名称猜测。

API 根地址不包含资源路径。例如 `https://api.openai.com` 对应 `/v1/responses` 和 `/v1/models`。自定义网关前缀原样保留。Responses 连接不能写为 Claude 直连；模型发现和请求仅使用该连接的 Bearer 凭据，不转发客户端令牌、Cookie、Anthropic 版本或 beta 头。

| 场景 | 范围 |
| --- | --- |
| 文字与系统提示 | 支持字符串与文字块、多轮 user/assistant 历史 |
| 图片 | 支持用户消息中的 PNG/JPEG/GIF/WebP base64 与 HTTPS URL |
| 函数工具 | 保留 schema、调用 ID、参数和成功的文字结果，保持文字与调用顺序；支持 auto/any/none/指定工具和禁用并行 |
| CLI Effort | output_config.effort 的 low/medium/high/max 映射为 reasoning.effort；上游需支持相应值；默认 temperature=1 不转发，其他采样设置与 Effort 同用时拒绝 |
| JSON 回复 | 转为 Claude message，公开模型 ID 和本次消息 ID 由 SwitchX 统一改写 |
| SSE 回复 | 等待完整 Responses JSON 后生成 Claude 事件；Unicode 文字与工具参数分片；没有上游逐字增量 |
| 结束原因 | 完成文字对应 end_turn，完整工具调用对应 tool_use；输出上限对应 max_tokens，不返回不完整工具调用 |
| 用量 | 缓存命中从总输入中拆出为 Claude cache_read_input_tokens；输出总数包含 reasoning，但不生成 Claude Thinking 块 |
| 生命周期 | 与 Messages 连接混合聚合；在途请求保留原发布绑定；取消和停止沿用路由生命周期 |
| 不支持 | Claude Thinking、assistant 预填充、文档等未知内容、失败工具结果、非空 stop_sequences、带注释或 refusal 输出、count_tokens、未知 Claude 查询参数 |

缓存提示与用户元数据不转发，可能影响缓存费用；发布预览会说明这些限制。CLI 的 `?beta=true` 是已识别的 Messages 标记，转换时移除；其他查询参数拒绝。无法转换的内容、无效输出、未结束上游和无效用量明确报错。HTTP 错误及无效 JSON 的原始正文不进入响应、日志或请求记录。

工具定义显式使用 `strict: false`，避免转换时改变原 schema 的可选属性含义；调用与结果使用 `call_id` 对应。参考 [OpenAI 官方 Responses 迁移说明](https://developers.openai.com/api/docs/guides/migrate-to-responses) 与 [Responses 创建接口](https://developers.openai.com/api/reference/resources/responses/methods/create)。

Effort 是显式请求转换，具体模型可用级别由上游校验；不根据模型名推断能力。推理请求的采样限制参考 [OpenAI 推理指南](https://developers.openai.com/api/docs/guides/reasoning)。Claude CLI 使用本适配时需关闭 Thinking，例如 `MAX_THINKING_TOKENS=0 claude`。

## 自动化与界面

- 适配单元测试覆盖历史顺序、图片、工具选择、成功和失败结果、输出上限、无效用量、未知内容和大段 Unicode SSE。
- 路由集成测试使用本机合成上游，覆盖混合协议、精确目的地、认证头隔离、预览限制、用量、失败脱敏、运行中更新、取消与停止。
- 服务测试覆盖旧连接协议缺省、认证校验、Responses 模型目录；MCP 模型编辑保留已有连接协议。
- 原生 Slint 回归覆盖上游协议选择、自动选用 Bearer、表单保存、连接卡片进入正确路由，以及两种窗口尺寸和浅深色。

| 尺寸 | 浅色 | 深色 |
| --- | --- | --- |
| 1000×680 | [连接设置](previews/claude-responses/responses-1000-light.png) | [连接设置](previews/claude-responses/responses-1000-dark.png) |
| 1400×900 | [连接设置](previews/claude-responses/responses-1400-light.png) | [连接设置](previews/claude-responses/responses-1400-dark.png) |

2026-10-19 完整 `make check` 通过：410 项 Rust 测试通过，2 项既有 CLI 探针默认忽略；41 项脚本测试中 2 项因缺少 squashfs-tools 与 pacman vercmp 跳过。Rust/Slint 格式检查与全目标 Clippy 均通过。完整日志为本地 `target/claude-m3-full-check.log`；四张截图由本次桌面测试生成并检查。

## 隔离 CLI 探针

`claude_route_probe` 的 `--responses` 选项使连接 A 使用 Messages、连接 B 使用 Responses。沿用隔离 CLI 副本、专用绝对目录、loopback 网络限制和恢复流程；只发送文字探针，关闭 Thinking，不使用正式账号或真实供应商。

```sh
cargo build --locked --bin switchx
cargo run --locked --example claude_route_probe -- /absolute/new/probe-directory --responses
```

此探针验证真实 Claude CLI 与合成上游的转换链路。图片、工具历史和异常负载由自动化合成测试覆盖；这些结果不代表真实供应商能力验收。

2026-10-19 使用隔离的 Claude Code 2.1.303 副本验证成功：依次收到 A、B 标记，产生 2 条路由用量和 2 个 CLI 日志文件，日志导入后仍为 2 条用量，最后恢复配置并停止路由。CLI 实际携带的 `output_config.effort` 和 `?beta=true` 均经过转换；Thinking 在隔离进程中关闭。探针目录为本地 `target/claude-m3-probe-20261019-final`，结果日志为 `target/claude-m3-cli-probe.log`。
