use super::LlmConfig;
use super::{
    append_session_state_lines, log_agent_warn, AgentObservation, AgentRunContext,
    ExpectedObservation, FailureAction, FailureDecision, PlanStepStatus, PlannedDecision,
    PlannerInput, RecoveryOutcome, ReplanScope, RuntimeControllerState, RuntimePlanStep,
    RuntimeSessionStateSnapshot, StepFailureKind, DEFAULT_MAX_REPLANS, DEFAULT_MAX_STEPS,
};
use crate::context_pack::{ContextBudgetSummary, ContextSectionSnapshot};
use crate::session_summary::summarize_for_markdown;
use crate::task_store::UserSessionStateRecord;
use crate::tool_registry::ToolAction;
use anyhow::{Context, Result};
use chrono::Utc;
use chrono_tz::Asia::Shanghai;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub(crate) struct AgentRunTrace {
    pub(super) trace_version: &'static str,
    pub(crate) run_id: String,
    pub(super) started_at: String,
    pub(super) finished_at: Option<String>,
    pub(super) duration_ms: Option<u128>,
    pub(super) success: bool,
    pub(super) error: Option<String>,
    pub(super) final_output: Option<String>,
    pub(super) user_input: String,
    pub(super) user_input_chars: usize,
    pub(super) source_type: String,
    pub(super) trigger_type: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) message_ids: Vec<String>,
    pub(super) message_count: usize,
    pub(super) task_id: Option<String>,
    pub(super) article_id: Option<String>,
    pub(super) session_text: Option<String>,
    pub(super) session_text_chars: usize,
    pub(super) context_token_present: bool,
    pub(crate) controller_state: RuntimeControllerState,
    pub(crate) current_step_index: Option<usize>,
    pub(crate) step_count: usize,
    pub(super) workspace_root: String,
    pub(super) llm_fallback_reason: Option<String>,
    pub(super) rule_parse_error: Option<String>,
    pub(super) recovery_action: Option<FailureAction>,
    pub(super) recovery_result: Option<RecoveryOutcome>,
    pub(super) recovery_attempts: Vec<RecoveryTrace>,
    pub(super) decisions: Vec<DecisionTrace>,
    pub(crate) observations: Vec<ObservationTrace>,
    pub(super) failures: Vec<FailureTrace>,
    pub(crate) active_plan_steps: Vec<RuntimePlanStep>,
    pub(super) pending_replan_scope: Option<ReplanScope>,
    pub(super) last_progress_note: Option<String>,
    pub(super) llm_calls: Vec<LlmCallTrace>,
    pub(super) tool_calls: Vec<ToolCallTrace>,
    pub(super) session_state_snapshot: Option<RuntimeSessionStateSnapshot>,
    /// 最终步的 runtime session state（用于回写持久层）
    pub(super) final_runtime_session_state: Option<RuntimeSessionStateSnapshot>,
    pub(crate) memory_hit_count: usize, // 实际注入 prompt 的记忆条数（= injected）
    pub(crate) memory_injected_count: usize, // 兼容字段：与 memory_hit_count 同值
    pub(crate) memory_retrieved_count: usize, // 从 DB 取出的候选记忆条数
    pub(crate) memory_total_chars: usize, // 注入记忆的总字符数（= injected_total_chars）
    pub(crate) memory_injected_total_chars: usize, // 兼容字段：与 memory_total_chars 同值
    pub(crate) memory_dropped_count: usize, // 被裁剪掉的记忆条数
    pub(crate) memory_ids: Vec<String>, // 注入记忆的 ID 列表
    // --- Retriever-level observability ---
    pub(crate) retriever_name: String,           // 使用的检索器名称
    pub(crate) retrieval_candidate_count: usize, // 检索器返回的候选条数
    pub(crate) retrieval_hit_count: usize,       // 经裁剪后实际命中的条数
    pub(crate) retrieval_latency_ms: u128,       // 检索耗时（毫秒）
    pub(crate) retrieval_mode: String,           // 检索模式（rule/hybrid/semantic/shadow）
    pub(crate) retrieval_fallback_reason: Option<String>, // 回退原因
    pub(crate) retrieval_scores_present: bool,   // 是否包含语义分数
    pub(super) persistent_state_present: bool,
    pub(super) persistent_state_source: Option<String>,
    pub(super) persistent_state_updated: bool,
    pub(super) persistent_state_slot_count: usize,
    pub(super) persistent_state_preview: Option<String>,
    // --- ContextPack-level observability (C3/C4) ---
    #[serde(default)]
    pub(crate) context_pack_present: bool,
    #[serde(default)]
    pub(crate) context_pack_section_count: usize,
    #[serde(default)]
    pub(crate) context_pack_total_chars: usize,
    pub(crate) context_pack_drop_reasons: Vec<String>,
    #[serde(skip_serializing)]
    pub(super) user_session_state: Option<UserSessionStateRecord>,
    #[serde(skip_serializing)]
    pub(super) trace_dir_root: PathBuf,
}

#[derive(Debug, Serialize)]
pub(super) struct DecisionTrace {
    pub(super) step: usize,

    pub(super) current_step_index: Option<usize>,

    pub(super) source: String,

    pub(super) decision_type: String,

    pub(super) summary: String,

    pub(super) plan_steps: Vec<String>,

    pub(super) progress_note: Option<String>,

    pub(super) expected_observation: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PromptSnapshot {
    pub(super) system_prompt: String,

    pub(super) raw_user_input: String,

    pub(super) user_prompt: String,

    pub(super) context_sections: Vec<ContextSectionSnapshot>,

    pub(super) context_budget_summary: ContextBudgetSummary,

    pub(super) context_summary: String,

    pub(super) request_body: String,

    pub(super) system_prompt_chars: usize,

    pub(super) raw_user_input_chars: usize,

    pub(super) user_prompt_chars: usize,

    pub(super) context_summary_chars: usize,

    pub(super) request_body_chars: usize,

    pub(super) estimated_prompt_chars: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct LlmCallTrace {
    pub(super) source: String,

    pub(super) model: String,

    pub(super) base_url: String,

    pub(super) prompt: PromptSnapshot,

    pub(super) raw_response: Option<String>,

    pub(super) raw_response_chars: Option<usize>,

    pub(super) message_content: Option<String>,

    pub(super) message_content_chars: Option<usize>,

    pub(super) response_status: Option<u16>,

    pub(super) attempts: usize,

    pub(super) success: bool,

    pub(super) error: Option<String>,

    pub(super) decision_summary: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ToolCallTrace {
    pub(super) step: usize,

    pub(super) tool_name: String,

    pub(super) path: Option<String>,

    pub(super) content_chars: Option<usize>,

    pub(super) output: Option<String>,

    pub(super) output_chars: Option<usize>,

    pub(super) success: bool,

    pub(super) error: Option<String>,

    pub(super) duration_ms: Option<u128>,

    #[serde(skip_serializing)]
    pub(super) started_at: Option<Instant>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ObservationTrace {
    pub(crate) step: usize,

    pub(crate) source: String,

    pub(crate) summary: String,

    pub(crate) content_chars: usize,
}

#[derive(Debug, Serialize)]
pub(super) struct FailureTrace {
    pub(super) step: usize,

    pub(super) current_step_index: Option<usize>,

    pub(super) kind: StepFailureKind,

    pub(super) action: FailureAction,

    pub(super) replan_scope: Option<ReplanScope>,

    pub(super) source: String,

    pub(super) detail: String,

    pub(super) user_message: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct RecoveryTrace {
    pub(super) step: usize,

    pub(super) current_step_index: Option<usize>,

    pub(super) failure_kind: StepFailureKind,

    /// 映射前原始 action（failure 自带或映射表默认值）
    pub(super) original_action: FailureAction,

    /// 实际执行的 action（可能因防循环升级）
    pub(super) effective_action: FailureAction,

    /// 兼容旧消费方，保留 action 字段（值同 effective_action）
    pub(super) action: FailureAction,

    pub(super) outcome: RecoveryOutcome,

    pub(super) successful: bool,

    /// 本次恢复是否因防循环保护而被升级
    pub(super) escalated: bool,

    pub(super) source: String,

    pub(super) detail: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub(crate) struct AgentTraceIndexEntry {
    pub(super) trace_version: String,

    pub(crate) run_id: String,

    pub(super) started_at: String,

    pub(super) finished_at: Option<String>,

    pub(super) duration_ms: Option<u128>,

    pub(super) success: bool,

    pub(super) user_input: String,

    pub(super) user_input_chars: usize,

    pub(super) source_type: String,

    pub(super) trigger_type: Option<String>,

    pub(super) user_id: Option<String>,

    pub(super) message_ids: Vec<String>,

    pub(super) message_count: usize,

    pub(super) task_id: Option<String>,

    pub(super) article_id: Option<String>,

    pub(super) session_text_chars: usize,

    pub(super) context_token_present: bool,

    pub(crate) step_count: usize,

    pub(crate) llm_call_count: usize,

    pub(crate) tool_call_count: usize,

    pub(crate) observation_count: usize,

    pub(crate) final_output_chars: Option<usize>,

    pub(super) error: Option<String>,

    pub(super) llm_fallback_reason: Option<String>,

    #[serde(default)]
    pub(crate) memory_hit_count: usize,

    #[serde(default)]
    pub(crate) memory_injected_count: usize,

    #[serde(default)]
    pub(crate) memory_retrieved_count: usize,

    #[serde(default)]
    pub(crate) memory_total_chars: usize,

    #[serde(default)]
    pub(crate) memory_injected_total_chars: usize,

    #[serde(default)]
    pub(crate) memory_dropped_count: usize,

    #[serde(default)]
    pub(crate) recovery_attempt_count: usize,

    #[serde(default)]
    pub(crate) recovery_success_count: usize,

    #[serde(default)]
    pub(super) recovery_action: Option<String>,

    #[serde(default)]
    pub(super) recovery_result: Option<String>,

    #[serde(default)]
    pub(crate) retriever_name: String,

    #[serde(default)]
    pub(crate) retrieval_candidate_count: usize,

    #[serde(default)]
    pub(crate) retrieval_hit_count: usize,

    #[serde(default)]
    pub(crate) retrieval_latency_ms: u128,

    #[serde(default)]
    pub(crate) retrieval_mode: String,

    #[serde(default)]
    pub(crate) retrieval_fallback_reason: Option<String>,

    #[serde(default)]
    pub(crate) retrieval_scores_present: bool,

    #[serde(default)]
    pub(crate) context_pack_present: bool,

    #[serde(default)]
    pub(crate) context_pack_section_count: usize,

    #[serde(default)]
    pub(crate) context_pack_total_chars: usize,

    pub(crate) json_file: String,

    pub(crate) markdown_file: String,
}

impl AgentRunTrace {
    pub(crate) fn new(
        workspace_root: &std::path::Path,
        user_input: &str,
        context: AgentRunContext,
    ) -> Self {
        let message_count = context.message_ids.len();
        let session_text_chars = context
            .session_text
            .as_deref()
            .map(|value| value.chars().count())
            .unwrap_or(0);
        Self {
            trace_version: "agent_trace_v1",
            run_id: Uuid::new_v4().to_string(),
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            duration_ms: None,
            success: false,
            error: None,
            final_output: None,
            user_input: user_input.to_string(),
            user_input_chars: user_input.chars().count(),
            source_type: context.source_type,
            trigger_type: context.trigger_type,
            user_id: context.user_id,
            message_ids: context.message_ids,
            message_count,
            task_id: context.task_id,
            article_id: context.article_id,
            session_text: context.session_text,
            session_text_chars,
            context_token_present: context.context_token_present,
            controller_state: RuntimeControllerState::new(DEFAULT_MAX_STEPS, DEFAULT_MAX_REPLANS),
            current_step_index: None,
            step_count: 0,
            workspace_root: workspace_root.display().to_string(),
            llm_fallback_reason: None,
            rule_parse_error: None,
            recovery_action: None,
            recovery_result: None,
            recovery_attempts: Vec::new(),
            decisions: Vec::new(),
            observations: Vec::new(),
            failures: Vec::new(),
            active_plan_steps: Vec::new(),
            pending_replan_scope: None,
            last_progress_note: None,
            llm_calls: Vec::new(),
            tool_calls: Vec::new(),
            session_state_snapshot: None,
            final_runtime_session_state: None,
            trace_dir_root: workspace_root.join("data").join("agent_traces"),
            memory_hit_count: 0,
            memory_injected_count: 0,
            memory_retrieved_count: 0,
            memory_total_chars: 0,
            memory_injected_total_chars: 0,
            memory_dropped_count: 0,
            memory_ids: Vec::new(),
            retriever_name: String::new(),
            retrieval_candidate_count: 0,
            retrieval_hit_count: 0,
            retrieval_latency_ms: 0,
            retrieval_mode: String::new(),
            retrieval_fallback_reason: None,
            retrieval_scores_present: false,
            persistent_state_present: context.user_session_state.is_some(),
            persistent_state_source: if context.user_session_state.is_some() {
                Some("db".to_string())
            } else {
                None
            },
            persistent_state_updated: false,
            persistent_state_slot_count: context
                .user_session_state
                .as_ref()
                .map(|s| s.populated_slot_count())
                .unwrap_or(0),
            persistent_state_preview: context
                .user_session_state
                .as_ref()
                .map(|s| {
                    let mut parts = Vec::new();
                    if let Some(g) = &s.goal {
                        parts.push(format!("goal={}", summarize_for_markdown(g, 40)));
                    }
                    if let Some(st) = &s.current_subtask {
                        parts.push(format!("subtask={}", summarize_for_markdown(st, 40)));
                    }
                    if s.next_step.is_some() {
                        parts.push("has_next_step".to_string());
                    }
                    parts.join(", ")
                })
                .filter(|p| !p.is_empty()),
            context_pack_present: false,
            context_pack_section_count: 0,
            context_pack_total_chars: 0,
            context_pack_drop_reasons: Vec::new(),
            user_session_state: context.user_session_state,
        }
    }

    pub(crate) fn record_decision(&mut self, step: usize, source: &str, planned: &PlannedDecision) {
        if let Some(plan) = &planned.plan {
            let scope = self
                .pending_replan_scope
                .take()
                .unwrap_or(ReplanScope::Full);
            self.apply_plan_update(&plan.steps, scope);
        }
        if let Some(progress_note) = &planned.progress_note {
            self.last_progress_note = Some(progress_note.clone());
        }
        self.decisions.push(DecisionTrace {
            step,
            current_step_index: self.current_step_index,
            source: source.to_string(),
            decision_type: planned.decision.kind().to_string(),
            summary: planned.summary(),
            plan_steps: planned
                .plan
                .as_ref()
                .map(|plan| plan.steps.clone())
                .unwrap_or_default(),
            progress_note: planned.progress_note.clone(),
            expected_observation: planned
                .expected_observation
                .as_ref()
                .map(ExpectedObservation::summary),
        });
    }

    pub(crate) fn mark_next_plan_step_running(
        &mut self,
        expected_observation: Option<ExpectedObservation>,
    ) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Pending)
        {
            step.status = PlanStepStatus::Running;
            step.expected_observation = expected_observation;
        }
        self.sync_current_step_index();
    }

    pub(crate) fn mark_running_plan_step_done(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Done;
        }
        self.sync_current_step_index();
    }

    pub(crate) fn mark_running_plan_step_failed(&mut self) {
        if let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        {
            step.status = PlanStepStatus::Failed;
        }
        self.sync_current_step_index();
    }

    pub(crate) fn mark_running_plan_step_retrying(&mut self) -> usize {
        let Some(step) = self
            .active_plan_steps
            .iter_mut()
            .find(|step| step.status == PlanStepStatus::Running)
        else {
            return 0;
        };
        step.retry_count += 1;
        let retry_count = step.retry_count;
        self.sync_current_step_index();
        retry_count
    }

    pub(crate) fn mark_remaining_plan_steps_skipped(&mut self) {
        for step in &mut self.active_plan_steps {
            if step.status == PlanStepStatus::Pending {
                step.status = PlanStepStatus::Skipped;
            }
        }
        self.sync_current_step_index();
    }

    pub(crate) fn has_incomplete_plan_steps(&self) -> bool {
        self.active_plan_steps.iter().any(|step| {
            matches!(
                step.status,
                PlanStepStatus::Pending | PlanStepStatus::Running
            )
        })
    }

    pub(crate) fn running_plan_expected_observation(&self) -> Option<&ExpectedObservation> {
        self.active_plan_steps
            .iter()
            .find(|step| step.status == PlanStepStatus::Running)
            .and_then(|step| step.expected_observation.as_ref())
    }

    pub(crate) fn record_llm_fallback(&mut self, reason: &str) {
        self.llm_fallback_reason = Some(reason.to_string());
    }

    pub(crate) fn record_rule_parse_error(&mut self, error: &str) {
        self.rule_parse_error = Some(error.to_string());
    }

    pub(crate) fn record_session_state_snapshot(&mut self, snapshot: RuntimeSessionStateSnapshot) {
        self.session_state_snapshot = Some(snapshot);
    }

    pub(crate) fn record_final_runtime_session_state(
        &mut self,
        state: &RuntimeSessionStateSnapshot,
    ) {
        self.final_runtime_session_state = Some(state.clone());
    }

    pub(crate) fn record_observation(&mut self, observation: &AgentObservation) {
        self.observations.push(ObservationTrace {
            step: observation.step,
            source: observation.source.clone(),
            summary: truncate_for_trace(&observation.content, 240),
            content_chars: observation.content.chars().count(),
        });
    }

    pub(crate) fn record_failure(&mut self, step: usize, failure: &FailureDecision) {
        if failure.action == FailureAction::Replan {
            self.pending_replan_scope = failure.replan_scope.clone();
        }
        self.controller_state.record_failure();
        self.failures.push(FailureTrace {
            step,
            current_step_index: self.current_step_index,
            kind: failure.kind,
            action: failure.action,
            replan_scope: failure.replan_scope.clone(),
            source: failure.source.clone(),
            detail: failure.detail.clone(),
            user_message: failure.user_message.clone(),
        });
    }

    pub(crate) fn record_recovery_attempt(
        &mut self,
        step: usize,
        failure: &FailureDecision,
        outcome: RecoveryOutcome,
        escalated: bool,
        effective_action: FailureAction,
    ) {
        self.recovery_action = Some(effective_action);
        self.recovery_result = Some(outcome);
        self.recovery_attempts.push(RecoveryTrace {
            step,
            current_step_index: self.current_step_index,
            failure_kind: failure.kind,
            original_action: failure.action,
            effective_action,
            action: effective_action,
            successful: outcome == RecoveryOutcome::Continued,
            outcome,
            escalated,
            source: failure.source.clone(),
            detail: failure.detail.clone(),
        });
    }

    pub(crate) fn apply_plan_update(&mut self, steps: &[String], scope: ReplanScope) {
        let new_steps = steps
            .iter()
            .map(|description| RuntimePlanStep {
                description: description.clone(),
                status: PlanStepStatus::Pending,
                expected_observation: None,
                retry_count: 0,
            })
            .collect::<Vec<_>>();

        match scope {
            ReplanScope::Full => {
                self.active_plan_steps = new_steps;
            }
            ReplanScope::RemainingPlan => {
                let mut preserved = self
                    .active_plan_steps
                    .iter()
                    .filter(|step| step.status == PlanStepStatus::Done)
                    .cloned()
                    .collect::<Vec<_>>();
                preserved.extend(new_steps);
                self.active_plan_steps = preserved;
            }
            ReplanScope::CurrentStep => {
                let split_index = self
                    .active_plan_steps
                    .iter()
                    .position(|step| step.status != PlanStepStatus::Done)
                    .unwrap_or(self.active_plan_steps.len());
                let mut rebuilt = self.active_plan_steps[..split_index].to_vec();
                rebuilt.extend(new_steps);
                if split_index < self.active_plan_steps.len() {
                    let preserved_tail = self.active_plan_steps[(split_index + 1)..]
                        .iter()
                        .map(|step| RuntimePlanStep {
                            description: step.description.clone(),
                            status: PlanStepStatus::Pending,
                            expected_observation: None,
                            retry_count: 0,
                        })
                        .collect::<Vec<_>>();
                    rebuilt.extend(preserved_tail);
                }
                self.active_plan_steps = rebuilt;
            }
        }
        self.sync_current_step_index();
    }

    pub(crate) fn sync_current_step_index(&mut self) {
        self.current_step_index = self
            .active_plan_steps
            .iter()
            .position(|step| {
                matches!(
                    step.status,
                    PlanStepStatus::Pending | PlanStepStatus::Running | PlanStepStatus::Failed
                )
            })
            .map(|index| index + 1);
    }

    pub(super) fn consecutive_failures_for_current_step(
        &self,
        current_step_index: usize,
    ) -> Vec<&FailureTrace> {
        let mut failures = Vec::new();
        for failure in self.failures.iter().rev() {
            if failure.current_step_index != Some(current_step_index) {
                break;
            }
            failures.push(failure);
        }
        failures
    }

    pub(crate) fn configure_controller_limits(&mut self, max_steps: usize, max_replans: usize) {
        self.controller_state
            .configure_limits(max_steps, max_replans);
    }

    pub(crate) fn start_llm_call(
        &mut self,
        config: &LlmConfig,
        system_prompt: &str,
        planner_input: &PlannerInput,
        body: &serde_json::Value,
    ) -> usize {
        let request_body = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_string());
        self.llm_calls.push(LlmCallTrace {
            source: config.source.to_string(),
            model: config.model.clone(),
            base_url: config.base_url.clone(),
            prompt: PromptSnapshot {
                system_prompt: system_prompt.to_string(),
                raw_user_input: planner_input.raw_user_input.clone(),
                user_prompt: planner_input.assembled_user_prompt.clone(),
                context_sections: planner_input.context_sections.clone(),
                context_budget_summary: planner_input.context_budget_summary.clone(),
                context_summary: planner_input.context_summary.clone(),
                system_prompt_chars: system_prompt.chars().count(),
                raw_user_input_chars: planner_input.raw_user_input.chars().count(),
                user_prompt_chars: planner_input.assembled_user_prompt.chars().count(),
                context_summary_chars: planner_input.context_summary.chars().count(),
                request_body_chars: request_body.chars().count(),
                estimated_prompt_chars: system_prompt.chars().count()
                    + planner_input.assembled_user_prompt.chars().count()
                    + request_body.chars().count(),
                request_body,
            },
            raw_response: None,
            raw_response_chars: None,
            message_content: None,
            message_content_chars: None,
            response_status: None,
            attempts: 0,
            success: false,
            error: None,
            decision_summary: None,
        });
        self.llm_calls.len() - 1
    }

    pub(crate) fn finish_llm_call_success(
        &mut self,
        index: usize,
        attempts: usize,
        raw_response: &str,
        message_content: &str,
        status: u16,
        planned: &PlannedDecision,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.raw_response = Some(raw_response.to_string());
            call.raw_response_chars = Some(raw_response.chars().count());
            call.message_content = Some(message_content.to_string());
            call.message_content_chars = Some(message_content.chars().count());
            call.response_status = Some(status);
            call.attempts = attempts;
            call.success = true;
            call.decision_summary = Some(planned.summary());
        }
    }

    pub(crate) fn finish_llm_call_error(
        &mut self,
        index: usize,
        attempts: usize,
        status: Option<u16>,
        error: &str,
    ) {
        if let Some(call) = self.llm_calls.get_mut(index) {
            call.attempts = attempts;
            call.response_status = status;
            call.error = Some(error.to_string());
        }
    }

    pub(crate) fn start_tool_call(&mut self, step: usize, action: &ToolAction) -> usize {
        self.tool_calls.push(ToolCallTrace {
            step,
            tool_name: action.name().to_string(),
            path: action.path().map(ToOwned::to_owned),
            content_chars: action.content().map(|v| v.chars().count()),
            output: None,
            output_chars: None,
            success: false,
            error: None,
            duration_ms: None,
            started_at: Some(Instant::now()),
        });
        self.tool_calls.len() - 1
    }

    pub(crate) fn finish_tool_call_success(&mut self, index: usize, tool_name: &str, output: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.tool_name = tool_name.to_string();
            call.output = Some(output.to_string());
            call.output_chars = Some(output.chars().count());
            call.success = true;
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
        self.mark_running_plan_step_done();
    }

    pub(crate) fn finish_tool_call_error(&mut self, index: usize, error: &str) {
        if let Some(call) = self.tool_calls.get_mut(index) {
            call.error = Some(error.to_string());
            call.duration_ms = call.started_at.map(|v| v.elapsed().as_millis());
            call.started_at = None;
        }
        self.mark_running_plan_step_failed();
    }

    pub(crate) fn finish_success(&mut self, output: &str, duration: Duration) {
        self.success = true;
        self.final_output = Some(output.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
        self.mark_remaining_plan_steps_skipped();
    }

    pub(crate) fn finish_error(&mut self, error: &str, duration: Duration) {
        self.success = false;
        self.error = Some(error.to_string());
        self.finished_at = Some(Utc::now().to_rfc3339());
        self.duration_ms = Some(duration.as_millis());
        for call in &mut self.llm_calls {
            if !call.success && call.error.is_none() {
                call.error = Some(error.to_string());
            }
        }
    }
}

impl AgentRunTrace {
    pub(super) fn persist(&self) -> Result<PathBuf> {
        let day = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y-%m-%d")
            .to_string();
        let dir = self.trace_dir_root.join(&day);
        fs::create_dir_all(&dir)
            .with_context(|| format!("创建 agent trace 目录失败: {}", dir.display()))?;
        let timestamp = Utc::now()
            .with_timezone(&Shanghai)
            .format("%Y%m%dT%H%M%S")
            .to_string();
        let json_path = dir.join(format!("run_{}_{}.json", timestamp, self.run_id));
        let json_content = serde_json::to_string_pretty(self).context("序列化 agent trace 失败")?;
        fs::write(&json_path, format!("{json_content}\n"))
            .with_context(|| format!("写入 agent trace 失败: {}", json_path.display()))?;

        let markdown_path = dir.join(format!("run_{}_{}.md", timestamp, self.run_id));
        let markdown_content = self.to_markdown();
        fs::write(&markdown_path, markdown_content).with_context(|| {
            format!(
                "写入 agent trace markdown 失败: {}",
                markdown_path.display()
            )
        })?;

        let index_entry = AgentTraceIndexEntry {
            trace_version: self.trace_version.to_string(),
            run_id: self.run_id.clone(),
            started_at: self.started_at.clone(),
            finished_at: self.finished_at.clone(),
            duration_ms: self.duration_ms,
            success: self.success,
            user_input: summarize_for_markdown(&self.user_input, 240),
            user_input_chars: self.user_input_chars,
            source_type: self.source_type.clone(),
            trigger_type: self.trigger_type.clone(),
            user_id: self.user_id.clone(),
            message_ids: self.message_ids.clone(),
            message_count: self.message_count,
            task_id: self.task_id.clone(),
            article_id: self.article_id.clone(),
            session_text_chars: self.session_text_chars,
            context_token_present: self.context_token_present,
            step_count: self.step_count,
            llm_call_count: self.llm_calls.len(),
            tool_call_count: self.tool_calls.len(),
            observation_count: self.observations.len(),
            final_output_chars: self.final_output.as_ref().map(|v| v.chars().count()),
            error: self.error.clone().map(|v| summarize_for_markdown(&v, 240)),
            llm_fallback_reason: self
                .llm_fallback_reason
                .clone()
                .map(|v| summarize_for_markdown(&v, 240)),
            memory_hit_count: self.memory_hit_count,
            memory_injected_count: self.memory_hit_count,
            memory_retrieved_count: self.memory_retrieved_count,
            memory_total_chars: self.memory_total_chars,
            memory_injected_total_chars: self.memory_total_chars,
            memory_dropped_count: self.memory_dropped_count,
            recovery_attempt_count: self.recovery_attempts.len(),
            recovery_success_count: self
                .recovery_attempts
                .iter()
                .filter(|attempt| attempt.successful)
                .count(),
            recovery_action: self
                .recovery_action
                .as_ref()
                .map(|action| action.as_str().to_string()),
            recovery_result: self
                .recovery_result
                .as_ref()
                .map(|result| result.as_str().to_string()),
            retriever_name: self.retriever_name.clone(),
            retrieval_candidate_count: self.retrieval_candidate_count,
            retrieval_hit_count: self.retrieval_hit_count,
            retrieval_latency_ms: self.retrieval_latency_ms,
            retrieval_mode: self.retrieval_mode.clone(),
            retrieval_fallback_reason: self.retrieval_fallback_reason.clone(),
            retrieval_scores_present: self.retrieval_scores_present,
            context_pack_present: self.context_pack_present,
            context_pack_section_count: self.context_pack_section_count,
            context_pack_total_chars: self.context_pack_total_chars,
            json_file: json_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_string(),
            markdown_file: markdown_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or_default()
                .to_string(),
        };
        let index_path = dir.join("index.jsonl");
        let index_line =
            serde_json::to_string(&index_entry).context("序列化 agent trace index 失败")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_path)
            .with_context(|| format!("打开 agent trace index 失败: {}", index_path.display()))?;
        let mut line_with_nl = index_line;
        line_with_nl.push('\n');
        file.write_all(line_with_nl.as_bytes())
            .with_context(|| format!("写入 agent trace index 失败: {}", index_path.display()))?;
        self.write_daily_index_markdown(&dir, &day)?;
        Ok(json_path)
    }

    fn write_daily_index_markdown(&self, dir: &std::path::Path, day: &str) -> Result<()> {
        let index_path = dir.join("index.jsonl");
        let index_content = fs::read_to_string(&index_path)
            .with_context(|| format!("读取 agent trace index 失败: {}", index_path.display()))?;
        let mut entries = Vec::new();
        for (idx, line) in index_content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AgentTraceIndexEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    log_agent_warn(
                        "agent_trace_index_line_parse_failed",
                        vec![
                            ("line_no", json!(idx + 1)),
                            ("detail", json!(err.to_string())),
                            ("index_path", json!(index_path.display().to_string())),
                        ],
                    );
                }
            }
        }

        let markdown_path = dir.join("index.md");
        let markdown = render_daily_index_markdown(day, &entries);
        fs::write(&markdown_path, markdown).with_context(|| {
            format!(
                "写入 agent trace index markdown 失败: {}",
                markdown_path.display()
            )
        })?;
        Ok(())
    }

    pub(super) fn to_markdown(&self) -> String {
        let mut lines = vec![
            format!("# Agent Trace {}", self.run_id),
            String::new(),
            "## Summary".to_string(),
            String::new(),
            format!("- success: {}", self.success),
            format!("- started_at: {}", self.started_at),
            format!(
                "- finished_at: {}",
                self.finished_at.as_deref().unwrap_or("(running)")
            ),
            format!("- duration_ms: {}", self.duration_ms.unwrap_or(0)),
            format!("- step_count: {}", self.step_count),
            format!("- user_input_chars: {}", self.user_input_chars),
            format!("- source_type: {}", self.source_type),
            format!(
                "- trigger_type: {}",
                self.trigger_type.as_deref().unwrap_or("(none)")
            ),
            format!("- user_id: {}", self.user_id.as_deref().unwrap_or("(none)")),
            format!("- message_count: {}", self.message_count),
            format!("- task_id: {}", self.task_id.as_deref().unwrap_or("(none)")),
            format!(
                "- article_id: {}",
                self.article_id.as_deref().unwrap_or("(none)")
            ),
            format!("- session_text_chars: {}", self.session_text_chars),
            format!("- context_token_present: {}", self.context_token_present),
            format!(
                "- replan_budget: {}/{}",
                self.controller_state.replan_count, self.controller_state.max_replans
            ),
            format!("- failure_count: {}", self.controller_state.failure_count),
            format!("- ask_user_count: {}", self.controller_state.ask_user_count),
            format!("- recovery_attempt_count: {}", self.recovery_attempts.len()),
            format!(
                "- recovery_success_count: {}",
                self.recovery_attempts
                    .iter()
                    .filter(|attempt| attempt.successful)
                    .count()
            ),
            format!(
                "- recovery_action(last): {}",
                self.recovery_action
                    .as_ref()
                    .map(FailureAction::as_str)
                    .unwrap_or("(none)")
            ),
            format!(
                "- recovery_result(last): {}",
                self.recovery_result
                    .as_ref()
                    .map(RecoveryOutcome::as_str)
                    .unwrap_or("(none)")
            ),
            format!("- memory_hit_count: {} (injected)", self.memory_hit_count),
            format!("- memory_retrieved_count: {}", self.memory_retrieved_count),
            format!("- memory_dropped_count: {}", self.memory_dropped_count),
            format!(
                "- memory_total_chars: {} (injected)",
                self.memory_total_chars
            ),
            format!(
                "- retriever: {} (latency={}ms, candidates={}, hits={})",
                if self.retriever_name.is_empty() {
                    "(none)"
                } else {
                    &self.retriever_name
                },
                self.retrieval_latency_ms,
                self.retrieval_candidate_count,
                self.retrieval_hit_count,
            ),
            format!(
                "- retrieval_mode: {}{}",
                if self.retrieval_mode.is_empty() {
                    "(none)"
                } else {
                    &self.retrieval_mode
                },
                if let Some(ref reason) = self.retrieval_fallback_reason {
                    format!(" [fallback: {}]", reason)
                } else {
                    String::new()
                }
            ),
            format!(
                "- retrieval_scores_present: {}",
                self.retrieval_scores_present
            ),
            String::new(),
            "## User Input".to_string(),
            String::new(),
            "```text".to_string(),
            self.user_input.clone(),
            "```".to_string(),
        ];

        lines.push(String::new());
        lines.push("## Upstream Messages".to_string());
        lines.push(String::new());
        if self.message_ids.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for message_id in &self.message_ids {
                lines.push(format!("- {}", message_id));
            }
        }

        if let Some(session_text) = &self.session_text {
            lines.push(String::new());
            lines.push("## Session Text".to_string());
            lines.push(String::new());
            lines.push("```text".to_string());
            lines.push(summarize_for_markdown(session_text, 1200));
            lines.push("```".to_string());
        }

        if let Some(session_state_snapshot) = &self.session_state_snapshot {
            append_session_state_lines(&mut lines, session_state_snapshot);
        }

        if let Some(reason) = &self.llm_fallback_reason {
            lines.push(String::new());
            lines.push("## LLM Fallback".to_string());
            lines.push(String::new());
            lines.push(format!("- reason: {}", reason));
        }

        if let Some(error) = &self.rule_parse_error {
            lines.push(String::new());
            lines.push("## Rule Parse Error".to_string());
            lines.push(String::new());
            lines.push(format!("- error: {}", error));
        }

        lines.push(String::new());
        lines.push("## Decisions".to_string());
        lines.push(String::new());
        if self.decisions.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for decision in &self.decisions {
                lines.push(format!(
                    "- step={} current_step={} source={} type={} summary={}",
                    decision.step,
                    decision
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    decision.source,
                    decision.decision_type,
                    decision.summary
                ));
                if !decision.plan_steps.is_empty() {
                    for (idx, step_item) in decision.plan_steps.iter().enumerate() {
                        lines.push(format!("  - plan_step_{}: {}", idx + 1, step_item));
                    }
                }
                if let Some(progress_note) = &decision.progress_note {
                    lines.push(format!("  - progress_note: {}", progress_note));
                }
                if let Some(expected) = &decision.expected_observation {
                    lines.push(format!("  - expected_observation: {}", expected));
                }
            }
        }

        if !self.active_plan_steps.is_empty() {
            lines.push(String::new());
            lines.push("## Active Plan".to_string());
            lines.push(String::new());
            lines.push(format!(
                "- current_step_index: {}",
                self.current_step_index
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "(none)".to_string())
            ));
            for (idx, step) in self.active_plan_steps.iter().enumerate() {
                let mut line = format!(
                    "{}. [{}] {} (retry_count={})",
                    idx + 1,
                    step.status.as_str(),
                    step.description,
                    step.retry_count
                );
                if let Some(expected) = &step.expected_observation {
                    line.push_str(&format!(" | expect: {}", expected.summary()));
                }
                lines.push(line);
            }
            if let Some(progress_note) = &self.last_progress_note {
                lines.push(String::new());
                lines.push(format!("- progress_note: {}", progress_note));
            }
            lines.push(String::new());
        }

        lines.push(String::new());
        lines.push("## LLM Calls".to_string());
        lines.push(String::new());
        if self.llm_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.llm_calls.iter().enumerate() {
                lines.push(format!("### LLM Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- source: {}", call.source));
                lines.push(format!("- model: {}", call.model));
                lines.push(format!("- base_url: {}", call.base_url));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!("- attempts: {}", call.attempts));
                lines.push(format!(
                    "- response_status: {}",
                    call.response_status
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- estimated_prompt_chars: {}",
                    call.prompt.estimated_prompt_chars
                ));
                lines.push(format!(
                    "- system_prompt_chars: {}",
                    call.prompt.system_prompt_chars
                ));
                lines.push(format!(
                    "- raw_user_input_chars: {}",
                    call.prompt.raw_user_input_chars
                ));
                lines.push(format!(
                    "- user_prompt_chars: {}",
                    call.prompt.user_prompt_chars
                ));
                lines.push(format!(
                    "- context_summary_chars: {}",
                    call.prompt.context_summary_chars
                ));
                lines.push(format!(
                    "- context_max_chars: {}",
                    call.prompt.context_budget_summary.max_total_chars
                ));
                lines.push(format!(
                    "- context_final_chars: {}",
                    call.prompt.context_budget_summary.final_total_chars
                ));
                lines.push(format!(
                    "- trimmed_section_count: {}",
                    call.prompt.context_budget_summary.trimmed_section_count
                ));
                lines.push(format!(
                    "- dropped_section_count: {}",
                    call.prompt.context_budget_summary.dropped_section_count
                ));
                lines.push(format!(
                    "- context_section_count: {}",
                    call.prompt.context_sections.len()
                ));
                lines.push(format!(
                    "- request_body_chars: {}",
                    call.prompt.request_body_chars
                ));
                lines.push(format!(
                    "- raw_response_chars: {}",
                    call.raw_response_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- message_content_chars: {}",
                    call.message_content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(summary) = &call.decision_summary {
                    lines.push(format!("- decision_summary: {}", summary));
                }
                lines.push(String::new());
                lines.push("#### Raw User Input".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.raw_user_input, 800));
                lines.push("```".to_string());
                lines.push(String::new());
                lines.push("#### User Prompt".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.user_prompt, 800));
                lines.push("```".to_string());
                lines.push(String::new());
                lines.push("#### Context Summary".to_string());
                lines.push(String::new());
                lines.push("```text".to_string());
                lines.push(summarize_for_markdown(&call.prompt.context_summary, 800));
                lines.push("```".to_string());
                lines.push(String::new());
                lines.push("#### Context Sections".to_string());
                lines.push(String::new());
                if call.prompt.context_sections.is_empty() {
                    lines.push("- (none)".to_string());
                } else {
                    for section in &call.prompt.context_sections {
                        lines.push(format!(
                        "- kind={} priority={} included={} trimmed={} lines={} items={} chars={}/{}",
                        section.kind,
                        section.priority,
                        section.included,
                        section.trimmed,
                        section.line_count,
                        section.item_count,
                        section.char_count,
                        section.original_char_count
                    ));
                        if let Some(reason) = &section.trim_reason {
                            lines.push(format!("  - trim_reason: {}", reason.as_str()));
                        }
                        if let Some(reason) = &section.drop_reason {
                            lines.push(format!("  - drop_reason: {}", reason.as_str()));
                        }
                        lines.push("```text".to_string());
                        lines.push(summarize_for_markdown(&section.content, 400));
                        lines.push("```".to_string());
                    }
                }
                if let Some(content) = &call.message_content {
                    lines.push(String::new());
                    lines.push("#### Message Content Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(content, 1000));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Observations".to_string());
        lines.push(String::new());
        if self.observations.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for observation in &self.observations {
                lines.push(format!(
                    "- step={} source={} content_chars={} summary={}",
                    observation.step,
                    observation.source,
                    observation.content_chars,
                    observation.summary
                ));
            }
        }

        lines.push(String::new());
        lines.push("## Failures".to_string());
        lines.push(String::new());
        if self.failures.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for failure in &self.failures {
                lines.push(format!(
                    "- step={} current_step={} kind={} action={} scope={} source={} detail={}",
                    failure.step,
                    failure
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    failure.kind.as_str(),
                    failure.action.as_str(),
                    failure
                        .replan_scope
                        .as_ref()
                        .map(ReplanScope::as_str)
                        .unwrap_or("(none)"),
                    failure.source,
                    failure.detail
                ));
                if let Some(user_message) = &failure.user_message {
                    lines.push(format!("  - user_message: {}", user_message));
                }
            }
        }

        lines.push(String::new());
        lines.push("## Recovery Attempts".to_string());
        lines.push(String::new());
        if self.recovery_attempts.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for attempt in &self.recovery_attempts {
                lines.push(format!(
                    "- step={} current_step={} kind={} action={} outcome={} successful={} source={} detail={}",
                    attempt.step,
                    attempt
                        .current_step_index
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "(none)".to_string()),
                    attempt.failure_kind.as_str(),
                    attempt.action.as_str(),
                    attempt.outcome.as_str(),
                    attempt.successful,
                    attempt.source,
                    attempt.detail
                ));
            }
        }

        lines.push(String::new());
        lines.push("## Tool Calls".to_string());
        lines.push(String::new());
        if self.tool_calls.is_empty() {
            lines.push("- (none)".to_string());
        } else {
            for (idx, call) in self.tool_calls.iter().enumerate() {
                lines.push(format!("### Tool Call {}", idx + 1));
                lines.push(String::new());
                lines.push(format!("- step: {}", call.step));
                lines.push(format!("- tool_name: {}", call.tool_name));
                lines.push(format!(
                    "- path: {}",
                    call.path.as_deref().unwrap_or("(none)")
                ));
                lines.push(format!(
                    "- content_chars: {}",
                    call.content_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!(
                    "- output_chars: {}",
                    call.output_chars
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                lines.push(format!("- success: {}", call.success));
                lines.push(format!(
                    "- duration_ms: {}",
                    call.duration_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "(none)".to_string())
                ));
                if let Some(error) = &call.error {
                    lines.push(format!("- error: {}", error));
                }
                if let Some(output) = &call.output {
                    lines.push(String::new());
                    lines.push("#### Output Summary".to_string());
                    lines.push(String::new());
                    lines.push("```text".to_string());
                    lines.push(summarize_for_markdown(output, 1200));
                    lines.push("```".to_string());
                }
                lines.push(String::new());
            }
        }

        lines.push("## Final Output".to_string());
        lines.push(String::new());
        lines.push("```text".to_string());
        lines.push(summarize_for_markdown(
            self.final_output
                .as_deref()
                .unwrap_or_else(|| self.error.as_deref().unwrap_or("(none)")),
            1200,
        ));
        lines.push("```".to_string());
        lines.push(String::new());

        lines.join("\n")
    }
}

pub(super) fn truncate_for_trace(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_string();
    }
    let mut text: String = input.chars().take(max_chars).collect();
    text.push_str("...");
    text
}

fn render_daily_index_markdown(day: &str, entries: &[AgentTraceIndexEntry]) -> String {
    let total_runs = entries.len();
    let success_runs = entries.iter().filter(|entry| entry.success).count();
    let failed_runs = total_runs.saturating_sub(success_runs);
    let source_summary = summarize_sources(entries);

    let mut lines = vec![
        format!("# Agent Trace Daily Index {}", day),
        String::new(),
        format!("- total_runs: {}", total_runs),
        format!("- success_runs: {}", success_runs),
        format!("- failed_runs: {}", failed_runs),
        format!("- source_types: {}", source_summary),
        String::new(),
        "## Runs".to_string(),
        String::new(),
    ];

    if entries.is_empty() {
        lines.push("- (none)".to_string());
        lines.push(String::new());
        return lines.join("\n");
    }

    lines.push(
        "| Time | Status | Run | Source | Trigger | User | Msgs | Input | Files |".to_string(),
    );
    lines.push("| --- | --- | --- | --- | --- | --- | ---: | --- | --- |".to_string());

    let mut ordered = entries.to_vec();
    ordered.sort_by(|left, right| right.started_at.cmp(&left.started_at));

    for entry in ordered {
        let status = if entry.success { "ok" } else { "error" };
        let files = format!(
            "[json]({}) / [md]({})",
            entry.json_file, entry.markdown_file
        );
        lines.push(format!(
            "| {} | {} | `{}` | {} | {} | {} | {} | {} | {} |",
            sanitize_markdown_cell(&display_trace_time(&entry.started_at)),
            status,
            short_run_id(&entry.run_id),
            sanitize_markdown_cell(&entry.source_type),
            sanitize_markdown_cell(entry.trigger_type.as_deref().unwrap_or("(none)")),
            sanitize_markdown_cell(entry.user_id.as_deref().unwrap_or("(none)")),
            entry.message_count,
            sanitize_markdown_cell(&entry.user_input),
            files
        ));

        if !entry.success {
            let detail = entry
                .error
                .as_deref()
                .or(entry.llm_fallback_reason.as_deref())
                .unwrap_or("(none)");
            lines.push(format!(
                "| ↳ detail | {} |  |  |  |  |  | {} |  |",
                sanitize_markdown_cell(detail),
                sanitize_markdown_cell(&format!(
                    "steps={} llm={} tools={}",
                    entry.step_count, entry.llm_call_count, entry.tool_call_count
                ))
            ));
        }
    }

    lines.push(String::new());
    lines.join("\n")
}

fn summarize_sources(entries: &[AgentTraceIndexEntry]) -> String {
    if entries.is_empty() {
        return "(none)".to_string();
    }

    let mut counts = BTreeMap::new();
    for entry in entries {
        *counts.entry(entry.source_type.as_str()).or_insert(0usize) += 1;
    }

    counts
        .into_iter()
        .map(|(source, count)| format!("{source}({count})"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_trace_time(started_at: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(started_at)
        .map(|value| {
            value
                .with_timezone(&Shanghai)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|_| started_at.to_string())
}

fn short_run_id(run_id: &str) -> String {
    let short: String = run_id.chars().take(8).collect();
    if short.is_empty() {
        "(none)".to_string()
    } else {
        short
    }
}

fn sanitize_markdown_cell(input: &str) -> String {
    summarize_for_markdown(input, 80)
        .replace('\n', " ")
        .replace('|', "\\|")
}
