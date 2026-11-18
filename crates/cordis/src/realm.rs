/// 作用域标识（论文 `@@isolate` 的 realm symbol 的简化版）。
///
/// `ROOT` 是全局作用域：root 的绑定对所有 scope 可见（回退），
/// root 的事件监听器对所有 scope 的可见。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RealmId(pub u64);

impl RealmId {
    pub const ROOT: RealmId = RealmId(0);
}
