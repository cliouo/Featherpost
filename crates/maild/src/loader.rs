//! 声明式 loader：TOML 配置 → 插件装配。
//!
//! 装配模型：
//! 1. 解析 TOML：`[plugins.<name>]` 表逐项装配，`enabled = false` 跳过（缺失即启用）；
//! 2. 按「名字 → 工厂」注册表找工厂；未知插件记入 `LoadError::UnknownPlugin` 并继续；
//! 3. 工厂把 `<name>.config` 表反序列化为配置类型 `C`，`provide` 进上下文，
//!    再以配置构造插件并 `ctx.plugin()` 挂载；
//! 4. 挂载顺序 = 配置文件书写顺序（TOML 表保序）；依赖乱序由内核响应式 inject 兜底。
//!
//! 配置覆盖（intercept）：装配前对上下文 `intercept_config::<C>(frag)`，
//! 该 scope 内插件 `apply` 时 `get::<C>()` 读到「base 被内层 fragment 覆盖」的合并值。

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use cordis::{Context, FiberHandle, FiberState, Plugin, Service};
use serde::de::DeserializeOwned;

pub use crate::config::Merge;

/// loader 装配错误。
#[derive(Debug)]
pub enum LoadError {
    /// 顶层 TOML 语法错误。
    Parse(toml::de::Error),
    /// 顶层结构不合约定（如 `plugins` 不是表）。
    Structure(String),
    /// 配置引用了未注册的插件。
    UnknownPlugin(String),
    /// 插件配置反序列化失败。
    Config {
        plugin: String,
        source: toml::de::Error,
    },
    /// 插件装配失败（apply 报错或未产出预期服务）。
    Apply { plugin: String, detail: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Parse(e) => write!(f, "TOML 解析失败: {e}"),
            LoadError::Structure(s) => write!(f, "配置结构错误: {s}"),
            LoadError::UnknownPlugin(name) => write!(f, "未注册的插件: {name}"),
            LoadError::Config { plugin, source } => write!(f, "插件 {plugin} 配置错误: {source}"),
            LoadError::Apply { plugin, detail } => write!(f, "插件 {plugin} 装配失败: {detail}"),
        }
    }
}

impl Error for LoadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            LoadError::Parse(e) => Some(e),
            LoadError::Config { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// 插件工厂：`(装配上下文, TOML 插件项)` → 装配结果。
pub type Factory =
    Box<dyn Fn(&mut Assembly, &toml::Value) -> Result<(), LoadError> + Send + Sync + 'static>;

/// 静态注册的插件定义。
pub struct PluginDef {
    pub name: &'static str,
    pub factory: Factory,
}

/// 插件注册表（v1 静态编译进二进制，零 ABI 风险；动态插件是远期）。
#[derive(Default)]
pub struct Registry {
    defs: HashMap<&'static str, PluginDef>,
}

impl Registry {
    /// 注册插件定义（同名覆盖，返回旧定义）。
    pub fn register(&mut self, def: PluginDef) -> Option<PluginDef> {
        self.defs.insert(def.name, def)
    }

    pub fn get(&self, name: &str) -> Option<&Factory> {
        self.defs.get(name).map(|d| &d.factory)
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.defs.keys().copied().collect()
    }
}

/// 一次装配的产物：上下文 + 挂载句柄 + 非致命错误（未知插件 / 配置错误被跳过）。
pub struct Assembly {
    pub ctx: Context,
    pub handles: Vec<(String, FiberHandle)>,
    pub errors: Vec<LoadError>,
}

impl Assembly {
    /// 装配报告：(插件名, 状态)。守护进程健康检查 / 诊断用。
    pub fn report(&self) -> Vec<(String, FiberState)> {
        self.handles
            .iter()
            .map(|(n, h)| (n.clone(), h.state()))
            .collect()
    }

    /// 反序（LIFO）卸载全部插件。
    pub async fn dispose(&mut self) {
        let handles = std::mem::take(&mut self.handles);
        for (_, h) in handles.into_iter().rev() {
            h.dispose().await;
        }
    }
}

/// 从插件项里取 `config` 值（缺失 → `None`，走 Default）。
fn config_value(entry: &toml::Value) -> Option<toml::Value> {
    entry.get("config").cloned()
}

/// 插件项反序列化为配置类型（无 `config` 节 → 空表，走 `#[serde(default)]`）。
fn deserialize_config<C: DeserializeOwned>(
    plugin: &str,
    entry: &toml::Value,
) -> Result<C, LoadError> {
    let value = config_value(entry).unwrap_or_else(|| toml::Value::Table(toml::Table::new()));
    value.try_into::<C>().map_err(|source| LoadError::Config {
        plugin: plugin.into(),
        source,
    })
}

/// 从完整 TOML 中读取一个启用插件的 typed config；禁用或缺失返回 `None`。
///
/// 守护进程启动前的安全检查与正式 loader 共用这一解析入口。
/// 独立 `maild-client` 不依赖守护进程 crate，会镜像 `[plugins.api.config]` 的稳定字段；
/// 修改 API endpoint 配置 schema 时必须同步共享客户端的 parity fixture。
pub fn plugin_config<C: DeserializeOwned>(
    toml_text: &str,
    plugin: &str,
) -> Result<Option<C>, LoadError> {
    let root: toml::Value = toml::from_str(toml_text).map_err(LoadError::Parse)?;
    let Some(plugins) = root.get("plugins") else {
        return Ok(None);
    };
    let Some(table) = plugins.as_table() else {
        return Err(LoadError::Structure("`plugins` 必须是表".into()));
    };
    let Some(entry) = table.get(plugin) else {
        return Ok(None);
    };
    if entry.get("enabled").and_then(|value| value.as_bool()) == Some(false) {
        return Ok(None);
    }
    deserialize_config(plugin, entry).map(Some)
}

/// 从 TOML 文本装配插件（`plugins` 节缺失 → 空装配）。
///
/// 返回的 `Assembly` 里 `errors` 为逐插件非致命错误；只有顶层 TOML 语法 /
/// 结构错误才是 `Err`。
pub fn load(reg: &Registry, ctx: &Context, toml_text: &str) -> Result<Assembly, LoadError> {
    let root: toml::Value = toml::from_str(toml_text).map_err(LoadError::Parse)?;
    let mut asm = Assembly {
        ctx: ctx.clone(),
        handles: Vec::new(),
        errors: Vec::new(),
    };
    let Some(plugins) = root.get("plugins") else {
        return Ok(asm);
    };
    let Some(table) = plugins.as_table() else {
        return Err(LoadError::Structure("`plugins` 必须是表".into()));
    };
    for (name, entry) in table {
        // 允许 `enabled = false` 关闭（缺失即启用）
        if let Some(false) = entry.get("enabled").and_then(|v| v.as_bool()) {
            continue;
        }
        let Some(factory) = reg.get(name) else {
            asm.errors.push(LoadError::UnknownPlugin(name.clone()));
            continue;
        };
        if let Err(e) = factory(&mut asm, entry) {
            asm.errors.push(e);
        }
    }
    Ok(asm)
}

/// 声明式插件定义：TOML 配置反序列化为 `C` → `provide` → 构造插件挂载。
///
/// `make` 在配置已 provide 后调用（可在其中 `ctx.get::<C>()` 读取，含 intercept 合并）。
/// 配置类型 `C` 即插件间传递的「配置服务」，别的插件可以 inject 它。
/// 无 `config` 节时按空表反序列化——`#[serde(default)]` 的字段默认值照常生效。
pub fn plugin<C, P, F>(name: &'static str, make: F) -> PluginDef
where
    C: Service + DeserializeOwned,
    P: Plugin + 'static,
    F: Fn(&Context, Arc<C>) -> Arc<P> + Send + Sync + 'static,
{
    PluginDef {
        name,
        factory: Box::new(move |asm, entry| {
            let cfg = deserialize_config::<C>(name, entry)?;
            let cfg = Arc::new(cfg);
            asm.ctx.provide::<C>(cfg.clone());
            let p = make(&asm.ctx, cfg);
            let handle = asm.ctx.plugin(p);
            asm.handles.push((name.to_string(), handle));
            Ok(())
        }),
    }
}
