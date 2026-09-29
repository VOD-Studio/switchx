# 上游头像与图标选择验收

2026-09-29，macOS arm64、Slint 1.18.1。实现参考 `others/cc-switch` 的 `codexProviderPresets.ts`、`IconPicker.tsx` 与 `src/icons/extracted/`，资源快照为 `846de29`。本轮使用合成资料、本地测试和离屏 Slint 渲染，没有真实上游请求。

## 行为

- ChatGPT 订阅连接默认使用 OpenAI 绿色头像，现有 API 预设继续按准确地址显示品牌头像。
- API 和订阅表单均可点击头像，搜索 110 个内置图标的名称、ID 或关键词；支持滚动网格、选中反馈、无结果提示和恢复默认头像。
- 返回、完成和 Esc 关闭选择器并保留表单草稿。最终保存上游时才持久化头像；取消表单不修改保存记录。
- SQLite v10 增加可空 `providers.icon_id`。旧记录保持默认头像；显式覆盖优先于默认。头像与账号绑定、模型映射和 Codex 选项一起保存，API Key 留空继续保留原值。凭据和恢复 helper 兼容 v8/v9/v10。
- 图标和许可离线打包，图标 ID 只接受内置白名单，不加载用户 URL、文件或 SVG。头像不参与路由，不进入 Codex 配置和凭据。

## 检查

`make check`：161 项通过（lib 141、binary 10、router 10），1 项已有的本机 CLI 目录检查保持 ignored。Rust/Slint 格式与全部目标 Clippy `-D warnings` 通过。macOS 调试 bundle 构建成功。

聚焦检查覆盖 v9→v10 迁移、只读 helper 不迁移数据库、API Key 留空保留、旧保存入口保留头像、非法图标拒绝、ID 规范化、订阅绑定和模型选项保持，以及生产选择器回调中的搜索、覆盖、默认恢复和凭据草稿保持。

离屏预览直接使用应用的 `AppWindow` 与生产图标加载器。1180×800 和最小 1040×680、深浅两种主题生成 20 张预览；逐个渲染全部图标共 660 个状态，其中 220 个为 2x Retina 像素比例。全部无渲染异常。

渲染检查发现带内嵌 PNG 的 SVG 在 resvg 缩放后可能产生 RGB 通道大于 alpha 的预乘像素，A6API 和 nekocode 可复现软件渲染溢出。生产加载器将带位图的图标转为合法的 RGBA 图像，保留图案和配色；纯矢量继续使用 SVG。全部图标的预乘透明度约束和实际绘制均已核对。

截图为同组件的离屏渲染：

- [默认 OpenAI 与自定义上游列表](screenshots/provider-icons/providers-dark-1180x800.png)
- [自定义上游头像表单](screenshots/provider-icons/custom-editor-light-1180x800.png)
- [深色图标选择器](screenshots/provider-icons/picker-dark-1180x800.png)
- [浅色最小窗口](screenshots/provider-icons/picker-light-1040x680.png)
- [OpenAI 搜索与选中状态](screenshots/provider-icons/search-openai-dark-1180x800.png)
- [无结果状态](screenshots/provider-icons/search-empty-light-1040x680.png)

本轮尝试启动隔离原生夹具，但 Computer Use 持续返回 `cgWindowNotFound`，所以未完成原生点击验收。进程已终止，临时夹具已清理；其 Codex 配置、入口登录、保存账号文件、API 认证配置和模型映射均与启动前一致，未生成恢复日志。截图和程序化回调检查不代表原生鼠标、输入法或其他平台已验收。

## 复跑

```sh
make check
sh scripts/bundle-macos.sh
cargo run --locked --example provider_icons_preview -- /tmp/switchx-provider-icons-preview
```

最后一条命令只生成 PPM 图像和检查软件绘制，使用合成资料，不访问账户、SQLite、Codex 配置或原生窗口。
