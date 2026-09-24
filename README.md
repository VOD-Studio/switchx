# switchx

Rust + Slint 原生桌面应用，按 `docs/SWITCHX-PLAN.md` 的 M0 闸门逐步实现。

当前仅有应用骨架和**合成目录夹具**。界面不连接 Codex；`catalog_probe` 不读取账号、凭据或现有 Codex 配置，也不会写入 `CODEX_HOME`。夹具的能力字段只用于验证目录 schema，不能作为真实模型能力或发布模板。

```sh
cargo run
cargo test
cargo run --example catalog_probe > /tmp/switchx-models.json
CODEX_HOME="$(mktemp -d)" npx -y @openai/codex@0.156.1 \
  -c 'model_catalog_json="/tmp/switchx-models.json"' debug models
```

在 2026-09-24 的 macOS 本机测试中，Codex CLI 0.156.1 的 `debug models` 与 app-server `model/list` 均返回 `sx-ds-flash`、`sx-oai-coding`。这证明该版本能读取夹具目录，尚未证明真实 `/model` 交互、真实模型请求、Desktop 或 IDE 兼容。
