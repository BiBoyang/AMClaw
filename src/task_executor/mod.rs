use crate::pipeline::{Pipeline, PipelineFailureKind, PipelineResult};
use crate::task_store::{MarkTaskArchivedInput, TaskStore};
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use uuid::Uuid;

/// TaskExecutor 把 pipeline 任务执行从 poll_loop 主线程解耦到独立 worker 线程。
/// poll_loop 只负责 enqueue task_id，worker 负责实际的抓取与归档。
pub struct TaskExecutor {
    sender: Sender<String>,
    pending_count: Arc<AtomicUsize>,
    inflight: Arc<Mutex<HashSet<String>>>,
    _worker: JoinHandle<()>,
}

impl TaskExecutor {
    /// 启动 worker 线程，持有独立的 TaskStore 连接和 Pipeline clone。
    pub fn start(pipeline: Pipeline, db_path: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel::<String>();
        let pending_count = Arc::new(AtomicUsize::new(0));
        let inflight = Arc::new(Mutex::new(HashSet::new()));
        let pc = pending_count.clone();
        let inflight_worker = inflight.clone();
        let worker = thread::spawn(move || {
            let worker_id = Uuid::new_v4().to_string();
            log_task_executor_info("worker_started", vec![("worker_id", json!(&worker_id))]);
            let mut task_store = match TaskStore::open(&db_path) {
                Ok(store) => store,
                Err(err) => {
                    log_task_executor_error(
                        "worker_task_store_open_failed",
                        vec![("detail", json!(err.to_string()))],
                    );
                    return;
                }
            };
            while let Ok(task_id) = receiver.recv() {
                if let Err(err) = process_task(&pipeline, &mut task_store, &task_id, &worker_id) {
                    log_task_executor_error(
                        "task_execution_failed",
                        vec![
                            ("task_id", json!(task_id)),
                            ("detail", json!(err.to_string())),
                        ],
                    );
                }
                inflight_worker.lock().unwrap().remove(&task_id);
                pc.fetch_sub(1, Ordering::SeqCst);
            }
            log_task_executor_info("worker_shutdown", vec![]);
        });
        Self {
            sender,
            pending_count,
            inflight,
            _worker: worker,
        }
    }

    /// 投递一个 task_id 到 worker 队列。
    /// 若该 task_id 已在 inflight，则跳过去重，返回 false。
    pub fn enqueue(&self, task_id: String) -> bool {
        {
            let mut guard = self.inflight.lock().unwrap();
            if guard.contains(&task_id) {
                log_task_executor_warn(
                    "task_enqueue_skipped",
                    vec![
                        ("task_id", json!(task_id)),
                        ("reason", json!("already_inflight")),
                    ],
                );
                return false;
            }
            guard.insert(task_id.clone());
        }
        self.pending_count.fetch_add(1, Ordering::SeqCst);
        if let Err(err) = self.sender.send(task_id.clone()) {
            self.inflight.lock().unwrap().remove(&task_id);
            self.pending_count.fetch_sub(1, Ordering::SeqCst);
            log_task_executor_error(
                "task_enqueue_failed",
                vec![
                    ("task_id", json!(task_id)),
                    ("detail", json!(err.to_string())),
                ],
            );
            false
        } else {
            log_task_executor_info("task_enqueued", vec![("task_id", json!(task_id))]);
            true
        }
    }

    /// 等待所有已投递的任务执行完毕（主要用于测试同步断言）。
    /// 返回 `true` 表示在超时前全部 drain；`false` 表示超时。
    pub fn flush(&self) -> bool {
        let mut spins = 0;
        while self.pending_count.load(Ordering::SeqCst) > 0 && spins < 3000 {
            thread::sleep(Duration::from_millis(10));
            spins += 1;
        }
        let pending = self.pending_count.load(Ordering::SeqCst);
        if pending > 0 {
            log_task_executor_warn(
                "flush_timeout",
                vec![
                    ("pending_count", json!(pending)),
                    ("spins", json!(spins)),
                    ("wait_ms", json!(spins * 10)),
                ],
            );
            false
        } else {
            true
        }
    }
}

/// 在 worker 线程内执行单个任务：claim -> 抓取 -> 归档 -> 更新状态。
fn process_task(
    pipeline: &Pipeline,
    task_store: &mut TaskStore,
    task_id: &str,
    worker_id: &str,
) -> Result<()> {
    // 1. 原子领取任务（pending 或 lease 过期的 processing -> processing）
    const LEASE_SECS: u64 = 300;
    if !task_store.claim_task(task_id, worker_id, LEASE_SECS)? {
        log_task_executor_info(
            "task_claim_skipped",
            vec![
                ("task_id", json!(task_id)),
                ("reason", json!("already_claimed_or_missing")),
            ],
        );
        return Ok(());
    }

    // 2. 获取任务详情
    let Some(task) = task_store.get_task_by_id(task_id)? else {
        log_task_executor_warn(
            "task_not_found_after_claim",
            vec![("task_id", json!(task_id))],
        );
        return Ok(());
    };

    // pipeline 接受 PendingTaskRecord，字段完全一致，做兼容转换
    let pending_task = crate::task_store::PendingTaskRecord {
        task_id: task.task_id.clone(),
        article_id: task.article_id.clone(),
        normalized_url: task.normalized_url.clone(),
        original_url: task.original_url.clone(),
    };

    match pipeline.process_pending_task(&pending_task) {
        Ok(result) => {
            archive_success(task_store, &pending_task, &result)?;
        }
        Err(err) => {
            let err_msg = err.message.clone();
            match err.kind {
                PipelineFailureKind::AwaitingManualInput { ref page_kind } => {
                    let snapshot_path = err
                        .snapshot_path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let content_source = err.content_source.clone();
                    task_store
                        .mark_task_awaiting_manual_input(
                            &pending_task.task_id,
                            &err_msg,
                            page_kind,
                            snapshot_path.as_deref(),
                            content_source.as_deref(),
                        )
                        .with_context(|| {
                            format!(
                                "worker 更新 awaiting_manual_input 失败 task_id={}",
                                pending_task.task_id
                            )
                        })?;
                    log_task_executor_warn(
                        "pending_task_awaiting_manual_input",
                        vec![
                            ("task_id", json!(pending_task.task_id)),
                            ("status", json!("awaiting_manual_input")),
                            ("page_kind", json!(page_kind)),
                            ("detail", json!(err_msg)),
                        ],
                    );
                }
                PipelineFailureKind::Failed => {
                    task_store
                        .mark_task_failed(&pending_task.task_id, &err_msg)
                        .with_context(|| {
                            format!("worker 更新 failed 失败 task_id={}", pending_task.task_id)
                        })?;
                    log_task_executor_error(
                        "pending_task_failed",
                        vec![
                            ("task_id", json!(pending_task.task_id)),
                            ("status", json!("failed")),
                            ("error_kind", json!("pipeline_task_failed")),
                            ("detail", json!(err_msg)),
                        ],
                    );
                }
            }
        }
    }
    Ok(())
}

fn archive_success(
    task_store: &mut TaskStore,
    task: &crate::task_store::PendingTaskRecord,
    result: &PipelineResult,
) -> Result<()> {
    let output_path = result.output_path.to_string_lossy().to_string();
    let snapshot_path = result
        .snapshot_path
        .as_ref()
        .map(|path| path.to_string_lossy().to_string());
    task_store
        .mark_task_archived(
            &task.task_id,
            MarkTaskArchivedInput {
                output_path: &output_path,
                title: result.title.as_deref(),
                page_kind: Some(&result.page_kind),
                snapshot_path: snapshot_path.as_deref(),
                content_source: Some(&result.content_source),
                summary: result.summary.as_deref(),
            },
        )
        .with_context(|| format!("worker 更新 archived 失败 task_id={}", task.task_id))?;
    log_task_executor_info(
        "pending_task_archived",
        vec![
            ("task_id", json!(task.task_id)),
            ("status", json!("archived")),
            ("output_path", json!(output_path)),
        ],
    );
    Ok(())
}

fn log_task_executor_info(event: &str, fields: Vec<(&str, serde_json::Value)>) {
    crate::logging::emit_structured_log("info", event, fields);
}

fn log_task_executor_warn(event: &str, fields: Vec<(&str, serde_json::Value)>) {
    crate::logging::emit_structured_log("warn", event, fields);
}

fn log_task_executor_error(event: &str, fields: Vec<(&str, serde_json::Value)>) {
    crate::logging::emit_structured_log("error", event, fields);
}
