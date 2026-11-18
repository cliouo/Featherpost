//! 配置合并与覆盖（intercept 的配置化封装）。

use std::sync::Arc;

use cordis::{Context, Service};

/// 可合并的配置片段：`self` 为底，`frag` 覆盖（字段级，`frag` 赢）。
///
/// 用法：配置类型 derive `Clone` + `Deserialize` 后实现本 trait（字段级覆盖）。
pub trait Merge: Sized {
    /// 返回 `self` 与覆盖片段 `frag` 的合并（`frag` 赢）。
    fn merge(&self, frag: &Self) -> Self;
}

/// 给上下文套一层配置覆盖：此后 `get::<T>()` 读到「base 被 `frag` 覆盖」的合并值。
/// 内层覆盖外层（见内核 `intercept` 的注册序语义：外层先应用、内层后应用）。
pub fn intercept_config<T>(ctx: &Context, frag: T) -> Context
where
    T: Merge + Service + 'static,
{
    ctx.intercept::<T>(move |base: Arc<T>| Arc::new(base.merge(&frag)))
}
