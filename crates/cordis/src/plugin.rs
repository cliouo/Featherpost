use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::{Context, Service};

pub type ComponentError = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type PluginError = ComponentError;

/// 类型擦除的服务读取拦截器（`Arc<T> -> Arc<T>`）。
pub type InterceptFn =
    Arc<dyn Fn(Arc<dyn Any + Send + Sync>) -> Arc<dyn Any + Send + Sync> + Send + Sync>;

/// 构造强类型的服务读取拦截器（可用于组件 `inject_intercepts` 或上下文拦截）。
pub fn intercept_fn<T: Service>(
    f: impl Fn(Arc<T>) -> Arc<T> + Send + Sync + 'static,
) -> InterceptFn {
    let f: Arc<dyn Fn(Arc<T>) -> Arc<T> + Send + Sync> = Arc::new(f);
    Arc::new(move |base: Arc<dyn Any + Send + Sync>| {
        let value = base
            .downcast::<T>()
            .expect("intercept service type mismatch");
        let out: Arc<dyn Any + Send + Sync> = f(value);
        out
    })
}

/// 内核错误码（对齐上游 `CordisError.Code`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CordisErrorCode {
    /// 在已失活（已卸载）的上下文上注册副作用、挂载子组件或更新状态。
    InactiveEffect,
    /// 组件配置校验失败（对齐上游 `ValidationError`）。
    ValidationFailed,
    /// 组件 `apply` 执行失败。
    ApplyFailed,
}

/// 内核结构化错误（对齐上游 `CordisError`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CordisError {
    pub code: CordisErrorCode,
    pub message: String,
}

impl CordisError {
    pub fn new(code: CordisErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn inactive_effect() -> Self {
        Self::new(
            CordisErrorCode::InactiveEffect,
            "cannot create effect on inactive context",
        )
    }

    pub fn validation(message: impl Into<String>) -> Self {
        Self::new(CordisErrorCode::ValidationFailed, message)
    }

    pub fn apply_failed(message: impl Into<String>) -> Self {
        Self::new(CordisErrorCode::ApplyFailed, message)
    }
}

impl fmt::Display for CordisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CordisError {}

/// 组件（对齐上游 v4.0.0-rc.13 `Component`）：`inject`（共效果规范 𝑑）+ `apply`（效果函数）。
///
/// `apply` 是同步的——它在依赖就绪后注册效果（`ctx.effect_*` / `ctx.on_*` / `ctx.provide`），
/// 异步工作发生在效果与事件处理器内部，而非 `apply` 本身。
pub trait Component: Send + Sync + 'static {
    /// 可选组件名（诊断与日志标识用）。
    fn name(&self) -> Option<&'static str> {
        None
    }

    /// 声明依赖的服务类型（`TypeId` 列表）。全部就绪后才运行 `apply`。
    fn inject(&self) -> Vec<TypeId> {
        vec![]
    }

    /// 注入点拦截元数据（对齐上游 `#206` `inject: { svc: config }`）：
    /// 在外层 `ctx.intercept` 之前应用，且不会作用于组件自身 `provide` 的服务，也不会跨越 `isolate` 边界。
    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        HashMap::new()
    }

    fn apply(&self, ctx: &Context) -> Result<(), ComponentError>;
}

/// 带强类型配置与可选校验的组件（对齐上游 `Component<Config>` + `Config` schema 校验 + `fiber.update()`）。
pub trait ConfigurableComponent: Send + Sync + 'static {
    type Config: Clone + Send + Sync + 'static;

    fn name(&self) -> Option<&'static str> {
        None
    }

    fn inject(&self) -> Vec<TypeId> {
        vec![]
    }

    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        HashMap::new()
    }

    /// 在每次加载或 `update` 时校验/归一化配置（对应上游 `resolveConfig` / `ValidationError`）。
    fn validate(&self, config: Self::Config) -> Result<Self::Config, ComponentError> {
        Ok(config)
    }

    fn apply(&self, ctx: &Context, config: Self::Config) -> Result<(), ComponentError>;
}

/// 兼容旧称谓 `Plugin`（上游 `#307` 将 `Plugin` 重命名为 `Component`，保留 `ctx.plugin()` 兼容别名）。
pub trait Plugin: Send + Sync + 'static {
    /// 声明依赖的服务类型（`TypeId` 列表）。全部就绪后才运行 `apply`。
    fn inject(&self) -> Vec<TypeId>;

    fn apply(&self, ctx: &Context) -> Result<(), PluginError>;
}

impl<P: Plugin> Component for P {
    fn inject(&self) -> Vec<TypeId> {
        Plugin::inject(self)
    }

    fn apply(&self, ctx: &Context) -> Result<(), ComponentError> {
        Plugin::apply(self, ctx)
    }
}

/// 内部统一擦除接口，统一驱动无配置 `Component`、带配置 `ConfigurableComponent` 与旧 `Arc<dyn Plugin>`。
pub(crate) trait ErasedComponent: Send + Sync + 'static {
    fn name(&self) -> Option<&'static str>;
    fn inject(&self) -> Vec<TypeId>;
    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn>;
    fn validate_config(
        &self,
        raw: Option<&Arc<dyn Any + Send + Sync>>,
    ) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError>;
    fn apply_erased(
        &self,
        ctx: &Context,
        resolved_config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<(), ComponentError>;
}

pub(crate) struct DynComponentAdapter(pub(crate) Arc<dyn Component>);

impl ErasedComponent for DynComponentAdapter {
    fn name(&self) -> Option<&'static str> {
        self.0.name()
    }

    fn inject(&self) -> Vec<TypeId> {
        self.0.inject()
    }

    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        self.0.inject_intercepts()
    }

    fn validate_config(
        &self,
        raw: Option<&Arc<dyn Any + Send + Sync>>,
    ) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError> {
        Ok(raw.cloned())
    }

    fn apply_erased(
        &self,
        ctx: &Context,
        _resolved_config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<(), ComponentError> {
        self.0.apply(ctx)
    }
}

pub(crate) struct DynPluginAdapter(pub(crate) Arc<dyn Plugin>);

impl ErasedComponent for DynPluginAdapter {
    fn name(&self) -> Option<&'static str> {
        None
    }

    fn inject(&self) -> Vec<TypeId> {
        self.0.inject()
    }

    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        HashMap::new()
    }

    fn validate_config(
        &self,
        raw: Option<&Arc<dyn Any + Send + Sync>>,
    ) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError> {
        Ok(raw.cloned())
    }

    fn apply_erased(
        &self,
        ctx: &Context,
        _resolved_config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<(), ComponentError> {
        self.0.apply(ctx)
    }
}

pub(crate) struct ConfigurableAdapter<C: ConfigurableComponent>(pub(crate) Arc<C>);

impl<C: ConfigurableComponent> ErasedComponent for ConfigurableAdapter<C> {
    fn name(&self) -> Option<&'static str> {
        self.0.name()
    }

    fn inject(&self) -> Vec<TypeId> {
        self.0.inject()
    }

    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        self.0.inject_intercepts()
    }

    fn validate_config(
        &self,
        raw: Option<&Arc<dyn Any + Send + Sync>>,
    ) -> Result<Option<Arc<dyn Any + Send + Sync>>, CordisError> {
        let Some(raw_any) = raw else {
            return Err(CordisError::validation("missing component config"));
        };
        let Some(typed) = raw_any.downcast_ref::<C::Config>() else {
            return Err(CordisError::validation("component config type mismatch"));
        };
        let resolved = self
            .0
            .validate(typed.clone())
            .map_err(|e| CordisError::validation(e.to_string()))?;
        let erased: Arc<dyn Any + Send + Sync> = Arc::new(resolved);
        Ok(Some(erased))
    }

    fn apply_erased(
        &self,
        ctx: &Context,
        resolved_config: Option<Arc<dyn Any + Send + Sync>>,
    ) -> Result<(), ComponentError> {
        let cfg_any = resolved_config.ok_or_else(|| CordisError::validation("missing resolved config"))?;
        let cfg = cfg_any
            .downcast_ref::<C::Config>()
            .ok_or_else(|| CordisError::validation("resolved config type mismatch"))?
            .clone();
        self.0.apply(ctx, cfg)
    }
}

pub(crate) type FnComponentCallback =
    Box<dyn Fn(&Context) -> Result<(), ComponentError> + Send + Sync>;

pub(crate) struct FnComponent {
    pub(crate) inject: Vec<TypeId>,
    pub(crate) intercepts: HashMap<TypeId, InterceptFn>,
    pub(crate) callback: FnComponentCallback,
}

impl Component for FnComponent {
    fn inject(&self) -> Vec<TypeId> {
        self.inject.clone()
    }

    fn inject_intercepts(&self) -> HashMap<TypeId, InterceptFn> {
        self.intercepts.clone()
    }

    fn apply(&self, ctx: &Context) -> Result<(), ComponentError> {
        (self.callback)(ctx)
    }
}
