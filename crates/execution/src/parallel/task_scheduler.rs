//! The worker thread pool shared by every query of a database (DuckDB's `TaskScheduler`,
//! `task_scheduler.cpp`).
//!
//! Executors queue their tasks under a [`ProducerToken`]. Background workers run tasks of
//! any producer; the thread that runs a query works on its own producer's tasks while it
//! waits for the query to finish, so a query always makes progress even when every
//! worker is busy (e.g. with the outer query of a nested execution).

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// How much of its work a task does per call (DuckDB's `TaskExecutionMode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskExecutionMode {
    /// Run until the task is complete.
    ProcessAll,
    /// Do a bounded amount of work, then return so the caller can check on the query.
    ProcessPartial,
}

/// The outcome of [`Task::execute`] (DuckDB's `TaskExecutionResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskExecutionResult {
    Finished,
    /// The task must be executed again (after [`TaskExecutionMode::ProcessPartial`]).
    NotFinished,
    /// The task failed; the error was handed to its executor.
    Error,
}

/// A unit of work run by the scheduler's threads (DuckDB's `Task`).
pub(crate) trait Task: Send {
    fn execute(&mut self, mode: TaskExecutionMode) -> TaskExecutionResult;
}

/// A queued task and the producer it belongs to.
struct QueuedTask {
    producer: u64,
    task: Box<dyn Task>,
}

#[derive(Default)]
struct Queue {
    tasks: VecDeque<QueuedTask>,
    next_producer: u64,
}

/// The state shared by the scheduler, its workers and the producer tokens.
#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    /// Wakes background workers: once per queued task (DuckDB's semaphore `signal(n)`),
    /// and all of them when workers must stop.
    work: Condvar,
    /// Wakes the query threads waiting in [`ProducerToken::wait`]: when tasks are queued
    /// and when a query may be complete.
    done: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A background worker and the flag that keeps it running.
struct WorkerThread {
    marker: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

/// The pool of worker threads (DuckDB's `TaskScheduler`). `threads` counts the thread
/// running the query too, so `threads - 1` background workers are started (lazily, when
/// the first query needs them).
pub struct TaskScheduler {
    shared: Arc<Shared>,
    workers: Mutex<Vec<WorkerThread>>,
    /// The `threads` setting.
    requested_threads: AtomicUsize,
}

impl fmt::Debug for TaskScheduler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskScheduler")
            .field("threads", &self.number_of_threads())
            .finish_non_exhaustive()
    }
}

impl TaskScheduler {
    /// A scheduler running queries on `threads` threads (at least one).
    pub fn new(threads: usize) -> Self {
        Self {
            shared: Arc::default(),
            workers: Mutex::default(),
            requested_threads: AtomicUsize::new(threads.max(1)),
        }
    }

    /// The `threads` setting: how many threads may work on one query, the thread running
    /// it included (DuckDB's `NumberOfThreads`).
    pub fn number_of_threads(&self) -> usize {
        self.requested_threads.load(Ordering::Relaxed)
    }

    /// `SET threads`: changes the number of threads; workers are relaunched by the next
    /// query (`TaskScheduler::SetThreads` with one external thread).
    pub fn set_threads(&self, threads: usize) {
        self.requested_threads.store(threads.max(1), Ordering::Relaxed);
    }

    /// Starts or stops background workers so that `threads - 1` are running
    /// (`RelaunchThreads`). Stopped workers finish the task they are running first.
    fn relaunch_threads(&self) {
        let wanted = self.number_of_threads() - 1;
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        if workers.len() == wanted {
            return;
        }
        while workers.len() > wanted {
            if let Some(worker) = workers.pop() {
                worker.marker.store(false, Ordering::Release);
            }
        }
        while workers.len() < wanted {
            let marker = Arc::new(AtomicBool::new(true));
            let shared = Arc::clone(&self.shared);
            let thread_marker = Arc::clone(&marker);
            let spawned = std::thread::Builder::new()
                .name("rustdb-worker".to_owned())
                .spawn(move || execute_forever(&shared, &thread_marker));
            match spawned {
                Ok(handle) => workers.push(WorkerThread { marker, handle }),
                // Without more threads the query threads still run every task.
                Err(_) => break,
            }
        }
        drop(workers);
        self.shared.work.notify_all();
    }

    /// A token under which an executor queues its tasks (`CreateProducer`). Makes sure
    /// the background workers for the current `threads` setting are running.
    pub(crate) fn create_producer(&self) -> ProducerToken {
        self.relaunch_threads();
        let id = {
            let mut queue = self.shared.lock();
            queue.next_producer += 1;
            queue.next_producer
        };
        ProducerToken {
            id,
            shared: Arc::clone(&self.shared),
        }
    }
}

impl Drop for TaskScheduler {
    fn drop(&mut self) {
        let workers = std::mem::take(self.workers.get_mut().unwrap_or_else(PoisonError::into_inner));
        for worker in &workers {
            worker.marker.store(false, Ordering::Release);
        }
        self.shared.work.notify_all();
        for worker in workers {
            // A worker only panics if a task did; the task's executor has reported it.
            let _ = worker.handle.join();
        }
    }
}

/// A background worker's loop: run queued tasks until `marker` is cleared
/// (`ExecuteForever`).
fn execute_forever(shared: &Shared, marker: &AtomicBool) {
    let mut queue = shared.lock();
    loop {
        if !marker.load(Ordering::Acquire) {
            return;
        }
        let Some(mut queued) = queue.tasks.pop_front() else {
            queue = shared.work.wait(queue).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        drop(queue);
        let result = queued.task.execute(TaskExecutionMode::ProcessAll);
        if result == TaskExecutionResult::NotFinished {
            queue = shared.lock();
            queue.tasks.push_back(queued);
        } else {
            // Tasks are dropped outside the lock: their destructors may notify the queue.
            drop(queued);
            queue = shared.lock();
        }
    }
}

/// An executor's handle on the scheduler's queue (DuckDB's `ProducerToken`). Dropping it
/// discards the producer's tasks that are still queued.
pub(crate) struct ProducerToken {
    id: u64,
    shared: Arc<Shared>,
}

impl fmt::Debug for ProducerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProducerToken")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ProducerToken {
    /// Queues `tasks` (`ScheduleTasks`).
    pub fn schedule_tasks(&self, tasks: Vec<Box<dyn Task>>) {
        if tasks.is_empty() {
            return;
        }
        let count = tasks.len();
        let mut queue = self.shared.lock();
        queue.tasks.extend(tasks.into_iter().map(|task| QueuedTask {
            producer: self.id,
            task,
        }));
        drop(queue);
        // One worker per task rather than all of them (`semaphore.signal(n)`).
        for _ in 0..count {
            self.shared.work.notify_one();
        }
        self.shared.done.notify_all();
    }

    /// Takes the oldest queued task of this producer (`GetTaskFromProducer`).
    pub fn take_task(&self) -> Option<Box<dyn Task>> {
        let mut queue = self.shared.lock();
        let position = queue.tasks.iter().position(|queued| queued.producer == self.id)?;
        queue.tasks.remove(position).map(|queued| queued.task)
    }

    /// Blocks until a task of this producer is queued or `done()` holds. `done` is
    /// checked with the queue locked, so a state change followed by
    /// [`ProducerToken::notify`] is never missed.
    pub fn wait(&self, done: impl Fn() -> bool) {
        let mut queue = self.shared.lock();
        while !done() && !queue.tasks.iter().any(|queued| queued.producer == self.id) {
            queue = self
                .shared
                .done
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Wakes the threads waiting in [`ProducerToken::wait`] to re-check their condition.
    pub fn notify(&self) {
        drop(self.shared.lock());
        self.shared.done.notify_all();
    }
}

impl Drop for ProducerToken {
    fn drop(&mut self) {
        let removed: VecDeque<QueuedTask> = {
            let mut queue = self.shared.lock();
            let (mine, others) = std::mem::take(&mut queue.tasks)
                .into_iter()
                .partition(|queued| queued.producer == self.id);
            queue.tasks = others;
            mine
        };
        // Tasks are dropped outside the lock: their destructors may notify the queue.
        drop(removed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counter(Arc<AtomicUsize>, usize);

    impl Task for Counter {
        fn execute(&mut self, mode: TaskExecutionMode) -> TaskExecutionResult {
            self.0.fetch_add(1, Ordering::SeqCst);
            if mode == TaskExecutionMode::ProcessPartial && self.1 > 0 {
                self.1 -= 1;
                return TaskExecutionResult::NotFinished;
            }
            TaskExecutionResult::Finished
        }
    }

    #[test]
    fn workers_and_the_producer_run_every_task() {
        let scheduler = TaskScheduler::new(4);
        let producer = scheduler.create_producer();
        let count = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<Box<dyn Task>> = (0..100)
            .map(|_| Box::new(Counter(Arc::clone(&count), 0)) as Box<dyn Task>)
            .collect();
        producer.schedule_tasks(tasks);
        while let Some(mut task) = producer.take_task() {
            task.execute(TaskExecutionMode::ProcessAll);
        }
        producer.wait(|| count.load(Ordering::SeqCst) == 100);
        assert_eq!(count.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn set_threads_relaunches_workers() {
        let scheduler = TaskScheduler::new(3);
        drop(scheduler.create_producer());
        assert_eq!(scheduler.workers.lock().unwrap().len(), 2);
        scheduler.set_threads(1);
        drop(scheduler.create_producer());
        assert_eq!(scheduler.workers.lock().unwrap().len(), 0);
        assert_eq!(scheduler.number_of_threads(), 1);
    }

    #[test]
    fn dropping_a_producer_discards_its_tasks() {
        let scheduler = TaskScheduler::new(1);
        let producer = scheduler.create_producer();
        let count = Arc::new(AtomicUsize::new(0));
        producer.schedule_tasks(vec![Box::new(Counter(Arc::clone(&count), 0))]);
        drop(producer);
        assert!(scheduler.shared.lock().tasks.is_empty());
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
