use super::{AgentObservation, FailureDecision, ObservationKind};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum StepFailureKind {
    Transient,
    Expectation,
    LowValueObservation,
    RepeatedAction,
    BudgetExhausted,
    StalledTrajectory,
    TrajectoryDrift,
    ManualIntervention,
    Semantic,
    Irrecoverable,
}

impl StepFailureKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Expectation => "expectation",
            Self::LowValueObservation => "low_value_observation",
            Self::RepeatedAction => "repeated_action",
            Self::BudgetExhausted => "budget_exhausted",
            Self::StalledTrajectory => "stalled_trajectory",
            Self::TrajectoryDrift => "trajectory_drift",
            Self::ManualIntervention => "manual_intervention",
            Self::Semantic => "semantic",
            Self::Irrecoverable => "irrecoverable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureAction {
    RetryStep,
    Replan,
    AskUser,
    Abort,
}

impl FailureAction {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::RetryStep => "retry_step",
            Self::Replan => "replan",
            Self::AskUser => "ask_user",
            Self::Abort => "abort",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RecoveryOutcome {
    Continued,
    EscalatedToAskUser,
    Aborted,
    Failed,
}

impl RecoveryOutcome {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Continued => "continued",
            Self::EscalatedToAskUser => "escalated_to_ask_user",
            Self::Aborted => "aborted",
            Self::Failed => "failed",
        }
    }
}

/// 统一恢复策略定义（集中映射表，不分散 if/else）。
///
/// 映射规则（代码内唯一真相源）：
/// - Transient         → RetryStep (max 1) → 失败则 Replan
/// - LowValueObservation → Replan (max 1) → 再失败 AskUser
/// - RepeatedAction    → Replan (max 1) → 再失败 AskUser
/// - StalledTrajectory → Replan (max 1) → 再失败 AskUser
/// - TrajectoryDrift   → Replan (max 1) → 再失败 AskUser
/// - Expectation       → Replan (max 1) → 再失败 AskUser
/// - Semantic          → Replan (max 1) → 再失败 AskUser
/// - ManualIntervention → AskUser (立即)
/// - BudgetExhausted   → AskUser (立即)
/// - Irrecoverable     → Abort (立即)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoveryPolicy {
    pub(super) action: FailureAction,
    /// 该 failure kind 在当前 run 中最多允许的恢复尝试次数
    pub(super) max_attempts: usize,
    /// 超过 max_attempts 后自动升级到的 action
    pub(super) escalate_to: FailureAction,
}

impl RecoveryPolicy {
    pub(crate) fn no_recovery(action: FailureAction) -> Self {
        Self {
            action,
            max_attempts: 0,
            escalate_to: action,
        }
    }

    pub(crate) fn with_escalate(
        action: FailureAction,
        max_attempts: usize,
        escalate_to: FailureAction,
    ) -> Self {
        Self {
            action,
            max_attempts,
            escalate_to,
        }
    }
}

/// 默认恢复策略映射表（集中定义，所有 failure kind 的恢复行为从此处读取）。
pub(crate) fn default_recovery_for_failure(kind: StepFailureKind) -> RecoveryPolicy {
    match kind {
        StepFailureKind::Transient => {
            RecoveryPolicy::with_escalate(FailureAction::RetryStep, 1, FailureAction::Replan)
        }
        StepFailureKind::LowValueObservation
        | StepFailureKind::RepeatedAction
        | StepFailureKind::StalledTrajectory
        | StepFailureKind::TrajectoryDrift
        | StepFailureKind::Expectation
        | StepFailureKind::Semantic => {
            RecoveryPolicy::with_escalate(FailureAction::Replan, 1, FailureAction::AskUser)
        }
        StepFailureKind::ManualIntervention | StepFailureKind::BudgetExhausted => {
            RecoveryPolicy::no_recovery(FailureAction::AskUser)
        }
        StepFailureKind::Irrecoverable => RecoveryPolicy::no_recovery(FailureAction::Abort),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplanScope {
    CurrentStep,
    RemainingPlan,
    Full,
}

impl ReplanScope {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::CurrentStep => "current_step",
            Self::RemainingPlan => "remaining_plan",
            Self::Full => "full",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RuntimeControllerState {
    pub(crate) max_steps: usize,
    pub(crate) max_replans: usize,
    pub(crate) failure_count: usize,
    pub(crate) replan_count: usize,
    pub(crate) ask_user_count: usize,
    /// per-failure-kind 恢复尝试计数（防循环保护）。
    /// key: kind.as_str()，value: 该 kind 已触发的恢复次数。
    #[serde(skip)]
    pub(crate) recovery_kind_counts: HashMap<String, usize>,
}

impl RuntimeControllerState {
    pub(crate) fn new(max_steps: usize, max_replans: usize) -> Self {
        Self {
            max_steps,
            max_replans,
            failure_count: 0,
            replan_count: 0,
            ask_user_count: 0,
            recovery_kind_counts: HashMap::new(),
        }
    }

    pub(crate) fn configure_limits(&mut self, max_steps: usize, max_replans: usize) {
        self.max_steps = max_steps;
        self.max_replans = max_replans;
    }

    pub(crate) fn record_failure(&mut self) {
        self.failure_count += 1;
    }

    pub(crate) fn record_ask_user(&mut self) {
        self.ask_user_count += 1;
    }

    pub(crate) fn try_consume_replan(&mut self) -> bool {
        if self.replan_count >= self.max_replans {
            return false;
        }
        self.replan_count += 1;
        true
    }

    pub(crate) fn remaining_replans(&self) -> usize {
        self.max_replans.saturating_sub(self.replan_count)
    }

    /// 记录一次 failure kind 的恢复尝试，返回当前计数（含本次）。
    pub(crate) fn record_recovery_for_kind(&mut self, kind: &StepFailureKind) -> usize {
        let key = kind.as_str().to_string();
        let count = self.recovery_kind_counts.entry(key).or_insert(0);
        *count += 1;
        *count
    }
}

pub(crate) fn failure_to_observation(step: usize, failure: &FailureDecision) -> AgentObservation {
    AgentObservation {
        step,
        source: format!("failure:{}", failure.kind.as_str()),
        content: serde_json::to_string(&json!({
            "failure_kind": failure.kind.as_str(),
            "failure_action": failure.action.as_str(),
            "detail": failure.detail,
            "source": failure.source,
            "user_message": failure.user_message,
        }))
        .unwrap_or_else(|_| "{\"failure_kind\":\"unknown\"}".to_string()),
        kind: Some(ObservationKind::Text),
    }
}
