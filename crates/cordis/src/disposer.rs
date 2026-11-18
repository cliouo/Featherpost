use std::future::Future;
use std::pin::Pin;

/// 装箱的 `Send` future。
pub type BoxFuture<'a, T = ()> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 可逆效果的「逆」（对应 cordis `ctx.effect` 的返回形态）。
///
/// 统一同步/异步清理，由 `Context::dispose` LIFO await。Rust 无 async Drop，
/// 故异步逆走显式 `dispose().await`，同步逆亦可直接用 Drop（RAII）。
pub enum Disposer {
    Sync(Box<dyn FnOnce() + Send>),
    Async(BoxFuture<'static>),
}

impl Disposer {
    pub fn sync(f: impl FnOnce() + Send + 'static) -> Self {
        Self::Sync(Box::new(f))
    }

    pub fn async_fut<F>(f: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        Self::Async(Box::pin(f))
    }

    /// 执行该清理函数（同步立即调用，异步 `.await`）。
    pub async fn run(self) {
        match self {
            Self::Sync(f) => f(),
            Self::Async(f) => f.await,
        }
    }
}
