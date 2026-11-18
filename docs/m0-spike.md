# M0 Spike — 结果记录

> **历史记录**。文中的 `vendor/cordis/` 与 `packages/spike/` 已随
> [ADR-0047](decisions/0047-decoupled-brand-and-open-source-readiness.md) 从仓库移除；对应上游源码见
> [cordiverse/cordis](https://github.com/cordiverse/cordis)（rc.8 `8cc9e33`、rc.13 `bc9fae7`），Rust 内核的规格对照见
> [rust-port-design.md](rust-port-design.md)。

> 日期：2026-08-18。目标：验证 vendored cordis 能在 Bun 上运行（兼容门禁），打通 vendor + rescope 流程。

## 结论

✅ **通过**。Bun 1.3.14 与 Node 24.14.1 下，vendored cordis（`4.0.0-rc.8`，上游 SHA `8cc9e33`）四项核心机制全部验证通过。

## 验证项

| 机制 | 断言 | 结果 |
|---|---|---|
| 服务挂载（Service 子类） | `ctx.greeter instanceof Greeter` | ✅ |
| 注入解析（`inject: ['greeter']`） | echo 插件在 greeter 就绪后启动 | ✅ |
| 类型化事件（`serial` 分发） | `await ctx.serial('greet', 'maild')` → `hello, maild` | ✅ |
| 可逆副作用（`ctx.effect` 返回 disposer） | `fiber.dispose()` 后清理标志置位 | ✅ |

## 运行时结论（重要，修正了先前的判断）

- **Bun 门禁通过**：esbuild 打包产物在 Bun 1.3.14 下通过。
- **Bun 还能直接跑 cordis TS 源码**：实测 Bun 1.3.14 正确剥离了 `import { Dict } from 'cosmokit'` 这类**类型-only 命名导入**（运行时 cosmokit 并不导出 `Dict`/`Awaitable`/`Promisify`，但 direct-TS 全链路 `hi maild / cleaned: true` 正常）。
- 因此此前「Bun 直跑 TS 会因类型导入崩溃」的担心在 1.3.14 上不成立。**运行时维持 Bun，无需降级 Node 24**。

## 锁定决策

- **vendor**：`vendor/cordis/` 存放 core 源码，rescope 为 `@maild/cordis`；上游 SHA 与同步说明见 `vendor/cordis/README.md`。
- **构建路径**：esbuild bundle（`--alias:@maild/cordis=./vendor/cordis/src/index.ts`），`cosmokit` 内联、`@standard-schema/spec` 因纯类型被 tree-shake。产物 `packages/spike/dist/main.js` 单文件 51.6kb。
- **运行**：`pnpm test:node` / `pnpm test:bun`。

## 环境与工具链备注

- Bun 未预装：经 npm 镜像装 `bun@1.3.14`（平台二进制在 `@oven/bun-darwin-aarch64`），沙箱内可用。
- npm 缓存 EPERM（`~/.npm` 有 root 属主文件）：全程走 pnpm，store 落在工作区 `.pnpm-store/`。
- pnpm 11 需 `allowBuilds` 批准 esbuild/bun 的 postinstall（已写入 `pnpm-workspace.yaml`）。

## 留到下一里程碑

- `@maild/cordis` 尚未产出正式 `lib/`（当前 `exports` 指向 `src/*.ts`）；M1 前补 esbuild→lib + tsc 声明。
- monorepo 布局（`packages/*` 作为 workspace package）在 M1 搭建。

## 上游 `v4.0.0-rc.13` 同步记录（2026-11-13）

- **版本升级**：`vendor/cordis/` 已从 `4.0.0-rc.8`（`8cc9e33`）同步至上游最新 `v4.0.0-rc.13`（SHA `bc9fae7a92de7f74417e54eaf3decdecf71f937a`，见 `vendor/cordis/README.md`）。
- **新增规格断言**（`packages/spike/src/main.ts`）：
  - 分发期移除后续监听器即时生效（`#242`）；
  - 提供者在已提交消费者完成异步清理后才执行自身逆（`#196`）；
  - 组件 `apply` 失败进入 `FiberState.FAILED`（不因依赖刷新自动重入），经 `ctx.registry.replace()` 原地恢复为 `ACTIVE`，显式 `dispose()` 后收敛至 `DISPOSED`（`#98` / `#133` / `#266`）。
- **验证结果**：esbuild 单文件产物 `packages/spike/dist/main.js`（59.3kb）在 Node 24 与 Bun 1.3.14 下 `pnpm test:node` / `pnpm test:bun` 全部通过。
