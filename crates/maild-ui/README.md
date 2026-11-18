# maild-ui

默认前端的 Tauri 后端（ADR-0024 / ADR-0031）。产品名、前端 id、CLI 名与阅读器协议名全部来自仓库根的
`brand.toml`（Rust 侧经 `maild_brand::*`，webview 侧经 `packages/maild-ui/src/brand.ts`），源码里不写死。webview 侧在 `packages/maild-ui`。

## 形态

| 形态 | 命令 | 说明 |
|---|---|---|
| 开发 | `pnpm --dir packages/maild-ui dev` + `cargo run -p maild-ui` | 从 Vite 开发服务器加载，HMR |
| 内嵌 | `pnpm --dir packages/maild-ui build` + `cargo run -p maild-ui --features embedded` | 产物编进二进制；交付形态 |

`cargo test --workspace` 编译的是默认形态，**不需要 Node**。

## 身份

壳以 `frontend:<bundled_frontend_id>`（当前为 `frontend:dev.featherpost.ui`）主体连接 daemon，bootstrap 之后不持有 `user` 连接（ADR-0030 决策 6）。
它必须先被 daemon 认识：

```sh
featherpostctl plugin install --frontend crates/maild-ui/package   # <cli_ctl>，见 brand.toml
```

没装时界面显示「尚未注册为前端包」并给出这条命令；不会静默降级成 `user`。HTTP 端点直接拒绝。

## 不碰真实安装地体验

```sh
cargo run -p maild-ui --example dev_stack     # 预置假数据的临时 daemon，打印 socket 路径
cargo run -p maild-ui -- --socket <打印出来的路径>
```

两个假账户指向关着的端口，所以显示离线、正文来自缓存——这是 UX-SPEC §8 的设计状态。

## 三处同步

新增一个桥命令要同时改 `build.rs`（权限生成）、`capabilities/main.json`（主窗口放行）、
`src/lib.rs` 的 `generate_handler!`——少一处构建即失败，这是故意的。
