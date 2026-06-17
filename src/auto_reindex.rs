//! Debounced background reindex coordination.
//!
//! The search index is shared by long-lived MCP sessions. This coordinator lets
//! tools request a refresh without blocking normal requests on the async runtime.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Notify, oneshot};

use crate::search::{ReindexReport, SearchError, SearchIndex};

#[derive(Debug, Clone)]
pub struct ReindexRequest {
    pub sources: Vec<String>,
    pub workspace_paths: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReindexErrorKind {
    Scope,
    Busy,
    Internal,
}

#[derive(Debug, Clone)]
pub struct ReindexError {
    pub kind: ReindexErrorKind,
    pub message: String,
}

impl ReindexError {
    fn scope(message: impl Into<String>) -> Self {
        Self {
            kind: ReindexErrorKind::Scope,
            message: message.into(),
        }
    }

    fn busy(message: impl Into<String>) -> Self {
        Self {
            kind: ReindexErrorKind::Busy,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ReindexErrorKind::Internal,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct Pending {
    generation: u64,
    request: ReindexRequest,
    due_at: Instant,
}

#[derive(Debug)]
struct Waiter {
    generation: u64,
    sender: oneshot::Sender<Result<ReindexReport, ReindexError>>,
}

#[derive(Debug, Default)]
struct State {
    generation: u64,
    running: bool,
    pending: Option<Pending>,
    waiters: Vec<Waiter>,
}

pub struct AutoReindexer {
    search: Arc<SearchIndex>,
    debounce: Duration,
    state: Mutex<State>,
    notify: Notify,
}

#[derive(Debug, Clone, Copy)]
enum MergePolicy {
    Debounced,
    Force,
}

impl AutoReindexer {
    pub fn new(search: Arc<SearchIndex>, debounce: Duration) -> Arc<Self> {
        let this = Arc::new(Self {
            search,
            debounce,
            state: Mutex::new(State::default()),
            notify: Notify::new(),
        });
        let worker = this.clone();
        tokio::spawn(async move { worker.run_loop().await });
        this
    }

    pub async fn schedule(&self, request: ReindexRequest) -> u64 {
        let mut state = self.state.lock().await;
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        let due_at = Instant::now()
            .checked_add(self.debounce)
            .unwrap_or_else(Instant::now);
        state.pending = Some(merge_pending(
            state.pending.take(),
            Pending {
                generation,
                request,
                due_at,
            },
            MergePolicy::Debounced,
        ));
        drop(state);
        self.notify.notify_waiters();
        generation
    }

    pub async fn force_and_wait(
        &self,
        request: ReindexRequest,
    ) -> Result<ReindexReport, ReindexError> {
        let (sender, receiver) = oneshot::channel();
        let generation = {
            let mut state = self.state.lock().await;
            state.generation = state.generation.saturating_add(1);
            let generation = state.generation;
            state.pending = Some(merge_pending(
                state.pending.take(),
                Pending {
                    generation,
                    request,
                    due_at: Instant::now(),
                },
                MergePolicy::Force,
            ));
            state.waiters.push(Waiter { generation, sender });
            generation
        };
        self.notify.notify_waiters();

        match receiver.await {
            Ok(result) => result,
            Err(_) => Err(ReindexError::internal(format!(
                "reindex coordinator dropped waiter for generation {generation}"
            ))),
        }
    }

    async fn run_loop(self: Arc<Self>) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let pending = {
                let state = self.state.lock().await;
                if state.running {
                    None
                } else {
                    state.pending.clone()
                }
            };

            let Some(pending) = pending else {
                notified.as_mut().await;
                continue;
            };

            if let Some(delay) = pending.due_at.checked_duration_since(Instant::now()) {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = &mut notified => {}
                }
                continue;
            }

            let pending = {
                let mut state = self.state.lock().await;
                if state.running {
                    continue;
                }
                let Some(pending) = state.pending.take() else {
                    continue;
                };
                if pending
                    .due_at
                    .checked_duration_since(Instant::now())
                    .is_some()
                {
                    state.pending = Some(pending);
                    continue;
                }
                state.running = true;
                pending
            };

            let search = self.search.clone();
            let spec = pending.request.clone();
            let (sender, receiver) = oneshot::channel();
            let spawn_result = thread::Builder::new()
                .name("spark-mcp-reindex".to_string())
                .spawn(move || {
                    let _ = sender.send(search.reindex_scoped(
                        &spec.sources,
                        &spec.workspace_paths,
                        &spec.reason,
                    ));
                });

            let outcome: Result<ReindexReport, ReindexError> = match spawn_result {
                Ok(_handle) => match receiver.await {
                    Ok(Ok(report)) => Ok(report),
                    Ok(Err(err)) => Err(classify_reindex_error(err)),
                    Err(_) => Err(ReindexError::internal(
                        "reindex worker thread dropped without reporting".to_string(),
                    )),
                },
                Err(err) => Err(ReindexError::internal(format!(
                    "failed to spawn reindex worker thread: {err}"
                ))),
            };

            let mut state = self.state.lock().await;
            state.running = false;
            let finished_generation = pending.generation;
            let mut remaining = Vec::new();
            for waiter in state.waiters.drain(..) {
                if waiter.generation <= finished_generation {
                    let _ = waiter.sender.send(outcome.clone());
                } else {
                    remaining.push(waiter);
                }
            }
            state.waiters = remaining;
            drop(state);
            self.notify.notify_waiters();
        }
    }
}

fn classify_reindex_error(err: SearchError) -> ReindexError {
    match err {
        SearchError::ReindexScope(message) => ReindexError::scope(message),
        SearchError::ReindexBusy => ReindexError::busy("reindex already in progress; retry later"),
        other => ReindexError::internal(other.to_string()),
    }
}

fn merge_pending(existing: Option<Pending>, mut incoming: Pending, policy: MergePolicy) -> Pending {
    let Some(existing) = existing else {
        return incoming;
    };

    incoming.request = merge_request(existing.request, incoming.request);
    incoming.due_at = match policy {
        MergePolicy::Force => incoming.due_at.min(existing.due_at),
        MergePolicy::Debounced => {
            let now = Instant::now();
            if existing.due_at <= now {
                existing.due_at
            } else {
                incoming.due_at.max(existing.due_at)
            }
        }
    };
    incoming.generation = incoming.generation.max(existing.generation);
    incoming
}

fn merge_request(mut left: ReindexRequest, right: ReindexRequest) -> ReindexRequest {
    left.sources.extend(right.sources);
    left.sources.sort();
    left.sources.dedup();
    left.workspace_paths = merge_workspace_paths(left.workspace_paths, right.workspace_paths);
    left.reason = right.reason;
    left
}

fn merge_workspace_paths(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    let mut merged = left;
    merged.extend(right);
    merged.sort();
    merged.dedup();
    merged
}

#[cfg(test)]
mod tests {
    use super::merge_workspace_paths;

    #[test]
    fn merge_workspace_paths_unions_distinct_non_empty_paths() {
        let merged = merge_workspace_paths(
            vec!["spark/src/a.ads".to_string()],
            vec!["spark/src/b.ads".to_string()],
        );

        assert_eq!(
            merged,
            vec!["spark/src/a.ads".to_string(), "spark/src/b.ads".to_string()]
        );
    }

    #[test]
    fn merge_workspace_paths_preserves_non_empty_side() {
        assert_eq!(
            merge_workspace_paths(Vec::new(), vec!["spark/src/a.ads".to_string()]),
            vec!["spark/src/a.ads".to_string()]
        );
        assert_eq!(
            merge_workspace_paths(vec!["spark/src/a.ads".to_string()], Vec::new()),
            vec!["spark/src/a.ads".to_string()]
        );
    }
}
