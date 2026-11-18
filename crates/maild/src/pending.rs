//! 写路径的领域类型（ADR-0027）：op 种类、状态机与载荷。
//!
//! 状态机的核心是**「结果未知」是一个独立终态**：命令已发出但没看到 tagged 响应时，
//! 可重放的操作（flag）可以自动重试，不可重放的操作（move）必须把事实交给用户裁决——
//! 重放会在目标文件夹产生第二份副本，而我们无从判断第一次到底成没成。

use serde::{Deserialize, Serialize};

/// 自动重试上限。超过后转 `failed`，交用户裁决，避免无限重放。
pub const MAX_AUTO_ATTEMPTS: i64 = 5;

/// 可写的 IMAP 系统标记。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFlag {
    Seen,
    Flagged,
    Answered,
    Deleted,
}

impl MessageFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Seen => "seen",
            Self::Flagged => "flagged",
            Self::Answered => "answered",
            Self::Deleted => "deleted",
        }
    }

    /// 库内列名（与 `as_str` 相同，但语义不同，分开写以免将来改一处漏一处）。
    pub fn column(self) -> &'static str {
        self.as_str()
    }

    /// IMAP 线上的系统标记名。
    pub fn imap_name(self) -> &'static str {
        match self {
            Self::Seen => "\\Seen",
            Self::Flagged => "\\Flagged",
            Self::Answered => "\\Answered",
            Self::Deleted => "\\Deleted",
        }
    }

    pub fn parse(value: &str) -> Option<MessageFlag> {
        match value {
            "seen" => Some(Self::Seen),
            "flagged" => Some(Self::Flagged),
            "answered" => Some(Self::Answered),
            "deleted" => Some(Self::Deleted),
            _ => None,
        }
    }
}

/// op 种类。**可重放性由种类决定，且在入队那一刻就已确定。**
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpKind {
    /// `UID STORE ±FLAGS`。幂等，可重放。
    Flag,
    /// `UID MOVE`（或降级 `COPY` + `\Deleted`）。**不可重放**。
    Move,
    /// `APPEND` 到已发送（ADR-0028 决策 9）。**不可重放**——重放会在已发送里留下两份。
    Append,
}

impl OpKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Move => "move",
            Self::Append => "append",
        }
    }

    pub fn parse(value: &str) -> Option<OpKind> {
        match value {
            "flag" => Some(Self::Flag),
            "move" => Some(Self::Move),
            "append" => Some(Self::Append),
            _ => None,
        }
    }

    /// 重放是否安全。决定「结果未知」时自动重试还是交用户裁决。
    pub fn replayable(self) -> bool {
        matches!(self, Self::Flag)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpState {
    Pending,
    InFlight,
    Done,
    Failed,
    /// 命令已发出，未见 tagged 响应。**不是 failed 的一种。**
    Unknown,
}

impl OpState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<OpState> {
        match value {
            "pending" => Some(Self::Pending),
            "in_flight" => Some(Self::InFlight),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// 「未落定」：仍会被执行，或仍在等用户裁决。对账必须跳过这些消息。
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::InFlight | Self::Unknown)
    }
}

/// flag 类载荷。`previous` 使失败时能把本地状态还原成用户点击之前的样子。
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlagPayload {
    pub flag: MessageFlag,
    pub value: bool,
    #[serde(default)]
    pub previous: bool,
}

/// move 类载荷。`target` 是目标文件夹的**服务器线上原名**。
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MovePayload {
    pub target: String,
}

/// 队列里的一条待办。
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingOp {
    pub id: i64,
    pub account: String,
    pub folder_id: i64,
    /// 所属文件夹的服务器线上原名（读取时 join 得到）。
    pub folder: String,
    pub uid: String,
    pub kind: OpKind,
    pub payload: String,
    pub state: OpState,
    pub attempts: i64,
    pub created_at: u64,
    pub updated_at: u64,
    pub detail: String,
}

impl PendingOp {
    pub fn flag_payload(&self) -> Option<FlagPayload> {
        (self.kind == OpKind::Flag)
            .then(|| serde_json::from_str(&self.payload).ok())
            .flatten()
    }

    pub fn move_payload(&self) -> Option<MovePayload> {
        (self.kind == OpKind::Move)
            .then(|| serde_json::from_str(&self.payload).ok())
            .flatten()
    }
}

/// 服务器上观察到的一条消息的标记位快照。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ObservedFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub deleted: bool,
}

/// 一轮对账的结果。
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReconcileOutcome {
    /// 标记位被服务器改写的行数。
    pub updated: usize,
    /// 因为在服务器上已经消失而删除的行数（只在全量轮次发生）。
    pub removed: usize,
    /// 被本地保护而跳过的行数（最近改动或有未落定的 op）。
    pub protected: usize,
}
