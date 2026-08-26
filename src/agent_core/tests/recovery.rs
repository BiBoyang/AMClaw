use super::super::{
    classify_tool_execution_failure, AgentCore, AgentRunContext, AgentRunTrace, FailureAction,
    FailureDecision, RecoveryOutcome, ReplanScope, StepFailureKind,
};
use super::temp_workspace;
#[test]
fn transient_failure_is_classified_as_retry_step() {
    let failure = classify_tool_execution_failure("tool:read".to_string(), "operation timed out");
    assert_eq!(failure.kind, StepFailureKind::Transient);
    assert_eq!(failure.action, FailureAction::RetryStep);
}

#[test]
fn transient_failure_retry_then_replan() {
    let workspace = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        workspace.clone(),
        3,
        vec![super::super::AgentDecision::Final("noop".to_string())],
    )
    .expect("初始化 agent 失败");
    let mut trace = AgentRunTrace::new(&workspace, "retry", AgentRunContext::agent_demo());
    trace.configure_controller_limits(3, 3);

    // 第一次 Transient -> RetryStep（原始 action）
    let first = agent
        .handle_recorded_failure(
            1,
            FailureDecision {
                kind: StepFailureKind::Transient,
                action: FailureAction::RetryStep,
                replan_scope: None,
                detail: "timeout".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        )
        .expect_err("第一次应为 RetryStep，返回失败");
    assert!(first.to_string().contains("timeout"));
    let first_attempt = trace.recovery_attempts.last().unwrap();
    assert_eq!(first_attempt.original_action, FailureAction::RetryStep);
    assert_eq!(first_attempt.effective_action, FailureAction::RetryStep);
    assert_eq!(first_attempt.action, FailureAction::RetryStep);
    assert!(!first_attempt.escalated);

    // 第二次 Transient -> 因防循环升级为 Replan
    let second = agent
        .handle_recorded_failure(
            2,
            FailureDecision {
                kind: StepFailureKind::Transient,
                action: FailureAction::RetryStep,
                replan_scope: None,
                detail: "timeout again".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        )
        .expect("升级后应成功 Replan");
    assert!(matches!(second, super::super::LoopControl::Continue(_)));
    let last = trace.recovery_attempts.last().unwrap();
    assert!(last.escalated, "第二次应标记为 escalated");
    assert_eq!(last.original_action, FailureAction::RetryStep);
    assert_eq!(last.effective_action, FailureAction::Replan);
    assert_eq!(last.action, FailureAction::Replan);
    assert_eq!(last.outcome, RecoveryOutcome::Continued);
}

#[test]
fn low_value_observation_replan_then_ask_user() {
    let workspace = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        workspace.clone(),
        3,
        vec![super::super::AgentDecision::Final("noop".to_string())],
    )
    .expect("初始化 agent 失败");
    let mut trace = AgentRunTrace::new(&workspace, "lvo", AgentRunContext::agent_demo());
    trace.configure_controller_limits(3, 3);

    // 第一次 LowValueObservation -> Replan
    let first = agent
        .handle_recorded_failure(
            1,
            FailureDecision {
                kind: StepFailureKind::LowValueObservation,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::RemainingPlan),
                detail: "no new info".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        )
        .expect("第一次 Replan 应成功");
    assert!(matches!(first, super::super::LoopControl::Continue(_)));
    assert!(!trace.recovery_attempts.last().unwrap().escalated);

    // 第二次 LowValueObservation -> 升级 AskUser
    let second = agent
        .handle_recorded_failure(
            2,
            FailureDecision {
                kind: StepFailureKind::LowValueObservation,
                action: FailureAction::Replan,
                replan_scope: Some(ReplanScope::RemainingPlan),
                detail: "still no new info".to_string(),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        )
        .expect("升级后应 ask_user");
    assert!(matches!(second, super::super::LoopControl::Finish(_)));
    let last = trace.recovery_attempts.last().unwrap();
    assert!(last.escalated, "第二次应标记为 escalated");
    assert_eq!(last.outcome, RecoveryOutcome::EscalatedToAskUser);
}

#[test]
fn recovery_loop_guard_prevents_infinite_escalation() {
    let workspace = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        workspace.clone(),
        3,
        vec![super::super::AgentDecision::Final("noop".to_string())],
    )
    .expect("初始化 agent 失败");
    let mut trace = AgentRunTrace::new(&workspace, "loop", AgentRunContext::agent_demo());
    trace.configure_controller_limits(3, 3);

    // 连续触发同一 kind 多次，确保不会无限循环
    for i in 1..=4 {
        let _ = agent.handle_recorded_failure(
            i,
            FailureDecision {
                kind: StepFailureKind::Transient,
                action: FailureAction::RetryStep,
                replan_scope: None,
                detail: format!("attempt {}", i),
                source: "test".to_string(),
                user_message: None,
            },
            &mut trace,
        );
    }

    // Transient 的 max_attempts = 1，所以第 2 次就升级为 Replan
    // 第 3、4 次仍然是 Replan（因为 Replan 的 max_attempts = 1，但 Replan 成功执行，
    // 下一次再遇到 Transient 已经是新 kind 计数... 不，是同 kind 计数继续累加）
    // 实际上第 3 次时 kind_count=3 > max_attempts(1)，仍然升级，但 escalate_to 也是 Replan
    // 第 4 次同理
    let transient_attempts: Vec<_> = trace
        .recovery_attempts
        .iter()
        .filter(|a| a.failure_kind == StepFailureKind::Transient)
        .collect();
    assert_eq!(transient_attempts.len(), 4);
    // 第一次未升级
    assert!(!transient_attempts[0].escalated);
    // 第 2~4 次都被升级（因为 kind_count 一直 > 1）
    assert!(transient_attempts[1].escalated);
    assert!(transient_attempts[2].escalated);
    assert!(transient_attempts[3].escalated);
}

#[test]
fn trace_records_recovery_attempt_action_outcome() {
    let workspace = temp_workspace();
    let agent = AgentCore::with_scripted_decisions(
        workspace.clone(),
        3,
        vec![super::super::AgentDecision::Final("noop".to_string())],
    )
    .expect("初始化 agent 失败");
    let mut trace = AgentRunTrace::new(&workspace, "trace", AgentRunContext::agent_demo());
    trace.configure_controller_limits(3, 3);

    // 记录一次 recovery
    let _ = agent.handle_recorded_failure(
        1,
        FailureDecision {
            kind: StepFailureKind::Semantic,
            action: FailureAction::Replan,
            replan_scope: Some(ReplanScope::CurrentStep),
            detail: "语义错误".to_string(),
            source: "test".to_string(),
            user_message: None,
        },
        &mut trace,
    );

    assert_eq!(trace.recovery_attempts.len(), 1);
    let attempt = &trace.recovery_attempts[0];
    assert_eq!(attempt.failure_kind, StepFailureKind::Semantic);
    assert_eq!(attempt.action, FailureAction::Replan);
    assert_eq!(attempt.outcome, RecoveryOutcome::Continued);
    assert_eq!(attempt.step, 1);
    assert_eq!(attempt.source, "test");
    assert_eq!(attempt.detail, "语义错误");
    assert!(!attempt.escalated);
    assert!(attempt.successful);
}
