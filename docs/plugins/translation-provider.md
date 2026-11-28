# 第三方热插拔翻译插件开发者指南（`translation_provider`）

> 架构决策依据：[ADR-0050](../decisions/0050-email-translation-and-provider-plugins.md) · 通用包与安全边界：[外部插件规范](README.md) · 最小可运行参考实现：[`plugins/example-translator/`](../../plugins/example-translator/)

Featherpost Studio 支持通过 **热插拔外部进程插件** 接入任意第三方翻译引擎（如 DeepL、Google Translate、Azure Translator、本地大模型服务或企业私有网关）。插件可在运行时通过 CLI 或「设置 › 扩展」安装、启用、停用、升级、回滚与卸载，**无需重新编译 Featherpost**。

本文档提供从**空目录**开始构建、打包、安装与调试一个合规 `translation_provider` 插件的完整指南（附 Rust 与 Python 双语言可运行骨架）。

---

## 1. 架构与安全边界总览

```text
Featherpost Studio (maild-ui)
  ├─ 内置 OpenAI 兼容 Provider（直走 maild-proxy）
  └─ ExternalTranslationProviderAdapter
        │  同 UID 流式 IPC（Unix Socket / Windows Named Pipe）
        ▼
featherpostd / featherpost-agent (Cordis TranslationProviderRegistry)
        │  反向 JSON-RPC 2.0（读写分离、并发受限、可取消）
        ▼
你的第三方插件进程 (org.example.translator)
```

关键架构约束：

1. **进程隔离，而非沙箱**：插件以独立进程运行，通过本机同 UID 流式 IPC（Unix Domain Socket / Windows Named Pipe）连接守护进程。插件与守护进程处于同一操作系统用户（同 UID）信任域内；用户安装前应像审查本地原生程序一样审查插件来源。
2. **禁止静默回退**：一旦用户在「设置 › 翻译」选中你的插件，若插件离线、停用、未注册或升级换代，宿主会明确向用户报错，**绝不**将邮件悄悄转交给内置或其他翻译服务。
3. **保留内置命名空间**：`builtin` 与 `builtin.*`（大小写不敏感，如 `builtin.openai_compatible`）保留给内建服务，外部 `plugin.toml` 与安装事务均 fail-closed 拒绝使用。
4. **严格真实环境变量名**：所有宿主注入环境变量均由 [`crates/maild-brand/src/generated.rs`](../../crates/maild-brand/src/generated.rs) 单源生成，实际名称为：
   - `FEATHERPOST_PLUGIN_TOKEN`
   - `FEATHERPOST_PLUGIN_DATA_DIR`
   - `FEATHERPOST_CONFIG`
   > **注意**：请勿使用早期历史代号 `MAILD_*`，否则插件在生产环境中将找不到认证凭据与配置路径。

---

## 2. 从空目录创建插件包

### 2.1 目录结构

受管插件安装在 `featherpostd paths` 输出的 `plugins/<id>/` 目录下。从空目录开始准备待安装源码/打包目录：

```sh
mkdir -p my-translator/bin
cd my-translator
```

典型包布局如下（目录名在安装后必须与 `plugin.toml` 的 `id` 完全一致）：

```text
my-translator/
├── plugin.toml              # 必填：schema_version = 2，最大 64 KiB
├── bin/
│   ├── my-translator        # lifecycle = "supervised" 时必填（可执行文件）
│   └── my-translator-ui     # 可选：ui_executable，供「打开插件设置」启动
└── README.md                # 可选说明
```

打包红线（安装事务会 fail-closed 校验）：

- 包内不得包含符号链接、设备文件或 FIFO；
- `executable` 与 `ui_executable` 必须是包内相对路径，不得包含 `..` 或指向包外；
- 目录与文件在 Unix 上不可对 group / other 可写；
- 包目录本身是可升级/回滚/卸载的**不可变交付物**，运行时配置、缓存或状态文件**必须**写入 `FEATHERPOST_PLUGIN_DATA_DIR`。

### 2.2 编写 `plugin.toml`

```toml
schema_version = 2
protocol_version = 1
id = "org.example.my-translator"
name = "My Translator"
description = "Hot-pluggable third-party email translation provider"
version = "0.1.0"
lifecycle = "supervised"            # "supervised" 或 "independent"
capabilities = ["translation_provider"]
executable = "bin/my-translator"    # 仅 supervised 必填；independent 禁止声明
# ui_executable = "bin/my-translator-ui"  # 可选：独立原生设置窗口

[l10n.en]
name = "My Translator"
description = "Hot-pluggable third-party email translation provider"

[l10n."zh-Hans"]
name = "我的翻译插件"
description = "支持热插拔的第三方邮件翻译服务"
```

关键字段说明：

| 字段 | 规则 |
|---|---|
| `schema_version` | 使用 `2` 以支持 `description` 与 `[l10n.<tag>]`（`1` 仍受支持但拒绝这两个键）。 |
| `protocol_version` | 固定为 `1`。 |
| `id` | 反向域名格式（如 `org.example.my-translator`）；严禁 `builtin` / `builtin.*`。 |
| `version` | 三段语义化版本 `MAJOR.MINOR.PATCH`；升级时新版本必须**严格高于**已安装版本。 |
| `capabilities` | 必须包含 `"translation_provider"`；若同时兼任邮箱收发扩展可同时声明 `"transport_provider"`。 |
| `lifecycle` | `"supervised"`（由守护进程按启停托管启动）或 `"independent"`（由外部进程/服务管理器自行运行）。 |
| `executable` | 仅 `supervised` 必填；`independent` 声明该字段将被**整包拒绝**。 |
| `ui_executable` | 可选。在「设置 › 翻译」或「设置 › 扩展」点击“打开插件设置 / 管理”时，由守护进程以单实例受管方式启动。**仅注入** `FEATHERPOST_CONFIG` 与 `FEATHERPOST_PLUGIN_DATA_DIR`，**绝不注入** `FEATHERPOST_PLUGIN_TOKEN` 或邮件内容。 |
| `[l10n.<tag>]` | 可选，最多 16 种 BCP 47 语言；`name` ≤ 160 字节、`description` ≤ 1024 字节，展示名称与包版本始终以 `plugin.toml` 为准。 |

### 2.3 `supervised` vs `independent` 生命周期对比

| 维度 | `supervised` | `independent` |
|---|---|---|
| 进程管理 | 启用后由守护进程自动拉起，停用/升级/卸载/停机时经进程组（Unix）或 Job Object（Windows）整体清扫。**必须保持前台运行，禁止 daemonize / double-fork。** | 由作者或用户自行启动（例如常驻 systemd / launchd 服务或独立 GUI 应用）。 |
| 认证凭据 | 每次 spawn 自动通过环境变量 `FEATHERPOST_PLUGIN_TOKEN` 注入全新轮换令牌。 | 用户执行 `featherpostctl plugin-token <id>` 一次性获取（每次获取立即轮换并吊销旧令牌），通过环境变量传入你的进程。 |
| 停用撤销 | 停止进程并立即撤销已注册 `generation`。 | 即使你的进程未退出，守护进程在 `disable` / `remove` / `upgrade` / `rollback` 时也会**立即撤销**其 `generation`，所有在途与后续调用立刻失败 `-32018`。 |
| 退出策略 | `exit 0` 视为主动停止不重启；非零退出按 2s → 60s 指数退避，连续最多 8 次（存活 60s 重置计数）。 | 由外部管理器决定；重连后重新 `authenticate` + `translationProviderRegister` 即获得新 `generation`。 |

---

## 3. 环境变量与 IPC 发现

`supervised` 插件启动时在一个**干净的最小环境**中运行（工作目录固定为包根目录，stdio 关闭），宿主仅注入以下三个环境变量：

- `FEATHERPOST_PLUGIN_TOKEN`：本次连接用于 `authenticate` 的一次性会话凭证（请使用内存清零类型如 `Zeroizing<String>`，绝不写入日志、argv 或明文磁盘文件）。
- `FEATHERPOST_PLUGIN_DATA_DIR`：为该插件 ID 独立分配的 `0700` 可变数据目录（用于保存非秘密配置或本地缓存；API Key 等秘密应优先存入系统钥匙串 / 平台凭据库）。
- `FEATHERPOST_CONFIG`：当前活动 `config.toml` 的绝对路径，用于发现对应的同 UID 流式 IPC Socket / Named Pipe 路径。

> **重要**：`translation_provider` 必须使用**同 UID 本地流式 IPC**（Unix socket 或 Windows named pipe）。HTTP 与 stdio 端点不接受 `authenticate` 或 `translationProviderRegister`（返回 `-32015`）。

---

## 4. 完整 JSON-RPC 2.0 线协议契约

流式 IPC 使用 **NDJSON（每条 JSON-RPC 2.0 消息占一行，以 `\n` 结尾）**。

### 4.1 帧大小、批次、并发与超时硬上限

| 约束项 | 硬上限 / 默认值 | 说明 |
|---|---|---|
| 双向单帧上限 | **1 MiB（含末尾 `\n`）** | 超过即拒绝/断连；必须按整帧序列化字节数而非纯字符数评估。 |
| 批次项数上限 | `min(64, 插件声明值)`（默认 `16`，插件声明 `1..=256`） | 宿主与插件协商取较小值。 |
| 批次字符上限 | `min(64,000, 插件声明值)`（默认 `12,000`，插件声明 `1..=256,000`） | 宿主与插件协商取较小值。 |
| 单插件并发槽 | `max_concurrency.clamp(1, 4)` | 每端点通过 `Semaphore` 限流；超过 4 会被钳位到 4。 |
| 排队与全局预算 | 每端点排队 `4` / 每会话在途 `8` / 全局在途 `16` / 全局在途载荷 `4 MiB` | 满载返回 `-32022`（`PROVIDER_BUSY`）。 |
| 单次翻译总超时 | 默认 `45s`（范围 `50ms..=60s`） | 含排队与等待应答；超时返回 `-32019`，迟到应答被静默丢弃。 |
| 探测超时 | `15s`（上限 `15s`） | `translation/probe` 使用固定无邮件测试样本。 |

### 4.2 第一步：`authenticate`（连接首帧）

建立流式连接后，**第一条请求必须是 `authenticate`**。认证成功后连接主体锁定为 `plugin:<id>`；失败返回 `-32014` 并立即关闭连接。

```json
{"jsonrpc":"2.0","id":1,"method":"authenticate","params":{"token":"<FEATHERPOST_PLUGIN_TOKEN>"}}
```

成功应答：

```json
{"jsonrpc":"2.0","id":1,"result":{"principal":"plugin:org.example.my-translator"}}
```

### 4.3 第二步：`translationProviderRegister`

认证成功后注册翻译能力元数据（展示名、描述、多语言 `l10n` 与包版本由宿主从 `plugin.toml` 读取，不在注册参数中重复声明）：

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "translationProviderRegister",
  "params": {
    "supported_languages": ["*"],
    "max_batch_items": 16,
    "max_batch_chars": 12000,
    "max_concurrency": 2,
    "supports_cancel": true,
    "variant_revision": "deepl-pro-v2"
  }
}
```

字段约束：

- `supported_languages`：BCP 47 标签数组或 `["*"]`（最多 128 项，每项 ≤ 35 字节，不允许重复）。
- `variant_revision`：不含控制字符、≤ 128 字节的非秘密修订标识（如引擎/模型版本）。当你的插件切换底层模型或词表时，**务必更新 `variant_revision`**，宿主会将其纳入 SQLite 缓存键，防止旧模型缓存污染新模型输出；同一封邮件的多批次会冻结同一 `(generation, variant_revision)`，避免跨代拼接。

成功应答：

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "result": {
    "provider_id": "org.example.my-translator",
    "generation": 1,
    "max_batch_items": 16,
    "max_batch_chars": 12000,
    "max_concurrency": 2
  }
}
```

> **双角色插件说明**：若插件同时声明了 `transport_provider` 与 `translation_provider`，可在同一条已认证连接上分别调用 `providerRegister` 与 `translationProviderRegister`。两条角色各自维护独立的属主与 `generation`，互不干扰。

### 4.4 反向调用 1：`translation/translate`（daemon → 插件）

宿主向插件发起翻译请求（`id` 可能为数字或字符串，**必须原样回显 `id` 的值与类型**）：

```json
{
  "jsonrpc": "2.0",
  "id": 101,
  "method": "translation/translate",
  "params": {
    "job_id": "tr-7-a1b2c3d4",
    "batch_id": "tr-7-a1b2c3d4:b0",
    "source_lang": "en",
    "target_lang": "zh-CN",
    "content_kind": "html",
    "items": [
      { "id": 0, "text": "Quarterly Update" },
      { "id": 1, "text": "Please review [[1]]the financial report[[/1]] before Friday." }
    ]
  }
}
```

成功应答：

```json
{
  "jsonrpc": "2.0",
  "id": 101,
  "result": {
    "items": [
      { "id": 0, "text": "季度更新" },
      { "id": 1, "text": "请在周五前审阅[[1]]财务报告[[/1]]。" }
    ],
    "detected_source_lang": "en",
    "variant_revision": "deepl-pro-v2"
  }
}
```

严格响应校验规则（任一条违反即被宿主判定为 `invalid_response` / `-32020`）：

1. 返回 `items` 的 `id` 集合必须与请求 `items` **一一对应**（无缺失、无重复、无未知 ID）；
2. 响应字节数不得异常膨胀（受宿主 `DEFAULT_MAX_EXPANSION_RATIO` 保护），整帧含 `\n` 不超过 `1 MiB`；
3. 若返回了 `variant_revision`，必须与当前冻结的注册 `variant_revision` 一致；
4. 业务失败请返回 JSON-RPC `error` 对象（如 `code: -32020`，`message` 为简短脱敏说明；宿主会剥离控制字符并截断至 256 字节，请勿在错误消息中回显邮件原文或 API Key）。

### 4.5 `[[1]]...[[/1]]` 链接占位符保留规则

当 `content_kind = "html"` 时，宿主在 Rust 侧将正文中的安全行内链接 `<a href="...">文本</a>` 替换为**局部编号占位符** `[[k]]...[[/k]]`（真实 `/link/<token>/<n>` URL 永不离开宿主进程）：

- 若原邮件文本中本身包含字面量 `[[1]]`，宿主会自动避开已有编号并选用不相交的槽位号（如 `[[2]]...[[/2]]`）；
- **插件必须保持占位符成对、闭合、不重复、不嵌套、编号不变**，只翻译标记内部的自然语言文本：
  - 正确：`Please click [[1]]here[[/1]]` → `请点击[[1]]这里[[/1]]`
  - 错误：丢失 `[[/1]]`、改写成 `[1]...[/1]`、重复输出两次 `[[1]]` 或凭空编造 `[[99]]`
- **绝对不要输出原始 HTML 标签**：宿主在第二遍重写时只以纯文本转义方式写入模型输出，任何 `<script>` / `<a href>` 等原始 HTML 都会被转义显示。若占位符校验失败，宿主会自动安全退化为纯文本转义渲染，确保零 XSS 风险。

### 4.6 反向调用 2：`translation/probe`（daemon → 插件）

用户在「设置 › 翻译」点击“测试连接”时触发，**不包含任何邮件内容**，超时上限 15 秒：

```json
{"jsonrpc":"2.0","id":102,"method":"translation/probe","params":{"target_lang":"zh-CN"}}
```

成功应答：

```json
{"jsonrpc":"2.0","id":102,"result":{"ok":true,"detail":"ready"}}
```

探测失败（例如凭据缺失、上游鉴权失败或网络不通）时，返回 `{"ok": false, "detail": "auth_failed"}` 或标准 JSON-RPC 错误。

### 4.7 反向通知：`translation/cancel`（daemon → 插件，无 `id`）

当用户取消翻译、切换邮件、释放文档或关闭 Studio 窗口时，若注册时声明 `supports_cancel = true`，宿主会尽力发送单向 JSON-RPC 通知：

```json
{"jsonrpc":"2.0","method":"translation/cancel","params":{"request_id":101}}
```

> **非阻塞实现硬要求**：插件**必须**采用「独立读循环 + 异步工作任务 + 串行写队列」的读写分离结构。若你在读循环里同步 `await` 翻译请求，你将无法在慢翻译进行中收到 `translation/cancel`，也无法利用 `max_concurrency > 1` 并发处理多批次。收到取消后请尽快中止上游 HTTP 请求并丢弃该 `request_id` 的回写。

### 4.8 稳定 JSON-RPC 错误码速查

| 错误码 | 常量 / 含义 | 触发场景 |
|---|---|---|
| `-32014` | 认证失败 | 无效或已吊销的 `FEATHERPOST_PLUGIN_TOKEN`（连接立即关闭）。 |
| `-32015` | 传输/身份缝不可用 | 在 HTTP / stdio 上尝试认证，或 `plugin_auth.toml` 处于停摆态。 |
| `-32016` | 缺少权限或跨会话拒绝 | 未声明 `translation_provider`，或非受托主体调用受限方法。 |
| `-32018` | `PROVIDER_UNAVAILABLE` | 插件未注册、已停用、已卸载、版本变化、代次不匹配、断线或被新连接顶替。 |
| `-32019` | `PROVIDER_TIMEOUT` | 翻译或探测超过总预算时限。 |
| `-32020` | `PROVIDER_ERROR` | 插件返回业务错误或畸形响应（如 ID 集合不完整）。 |
| `-32022` | `PROVIDER_BUSY` | 端点排队、单会话或全局并发/字节预算满载。 |
| `-32023` | `PROVIDER_CANCELLED` | 已被 `cancelTranslation` 或调用方断线取消。 |

---

## 5. 继承全局代理（`getProxyConfig`）与信任边界

如果插件需要访问外部翻译 API 并希望遵循 Featherpost「设置 › 网络与代理」中的全局代理配置，可在已认证的同 UID 流式连接上调用：

```json
{"jsonrpc":"2.0","id":3,"method":"getProxyConfig","params":{"include_secret":true}}
```

- 不需要代理密码时传 `{}`（或 `include_secret: false`），响应只含代理模式、主机、端口、用户名、绕过列表与系统代理快照；
- 需要带密码认证的自定义代理时传 `{"include_secret": true}`（仅限本地流式 IPC，HTTP/stdio 禁止返回密码）。
- **信任边界声明**：`getProxyConfig(include_secret=true)` 反映了本地同 UID 原生程序的既有信任边界（同 UID 本地进程本就可以访问用户主目录下的进程资源）。请将取到的代理密码保存在内存中按需使用，绝不打印到 stderr 或落盘。

---

## 6. 跨语言可运行骨架

### 6.1 Rust 骨架（推荐，复用 `maild-client`）

仓库内置完整可运行的 Rust 参考插件位于 [`plugins/example-translator/src/main.rs`](../../plugins/example-translator/src/main.rs)，核心模式如下：

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use maild_client::{
    DiscoveryOptions, Incoming, JsonRpcError, PluginTranslateRequest, PluginTranslateResponse,
    PluginTranslationCancelNotification, PluginTranslationProbeRequest,
    PluginTranslationProbeResponse, TranslatedBatchItem, TranslationProviderRegisterRequest,
};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 读取真实品牌环境变量（切勿写 MAILD_*）
    let token = Zeroizing::new(std::env::var("FEATHERPOST_PLUGIN_TOKEN")?);
    let _data_dir = std::env::var("FEATHERPOST_PLUGIN_DATA_DIR").ok();
    let config_path = std::env::var("FEATHERPOST_CONFIG").ok().map(Into::into);

    // 2. 发现并连接同 UID 本地流式 IPC
    let endpoint = maild_client::discover_endpoint(&DiscoveryOptions {
        config_path,
        ..DiscoveryOptions::default()
    })?;
    let mut conn = maild_client::connect_stream(&endpoint).await?;

    // 3. 认证 → 注册 translation_provider
    conn.authenticate(&token).await?;
    let register = TranslationProviderRegisterRequest {
        supported_languages: vec!["*".into()],
        max_batch_items: Some(16),
        max_batch_chars: Some(12_000),
        max_concurrency: Some(2),
        supports_cancel: true,
        variant_revision: "my-engine-v1".into(),
    };
    conn.call("translationProviderRegister", serde_json::to_value(&register)?, |_| {}).await?;

    // 4. 读写分离：独立串行写任务 + 每请求独立异步任务 + 取消表
    let (mut reader, mut writer) = conn.into_split();
    let (reply_tx, mut reply_rx) = mpsc::channel::<(Value, Result<Value, JsonRpcError>)>(32);
    let writer_task = tokio::spawn(async move {
        while let Some((id, outcome)) = reply_rx.recv().await {
            if writer.respond(&id, outcome).await.is_err() {
                break;
            }
        }
    });

    let in_flight: Arc<Mutex<HashMap<String, watch::Sender<bool>>>> = Arc::default();

    while let Ok(frame) = reader.next_incoming().await {
        match frame {
            Incoming::Request { id, method, params } => {
                let tx = reply_tx.clone();
                match method.as_str() {
                    "translation/translate" => {
                        let key = id.to_string();
                        let (cancel_tx, mut cancel_rx) = watch::channel(false);
                        in_flight.lock().unwrap().insert(key.clone(), cancel_tx);
                        let map = Arc::clone(&in_flight);
                        tokio::spawn(async move {
                            let req: PluginTranslateRequest = match serde_json::from_value(params) {
                                Ok(r) => r,
                                Err(e) => {
                                    map.lock().unwrap().remove(&key);
                                    let _ = tx.send((id, Err(JsonRpcError {
                                        code: -32602, message: e.to_string(), data: None,
                                    }))).await;
                                    return;
                                }
                            };
                            // 在此调用你的上游翻译 API，同时 tokio::select! 监听 cancel_rx.changed()
                            let items = req.items.into_iter().map(|it| TranslatedBatchItem {
                                id: it.id,
                                text: format!("[{}] {}", req.target_lang, it.text),
                            }).collect();
                            map.lock().unwrap().remove(&key);
                            if *cancel_rx.borrow() { return; }
                            let resp = PluginTranslateResponse {
                                items,
                                detected_source_lang: (req.source_lang != "auto").then_some(req.source_lang),
                                variant_revision: Some("my-engine-v1".into()),
                            };
                            let _ = tx.send((id, Ok(serde_json::to_value(resp).unwrap()))).await;
                        });
                    }
                    "translation/probe" => {
                        tokio::spawn(async move {
                            let req: PluginTranslationProbeRequest = serde_json::from_value(params).unwrap();
                            let resp = PluginTranslationProbeResponse {
                                ok: true,
                                detail: format!("ready for {}", req.target_lang),
                            };
                            let _ = tx.send((id, Ok(serde_json::to_value(resp).unwrap()))).await;
                        });
                    }
                    other => {
                        let _ = tx.send((id, Err(JsonRpcError {
                            code: -32601, message: format!("unsupported method: {other}"), data: None,
                        }))).await;
                    }
                }
            }
            Incoming::Notification(n) if n.method == "translation/cancel" => {
                if let Ok(c) = serde_json::from_value::<PluginTranslationCancelNotification>(n.params) {
                    if let Some(cancel_tx) = in_flight.lock().unwrap().remove(&c.request_id.to_string()) {
                        cancel_tx.send_replace(true);
                    }
                }
            }
            _ => {}
        }
    }

    drop(reply_tx);
    let _ = writer_task.await;
    Ok(())
}
```

### 6.2 Python 3 骨架（`asyncio` 零外部依赖，Unix Domain Socket）

无需任何 Rust 依赖，只要支持 Unix Domain Socket（或 Windows Named Pipe）与 JSON 即可编写插件。将以下脚本保存为 `bin/my-translator` 并赋予可执行权限（`chmod +x bin/my-translator`）：

```python
#!/usr/bin/env python3
"""Minimal non-blocking Featherpost translation_provider plugin in Python 3 (asyncio)."""

import asyncio
import json
import os
import pathlib
import sys
import tomllib

VARIANT_REVISION = "python-example-v1"
MAX_FRAME_BYTES = 1024 * 1024


def discover_socket_path() -> str:
    """Resolve the daemon Unix socket from FEATHERPOST_CONFIG (or platform default)."""
    if sock := os.environ.get("FEATHERPOST_API_SOCKET"):
        return sock
    cfg_path = os.environ.get("FEATHERPOST_CONFIG")
    if cfg_path and pathlib.Path(cfg_path).is_file():
        with open(cfg_path, "rb") as f:
            cfg = tomllib.load(f)
        api_sock = cfg.get("api", {}).get("socket")
        if api_sock:
            p = pathlib.Path(api_sock)
            return str(p if p.is_absolute() else (pathlib.Path(cfg_path).parent / p))
        return str(pathlib.Path(cfg_path).parent / "featherpost.sock")
    # macOS default: ~/Library/Application Support/Featherpost/featherpost.sock
    if sys.platform == "darwin":
        return str(pathlib.Path.home() / "Library/Application Support/Featherpost/featherpost.sock")
    xdg_runtime = os.environ.get("XDG_RUNTIME_DIR") or str(pathlib.Path.home() / ".local/share")
    return str(pathlib.Path(xdg_runtime) / "featherpost/featherpost.sock")


async def send_frame(writer: asyncio.StreamWriter, lock: asyncio.Lock, payload: dict) -> None:
    line = (json.dumps(payload, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    if len(line) > MAX_FRAME_BYTES:
        raise ValueError("JSON-RPC frame exceeds 1 MiB limit")
    async with lock:
        writer.write(line)
        await writer.drain()


async def read_frame(reader: asyncio.StreamReader) -> dict | None:
    raw = await reader.readline()
    if not raw:
        return None
    if len(raw) > MAX_FRAME_BYTES:
        raise ValueError("incoming frame exceeds 1 MiB limit")
    return json.loads(raw.decode("utf-8"))


async def translate_items(params: dict) -> dict:
    """Replace this coroutine with your actual async HTTP translation call (preserving [[k]]...[[/k]])."""
    target_lang = params["target_lang"]
    source_lang = params.get("source_lang", "auto")
    translated = [
        {"id": item["id"], "text": f"[{target_lang}] {item['text']}"}
        for item in params["items"]
    ]
    out = {"items": translated, "variant_revision": VARIANT_REVISION}
    if source_lang and source_lang != "auto":
        out["detected_source_lang"] = source_lang
    return out


async def main() -> None:
    token = os.environ.get("FEATHERPOST_PLUGIN_TOKEN", "").strip()
    if not token:
        raise SystemExit("Missing FEATHERPOST_PLUGIN_TOKEN")
    _data_dir = os.environ.get("FEATHERPOST_PLUGIN_DATA_DIR", "")

    reader, writer = await asyncio.open_unix_connection(discover_socket_path(), limit=MAX_FRAME_BYTES)
    write_lock = asyncio.Lock()

    # 1. Authenticate (MUST be first request)
    await send_frame(writer, write_lock, {
        "jsonrpc": "2.0", "id": 1, "method": "authenticate", "params": {"token": token},
    })
    auth_resp = await read_frame(reader)
    if not auth_resp or "error" in auth_resp:
        raise SystemExit(f"authenticate failed: {auth_resp}")

    # 2. Register translation provider capabilities
    await send_frame(writer, write_lock, {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "translationProviderRegister",
        "params": {
            "supported_languages": ["*"],
            "max_batch_items": 16,
            "max_batch_chars": 12000,
            "max_concurrency": 2,
            "supports_cancel": True,
            "variant_revision": VARIANT_REVISION,
        },
    })
    reg_resp = await read_frame(reader)
    if not reg_resp or "error" in reg_resp:
        raise SystemExit(f"translationProviderRegister failed: {reg_resp}")

    # 3. Non-blocking dispatch loop with cancellation table
    in_flight: dict[str, asyncio.Task] = {}

    async def handle_translate_task(req_id, params: dict) -> None:
        key = json.dumps(req_id)
        try:
            result = await translate_items(params)
            await send_frame(writer, write_lock, {"jsonrpc": "2.0", "id": req_id, "result": result})
        except asyncio.CancelledError:
            return
        except Exception as exc:
            await send_frame(writer, write_lock, {
                "jsonrpc": "2.0",
                "id": req_id,
                "error": {"code": -32020, "message": str(exc)[:200]},
            })
        finally:
            in_flight.pop(key, None)

    try:
        while (frame := await read_frame(reader)) is not None:
            method = frame.get("method")
            if not method:
                continue
            if "id" not in frame:
                # Notification
                if method == "translation/cancel":
                    cancel_key = json.dumps(frame.get("params", {}).get("request_id"))
                    if task := in_flight.pop(cancel_key, None):
                        task.cancel()
                continue

            req_id = frame["id"]
            params = frame.get("params") or {}
            if method == "translation/translate":
                key = json.dumps(req_id)
                in_flight[key] = asyncio.create_task(handle_translate_task(req_id, params))
            elif method == "translation/probe":
                target = params.get("target_lang", "en")
                await send_frame(writer, write_lock, {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {"ok": True, "detail": f"python translator ready for {target}"},
                })
            else:
                await send_frame(writer, write_lock, {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "error": {"code": -32601, "message": f"unsupported method: {method}"},
                })
    finally:
        for task in list(in_flight.values()):
            task.cancel()
        writer.close()
        await writer.wait_closed()


if __name__ == "__main__":
    asyncio.run(main())
```

---

## 7. 安装、启停、升级、回滚与调试

### 7.1 通过 `featherpostctl` CLI 操作（全部运行时生效，无需重启守护进程）

```sh
# 1. 安装插件目录（自动 staging 校验并原子发布）
featherpostctl plugin install --source ./my-translator

# 2. 查看已安装插件与状态
featherpostctl plugin list

# 3. 启用插件（supervised 会立即启动并注册；Studio 设置页实时热刷新）
featherpostctl plugin enable org.example.my-translator

# （若为 independent 插件）获取一次性认证令牌并启动你的进程
export FEATHERPOST_PLUGIN_TOKEN="$(featherpostctl plugin-token org.example.my-translator)"
./my-translator/bin/my-translator

# 4. 停用插件（立即撤销 generation，含 independent 插件）
featherpostctl plugin disable org.example.my-translator

# 5. 升级插件（plugin.toml 的 version 必须严格高于当前版本，旧版自动保留至 .rollback）
featherpostctl plugin install --source ./my-translator-v0.2.0

# 6. 回滚到上一版本（再次执行可切回）
featherpostctl plugin rollback org.example.my-translator

# 7. 卸载插件（不删除 FEATHERPOST_PLUGIN_DATA_DIR 中的用户数据）
featherpostctl plugin remove org.example.my-translator
```

### 7.2 通过 Featherpost Studio 图形界面操作

1. 打开 **设置 › 扩展**，点击「安装扩展」选择插件目录，核对预览面板中声明的 `translation_provider` 权限后确认安装；
2. 在扩展列表中启用插件；若插件声明了 `ui_executable`，可直接点击「打开插件设置」配置插件自有参数；
3. 切到 **设置 › 翻译**，无需重启即可看到新插件实时出现且状态为「在线」，选中并点击「测试连接」验证 `translation/probe`；
4. 升级/回滚/卸载同样可在「设置 › 扩展」中完成，状态通过 `maild://translation-providers` 推送即时同步到翻译设置与阅读区。

### 7.3 验收清单（上线前自检）

- [ ] `plugin.toml` 使用 `schema_version = 2`、`protocol_version = 1`、`capabilities = ["translation_provider"]`，且 ID 不以 `builtin` 开头；
- [ ] 读取真实环境变量 `FEATHERPOST_PLUGIN_TOKEN`、`FEATHERPOST_PLUGIN_DATA_DIR`、`FEATHERPOST_CONFIG`；
- [ ] 读写分离非阻塞：在 10 秒慢翻译期间发送 `translation/cancel` 能立刻中止并返回空闲；
- [ ] `translation/translate` 返回项 `id` 与请求完全一致，且完整保留 `[[1]]...[[/1]]` 成对占位符；
- [ ] 单帧含 `\n` 不超过 `1 MiB`，错误消息不回显邮件正文或密钥；
- [ ] 切换底层模型/词表时递增 `variant_revision`；
- [ ] `featherpostctl plugin disable` / `upgrade` / `rollback` 后在途任务快速失败且新代次干净生效。
