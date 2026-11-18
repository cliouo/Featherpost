use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::oneshot;

use crate::realm::RealmId;
use crate::Service;

#[derive(Default)]
struct DependentsState {
    fibers: HashSet<u64>,
    waiters: Vec<oneshot::Sender<()>>,
}

/// 单次服务提供的运行时实例记录（对齐上游 `reflect.ts` 的 `Impl`）。
///
/// 追踪单调递增 `uid`（使替换同类型绑定也能驱动依赖方重载）与 `dependents` 集合
/// （使提供者在卸载或撤回绑定时等待所有已提交消费者完成异步清理后再释放底层资源）。
pub(crate) struct ImplRecord {
    pub(crate) uid: u64,
    pub(crate) realm: RealmId,
    pub(crate) type_id: TypeId,
    pub(crate) value: Arc<dyn Any + Send + Sync>,
    pub(crate) provider_fiber: u64,
    pub(crate) active: AtomicBool,
    dependents: Mutex<DependentsState>,
}

impl ImplRecord {
    pub(crate) fn new(
        uid: u64,
        realm: RealmId,
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
        provider_fiber: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            uid,
            realm,
            type_id,
            value,
            provider_fiber,
            active: AtomicBool::new(true),
            dependents: Mutex::new(DependentsState::default()),
        })
    }

    pub(crate) fn add_dependent(&self, fiber_id: u64) {
        if fiber_id == self.provider_fiber {
            return;
        }
        self.dependents.lock().unwrap().fibers.insert(fiber_id);
    }

    pub(crate) fn remove_dependent(&self, fiber_id: u64) {
        let waiters = {
            let mut st = self.dependents.lock().unwrap();
            if !st.fibers.remove(&fiber_id) || !st.fibers.is_empty() {
                return;
            }
            std::mem::take(&mut st.waiters)
        };
        for tx in waiters {
            let _ = tx.send(());
        }
    }

    pub(crate) fn snapshot_dependents(&self) -> Vec<u64> {
        self.dependents
            .lock()
            .unwrap()
            .fibers
            .iter()
            .copied()
            .collect()
    }

    pub(crate) async fn wait_no_dependents(&self) {
        loop {
            let rx = {
                let mut st = self.dependents.lock().unwrap();
                if st.fibers.is_empty() {
                    return;
                }
                let (tx, rx) = oneshot::channel();
                st.waiters.push(tx);
                rx
            };
            let _ = rx.await;
        }
    }
}

/// 类型键控服务表（共效果上下文 `Σ = (k:K) ⇀ 𝒱k` 的 Rust 近似）。
///
/// 两层键 `(realm, TypeId)`：realm 即论文 `@@isolate` 的 realm symbol——
/// 不同 realm 下同一类型是独立绑定（per-account 多实例）。
#[derive(Default)]
pub struct Registry {
    map: HashMap<(RealmId, TypeId), Arc<ImplRecord>>,
    next_uid: u64,
}

impl Registry {
    fn alloc_uid(&mut self) -> u64 {
        self.next_uid += 1;
        self.next_uid
    }

    pub fn provide<T: Service>(
        &mut self,
        realm: RealmId,
        svc: Arc<T>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        let erased: Arc<dyn Any + Send + Sync> = svc;
        let (_, prev) = self.provide_record(realm, TypeId::of::<T>(), erased, 0);
        prev.map(|r| Arc::clone(&r.value))
    }

    pub(crate) fn provide_record(
        &mut self,
        realm: RealmId,
        type_id: TypeId,
        value: Arc<dyn Any + Send + Sync>,
        provider_fiber: u64,
    ) -> (Arc<ImplRecord>, Option<Arc<ImplRecord>>) {
        let uid = self.alloc_uid();
        let rec = ImplRecord::new(uid, realm, type_id, value, provider_fiber);
        let prev = self.map.insert((realm, type_id), Arc::clone(&rec));
        if let Some(ref old) = prev {
            old.active.store(false, Ordering::Release);
        }
        (rec, prev)
    }

    pub fn revoke(&mut self, realm: RealmId, id: TypeId) -> Option<Arc<dyn Any + Send + Sync>> {
        let removed = self.map.remove(&(realm, id))?;
        removed.active.store(false, Ordering::Release);
        Some(Arc::clone(&removed.value))
    }

    /// 值校验式撤销：仅当当前绑定就是 `expected`（指针相等）才移除。
    ///
    /// 比 `revoke` 语义更精确——只撤「我 provide 的那份」，不会误删
    /// 卸载窗口期内被其他 fiber 重新 provide 的同类型新绑定。
    pub fn revoke_if(
        &mut self,
        realm: RealmId,
        id: TypeId,
        expected: &Arc<dyn Any + Send + Sync>,
    ) -> Option<Arc<dyn Any + Send + Sync>> {
        let key = (realm, id);
        match self.map.get(&key) {
            Some(current) if Arc::ptr_eq(&current.value, expected) => {
                let removed = self.map.remove(&key)?;
                removed.active.store(false, Ordering::Release);
                Some(Arc::clone(&removed.value))
            }
            _ => None,
        }
    }

    /// 按 `ImplRecord` 精确撤回。
    pub(crate) fn revoke_record(&mut self, rec: &Arc<ImplRecord>) -> bool {
        rec.active.store(false, Ordering::Release);
        let key = (rec.realm, rec.type_id);
        match self.map.get(&key) {
            Some(current) if current.uid == rec.uid => {
                self.map.remove(&key);
                true
            }
            _ => false,
        }
    }

    pub fn get<T: Service>(&self, realm: RealmId) -> Option<Arc<T>> {
        let rec = self.get_active_record(realm, TypeId::of::<T>())?;
        Arc::clone(&rec.value).downcast::<T>().ok()
    }

    pub(crate) fn get_active_record(
        &self,
        realm: RealmId,
        id: TypeId,
    ) -> Option<Arc<ImplRecord>> {
        let rec = self.map.get(&(realm, id))?;
        if !rec.active.load(Ordering::Acquire) {
            return None;
        }
        Some(Arc::clone(rec))
    }

    pub fn contains(&self, realm: RealmId, id: TypeId) -> bool {
        self.get_active_record(realm, id).is_some()
    }
}
