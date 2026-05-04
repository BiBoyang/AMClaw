use thiserror::Error;

/// task_store 模块的错误类型
#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum TaskStoreError {
    #[error("无效 URL: {0}")]
    InvalidUrl(String),
    #[error("不支持内网/本地地址: {0}")]
    PrivateNetworkUrl(String),
    #[error("仅支持 http/https URL: {0}")]
    UnsupportedScheme(String),
    #[error("{0}")]
    Validation(String),
    #[error("数据库错误: {0}")]
    Database(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTaskRecord {
    pub article_id: String,
    pub task_id: String,
    pub normalized_url: String,
    pub original_url: String,
    pub created_new: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStatusRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub title: Option<String>,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub status: String,
    pub retry_count: i64,
    pub last_error: Option<String>,
    pub output_path: Option<String>,
    pub snapshot_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTaskRecord {
    pub task_id: String,
    pub status: String,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub normalized_url: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedTaskRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content_source: Option<String>,
    pub page_kind: Option<String>,
    pub output_path: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTaskRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub original_url: String,
}

/// 可被 worker 领取的任务（pending 或 lease 已过期的 processing）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimableTaskRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub original_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkTaskArchivedInput<'a> {
    pub output_path: &'a str,
    pub title: Option<&'a str>,
    pub page_kind: Option<&'a str>,
    pub snapshot_path: Option<&'a str>,
    pub content_source: Option<&'a str>,
    pub summary: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskContentRecord {
    pub task_id: String,
    pub article_id: String,
    pub normalized_url: String,
    pub original_url: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSessionRecord {
    pub user_id: String,
    pub merged_text: String,
    pub message_ids: Vec<String>,
    pub updated_at: String,
}

/// 待补发的消息段记录
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChunkRecord {
    pub id: i64,
    pub user_id: String,
    pub context_token: String,
    pub chunk_text: String,
    pub chunk_index: usize,
    pub chunk_total: usize,
    pub created_at: String,
}

/// 记忆类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    /// 用户显式要求记住（最高优先级）
    Explicit,
    /// 项目级事实（模块职责、约束、边界）
    ProjectFact,
    /// 用户偏好（回复风格、输出形式、工作方式）
    UserPreference,
    /// 经验教训（失败模式、有效处理方式）
    Lesson,
    /// 系统自动提炼的主题/偏好
    Auto,
}

impl MemoryType {
    /// 默认优先级
    pub fn default_priority(&self) -> i64 {
        match self {
            Self::Explicit => 100,
            Self::ProjectFact => 85,
            Self::UserPreference => 80,
            Self::Lesson => 75,
            Self::Auto => 60,
        }
    }

    /// 类型标识字符串（DB 存储和序列化使用）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ProjectFact => "project_fact",
            Self::UserPreference => "user_preference",
            Self::Lesson => "lesson",
            Self::Auto => "auto",
        }
    }

    /// 在 prompt 中呈现的前缀标签
    pub fn label_prefix(&self) -> &'static str {
        match self {
            Self::Explicit | Self::Auto => "[记忆]",
            Self::ProjectFact => "[项目]",
            Self::UserPreference => "[偏好]",
            Self::Lesson => "[经验]",
        }
    }

    /// 是否允许覆盖（promote）另一个类型
    /// 优先级高的可以覆盖优先级低的
    pub fn can_promote(&self, other: &Self) -> bool {
        self.default_priority() > other.default_priority()
    }
}

impl std::fmt::Display for MemoryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for MemoryType {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "explicit" => Ok(Self::Explicit),
            "project_fact" => Ok(Self::ProjectFact),
            "user_preference" => Ok(Self::UserPreference),
            "lesson" => Ok(Self::Lesson),
            "auto" => Ok(Self::Auto),
            _ => Err(format!("未知 memory_type: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMemoryRecord {
    pub id: String,
    pub user_id: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub status: String,
    pub priority: i64,
    pub last_used_at: Option<String>,
    pub use_count: i64,
    pub retrieved_count: i64,
    pub injected_count: i64,
    pub useful: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// 用户会话结构化状态（跨会话持久化）
///
/// v2 新增 7-slot 完整 session state 字段：
/// - goal, current_subtask, next_step: Option<String>
/// - constraints, confirmed_facts, done_items, open_questions: JSON TEXT 数组
///
/// 旧字段（last_user_intent, current_task, blocked_reason）保留兼容。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UserSessionStateRecord {
    pub user_id: String,
    pub last_user_intent: Option<String>,
    pub current_task: Option<String>,
    pub next_step: Option<String>,
    pub blocked_reason: Option<String>,
    // v2: 7-slot session state
    pub goal: Option<String>,
    pub current_subtask: Option<String>,
    pub constraints_json: Option<String>,
    pub confirmed_facts_json: Option<String>,
    pub done_items_json: Option<String>,
    pub open_questions_json: Option<String>,
    pub updated_at: String,
}

impl UserSessionStateRecord {
    /// 从 JSON 字符串解析字符串数组（内部辅助）
    fn parse_json_array(json_str: &Option<String>) -> Vec<String> {
        match json_str {
            Some(s) if !s.trim().is_empty() => {
                serde_json::from_str::<Vec<String>>(s).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// 将字符串数组序列化为 JSON（内部辅助）
    fn serialize_json_array(items: &[String]) -> Option<String> {
        if items.is_empty() {
            None
        } else {
            serde_json::to_string(items).ok()
        }
    }

    pub fn constraints(&self) -> Vec<String> {
        Self::parse_json_array(&self.constraints_json)
    }

    pub fn set_constraints(&mut self, items: Vec<String>) {
        self.constraints_json = Self::serialize_json_array(&items);
    }

    pub fn confirmed_facts(&self) -> Vec<String> {
        Self::parse_json_array(&self.confirmed_facts_json)
    }

    pub fn set_confirmed_facts(&mut self, items: Vec<String>) {
        self.confirmed_facts_json = Self::serialize_json_array(&items);
    }

    pub fn done_items(&self) -> Vec<String> {
        Self::parse_json_array(&self.done_items_json)
    }

    pub fn set_done_items(&mut self, items: Vec<String>) {
        self.done_items_json = Self::serialize_json_array(&items);
    }

    pub fn open_questions(&self) -> Vec<String> {
        Self::parse_json_array(&self.open_questions_json)
    }

    pub fn set_open_questions(&mut self, items: Vec<String>) {
        self.open_questions_json = Self::serialize_json_array(&items);
    }

    /// 返回 7 槽位中非空字段的数量（用于 trace 观测）
    pub fn populated_slot_count(&self) -> usize {
        let mut count = 0;
        if self.goal.is_some() {
            count += 1;
        }
        if self.current_subtask.is_some() {
            count += 1;
        }
        if self.constraints_json.is_some() {
            count += 1;
        }
        if self.confirmed_facts_json.is_some() {
            count += 1;
        }
        if self.done_items_json.is_some() {
            count += 1;
        }
        if self.next_step.is_some() {
            count += 1;
        }
        if self.open_questions_json.is_some() {
            count += 1;
        }
        count
    }

    /// 7 槽位是否全部为空
    pub fn is_v2_empty(&self) -> bool {
        self.goal.is_none()
            && self.current_subtask.is_none()
            && self.constraints_json.is_none()
            && self.confirmed_facts_json.is_none()
            && self.done_items_json.is_none()
            && self.next_step.is_none()
            && self.open_questions_json.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 内容为空或仅 whitespace
    Empty,
    /// 内容超过单条长度限制
    TooLong,
    /// 与已有记忆规范化后重复
    Duplicate,
    /// 低优先级类型不能覆盖高优先级类型
    LowerPriorityWouldDowngradeHigher,
    /// user_id 或内容格式无效
    Invalid,
    /// 内容被判定为噪声（过短、黑名单短句等）
    Noise,
    /// 存储写入失败
    StorageError,
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "empty_content"),
            Self::TooLong => write!(f, "too_long"),
            Self::Duplicate => write!(f, "duplicate"),
            Self::LowerPriorityWouldDowngradeHigher => {
                write!(f, "lower_priority_would_downgrade_higher")
            }
            Self::Invalid => write!(f, "invalid"),
            Self::Noise => write!(f, "noise"),
            Self::StorageError => write!(f, "storage_error"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromoteReason {
    /// 高优先级类型提升了已有低优先级类型记忆
    TypePromotesLower { from: MemoryType, to: MemoryType },
}

impl std::fmt::Display for PromoteReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TypePromotesLower { from, to } => {
                write!(f, "{}_promotes_{}", from.as_str(), to.as_str())
            }
        }
    }
}

/// 写入决策结果
#[derive(Debug, Clone)]
pub enum WriteDecision {
    /// 新写入成功
    Written(Box<UserMemoryRecord>),
    /// 跳过写入
    Skipped {
        content_preview: String,
        reason: SkipReason,
    },
    /// 提升已有 auto 为 explicit（更新已有记录的 type 和 priority）
    Promoted { id: String, reason: PromoteReason },
}

/// 单次 run 的写侧状态
#[derive(Debug, Clone, Default)]
pub struct MemoryWriteState {
    /// 候选数量
    pub candidate_count: usize,
    /// 写入成功
    pub written: Vec<UserMemoryRecord>,
    /// 被跳过
    pub skipped: Vec<(String, SkipReason)>,
    /// 被提升
    pub promoted: Vec<(String, PromoteReason)>,
}

impl MemoryWriteState {
    #[cfg(test)]
    pub fn written_count(&self) -> usize {
        self.written.len()
    }

    #[cfg(test)]
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// 记录一次写入决策
    pub(super) fn record(&mut self, decision: WriteDecision) {
        match decision {
            WriteDecision::Written(record) => self.written.push(*record),
            WriteDecision::Skipped {
                content_preview,
                reason,
            } => self.skipped.push((content_preview, reason)),
            WriteDecision::Promoted { id, reason } => self.promoted.push((id, reason)),
        }
    }
}

/// Feedback 事件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackKind {
    /// 从 DB 中被检索出来
    Retrieved,
    /// 被注入到 prompt
    Injected,
    /// 被确认有用
    Useful,
}

/// 单次 run 的 feedback 状态（只记录，不直接改 store）
#[derive(Debug, Clone, Default)]
pub struct MemoryFeedbackState {
    /// memory_id → (retrieved 次数, injected 次数, useful 次数)
    feedback: std::collections::HashMap<String, (usize, usize, usize)>,
}

impl MemoryFeedbackState {
    pub fn record(&mut self, memory_id: &str, kind: FeedbackKind) {
        let entry = self
            .feedback
            .entry(memory_id.to_string())
            .or_insert((0, 0, 0));
        match kind {
            FeedbackKind::Retrieved => entry.0 += 1,
            FeedbackKind::Injected => entry.1 += 1,
            FeedbackKind::Useful => entry.2 += 1,
        }
    }

    /// 所有产生 feedback 的 memory ID 列表
    pub fn memory_ids(&self) -> Vec<String> {
        self.feedback.keys().cloned().collect()
    }

    /// 检索次数
    pub fn retrieved_count(&self, memory_id: &str) -> usize {
        self.feedback
            .get(memory_id)
            .map(|(r, _, _)| *r)
            .unwrap_or(0)
    }

    /// 注入次数
    pub fn injected_count(&self, memory_id: &str) -> usize {
        self.feedback
            .get(memory_id)
            .map(|(_, i, _)| *i)
            .unwrap_or(0)
    }

    /// 有用次数
    pub fn useful_count(&self, memory_id: &str) -> usize {
        self.feedback
            .get(memory_id)
            .map(|(_, _, u)| *u)
            .unwrap_or(0)
    }

    /// 是否有任何 feedback
    #[cfg(test)]
    pub fn has_feedback(&self) -> bool {
        !self.feedback.is_empty()
    }
}
