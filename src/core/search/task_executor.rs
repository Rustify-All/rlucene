/*
 * Licensed to the Apache Software Foundation (ASF) under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The ASF licenses this file to You under the Apache License, Version 2.0
 * (the "License"); you may not use this file except in compliance with
 * the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use crate::core::util::IOUtils;
use crate::core::util::error::lucene_error::{CaughtResult, CaughtResultExt, LuceneError, Result};
use parking_lot::Mutex;
use rayon::ThreadPool;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Executor wrapper responsible for the execution of concurrent tasks. Used to parallelize search
/// across segments as well as query rewrite in some cases. Exposes a single
/// [`Self::invoke_all`] method that takes a collection of callables and executes them concurrently.
/// Once all but one task have been submitted to the executor, it tries to run as many tasks as
/// possible on the calling thread, then waits for all tasks that have been executed in parallel on
/// the executor to be completed and then returns a list with the obtained results.
pub struct TaskExecutor {
  executor: Option<Arc<ThreadPool>>,
  #[cfg(test)]
  offloaded_task_counter: Option<Arc<AtomicUsize>>,
}

impl TaskExecutor {
  /// Creates a TaskExecutor instance.
  ///
  /// `executor` is the executor to be used for running tasks concurrently.
  pub fn new(executor: Arc<ThreadPool>) -> Self {
    Self {
      executor: Some(executor),
      #[cfg(test)]
      offloaded_task_counter: None,
    }
  }

  pub(crate) fn direct() -> Self {
    Self {
      executor: None,
      #[cfg(test)]
      offloaded_task_counter: None,
    }
  }

  #[cfg(test)]
  pub(crate) fn is_direct(&self) -> bool {
    self.executor.is_none()
  }

  /// Execute all the callables provided as an argument, wait for them to complete and return the
  /// obtained results. If more than one callable returns an error, the subsequent errors
  /// will be added as suppressed errors to the first one that was caught. Additionally, if one
  /// task returns an error, all other tasks from the same group are cancelled, to avoid needless
  /// computation as their results would not be exposed anyways.
  pub fn invoke_all<T, F, I>(&self, callables: I) -> Result<Vec<T>>
  where
    T: Send,
    F: FnOnce() -> Result<T> + Send,
    I: IntoIterator<Item = F>,
  {
    let callables = callables.into_iter();
    let mut tasks = smallvec::SmallVec::<[Task<F, T>; 1]>::with_capacity(callables.size_hint().0);
    for callable in callables {
      tasks.push(Task::new(callable));
    }
    let count = tasks.len();

    if count > 1
      && let Some(executor) = &self.executor
    {
      let task_id = AtomicUsize::new(0);
      executor.in_place_scope(|scope| {
        // Fork execution of count - 1 tasks to execute at least one task on the current thread to
        // minimize needless forking and blocking of the current thread.
        for _ in 0..count - 1 {
          #[cfg(test)]
          if let Some(counter) = &self.offloaded_task_counter {
            counter.fetch_add(1, Ordering::SeqCst);
          }
          scope.spawn(|_| {
            let id = task_id.fetch_add(1, Ordering::SeqCst);
            if id < count {
              tasks[id].run(&tasks);
            }
          });
        }

        // Try to execute as many tasks as possible on the current thread to minimize context
        // switching in case of long-running concurrent tasks as well as dead-locking if the current
        // thread is part of the executor and the executor has limited parallelism.
        loop {
          let id = task_id.fetch_add(1, Ordering::SeqCst);
          if id >= count {
            break;
          }
          tasks[id].run(&tasks);
          if id >= count - 1 {
            break;
          }
        }
      });
    } else {
      // No task is shared in this branch. Keep outcomes and cancelled callables in
      // their slots so collect_results retains the same error and drop order.
      let mut remaining = tasks.iter_mut();
      while let Some(task) = remaining.next() {
        *task.started_or_cancelled.get_mut() = true;
        let outcome = match task.callable.get_mut().take() {
          Some(callable) => catch_unwind(AssertUnwindSafe(callable)),
          None => Ok(Err(LuceneError::illegal_state(
            "the task that starts must still own its callable",
          ))),
        };
        let failed = !matches!(outcome, Ok(Ok(_)));
        *task.outcome.get_mut() = Some(TaskOutcome::Completed(outcome));
        if failed {
          for task in remaining {
            *task.started_or_cancelled.get_mut() = true;
            *task.outcome.get_mut() = Some(TaskOutcome::Cancelled);
          }
          break;
        }
      }
    }

    collect_results(tasks)
  }

  #[cfg(test)]
  pub(crate) fn set_offloaded_task_counter(&mut self, counter: Arc<AtomicUsize>) {
    self.offloaded_task_counter = Some(counter);
  }
}

enum TaskOutcome<T> {
  Completed(CaughtResult<T>),
  Cancelled,
}

struct Task<F, T> {
  callable: Mutex<Option<F>>,
  outcome: Mutex<Option<TaskOutcome<T>>>,
  started_or_cancelled: AtomicBool,
}

impl<F, T> Task<F, T>
where
  F: FnOnce() -> Result<T> + Send,
{
  fn new(callable: F) -> Self {
    Self {
      callable: Mutex::new(Some(callable)),
      outcome: Mutex::new(None),
      started_or_cancelled: AtomicBool::new(false),
    }
  }

  fn run(&self, tasks: &[Self]) {
    if self
      .started_or_cancelled
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_err()
    {
      return;
    }

    let outcome = match self.callable.lock().take() {
      Some(callable) => catch_unwind(AssertUnwindSafe(callable)),
      None => Ok(Err(LuceneError::illegal_state(
        "the task that starts must still own its callable",
      ))),
    };
    let failed = !matches!(outcome, Ok(Ok(_)));
    *self.outcome.lock() = Some(TaskOutcome::Completed(outcome));
    if failed {
      cancel_all(tasks);
    }
  }

  fn cancel(&self) {
    if self
      .started_or_cancelled
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      *self.outcome.lock() = Some(TaskOutcome::Cancelled);
    }
  }
}

fn collect_results<F, T>(tasks: smallvec::SmallVec<[Task<F, T>; 1]>) -> Result<Vec<T>> {
  let mut first_failure: Option<CaughtResult<T>> = None;
  let mut results = Vec::with_capacity(tasks.len());
  for task in tasks {
    let outcome = task
      .outcome
      .into_inner()
      .ok_or_else(|| LuceneError::illegal_state("all tasks must be completed or cancelled"))?;
    match outcome {
      TaskOutcome::Completed(Ok(Ok(result))) => results.push(result),
      TaskOutcome::Completed(failure) => match first_failure.as_mut() {
        Some(first_failure) => {
          first_failure.add_suppressed(failure, "panic while executing a task")
        },
        None => first_failure = Some(failure),
      },
      TaskOutcome::Cancelled => {},
    }
  }

  if let Some(first_failure) = first_failure {
    return IOUtils::rethrow_always(first_failure);
  }
  Ok(results)
}

fn cancel_all<F, T>(tasks: &[Task<F, T>])
where
  F: FnOnce() -> Result<T> + Send,
{
  for task in tasks {
    task.cancel();
  }
}
