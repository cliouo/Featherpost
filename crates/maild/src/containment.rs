//! ADR-0020：supervised 子进程的进程 containment。
//!
//! Unix：子进程自为进程组长（`process_group(0)`），停止走 SIGTERM→SIGKILL 组阶梯，
//! 收尾探测组空；Windows：Job Object kill-on-close，后代无法脱离。setsid 主动逃逸
//! 不可见，是文档化残余风险（M5 沙箱解决）。

use std::time::Duration;

/// 停止阶梯与组空探测的重试预算：短暂 zombie / 信号投递窗口内收敛即视为干净。
#[cfg(unix)]
const PROBE_ATTEMPTS: u32 = 3;
#[cfg(unix)]
const PROBE_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(unix)]
pub(crate) struct ChildContainment {
    pgid: i32,
}

#[cfg(unix)]
impl ChildContainment {
    /// 让子进程成为新进程组组长（pgid = 子 pid）。
    pub(crate) fn prepare(command: &mut tokio::process::Command) {
        command.process_group(0);
    }

    /// spawn 成功后装配 containment 句柄；子 pid 缺失（已被 reap）时返回 None。
    pub(crate) fn arm(child: &tokio::process::Child) -> Option<Self> {
        let pid = child.id()?;
        Some(Self {
            pgid: pid as i32,
        })
    }

    fn signal_group(&self, signal: i32) {
        // 组已不存在时 ESRCH 是正常路径，忽略错误即可。
        unsafe {
            libc::killpg(self.pgid, signal);
        }
    }

    fn group_alive(&self) -> bool {
        unsafe { libc::killpg(self.pgid, 0) == 0 }
    }

    /// 外部停止的完整阶梯（ADR-0020 §1）：SIGTERM 组 → 预算内等直接子进程退出 →
    /// 超时 SIGKILL 组 → 组空探测。返回 false 表示组内仍有成员（leaked）。
    pub(crate) async fn stop(
        &self,
        direct: &mut tokio::process::Child,
        budget: Duration,
    ) -> bool {
        self.signal_group(libc::SIGTERM);
        if tokio::time::timeout(budget, direct.wait()).await.is_err() {
            self.signal_group(libc::SIGKILL);
            let _ = direct.wait().await;
        }
        self.probe_until_empty().await
    }

    /// 直接子进程自然退出后的组清扫（ADR-0020 §3）：契约要求前台运行，留下组内
    /// 后台成员即违约，一律 SIGKILL。返回 false 表示清扫后仍有成员（leaked）。
    pub(crate) async fn sweep_after_exit(&self) -> bool {
        if !self.group_alive() {
            return true;
        }
        self.terminate_after_exit();
        self.probe_until_empty().await
    }

    /// Synchronous half of the natural-exit sweep. Callers that must relaunch immediately can
    /// terminate leftover group members before dropping the old containment handle.
    pub(crate) fn terminate_after_exit(&self) {
        self.signal_group(libc::SIGKILL);
    }

    async fn probe_until_empty(&self) -> bool {
        for _ in 0..PROBE_ATTEMPTS {
            if !self.group_alive() {
                return true;
            }
            tokio::time::sleep(PROBE_INTERVAL).await;
        }
        !self.group_alive()
    }
}

#[cfg(windows)]
struct JobHandle(windows_sys::Win32::Foundation::HANDLE);
// HANDLE 是内核句柄值，无线程亲和性；跨 await 持有安全。
#[cfg(windows)]
unsafe impl Send for JobHandle {}
#[cfg(windows)]
unsafe impl Sync for JobHandle {}

#[cfg(windows)]
pub(crate) struct ChildContainment {
    job: JobHandle,
}

#[cfg(windows)]
impl ChildContainment {
    /// Windows 上 containment 在 spawn 后经 Job Object 装配，spawn 选项无需变更。
    pub(crate) fn prepare(_command: &mut tokio::process::Command) {}

    /// 创建 kill-on-close job 并把子进程指派进去。任一步失败返回 None（调用方按
    /// spawn 失败处理——宁可拒绝启动也不无 containment 运行）。
    pub(crate) fn arm(child: &tokio::process::Child) -> Option<Self> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let job: HANDLE = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                CloseHandle(job);
                return None;
            }
            // 理论竞态窗口：子进程在指派前 spawn 后代。契约本就禁止 daemonize；
            // CREATE_SUSPENDED+ResumeThread 需要线程快照，v1 不引入。
            let handle = child.raw_handle()? as HANDLE;
            if AssignProcessToJobObject(job, handle) == 0 {
                CloseHandle(job);
                return None;
            }
            Some(Self {
                job: JobHandle(job),
            })
        }
    }

    /// 终止整个 job（后代无法脱离 job，一次调用覆盖全部）。返回值恒 clean：
    /// Windows 的 containment 是完备的，leaked 仅是 Unix 组语义的概念。
    pub(crate) async fn stop(
        &self,
        direct: &mut tokio::process::Child,
        budget: Duration,
    ) -> bool {
        self.terminate_job();
        let _ = tokio::time::timeout(budget, direct.wait()).await;
        true
    }

    /// 直接子进程自然退出的清扫：后代仍在 job 内，一并终止。
    pub(crate) async fn sweep_after_exit(&self) -> bool {
        self.terminate_after_exit();
        true
    }

    pub(crate) fn terminate_after_exit(&self) {
        self.terminate_job();
    }

    fn terminate_job(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            TerminateJobObject(self.job.0, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for ChildContainment {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;
        unsafe {
            CloseHandle(self.job.0);
        }
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) struct ChildContainment;

#[cfg(not(any(unix, windows)))]
impl ChildContainment {
    pub(crate) fn prepare(_command: &mut tokio::process::Command) {}
    pub(crate) fn arm(_child: &tokio::process::Child) -> Option<Self> {
        None
    }
    pub(crate) async fn stop(
        &self,
        direct: &mut tokio::process::Child,
        budget: Duration,
    ) -> bool {
        let _ = direct.kill();
        let _ = tokio::time::timeout(budget, direct.wait()).await;
        false
    }
    pub(crate) async fn sweep_after_exit(&self) -> bool {
        false
    }
    pub(crate) fn terminate_after_exit(&self) {}
}

/// 组空探测语义单测（Unix）：对不存在的组 probe 返回干净、信号投递静默成功。
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dead_process_group_probes_clean() {
        // 占住一个确定不存在的 pgid：ESRCH 路径必须判定为「组空」而非 leaked。
        let guard = ChildContainment { pgid: i32::MAX - 1 };
        assert!(!guard.group_alive());
        assert!(guard.probe_until_empty().await);
    }

    #[tokio::test]
    async fn live_group_is_killed_and_then_probes_clean() {
        use std::os::unix::process::CommandExt as _;
        // 自建一个双进程组：组长 sleep + 组内成员 sh 循环，验证 sweep 全覆盖。
        let mut command = std::process::Command::new("/bin/sh");
        command.arg("-c").arg("sleep 30 & wait");
        command.process_group(0);
        let mut child = tokio::process::Command::from(command).spawn().unwrap();
        let guard = ChildContainment::arm(&child).unwrap();
        assert!(guard.group_alive(), "新组应当存活");
        assert!(guard.stop(&mut child, Duration::from_secs(2)).await, "停止后组必须清空");
        assert!(!guard.group_alive());
    }
}
