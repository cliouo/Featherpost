use std::any::Any;

/// 服务：可被 `provide` / `get` 的组件。一个服务对应一个具体类型。
///
/// 对应论文共效果上下文 `Σ = (k:K) ⇀ 𝒱k` 中的值类型 `𝒱k`。
pub trait Service: Any + Send + Sync {
    fn name(&self) -> &'static str;
}
