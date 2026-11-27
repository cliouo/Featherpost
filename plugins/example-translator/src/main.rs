//! 最小可运行外部翻译 Provider 插件示例（ADR-0050）。
//!
//! 演示：
//! 1. 读取宿主注入的真实环境变量：`FEATHERPOST_PLUGIN_TOKEN`、`FEATHERPOST_PLUGIN_DATA_DIR`、`FEATHERPOST_CONFIG`；
//! 2. 通过 `maild_client` 连接同 UID 本地流式 IPC，执行 `authenticate` → `translationProviderRegister`；
//! 3. 读写分离与非阻塞请求派发：每条 `translation/translate` 在独立任务中运行，
//!    读循环在翻译进行中仍可即时接收 `translation/cancel { request_id }`、并发批次与 `translation/probe`。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maild_client::{
    DiscoveryOptions, Endpoint, Incoming, JsonRpcError, PluginTranslateRequest,
    PluginTranslateResponse, PluginTranslationCancelNotification, PluginTranslationProbeRequest,
    PluginTranslationProbeResponse, StreamConnection, TranslatedBatchItem,
    TranslationProviderRegisterRequest,
};
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroizing;

const VARIANT_REVISION: &str = "example-echo-v1";
const MAX_CONCURRENCY: u32 = 2;

#[derive(Clone)]
struct TranslatorConfig {
    /// 可选延迟（毫秒），用于本地演示并发与 `translation/cancel` 取消通路。
    simulated_delay: Duration,
}

impl Default for TranslatorConfig {
    fn default() -> Self {
        Self {
            simulated_delay: Duration::from_millis(0),
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("[example-translator] 退出: {err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let token = std::env::var(maild_brand::PLUGIN_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(Zeroizing::new)
        .ok_or_else(|| {
            format!(
                "缺少认证令牌环境变量 {}（supervised 插件由 daemon 自动注入，independent 插件请用 {} plugin-token <id> 获取）",
                maild_brand::PLUGIN_TOKEN_ENV,
                maild_brand::CLI_CTL
            )
        })?;

    // 宿主为每个插件提供独立的 0700 数据目录与活动配置文件路径。
    let _data_dir = std::env::var(maild_brand::PLUGIN_DATA_ENV).ok();
    let config_path = std::env::var(maild_brand::CONFIG_ENV).ok().map(Into::into);

    let endpoint = maild_client::discover_endpoint(&DiscoveryOptions {
        config_path,
        ..DiscoveryOptions::default()
    })
    .map_err(|error| format!("无法发现宿主 IPC endpoint: {error}"))?;

    let config = TranslatorConfig::default();
    serve_endpoint(endpoint, &token, config).await
}

async fn serve_endpoint(
    endpoint: Endpoint,
    token: &str,
    config: TranslatorConfig,
) -> Result<(), String> {
    if !endpoint.supports_notifications() {
        return Err("translation_provider 需要本地流式 IPC endpoint（Unix socket / named pipe）".into());
    }
    let conn = maild_client::connect_stream(&endpoint)
        .await
        .map_err(|error| format!("连接宿主 IPC 失败: {error}"))?;
    serve_connection(conn, token, config).await
}

/// 可在测试中复用的协议主循环：认证 → 注册 → 读写分离非阻塞处理反向调用。
async fn serve_connection(
    mut conn: StreamConnection,
    token: &str,
    config: TranslatorConfig,
) -> Result<(), String> {
    conn.authenticate(token)
        .await
        .map_err(|error| format!("authenticate 失败: {error}"))?;

    let register_req = TranslationProviderRegisterRequest {
        supported_languages: vec!["*".into()],
        max_batch_items: Some(16),
        max_batch_chars: Some(12_000),
        max_concurrency: Some(MAX_CONCURRENCY),
        supports_cancel: true,
        variant_revision: VARIANT_REVISION.into(),
    };
    let register_params = serde_json::to_value(&register_req)
        .map_err(|error| format!("序列化注册参数失败: {error}"))?;
    let registered = conn
        .call("translationProviderRegister", register_params, |_| {})
        .await
        .map_err(|error| format!("translationProviderRegister 失败: {error}"))?;
    eprintln!(
        "[example-translator] 注册成功: generation={}",
        registered.get("generation").and_then(Value::as_u64).unwrap_or(0)
    );

    let (mut reader, mut writer) = conn.into_split();
    let (reply_tx, mut reply_rx) = mpsc::channel::<(Value, Result<Value, JsonRpcError>)>(32);

    // 独立写任务：多条并发翻译任务完成后各自把响应发到这里串行写回，帧不交错。
    let writer_task = tokio::spawn(async move {
        while let Some((id, outcome)) = reply_rx.recv().await {
            if writer.respond(&id, outcome).await.is_err() {
                break;
            }
        }
    });

    // 在途请求取消表：request_id 字符串 -> watch::Sender<bool>。
    let in_flight: Arc<Mutex<HashMap<String, watch::Sender<bool>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    loop {
        let frame = match reader.next_incoming().await {
            Ok(frame) => frame,
            Err(error) => {
                eprintln!("[example-translator] IPC 断开: {error}");
                break;
            }
        };
        match frame {
            Incoming::Request { id, method, params } => {
                let id_key = id.to_string();
                let reply_tx = reply_tx.clone();
                let in_flight_clone = Arc::clone(&in_flight);
                let config = config.clone();

                match method.as_str() {
                    "translation/translate" => {
                        let (cancel_tx, mut cancel_rx) = watch::channel(false);
                        in_flight_clone
                            .lock()
                            .unwrap()
                            .insert(id_key.clone(), cancel_tx);

                        // 脱离读循环异步执行，确保运行期间读循环仍能读取 `translation/cancel`。
                        tokio::spawn(async move {
                            let outcome =
                                handle_translate(params, config.simulated_delay, &mut cancel_rx)
                                    .await;
                            in_flight_clone.lock().unwrap().remove(&id_key);
                            if let Some(outcome) = outcome {
                                let _ = reply_tx.send((id, outcome)).await;
                            }
                        });
                    }
                    "translation/probe" => {
                        tokio::spawn(async move {
                            let outcome = handle_probe(params);
                            let _ = reply_tx.send((id, outcome)).await;
                        });
                    }
                    other => {
                        let _ = reply_tx
                            .send((
                                id,
                                Err(JsonRpcError {
                                    code: -32601,
                                    message: format!("unsupported method: {other}"),
                                    data: None,
                                }),
                            ))
                            .await;
                    }
                }
            }
            Incoming::Notification(notification) => {
                if notification.method == "translation/cancel" {
                    if let Ok(cancel) = serde_json::from_value::<PluginTranslationCancelNotification>(
                        notification.params,
                    ) {
                        let key = cancel.request_id.to_string();
                        if let Some(tx) = in_flight.lock().unwrap().remove(&key) {
                            tx.send_replace(true);
                        }
                    }
                }
            }
            Incoming::Response { .. } => {}
        }
    }

    // 取消所有残留任务并等待写任务退出。
    for (_, tx) in in_flight.lock().unwrap().drain() {
        tx.send_replace(true);
    }
    drop(reply_tx);
    let _ = writer_task.await;
    Ok(())
}

/// 收到取消信号返回 `None`（跳过回写）；正常完成返回 `Some(Result)`。
async fn handle_translate(
    params: Value,
    simulated_delay: Duration,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Option<Result<Value, JsonRpcError>> {
    let request: PluginTranslateRequest = match serde_json::from_value(params) {
        Ok(req) => req,
        Err(error) => {
            return Some(Err(JsonRpcError {
                code: -32602,
                message: format!("invalid translation/translate params: {error}"),
                data: None,
            }));
        }
    };

    if !simulated_delay.is_zero() {
        tokio::select! {
            _ = tokio::time::sleep(simulated_delay) => {}
            _ = wait_cancelled(cancel_rx) => return None,
        }
    }
    if *cancel_rx.borrow() {
        return None;
    }

    // 最小可运行示例：为每条输入加上 `[target_lang]` 演示前缀，保持 id 与顺序一一对应。
    let items = request
        .items
        .into_iter()
        .map(|item| TranslatedBatchItem {
            id: item.id,
            text: format!("[{}] {}", request.target_lang, item.text),
        })
        .collect();
    let response = PluginTranslateResponse {
        items,
        detected_source_lang: (request.source_lang != "auto").then_some(request.source_lang),
        variant_revision: Some(VARIANT_REVISION.into()),
    };
    Some(
        serde_json::to_value(response).map_err(|error| JsonRpcError {
            code: -32020,
            message: format!("serialize response failed: {error}"),
            data: None,
        }),
    )
}

fn handle_probe(params: Value) -> Result<Value, JsonRpcError> {
    let request: PluginTranslationProbeRequest =
        serde_json::from_value(params).map_err(|error| JsonRpcError {
            code: -32602,
            message: format!("invalid translation/probe params: {error}"),
            data: None,
        })?;
    let response = PluginTranslationProbeResponse {
        ok: true,
        detail: format!("example translator ready for {}", request.target_lang),
    };
    serde_json::to_value(response).map_err(|error| JsonRpcError {
        code: -32020,
        message: format!("serialize probe failed: {error}"),
        data: None,
    })
}

async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn handle_translate_and_probe_and_cancel_work_non_blocking() {
        let probe = handle_probe(json!({ "target_lang": "zh-Hans" })).unwrap();
        assert_eq!(probe["ok"], true);

        let (_tx, mut rx) = watch::channel(false);
        let translated = handle_translate(
            json!({
                "job_id": "job-1",
                "source_lang": "en",
                "target_lang": "zh-Hans",
                "content_kind": "plain",
                "items": [{ "id": 1, "text": "Hello" }]
            }),
            Duration::from_millis(0),
            &mut rx,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(translated["items"][0]["id"], 1);
        assert_eq!(translated["items"][0]["text"], "[zh-Hans] Hello");

        let (cancel_tx, mut cancel_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            handle_translate(
                json!({
                    "job_id": "job-slow",
                    "source_lang": "en",
                    "target_lang": "ja",
                    "content_kind": "plain",
                    "items": [{ "id": 1, "text": "Slow" }]
                }),
                Duration::from_secs(30),
                &mut cancel_rx,
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(15)).await;
        cancel_tx.send_replace(true);
        let outcome = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancel must stop in-flight work immediately")
            .unwrap();
        assert!(outcome.is_none());
    }
}
