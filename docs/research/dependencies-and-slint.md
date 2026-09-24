# SwitchX 依赖版本与 Slint 能力边界研究

研究日期：2026-09-24，Asia/Shanghai（UTC+08:00）。本文服务于 Rust + Slint 原生桌面方案，覆盖依赖选型、界面与动画的技术可行性，不代替产品需求或实现设计。

**本文确认的是查询时可见的发布记录和官方接口，最新可查不等于这些版本的组合已经编译。** 本次没有创建 Cargo 工程、安装依赖、更新工具链、构建或运行应用，也没有验证 Windows、macOS、Linux 的实际表现。

## 1. 研究方法与证据等级

- 优先已有框架能力，以单篇文档交付，只使用官方文档、官方仓库、crates.io 第一方 API，并行只读检索。
- 开始时工作区仅见 `.gitignore` 与 `others/cc-switch`；未发现对本文件生效的 `AGENTS.md`。本文不把参考项目已有依赖当作最新版本依据。
- Rust 以官方 stable channel manifest 与发布公告交叉核实。manifest 的发布日期与 rustc 提交日期分别记录，避免混淆。
- crates.io 首轮 API 抓取时间为 **2026-09-24 03:44–03:45 UTC**，随后复核关键元数据。读取 `/api/v1/crates/{crate}` 的完整 `versions` 列表，对比 `crate.max_stable_version`、`max_version`、`newest_version`。
- 筛选条件：查询时已经发布、`yanked=false`、SemVer 的 prerelease 部分为空；然后按主、次、补丁版本数值排序。`+` 后的 build metadata 不参与是否预发布的判断。本文的 stable 包括 `0.x` 非预发布版本，不表示 API 已进入 1.0 或通过生产验收。[C01]
- 表中日期取版本对象的 `created_at`，统一使用 **UTC**；不是 GitHub release 时间、爬虫时间或 `updated_at`。表中 MSRV 是发布者填写的 `rust_version`，不是本次通过编译测出的最低版本。
- `rust_version=null` 标为“未声明／未验证”，不解释为“任意 Rust 版本均支持”。当前 yank 状态也是快照，不能用今天的 API 还原任意历史时刻的 yank 状态。[C02]
- Slint 优先使用 `v1.18.1` 仓库文件和标明 `1.18.1` 的 Rustdoc；语言指南使用官方 `latest` 页面在本次查询中的内容。尝试固定版本的 `releases.slint.dev/1.18.1/docs/slint/...` 页面未能通过浏览工具读取，不能声称语言指南已全部逐页固定到该标签。

下文“已核实”指官方来源支持；“建议”指基于证据做出的 SwitchX 设计取舍；“待验证”指尚无本地构建、运行或平台验收证据。

## 2. 最新稳定版本快照

Rust stable manifest 返回 `1.98.1 (48a229cea 2026-09-01)`，manifest 日期为 `2026-09-03`；官方同日发布 Rust 1.98.1。**最新稳定工具链为 1.98.1，而不是将提交日期当作发布日期。** Beta、nightly 不进入候选。[R00a]、[R00b]

### 2.1 Registry 稳定版、发布日期与声明 MSRV

除 Rust 外，下表全部选中版本的 `yanked` 均为 `false`，且与查询返回的 `max_stable_version` 一致。`serde_json`、`tracing-subscriber`、`tokio-util` 是为 JSON、日志输出、任务取消补查的相关依赖。

| 组件 | 最新非 yanked stable | 发布时间 UTC | 声明 MSRV | 在 SwitchX 中的候选用途 | 证据 |
| --- | --- | --- | --- | --- | --- |
| Rust | **1.98.1** | 2026-09-03，官方发布日 | 不适用：这是工具链 | 首次构建的候选工具链 | [R00a]、[R00b] |
| slint | **1.18.1** | 2026-09-21 08:52:15 | 1.92 | UI、事件循环、托盘 | [R01] |
| slint-build | **1.18.1** | 2026-09-21 08:52:01 | 1.92 | 构建期编译 `.slint` | [R02] |
| tokio | **1.53.1** | 2026-07-20 17:06:09 | 1.71 | 后台异步运行时 | [R03] |
| axum | **0.8.9** | 2026-04-14 07:55:20 | 1.80 | 需要本地 HTTP 服务时采用 | [R04] |
| reqwest | **0.13.5** | 2026-09-08 20:55:26 | 1.85.0 | 上游 HTTP 客户端 | [R05] |
| rusqlite | **0.40.2** | 2026-08-08 14:21:52 | 未声明；数字 MSRV 未验证 | 确认需要事务和结构化历史后采用 | [R06]、[D01] |
| toml_edit | **0.25.15+spec-1.1.0** | 2026-09-11 01:44:08 | 1.85 | 尽量保留排版的 TOML 编辑 | [R07] |
| notify | **8.2.0** | 2025-08-03 14:54:41 | 1.77 | 外部配置文件变更监听 | [R08] |
| keyring | **4.2.0** | 2026-08-29 23:48:56 | 1.88.0 | 操作系统凭据存储 | [R09] |
| tray-icon | **0.25.1** | 2026-09-16 01:45:07 | 1.90 | Slint 内建托盘不能满足需求时备选 | [R10] |
| muda | **0.20.0** | 2026-09-11 17:51:43 | 1.90 | 需要自行集成原生菜单时备选 | [R11] |
| rfd | **0.17.2** | 2026-01-12 20:57:19 | 未声明／未验证 | 原生文件选择对话框 | [R12] |
| serde | **1.0.229** | 2026-07-18 23:05:13 | 1.56 | 数据序列化 | [R13] |
| serde_json | **1.0.151** | 2026-07-20 05:54:44 | 1.71 | JSON 配置与网络数据 | [R14] |
| tracing | **0.1.44** | 2025-12-18 14:26:12 | 1.65.0 | 结构化事件与诊断 | [R15] |
| tracing-subscriber | **0.3.23** | 2026-03-13 10:04:15 | 1.65.0 | 日志订阅、过滤与输出 | [R16] |
| tokio-util | **0.7.19** | 2026-07-21 12:10:46 | 1.71 | 按需要引入取消等工具 | [R17] |

`rusqlite 0.40.2` 的随包 README 声明支持“发布时最新稳定 Rust”，并表示旧版本可能也能编译；它没有给出固定数字。不能仅取上表最大声明值 `1.92`，就宣布整个应用的 MSRV 是 1.92。[D01]

### 2.2 明确排除与容易误判的版本

| 情况 | 本次观测 | 处理 |
| --- | --- | --- |
| notify 的 registry 最大版本不是 stable | `max_version`、`newest_version` 为 **9.0.0-rc.5**，发布于 2026-08-30 08:02:34 UTC，声明 Rust 1.88；`max_stable_version` 为 **8.2.0** | 不把 RC 写进稳定版基线。[R08] |
| toml_edit 名字中出现连字符 | `0.25.15+spec-1.1.0` 的连字符位于 `+` 后；prerelease 为空 | 它是稳定版。Cargo 版本要求不应把 build metadata 当成约束；以后锁定的完整版本以 lockfile 为准。[R07]、[C01] |
| keyring 历史撤回版 | `4.1.3`，`yanked=true` | 选 4.2.0；不把“看见过发布记录”当成可新选版本。[R09] |
| tray-icon 历史撤回版 | `0.21.1`，`yanked=true` | 选 0.25.1。[R10] |
| axum / toml_edit 历史撤回版 | 分别为 `0.8.2` / `0.23.8`，`yanked=true` | 均从稳定候选集合排除。[R04]、[R07] |
| tracing / tracing-subscriber 历史撤回版 | 分别为 `0.1.42` / `0.3.21`，`yanked=true` | 不以时间接近或版本看似稳定为理由纳入。[R15]、[R16] |

其他 API 中也有较早的 alpha、beta、RC 或撤回记录，例如 reqwest `0.13.0-rc.1`、keyring `4.0.0-rc.3`；它们不改变上表结果。除 notify 外，本次各包的 `max_version` 与所选 stable 一致；Slint 与 slint-build 的返回列表未见 prerelease 或 yanked 版本。上表的排除示例并非所有历史撤回记录的清单。[R01]、[R02]、[R05]、[R09]

Yank 通常阻止新的默认依赖解析选中该版本，并不删除发行包，也不使已经存在的 lockfile 自动失效。因此，“没有 yanked 直接依赖”与“完整依赖树可用”是不同结论。[C02]

## 3. 依赖组合建议与版本陷阱

以下为候选方案，尚未生成或解析 `Cargo.lock`。

1. **工具链候选为 Rust 1.98.1，Slint 与 slint-build 同步固定为 1.18.1。** `slint-build 1.18.1` 对内部编译器使用 `=1.18.1`，不要把 UI 运行库、构建库、内部 `i-slint-*` 分别升级成“各自最新”。[R00a]、[R01]、[R02]、[D02]
2. **首选 Slint 自带托盘与菜单能力。** `tray-icon`、`muda` 的最新版虽已核实，但不因此成为必须新增的直接依赖。Slint 1.17 已正式介绍 `SystemTrayIcon`。[S01]
3. **避免强制统一原生菜单依赖版本。** `tray-icon 0.25.1` 要求 `muda ^0.20`；Slint `v1.18.1` 的 Winit backend 在 macOS/Windows 声明的是可选 `muda 0.19.0`。这是不同的 SemVer 分支；是否共同进入依赖树取决于 features 和平台，本次未解析。存在两份依赖不自动意味着构建失败，也不能据此假定两套菜单对象可互传。[D03]、[S02]
4. **按实际功能启用后台依赖。** Tokio 支持网络任务；axum 仅在需要本地 HTTP/代理入口时加入。仅做桌面配置切换，不应为 UI 到 Rust 的调用引入本地 HTTP 层。SQLite 同样以事务、查询、历史记录需求为引入条件。这是方案取舍，不是框架限制。
5. **reqwest 0.13 的 features 不能照抄 0.12 教程。** 本版注册表中 `default-tls` 指向 `rustls`；公开特性名包括 `rustls`、`json`、`stream`、`query`。默认集合还含 `system-proxy`。以后若 SwitchX 自身承接本机代理流量，需明确上游客户端是否使用系统代理，防止形成回环；这是设计要求，尚未实测。[R05]、[D04]
6. **keyring 4 已调整接口与存储组织。** 默认 `v1` feature 提供简单跨平台凭据操作；要精确控制 credential store，官方建议使用 `keyring-core` 及具体 store，而不是启用包罗多个后端的 `cli` feature。不能沿用 keyring 3 的 feature 名称和初始化假设。本文不额外扩展调查所有 store 的版本。[R09]、[D05]
7. **rusqlite 的 `bundled` 会编译随包 SQLite。** 它能减少系统 SQLite 版本差异，但并不消除 C 编译器和目标平台构建条件；本次未验证 bundled、系统 SQLite 或任何跨平台打包组合。[D01]
8. **toml_edit 是格式保留编辑工具，不是完整配置事务系统。** 官方列有格式保留限制；文件替换、备份、失败恢复和外部并发修改仍需产品自己处理。[D06]
9. **notify 事件不是可靠事务日志。** 编辑器可能原地修改或替换文件，网络文件系统可能不发事件。建议以事件触发重新读取和校验，处理自身写入回声；必要时使用 `PollWatcher`，而不是据事件种类直接断言配置状态。[D07]
10. **rfd 不能被概括为“任何线程都能异步打开”。** macOS 真正异步依赖有效的 `NSApplication` 和窗口环境；官方建议主线程发起。Linux 的 XDG Portal 路径依赖对应 portal backend，相关对话框还涉及 Zenity。它是平台集成待验证项。[D08]

## 4. Slint 自定义 UI 与动画

### 4.1 可以建立品牌化界面，但不承诺系统控件外观

Slint 支持组合基础元素、自定义组件、属性与回调，适合实现 SwitchX 的侧栏、供应商卡片、状态标记、设置表单和提示层。官方自定义控件指南同时要求考虑鼠标、键盘、焦点和无障碍；仅画出按钮轮廓不构成完整按钮行为。[S03]

从 **1.16 起，官方将 Fluent 设为各平台默认样式**，其他内建样式逐步减少维护并计划弃用；`native` 仍可选择，不能承诺默认 UI 自动成为原生 AppKit/WinUI 控件。这里的原生桌面指 Rust 应用及其窗口系统集成，具体视觉可以统一定制。[S04]

建议使用统一的颜色、间距、圆角、文字层级，保留清晰的焦点和选中态；表单先使用成熟控件组合，再定制需要体现品牌的区域。操作系统材质效果（如窗口背景模糊）并未因支持透明色、阴影而自动获得；本次未核实统一、稳定、跨平台的相关 Slint API，故不作为基础承诺。

### 4.2 动画有声明式支持，也有类型和执行边界

官方语言参考确认 `animate`、状态和过渡，并允许设置时长、延迟、缓动、迭代等属性。可动画类型包括数值、长度、角度、颜色及 brush；字符串、布尔值、图片和任意结构体不能直接当作可插值属性。渐变、阴影等最终效果仍受 renderer 能力限制。[S05]

当前参考也列出 spring easing，并说明弹簧的收敛时间不保证等于 `duration`。因此建议用普通时长与缓动完成基本反馈，弹簧只作为实际视觉试验后的可选项；不承诺 CSS 动画、任意效果图或固定帧率可以直接迁移。[S05]

Slint 的动画需要 UI 事件循环及时运行。文件操作、同步 HTTP、数据库、密钥存储操作如果占据 UI 线程，会破坏动画流畅性；框架存在动画 API 不等于应用不会卡顿。[S06]

“跟随系统减少动态效果”尚未核实有统一稳定接口。已查 `Platform` 参考未找到足以确认该能力的说明；这不等于证明框架绝对不支持。建议先提供应用内“减少动画”设置，关闭位移、缩放和循环装饰，保留必要状态反馈。[S07]

## 5. Backend 与 renderer 的实际边界

Backend 处理窗口系统和事件循环，renderer 负责绘制；它们不是同一个选择。Winit 支持桌面的 macOS、Windows、Linux X11/Wayland。运行时 `SLINT_BACKEND` 只能选择已经编入的能力，不能动态补装 renderer。[S08]、[S09]

| Renderer | 官方确认的能力与限制 | SwitchX 的建议 |
| --- | --- | --- |
| FemtoVG | OpenGL GPU 渲染；另有 FemtoVG-WGPU 路径支持 Metal、Vulkan、Direct3D。官方提示部分文本和路径效果可能不够理想 | 作为较轻的候选，实际对比中文、小字号与图标边缘。[S08] |
| Skia | 多种 GPU API；官方指出磁盘占用较大。Winit 另列 `winit-skia-software` | 作为中文与视觉质量优先的候选；构建体积和发布成本待测。[S08]、[S09] |
| Slint software | CPU、局部重绘；官方列出旋转/缩放、阴影、圆角与裁剪组合、文字描边等限制，且页面仍列出文字脚本限制 | 不能承诺与 GPU 路径视觉等价，也不能未经中文验证直接指定为最终降级路径。[S08] |
| Vello | 基于 WGPU 的 compute 渲染；官方明确标为 experimental，要求支持 compute shader 的 GPU，无软件 fallback | 不作为本方案的稳定默认 renderer。[S08] |
| Qt renderer | 依赖 Qt backend，使用 QPainter；官方列为软件渲染 | 不为“原生”二字额外引入 Qt；若选择它需单独验证系统依赖和许可。[S08]、[S10] |

注意 **`winit-skia-software` 与 `winit-software` 不相同**，不能把 Slint software 的限制直接套到 Skia CPU 渲染，也不能把 Skia 的表现视为 Slint software 的保证。[S09]

建议首次技术验证采用 **Winit + Skia**，同时对比 FemtoVG。若显式关闭 Slint 默认 features，候选集合为 `std`、`backend-winit`、`renderer-skia`、`accessibility`、`system-tray`、`compat-1-18`；这些名称在 1.18.1 元数据中存在，但该集合尚未构建。`compat-1-2` 在该版本是会启用 `compat-1-18` 的兼容入口，不能凭旧教程遗漏当前兼容 feature。[R01]、[S10]

默认 features、backend 初始化优先级、renderer 回退顺序受编译选项影响。应在启动诊断中记录实际选择，不能仅根据机器有 GPU 就报告“正在使用硬件加速”。[S08]、[S09]

## 6. 托盘与原生菜单集成

### 6.1 首选内建 SystemTrayIcon

**Slint 1.17 已加入内建托盘，1.18.1 的 `system-tray` feature 默认启用。** 不能再以“Slint 没有托盘”为理由必选 `tray-icon + muda`。[S01]、[R01]、[S10]

当前参考定义 `SystemTrayIcon` 为独立顶层组件；它没有普通窗口对象，生命周期与实例持有、可见性和事件循环有关。窗口与托盘实例的 Slint globals 各有一份，不自动共享。建议以 Rust 应用状态更新两者，并通过菜单明确提供“打开主窗口”和“退出”。[S11]

| 平台 | 官方机制 | 产品不能忽略的边界 |
| --- | --- | --- |
| Windows | `Shell_NotifyIcon` 通知区域图标 | 显示、菜单点击与窗口恢复需实测。[S11] |
| macOS | `NSStatusItem` 菜单栏图标 | 有菜单时点击通常打开菜单，不应把三平台统一左键回调当成恢复窗口的唯一入口。[S11] |
| Linux / BSD | D-Bus `StatusNotifierItem` / AppIndicator | 需要桌面环境或扩展承载；普通 X11 system tray 不支持，GNOME 等环境可能需扩展。[S11] |

托盘菜单里的 `MenuItem.shortcut` 不提供全局快捷键。关闭窗口后的驻留、完全退出、托盘不可用时的入口都需按平台验收；不能仅测试“图标出现”。官方当前托盘文档关于必需 Menu 与无 Menu 点击情形的描述也有不一致，方案不依赖无菜单分支。[S11]

### 6.2 何时才引入 tray-icon / muda

仅在内建接口的具体缺口已经复现时考虑。`tray-icon` 官方说明：macOS 需要主线程事件循环并在主线程创建图标；Windows 和 Linux AppIndicator 路径要求对应线程的事件循环；KSNI 后端维护自己的 worker。Linux 默认 AppIndicator 路径涉及 GTK 3、libxdo、AppIndicator 库，`ksni` 则有不同依赖条件。[D09]

`muda` 提供原生菜单，Linux GTK 3/4 后端有相应系统依赖。两库的 Winit 示例要求将事件送回现有事件循环以唤醒 UI；这不是“另外启动一个 Winit 循环即可无缝接入 Slint”的证明。不要为了托盘再建一套独立 GUI 主循环。[D09]、[D10]

如确需访问 Slint 的底层 Winit 对象，`unstable-winit-030` 是明确带 unstable 标记的接口；不能把它描述为普通稳定 API。是否需要该入口以及如何接事件，本次没有实现验证。[R01]、[S10]

## 7. 后台线程与 UI 数据模型

### 7.1 主线程负责 UI，后台运行 Tokio

Slint 官方要求 UI 线程尽量少做工作，并提供 `invoke_from_event_loop`。组件 `Weak` 可以跨线程传递，但普通 `upgrade()` 只有在组件创建线程才能成功；`upgrade_in_event_loop` 会投递回 UI 事件循环，组件已释放时回调不会执行。[S06]、[S12]

`slint::spawn_local` 在 Slint 的事件循环执行 future，**不等于提供 Tokio reactor**。官方专门列出 Tokio 的运行时上下文、驱动和公平性约束；在 Slint 主线程使用 Tokio current-thread scheduler 不成立，也不推荐直接套 `#[tokio::main]`。[S13]

建议的最小线程关系如下，图中为设计建议，未实现：

```mermaid
flowchart LR
    UI[主线程：Slint 窗口、托盘、UI 模型]
    Runtime[后台：Tokio 多线程运行时]
    Storage[独立线程：持续数据库工作]
    UI -->|提交命令| Runtime
    Runtime -->|请求与结果| Storage
    Runtime -->|普通数据，经 UI 事件循环投递| UI
```

UI 回调通过持有的 Tokio runtime handle 提交异步任务，runtime 的所有者维持其生命周期，并显式启用 I/O 与 timer。持续数据库工作可以由独立线程持有连接；短时阻塞工作才使用 `spawn_blocking`。已启动的 `spawn_blocking` 不能靠 abort 强制取消，关闭超时也不等于工作已经停止。[D11]、[D12]

建议给状态结果附操作标识，合并高频进度，避免旧请求覆盖新状态；退出时先停止接收命令，再结束网络和存储工作。这些是应用应实现的行为，不是 Slint 自动保证的事务或取消语义。

### 7.2 VecModel / ModelRc 属于 UI 模型层

`VecModel` 明确为 `!Send`、`!Sync`；`ModelRc` 包装基于 `Rc` 的模型。不能用 `Arc<Mutex<VecModel<_>>>` 就宣称可以从后台安全更新界面。后台传递普通数据，在 UI 线程创建或修改模型。[S14]、[S15]

可变自定义 `Model` 需要变更通知；官方建议使用 `ModelNotify`，逐行更新通常比不断替换整个模型更有效。双向编辑要求实际实现 `set_row_data`；该 trait 的默认实现并不会替应用持久化修改。[S15]

`ListView` 会按可见性实例化条目，适合供应商或记录列表；它不替应用解决后台全量载入、排序成本和无限日志增长。因此仍应限制诊断列表容量，并验证真实数据量下的滚动和筛选。[S16]

## 8. 中文字体、输入与无障碍

### 8.1 中文字体不是仅设一个 font-family 就完成

Slint 可使用系统字体，也可在 `.slint` 编译时导入 `.ttf`、`.ttc`、`.otf`，并通过 `font-family` / `default-font-family` 选择。官方推荐把自定义字体在编译时纳入。[S17]

建议首轮先验证系统字体；若目标机器字形覆盖或布局不一致，再随应用分发具有明确再分发许可的中文字体。字体文件支持不等于缺失字形一定能 fallback，也不证明 Emoji、混合脚本、粗体、中文输入法候选窗都已通过验收。

当前动态字体集合入口 `fontique_011::shared_collection` 需要 **`unstable-fontique-011`**。不要把它宣传为稳定字体 API，也不要用这一不稳定接口证明所有 renderer 的 fallback 都一致。[S18]

建议最少验证简繁中文、英文和数字混排、长路径省略、中文搜索、输入法组合文本与光标、不同缩放比例及字体缺失。软件 renderer 的官方文字脚本限制须与实际所选 renderer 分开验证；本文没有据此断言“Slint 完全不支持中文”。[S08]、[S17]

### 8.2 无障碍需要控件语义与真机检查

`accessibility` 默认启用，官方措辞是尝试向操作系统暴露 UI 树；`v1.18.1` 的 Winit backend 明确连接 AccessKit 与 `accesskit_winit`。这证明有集成，不证明任意自绘组件自动满足无障碍标准。[S10]、[S02]

自定义交互组件应声明 `accessible-role`，提供标签、当前值或状态及相应 action；只设置颜色和 `TouchArea` 不足以表达按钮语义。角色是使用其他 accessibility 属性与回调的前提。[S19]

建议实际检查键盘导航、焦点可见性、屏幕阅读器、错误提示和状态反馈。官方推荐 Windows Accessibility Insights 与 macOS Accessibility Inspector，且说明后者需要应用以 bundle 形式构建。VoiceOver、Narrator、Linux 屏幕阅读器的端到端兼容性本次均未验证。[S20]

## 9. License 的可用范围与条件

Slint `v1.18.1` 的运行库与语言工具不是整体 MIT/Apache：官方列出 **GPLv3、Royalty-free、商业许可** 三条路径。文档、示例和第三方素材还有各自许可；不能把“文档和示例可按 MIT 使用”推导为整个框架同样许可。[L01]

| 路径 | 对 SwitchX 的实际意义 |
| --- | --- |
| Royalty-free 2.0 | 适用于定义中的桌面、移动和 Web 应用，也可用于闭源桌面应用；必须履行 attribution 条件。不是“免费且无条件”。[L02] |
| GPLv3 | 可用于符合 GPL 要求的分发；不能仅给自己的源文件加 MIT 声明，就认定组合二进制无需考虑 GPL 条款。[L01] |
| 商业许可 | 适用于需要相应商业权利或免除 Royalty-free attribution 的情况；具体权利以合同为准，本文未调查价格与采购条款。[L01] |

Royalty-free 2.0 第 2 节提供两种 attribution 路径：在可从顶层菜单进入的 About 界面展示 `AboutSlint`（没有该界面时按条款使用启动画面），或在容易找到的公开网页展示 Slint badge。**建议 SwitchX 预留 AboutSlint 入口，并随发行材料保留所选许可及相关声明。**[L02]

其第 3 节还限制单独分发 Slint、嵌入式用途及暴露 Slint API 的应用。当前普通桌面工具方案可以按该范围评估；以后若做通用 UI SDK 或运行时 API 转售，不能沿用同一结论。字体、图标及其他依赖的许可仍须分别核对；本次没有完成全量第三方许可清单。[L02]

## 10. 美观动画的可实现设计建议

以下是 SwitchX 的设计建议与待测参数，不是 Slint 官方性能承诺，也不是已经完成的视觉稿。

| 场景 | 建议效果 | 技术落点与边界 |
| --- | --- | --- |
| 悬停、按下、选中 | 100–160 ms 的颜色、边框或透明度反馈 | 用属性动画；键盘焦点独立可见。[S05]、[S03] |
| 页面切换 | 160–220 ms 的淡入与短距离位移 | 控制动画元素数量，不在切页时同步读配置。[S05]、[S06] |
| 配置切换成功 | 活跃状态标记与简短文字提示 | 成功必须由后台操作结果确认，不能以动画结束代替成功。 |
| 请求进行中 | 明确 busy 状态，必要时局部进度提示 | 合并进度更新；不承诺按每个网络片段刷新 UI。[S12]、[S15] |
| 长列表 | 简洁行布局，稳定高度与清晰筛选反馈 | 使用 ListView；复杂阴影和每行循环动画需性能测试。[S16]、[S08] |
| 减少动态效果 | 保留文字、颜色和状态变化，取消位移及装饰循环 | 先提供应用设置，系统自动同步能力仍待核实。[S07] |

首轮可用性目标是“操作明确、中文清楚、动画不阻塞配置操作”。60/120 Hz 的流畅度、安装包体积、启动耗时、内存与电池影响都应测量，不写成已经获得的 Slint 保证。

## 11. 后续验证门槛（本次全部未执行）

1. 以 Rust 1.98.1 和明确 features 生成实际依赖树、lockfile；验证 Slint/slint-build 同版本，检查重复的原生库与 MSRV 缺口。
2. 在 macOS、Windows、Linux 的目标架构分别构建；Linux 同时检查 X11/Wayland、D-Bus、portal 和托盘宿主条件。单平台成功不代表三平台通过。
3. 用同一小界面对比 Skia、FemtoVG、Skia software：中文、小字号、圆角裁剪、阴影、缩放、输入法、动画和启动失败路径。
4. 验证隐藏窗口后托盘驻留、从菜单恢复、完整退出、托盘不可用时仍有可操作入口；分别测试窗口和托盘状态同步。
5. 在持续网络、数据库、文件监听负载下测 UI 响应；验证取消、过期结果、退出顺序及阻塞任务超时。
6. 验证凭据存储锁定、缺失服务、授权失败；确认错误能够返回 UI，秘密不会进入日志或普通配置备份。这是应用行为要求，不是仅引入 keyring 就达成。
7. 验证配置保留、外部编辑冲突、原子替换、损坏配置和恢复；不能把 toml_edit + notify 视为已经完成可靠切换。
8. 用实际应用 bundle 检查键盘、屏幕阅读器、中文字体再分发条件和 AboutSlint attribution。

达到以上门槛后，才能把“资料支持的候选组合”升级为“已经构建并验证的 SwitchX 基线”。

## 12. 官方证据索引

所有下列来源均于 2026-09-24 查询。Registry 链接是可重复查询的端点，并非不可变快照；第 2 节保存本次观测值。固定版本 Rustdoc 属于 crate 随包官方文档。`latest` 和仓库默认分支以后可能变化，复核时应重新对照发行标签。

### 版本与 Cargo 语义

- Rust stable manifest [R00a]；Rust 1.98.1 发布公告 [R00b]。
- UI 与异步网络：slint [R01]、slint-build [R02]、tokio [R03]、axum [R04]、reqwest [R05]。
- 存储与系统集成：rusqlite [R06]、toml_edit [R07]、notify [R08]、keyring [R09]、tray-icon [R10]、muda [R11]、rfd [R12]。
- 数据与诊断：serde [R13]、serde_json [R14]、tracing [R15]、tracing-subscriber [R16]、tokio-util [R17]。
- Cargo 版本要求 [C01]；yank 语义 [C02]。

[R00a]: https://static.rust-lang.org/dist/channel-rust-stable.toml
[R00b]: https://blog.rust-lang.org/2026/09/03/Rust-1.98.1/
[R01]: https://crates.io/api/v1/crates/slint
[R02]: https://crates.io/api/v1/crates/slint-build
[R03]: https://crates.io/api/v1/crates/tokio
[R04]: https://crates.io/api/v1/crates/axum
[R05]: https://crates.io/api/v1/crates/reqwest
[R06]: https://crates.io/api/v1/crates/rusqlite
[R07]: https://crates.io/api/v1/crates/toml_edit
[R08]: https://crates.io/api/v1/crates/notify
[R09]: https://crates.io/api/v1/crates/keyring
[R10]: https://crates.io/api/v1/crates/tray-icon
[R11]: https://crates.io/api/v1/crates/muda
[R12]: https://crates.io/api/v1/crates/rfd
[R13]: https://crates.io/api/v1/crates/serde
[R14]: https://crates.io/api/v1/crates/serde_json
[R15]: https://crates.io/api/v1/crates/tracing
[R16]: https://crates.io/api/v1/crates/tracing-subscriber
[R17]: https://crates.io/api/v1/crates/tokio-util
[C01]: https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html
[C02]: https://doc.rust-lang.org/cargo/commands/cargo-yank.html

### Slint 界面、运行时与平台能力

- 托盘发布说明 [S01]；Winit backend 固定标签源码 [S02]；自定义控件 [S03]；默认样式调整 [S04]。
- 动画语言 [S05]；Rust API 总览 [S06]；Platform 接口 [S07]；renderer 概览 [S08]；Winit 平台说明 [S09]；固定版本 features [S10]。
- 托盘接口 [S11]；Weak 跨线程投递 [S12]；spawn_local 与 Tokio 约束 [S13]；VecModel [S14]；Model [S15]；ListView [S16]。
- 字体指南 [S17]；不稳定字体集合入口 [S18]；无障碍属性 [S19]；无障碍检查建议 [S20]。

[S01]: https://slint.dev/blog/slint-1.17-released
[S02]: https://raw.githubusercontent.com/slint-ui/slint/v1.18.1/internal/backends/winit/Cargo.toml
[S03]: https://docs.slint.dev/latest/docs/slint/guide/development/custom-controls/
[S04]: https://slint.dev/blog/default-native-style-change
[S05]: https://docs.slint.dev/latest/docs/slint/reference/language/animations/
[S06]: https://docs.slint.dev/latest/docs/rust/slint/
[S07]: https://docs.slint.dev/latest/docs/slint/reference/platform/
[S08]: https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backends_and_renderers/
[S09]: https://docs.slint.dev/latest/docs/slint/guide/backends-and-renderers/backend_winit/
[S10]: https://docs.rs/slint/1.18.1/slint/docs/cargo_features/
[S11]: https://docs.slint.dev/latest/docs/slint/reference/window/systemtrayicon/
[S12]: https://docs.slint.dev/latest/docs/rust/slint/struct.Weak
[S13]: https://docs.slint.dev/latest/docs/rust/slint/fn.spawn_local
[S14]: https://docs.slint.dev/latest/docs/rust/slint/struct.VecModel
[S15]: https://docs.slint.dev/latest/docs/rust/slint/trait.Model
[S16]: https://docs.slint.dev/latest/docs/slint/reference/std-widgets/views/listview/
[S17]: https://docs.slint.dev/latest/docs/slint/guide/development/fonts/
[S18]: https://docs.slint.dev/latest/docs/rust/slint/fontique_011/fn.shared_collection
[S19]: https://docs.slint.dev/latest/docs/slint/reference/common/
[S20]: https://docs.slint.dev/latest/docs/slint/guide/development/best-practices/

### 配套库与许可证

- rusqlite 构建与 MSRV 政策 [D01]；slint-build 内部编译器约束 [D02]；tray-icon 的 muda 约束 [D03]。
- reqwest features [D04]；keyring 4 接口 [D05]；toml_edit 限制 [D06]；notify 已知问题 [D07]；rfd 平台条件 [D08]。
- tray-icon 平台说明 [D09]；muda 菜单集成 [D10]；Tokio runtime [D11]；阻塞任务取消边界 [D12]。
- Slint 1.18.1 许可说明 [L01]；Royalty-free 2.0 原文 [L02]。

[D01]: https://docs.rs/crate/rusqlite/0.40.2
[D02]: https://crates.io/api/v1/crates/slint-build/1.18.1/dependencies
[D03]: https://crates.io/api/v1/crates/tray-icon/0.25.1/dependencies
[D04]: https://docs.rs/reqwest/0.13.5/reqwest/
[D05]: https://docs.rs/keyring/4.2.0/keyring/
[D06]: https://docs.rs/toml_edit/0.25.15+spec-1.1.0/toml_edit/
[D07]: https://docs.rs/notify/8.2.0/notify/
[D08]: https://docs.rs/rfd/0.17.2/rfd/
[D09]: https://github.com/tauri-apps/tray-icon
[D10]: https://docs.rs/muda/0.20.0/muda/
[D11]: https://docs.rs/tokio/1.53.1/tokio/runtime/
[D12]: https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html
[L01]: https://raw.githubusercontent.com/slint-ui/slint/v1.18.1/LICENSE.md
[L02]: https://raw.githubusercontent.com/slint-ui/slint/v1.18.1/LICENSES/LicenseRef-Slint-Royalty-free-2.0.md

补充交叉证据：

- Slint 1.18.1 官方 release：https://github.com/slint-ui/slint/releases/tag/v1.18.1
- Slint 1.18 官方发布说明（2026-09-16）：https://slint.dev/blog/slint-1.18-released
- Slint 1.18.1 Cargo features 源文件：https://github.com/slint-ui/slint/blob/v1.18.1/api/rs/slint/Cargo.toml
- Royalty-free 2.0 官方 PDF：https://slint.dev/agreements/slint-royalty-free-license.pdf
