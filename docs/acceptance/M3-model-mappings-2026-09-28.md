# M3 模型探测与独立映射验收

2026-09-28，macOS arm64、Slint 1.18.1、本机 Codex CLI `0.158.0-alpha.2.1`。参照 `others/cc-switch` 的模型获取与映射表单，在 SwitchX 现有 Responses 路由中提供相同入口。全部请求使用本地假上游、合成凭据和临时 `CODEX_HOME`，没有调用真实供应商。

## 实现与自动检查

| 检查 | 结果 |
| --- | --- |
| 模型列表 | 根据表单地址和 Key 获取；编辑时可使用已保存凭据。无需先保存上游或填写模型 ID。支持 `data[].id` 与 `models[].slug/id`，去重排序；仅 404/405 尝试候选 `/v1/models`，认证错误不重试 |
| 请求边界 | 禁止重定向和系统代理，限制连接/读取时间及响应大小；错误提示不包含 Key 或上游响应正文。地址、Key、表单改变后清除旧列表并丢弃旧请求结果 |
| 多模型映射 | 同一 provider 可保存多个不同实际模型；公开 ID 唯一。显示名、上下文、思考等级、默认等级可独立编辑，无需 JSON；也可导入完整目录 |
| 元数据 | 手工目录采用明确的基础文本 Responses 配置，不从模型名称推断高级能力。导入目录保留指令、工具、输入类型和未知字段；相同参数的保存保留原档位说明和最大上下文 |
| 存储 | SQLite v5 升级到 v6，保留已有映射、凭据引用、备用策略与请求记录。改名事务失败时原记录保留，删除一条映射不删除其他映射或上游凭据 |
| 路由 | 发布按公开 ID 精确映射；一次获取 provider 的模型列表后逐个核对已选实际模型。备用只选相同实际模型的资料，忽略未发布的无关条目。运行或存在恢复记录时禁止编辑 |

最终源代码的 `make check` 通过：46 个库测试、1 个主程序测试、6 个路由集成测试，共 53 项；Rust/Slint 格式检查、Clippy `-D warnings` 和 macOS 调试 bundle 构建通过。没有新增依赖。

## 原生界面与 CLI

原生夹具验证了未保存表单的模型获取、已有连接的模型获取、列表选择填入模型 ID、Key 改变时清空列表，以及 HTTP 401 后保留手工模型输入。

在 Mock Alpha 上新增 `sx-ui-manual → manual-model`，填写显示名、192000 上下文、low/high 和默认 high；保存后重新打开，全部值保留。随后改名为 `sx-ui-renamed`，修改显示名与上下文为 224000，原来的三个映射仍存在。

通过原生页面预览并发布四个模型。Codex CLI 从页面生成的临时配置完成 `sx-mock-alpha → shared-model` 的文件工具两轮请求，以及 `sx-ui-renamed → manual-model` 的回答。SQLite 中三条记录均为 HTTP 200 / completed，公开 ID、provider 与实际模型一致。页面恢复并停止路由后，两次点击确认删除新映射，余下三个条目完整；退出后夹具核对原配置全文一致、journal 消失，并清理临时数据及合成凭据。

`--models` 隔离 CLI 探针曾通过三个目录条目的解析、同一 provider 的两个不同实际模型请求、四条完成记录、缺失额外模型时拒绝发布，以及预览失效/恢复冲突检查。随后增加导入参数不变时的元数据保留检查并修正编辑滚动定位；这两项均通过最终自动检查。最后构建的原生窗口再次验证列表保持当前选中模型、选择后字段同步、从列表下方打开编辑自动回到表单，以及深浅主题布局。

最终构建后的额外 `--models` 与原有 `--fallback` 复跑在凭据 helper 阶段遇到 macOS 钥匙串授权弹窗，CLI 报告 5000 ms 超时；这两次复跑没有完成模型请求，不能记为通过。Computer Use 无权操作 SecurityAgent，未改变钥匙串权限。相关夹具仍执行了配置恢复与合成凭据清理；最终界面列表复验使用表单内的合成 Key 完成。

## 复跑与边界

```sh
make check
sh scripts/bundle-macos.sh
cargo build --locked --example routed_cli_probe
cargo run --locked --example routed_cli_probe -- --models
cargo run --locked --example routed_cli_probe -- --desktop-models
```

macOS 调试程序跨可执行文件读取夹具凭据时，系统可能要求用户批准钥匙串访问；更换调试构建后需重新确认。模型列表只能证明 ID 可列出，上下文和思考档位由用户填写或目录导入，不能证明真实模型能力。保存映射后仍需重新预览发布、开启路由并重启目标 Codex。本轮未验证真实供应商、ChatGPT 订阅认证、Desktop/IDE 或其他平台。
