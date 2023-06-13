//! Asynchronous tasks executor.

use super::{
    context,
    rand::GlobalRng,
    runtime,
    time::{TimeHandle, TimeRuntime},
    // utils::mpsc,
};
use crate::assert_send_sync;
use async_task::{FallibleTask, Runnable};
use backtrace::Backtrace;
use erasable::{ErasablePtr, ErasedPtr};
use futures::pin_mut;
use rand::Rng;
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    ops::Deref,
    panic::{RefUnwindSafe, UnwindSafe},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Waker},
    thread::sleep,
    time::Duration,
};

use tracing::{error_span, info, trace, warn, Span};

pub use tokio::msim_adapter::{join_error, runtime_task};
pub use tokio::task::{yield_now, JoinError};
pub use tokio::{select, sync::watch};

pub mod join_set;
pub use join_set::JoinSet;

pub(crate) struct Executor {
    queue: Receiver,
    handle: TaskHandle,
    rand: GlobalRng,
    time: TimeRuntime,
    time_limit: Option<Duration>,

    // bz: queue for yield tasks
    yield_queue: Receiver,
}

/// A unique identifier for a task.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct TaskId(pub u64);

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Task({})", self.0)
    }
}

/// A unique identifier for a node.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.0)
    }
}

impl NodeId {
    pub(crate) const fn zero() -> Self {
        NodeId(0)
    }
}

pub(crate) struct NodeInfo {
    pub node: NodeId,
    pub name: String,
    span: Span,
}

#[derive(Debug)]
struct PanicWrapper {
    // how long should the node stay down. If None, node does not reboot.
    restart_after: Option<Duration>,
}

struct PanicHookGuard(Option<Box<dyn Fn(&std::panic::PanicInfo<'_>) + Sync + Send + 'static>>);

impl PanicHookGuard {
    fn new() -> Self {
        Self(Some(std::panic::take_hook()))
    }

    fn call_hook(&self, info: &std::panic::PanicInfo<'_>) {
        self.0.as_ref().unwrap()(info);
    }
}

impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            std::panic::set_hook(self.0.take().unwrap());
        }
    }
}

/// Kill the current node by panicking with a special type that tells the executor to kill the
/// current node instead of terminating the test.
pub fn kill_current_node(restart_after: Option<Duration>) {
    let handle = runtime::Handle::current();
    let restart_after = restart_after.unwrap_or_else(|| {
        Duration::from_millis(handle.rand.with(|rng| rng.gen_range(1000..3000)))
    });
    kill_current_node_impl(handle, Some(restart_after));
}

/// Kill the current node, and do not restart it automatically.
pub fn shutdown_current_node() {
    kill_current_node_impl(runtime::Handle::current(), None);
}

fn kill_current_node_impl(handle: runtime::Handle, restart_after: Option<Duration>) {
    let cur_node_id = context::current_node();

    if let Some(restart_after) = restart_after {
        info!(
            "killing node {}. Will restart in {:?}",
            cur_node_id, restart_after
        );
    } else {
        info!("shutting down node {}", cur_node_id);
    }
    handle.kill(cur_node_id);
    // panic with PanicWrapper so that run_all_ready can intercept it.
    std::panic::panic_any(PanicWrapper { restart_after });
}

/// bz: remove crate-private, since we need it in channel/Sender/Receiver
// pub(crate) struct TaskInfo {
pub struct TaskInfo {
    inner: Arc<NodeInfo>,
    /// A flag indicating that the task should be paused.
    paused: AtomicBool,
    /// A flag indicating that the task should no longer be executed.
    killed: watch::Sender<bool>,

    /// bz: task id
    task_id: Arc<TaskId>,
}

impl TaskInfo {
    fn new(node_id: NodeId, task_id: TaskId, name: String) -> Self {
        let span = error_span!(parent: None, "node", id = %node_id.0, task_id = %task_id.0, name);
        TaskInfo {
            inner: Arc::new(NodeInfo {
                node: node_id,
                name,
                span,
            }),
            paused: AtomicBool::new(false),
            killed: watch::channel(false).0,
            task_id: Arc::new(task_id),
        }
    }

    /// node, id
    pub fn node(&self) -> NodeId {
        self.inner.node
    }

    /// name
    pub fn name(&self) -> String {
        self.inner.name.clone()
    }

    /// span
    pub fn span(&self) -> Span {
        self.inner.span.clone()
    }

    /// is_killed
    pub fn is_killed(&self) -> bool {
        *self.killed.borrow()
    }
}

impl PartialEq for TaskInfo {
    fn eq(&self, other: &TaskInfo) -> bool {
        self.task_id == other.task_id
    }
}
impl Eq for TaskInfo {}

// // Instrumentation to capture the stack trace of points of interest in the code.
// // Code can call `instrumented_yield` to allow the scheduler to see the stack trace
// // of the task that just yielded.
// thread_local! {
//     static LAST_CAPTURE: Mutex<Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>> = Mutex::new(None);
// }

/// bz: flags

/// bz: DEBUG = true: print out necessary debug info
pub const DEBUG: bool = true;
const DEBUG_DETAIL: bool = false; // DEBUG_DETAIL = true: print out all debug info, this can be super long

static RELEASE: AtomicBool = AtomicBool::new(false); // bz: flag of whether it is the time to release yield tasks
static THRESHOLD: AtomicU64 = AtomicU64::new(9); // bz: we can random this number; whenever we have seen PASSED_INSTRUMENTED_YIELDS of calls of instrumented_yield().await, we set RELEASE to true
static PASSED_INSTRUMENTED_YIELDS: AtomicU64 = AtomicU64::new(0); // bz: how many calls of instrumented_yield().await we have seen

static RETRY_PICK_LIMIT: u64 = 2; // bz: the number limit of retrying to pick a non-yield task in executor, we should not need lock here

/// bz: set RELEASE to true
pub fn set_release_to_true() {
    RELEASE.store(true, Ordering::SeqCst);
}

/// bz: get the RELEASE flag
pub fn get_release() -> bool {
    let release = RELEASE.load(Ordering::SeqCst);
    release
}

#[derive(Default)]
struct YieldToScheduler(Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>);

impl Future for YieldToScheduler {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if DEBUG_DETAIL {
            info!("{}", "*****".repeat(15));
            info!("polling backtrace"); // bz: debug
        }

        let info = context::current_task(); // bz: current task info
        if DEBUG_DETAIL {
            info!(
                // bz: debug
                "working on task with node id {:?}, name {:?}, task id {:?}",
                info.node(),
                info.name(),
                info.task_id,
            );
        }

        let handle = runtime::Handle::current();
        let len_order = handle.task.size_of_order();
        if len_order == 0 {
            // bz: no pushed yield task yet, or all have been executed
            if DEBUG_DETAIL {
                info!("no tasks spawned by the yield node yet");
                info!("{}", "*****".repeat(15));
            }
            return Poll::Ready(());
        }

        // bz: return ready for non-yield node's tasks
        let yield_node_id = handle.task.yield_node_id.load(Ordering::SeqCst);
        if info.node() != NodeId(yield_node_id) {
            if DEBUG {
                info!("ready for tasks spawned by non-yield nodes");
                info!("{}", "*****".repeat(15));
            }
            return Poll::Ready(());
        }

        if get_release() {
            if DEBUG_DETAIL {
                // bz: the following if else is for debugging
                if len_order == 0 {
                    info!("already send all yield tasks to executor, waiting to run.");
                } else {
                    info!("already waked all yield tasks.");
                }
                info!("{}", "*****".repeat(15));
            }
            return Poll::Ready(());
        }

        let mut last_captures = handle.task.last_captures.lock().unwrap();
        let l_last_captures = last_captures.len();

        if DEBUG {
            info!(
                "#order = {:?} #last_captures = {:?}",
                len_order, l_last_captures
            );
        }

        let order = handle.task.order.lock().unwrap();
        let _task_id = &order[len_order - 1].task_id.clone();
        drop(order); // bz: early drop the lock

        if info.task_id == _task_id.clone() {
            if l_last_captures == len_order {
                if DEBUG_DETAIL {
                    info!("pushed the same task already, got polled again. skip.");
                }
            } else if l_last_captures == len_order - 1 {
                // bz: we havent push this task to last_captures, push now
                if DEBUG_DETAIL {
                    info!(
                        // bz: debug
                        "pushing task with node id {:?}, name {:?}, task id {:?}",
                        info.node(),
                        info.name(),
                        info.task_id,
                    );
                }
                let new_capture = Arc::new((info, cx.waker().clone(), Backtrace::new()));
                last_captures.push(new_capture);

                if DEBUG_DETAIL {
                    info!("#last_captures = {:?} (after push)", last_captures.len());
                }
            }
        }
        drop(last_captures);

        // bz: we initialize it with a random number
        let mut threshold = THRESHOLD.load(Ordering::SeqCst);
        if threshold == 0 {
            let rng = handle.rand;
            let t: u64 = rng.with(|rng| rng.gen_range(2..20)); // TODO: bz: what is a good estimate here?
            THRESHOLD.store(t, Ordering::SeqCst);
            threshold = t;

            if DEBUG {
                info!("initialize THRESHOLD = {:?}", THRESHOLD);
            }
        }

        // bz: check if its time to release yield tasks
        let n = PASSED_INSTRUMENTED_YIELDS.load(Ordering::SeqCst);
        if n >= threshold {
            if DEBUG {
                info!("Reached THRESHOLD = {:?}", threshold);
                info!("RELEASE ready (due to reaching THRESHOLD).");
                info!("{}", "*****".repeat(15));
            }

            set_release_to_true();
            return Poll::Ready(());
        }

        // // bz: check if we already pushed and pending on this task
        // for (i, e) in order.iter().enumerate() {
        //     info!(
        //         // bz: debug
        //         "checking task with node id {:?}, name {:?}, task id {:?}",
        //         e.node(),
        //         e.name(),
        //         e.task_id,
        //     );
        //     if info.task_id == e.task_id {
        //         info!("instrumented_yield() called twice before control returned to scheduler");
        //         // TODO: bz: we seen the second instrumented_yield() in the same task
        //         return Poll::Ready(());
        //     }
        // }

        if DEBUG_DETAIL {
            info!("{}", "*****".repeat(15));
        }

        Poll::Pending

        // // capture the current stack trace
        // LAST_CAPTURE.with(|last_capture| {
        //     let mut last_capture = last_capture.lock().unwrap();
        //     match (&self.0, last_capture.as_ref()) {
        //         (Some(this), Some(last)) => {
        //             assert!(Arc::ptr_eq(this, last));
        //             // we were polled again before control reached the scheduler
        //             info!("YieldToScheduler polled before being woken");
        //             Poll::Pending
        //         }
        //         (Some(_), None) => {
        //             // the scheduler cleared the capture, so we are ready to resume.
        //             info!("poll ready");
        //             Poll::Ready(())
        //             // Poll::Pending
        //         }
        //         (None, Some(_)) => {
        //             // If this happens and can't be avoided, we could keep a Vec instead of Option
        //             // in LAST_CAPTURE, although the scheduler will have no ability to change the
        //             // ordering of such events.
        //             info!("poll panic");
        //             panic!(
        //                 "instrumented_yield() called twice before control returned to scheduler"
        //             );
        //         }
        //         (None, None) => {
        //             info!("add to LAST_CAPTURE");
        //             trace!("capturing stack trace and yielding");
        //             let info = context::current_task();
        //             let new_capture = Arc::new((info, cx.waker().clone(), Backtrace::new()));
        //             *last_capture = Some(new_capture.clone());
        //             self.0 = Some(new_capture);
        //             Poll::Pending
        //         }
        //     }
        // })
    }
}

/// Capture the current stack trace and attempt to yield execution back to the scheduler.
///
/// Note that it is not possible to guarantee that execution immediately returns all the way to
/// the scheduler, as the yielding future may be wrapped in another future that polls other futures
/// (for instance a `select!` macro, or FuturesOrdered/FuturesUnordered collection). Also, it is
/// possible for an intermediate future to poll() the YieldToScheduler instance again before it is
/// woken.  However, we do guarantee not to wake the future until execution has returned to the
/// scheduler.
pub fn instrumented_yield() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    if DEBUG {
        // bz: debug
        info!("{}", "-----".repeat(15));
        // info!("instrumented_yield backtrace:\n{}", std::backtrace::Backtrace::force_capture());
    }

    // bz: record the current task
    let info = context::current_task(); // return type is TaskInfo
    let _ = PASSED_INSTRUMENTED_YIELDS.fetch_add(1, Ordering::SeqCst); // bz: increment #seen calls of instrumented_yield()

    if DEBUG {
        info!(
            // bz: debug
            "Yield current task with node id {:?}, name {:?}, task id {:?}",
            info.node(),
            info.name(),
            info.task_id,
        );
    }

    let handle = runtime::Handle::current();
    let len_order = handle.task.size_of_order();
    if len_order == 0 && !get_release() {
        let yield_node_id = handle.task.yield_node_id.load(Ordering::SeqCst);
        if yield_node_id == 0 {
            // bz: the 1st time of yielding tasks, all test nodes should be created already
            // we randomly pick a node to yield all its tasks
            let rng = handle.rand;
            let nodes = handle.task.nodes.lock().unwrap();
            let node_idx = rng.with(|rng| rng.gen_range(2..nodes.keys().len() + 1)); // 0 is main, 1 is client
            drop(nodes);

            handle
                .task
                .yield_node_id
                .store(node_idx.try_into().unwrap(), Ordering::SeqCst);

            if DEBUG {
                info!("PICK YIELD NodeId = {:?} to yield all its tasks.", node_idx);
            }
        }
    }

    let mut order = handle.task.order.lock().unwrap();
    let yield_node_id = handle.task.yield_node_id.load(Ordering::SeqCst);
    if info.node() == NodeId(yield_node_id) {
        order.push(info.clone());
    }

    if DEBUG {
        // bz: debug
        info!(
            "PASSED_INSTRUMENTED_YIELDS = {:?}",
            PASSED_INSTRUMENTED_YIELDS
        );
        info!("Current order is (after push): #order = {:?}", order.len());
        for e in order.iter() {
            info!(
                " - node id {:?}, name {:?}, task id {:?}",
                e.node(),
                e.name(),
                e.task_id
            );
        }
        info!("{}", "-----".repeat(15)); // bz: debug
    }

    drop(order);

    Box::pin(YieldToScheduler::default())
}

// fn take_last_capture() -> Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>> {
//     LAST_CAPTURE.with(|last_capture| last_capture.lock().unwrap().take())
// }

impl Executor {
    pub fn new(rand: GlobalRng) -> Self {
        let (sender, queue) = channel();
        let (yield_sender, yield_queue) = channel();
        Executor {
            queue,
            handle: TaskHandle {
                nodes: Arc::new(Mutex::new(HashMap::new())),
                sender,
                next_node_id: Arc::new(AtomicU64::new(1)),
                next_task_id: Arc::new(AtomicU64::new(1)),
                order: Arc::new(Mutex::new(Vec::new())),
                last_captures: Arc::new(Mutex::new(Vec::new())),
                yield_node_id: Arc::new(AtomicU64::new(0)),
                paused_node_id: Arc::new(Mutex::new(Vec::new())),
            },
            time: TimeRuntime::new(&rand),
            rand,
            time_limit: None,
            yield_queue,
        }
    }

    pub fn handle(&self) -> &TaskHandle {
        &self.handle
    }

    pub fn time_handle(&self) -> &TimeHandle {
        self.time.handle()
    }

    pub fn set_time_limit(&mut self, limit: Duration) {
        self.time_limit = Some(limit);
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        let mut task = self.spawn_on_main_task(future);

        // empty context to poll the result
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        loop {
            self.run_all_ready();
            if let Poll::Ready(val) = Pin::new(&mut task).poll(&mut cx) {
                let len_order = self.handle.size_of_order();
                let release = get_release();
                if len_order > 0 && !release {
                    // bz: we still have yield tasks, let them be ready
                    if DEBUG {
                        info!("RELEASE ready (due to poll ready in executor, but still has yield tasks).");
                    }
                    self.handle.set_release_and_wake_last_captures();

                    self.run_all_ready(); // bz: last round of call
                }

                if DEBUG {
                    info!("executor: done");
                }

                // bz: resume the paused kill/delete node tasks
                self.handle.resume_paused_node_id();

                return val;
            }

            //// bz: debug
            // info!("polling task in block_on: {:?}", task);

            let going = self.time.advance_to_next_event();
            assert!(going, "no events, the task will block forever");
            if let Some(limit) = self.time_limit {
                assert!(
                    self.time.handle().elapsed() < limit,
                    "time limit exceeded: {:?}",
                    limit
                )
            }
        }
    }

    fn spawn_on_main_task<F: Future>(&self, future: F) -> async_task::Task<F::Output> {
        let sender = self.handle.sender.clone();
        let info = Arc::new(TaskInfo::new(NodeId(0), TaskId(0), "main".into()));
        let (runnable, task) = unsafe {
            // Safety: The schedule is not Sync,
            // the task's Waker must be used and dropped on the original thread.
            async_task::spawn_unchecked(future, move |runnable| {
                sender.send((runnable, info.clone())).unwrap();
            })
        };
        runnable.schedule();
        task
    }

    /// bz: pick a yield/non-yield task
    fn pick_task(&self) -> Result<Option<(Runnable, Arc<TaskInfo>)>, TryRecvError> {
        let release = get_release();
        let len_order = self.handle.size_of_order();
        if release && len_order > 0 {
            return self.yield_queue.try_recv_yield(&self.rand);
        } else {
            return self.queue.try_recv_random(&self.rand);
        }
    }

    /// Drain all tasks from ready queue and run them.
    fn run_all_ready(&self) {
        let hook_guard = Arc::new(PanicHookGuard::new());
        let hook_guard_clone = Arc::downgrade(&hook_guard);
        std::panic::set_hook(Box::new(move |panic_info| {
            if panic_info
                .payload()
                .downcast_ref::<PanicWrapper>()
                .is_none()
            {
                if let Some(old_hook) = hook_guard_clone.upgrade() {
                    old_hook.call_hook(panic_info);
                }
            }
        }));

        let mut trys: u64 = 0;
        // // while let Ok((runnable, info)) = self.queue.try_recv_random(&self.rand) {
        // // while let Ok((runnable, info)) = self.queue.try_simple_schedule(&self.rand) {
        // while let Ok(ret) = self.queue.try_simple_schedule(&self.rand) {
        while let Ok(ret) = self.pick_task() {
            if ret.is_none() {
                // TODO: bz: this seems like it has never been called
                if trys >= RETRY_PICK_LIMIT {
                    // bz: assume no more non-yield tasks can be executed anymore, let yield tasks get ready
                    if DEBUG {
                        info!("RELEASE ready (due to long waiting and no more non-yield tasks to execute).")
                    }
                    self.handle.set_release_and_wake_last_captures();
                    continue;
                }

                // bz: cannot find any task in queue that is not in order, we wait for a while and retry
                trys += 1;
                if DEBUG {
                    info!(
                        "executor waits for 1 second and retry the {:?} time ...",
                        trys
                    );
                }
                sleep(Duration::from_secs(1));
                continue;
            }

            let (runnable, info) = ret.unwrap();

            // normal execution
            if *info.killed.borrow() {
                // killed task: must enter the task before dropping it, so that
                // Drop impls can run.
                let _guard = crate::context::enter_task(info);
                std::mem::drop(runnable);
                continue;
            } else if info.paused.load(Ordering::SeqCst) {
                // paused task: push to waiting list
                let mut nodes = self.nodes.lock().unwrap();
                nodes.get_mut(&info.node()).unwrap().paused.push(runnable);
                continue;
            }

            // run task
            if DEBUG_DETAIL && info.name() != "main" {
                info!(
                    // bz: debug
                    "-> executor running task with node id {:?}, name {:?}, task id {:?}",
                    info.node(),
                    info.name(),
                    info.task_id,
                );
            }

            let node_id = info.node();
            let _guard = crate::context::enter_task(info);
            let panic_guard = PanicGuard(self);

            let result = std::panic::catch_unwind(|| {
                runnable.run();
            });

            // if let Some(capture) = take_last_capture() {
            //     info!("wake LAST_CAPTURE (executor)"); // bz: debug
            //     let (_task, waker, _captured_stack) = &*capture;
            //     waker.wake_by_ref();
            //     // // Examine stack trace of previously yielded task
            //     // info!(
            //     //     "captured task info: {:?}\ncaptured stack: {:?}",
            //     //     _task.node(),
            //     //     _captured_stack
            //     // ); // bz: debug
            // }

            self.handle.wake_last_captures(); // bz: see if we can wake all yield tasks

            if let Err(err) = result {
                if let Some(panic_info) = err.downcast_ref::<PanicWrapper>() {
                    if let Some(restart_after) = panic_info.restart_after {
                        let task = self.spawn_on_main_task(async move {
                            crate::time::sleep(restart_after).await;
                            info!("restarting node {}", node_id);
                            runtime::Handle::current().restart(node_id);
                        });

                        task.fallible().detach();
                    }
                } else {
                    std::panic::resume_unwind(err);
                }
            }

            // panic guard only runs if runnable.run() panics - in that case
            // we must drop all tasks before exiting the task, since they may have Drop impls that
            // assume access to the current task/runtime.
            std::mem::forget(panic_guard);

            // advance time: 50-100ns
            let dur = Duration::from_nanos(self.rand.with(|rng| rng.gen_range(50..100)));
            self.time.advance(dur);
        }
    }
}

struct PanicGuard<'a>(&'a Executor);
impl<'a> Drop for PanicGuard<'a> {
    fn drop(&mut self) {
        trace!("panic detected - dropping all tasks immediately");
        self.0.queue.clear_inner();
    }
}

impl Deref for Executor {
    type Target = TaskHandle;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

#[derive(Clone)]
pub(crate) struct TaskHandle {
    sender: Sender,
    nodes: Arc<Mutex<HashMap<NodeId, Node>>>,
    next_node_id: Arc<AtomicU64>,
    next_task_id: Arc<AtomicU64>,

    // bz: random yield selections
    yield_node_id: Arc<AtomicU64>, // the node id we are going to yield all its tasks, initialize to 0

    // bz: record the order of yield tasks
    order: Arc<Mutex<Vec<Arc<TaskInfo>>>>,
    last_captures: Arc<Mutex<Vec<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>>>,

    // bz: sender of yield tasks
    yield_sender: Sender,

    // bz: record paused node id
    paused_node_id: Arc<Mutex<Vec<Arc<NodeId>>>>,
}
assert_send_sync!(TaskHandle);

struct Node {
    info: Arc<TaskInfo>,
    paused: Vec<Runnable>,
    /// A function to spawn the initial task.
    init: Option<Arc<dyn Fn(&TaskNodeHandle) + Send + Sync>>,
}

impl TaskHandle {
    /// bz: get size of order
    pub fn size_of_order(&self) -> usize {
        let order = self.order.lock().unwrap();
        return order.len();
    }

    /// bz: true if a task is in order
    pub fn is_in_order(&self, info: Arc<TaskInfo>) -> bool {
        let order = self.order.lock().unwrap();
        for x in order.iter() {
            match x.task_id == info.task_id {
                true => {
                    return true;
                }
                false => {
                    continue;
                }
            }
        }
        return false;
    }

    /// bz: wake all in last_captures
    pub fn wake_last_captures(&self) {
        let mut last_captures = self.last_captures.lock().unwrap();
        if get_release() && last_captures.len() > 0 {
            // bz:  wake up the yield tasks
            if DEBUG {
                info!("{}", ".....".repeat(15));
                info!("Wake all yield tasks and clear last_captures.");
            }

            for capture in last_captures.iter() {
                let (_task, waker, _captured_stack) = (*capture).deref();
                waker.wake_by_ref();

                if DEBUG {
                    info!(" - waking task id {:?}", _task.task_id);
                }
            }

            if DEBUG {
                info!("{}", ".....".repeat(15));
            }

            last_captures.clear();
        }
    }

    /// bz: set RELEASE true and wake all in last_captures
    pub fn set_release_and_wake_last_captures(&self) {
        set_release_to_true();
        self.wake_last_captures();
    }

    /// bz: push paused node id to paused_node_id
    pub fn push_paused_node_id(&self, id: NodeId) {
        let mut paused_node_id = self.paused_node_id.lock().unwrap();
        paused_node_id.push(id.into());
    }

    /// bz: resume the paused node id from paused_node_id
    pub fn resume_paused_node_id(&self) {
        if DEBUG {
            info!("resume paused kill/delete node tasks:")
        }
        let paused_node_id = self.paused_node_id.lock().unwrap();
        for id in paused_node_id.iter() {
            if DEBUG {
                info!(" - resume {:?}", id);
            }
            crate::runtime::Handle::current().resume(**id);
        }
    }

    /// Kill all tasks of the node.
    pub fn kill(&self, id: NodeId) {
        TimeHandle::current().disable_node_and_cancel_timers(id);

        let mut nodes = self.nodes.lock().unwrap();
        let node = nodes.get_mut(&id).expect("node not found");
        node.paused.clear();

        if DEBUG_DETAIL {
            info!(
                "killing task: with node id {:?}, name {:?}, task id {:?}",
                id,
                node.info.name(),
                *node.info.task_id
            );
        }

        let new_info = Arc::new(TaskInfo::new(id, *node.info.task_id, node.info.name()));
        let old_info = std::mem::replace(&mut node.info, new_info);
        old_info.killed.send_replace(true);
    }

    /// Kill all tasks of the node and restart the initial task.
    pub fn restart(&self, id: NodeId) {
        self.kill(id);
        TimeHandle::current().enable_node(id);

        let nodes = self.nodes.lock().unwrap();
        let node = nodes.get(&id).expect("node not found");
        if let Some(init) = &node.init {
            init(&TaskNodeHandle {
                sender: self.sender.clone(),
                info: node.info.clone(),
                yield_sender: self.yield_sender.clone(),
            });
        }
    }

    /// Pause all tasks of the node.
    pub fn pause(&self, id: NodeId) {
        let nodes = self.nodes.lock().unwrap();
        let node = nodes.get(&id).expect("node not found");
        node.info.paused.store(true, Ordering::SeqCst);
    }

    /// Resume the execution of the address.
    pub fn resume(&self, id: NodeId) {
        let mut nodes = self.nodes.lock().unwrap();
        let node = nodes.get_mut(&id).expect("node not found");
        node.info.paused.store(false, Ordering::SeqCst);

        // take paused tasks from waiting list and push them to ready queue
        for runnable in node.paused.drain(..) {
            self.sender.send((runnable, node.info.clone())).unwrap();
        }
    }

    /// Create a new node.
    pub fn create_node(
        &self,
        name: Option<String>,
        init: Option<Arc<dyn Fn(&TaskNodeHandle) + Send + Sync>>,
    ) -> TaskNodeHandle {
        let id = NodeId(self.next_node_id.fetch_add(1, Ordering::SeqCst));
        let name = name.unwrap_or_else(|| format!("node-{}", id.0));
        let task_id = TaskId(self.next_task_id.fetch_add(1, Ordering::SeqCst));

        if DEBUG_DETAIL {
            info!(
                // bz: debug
                "creating node id = {:?} name = {:?} task id = {:?}",
                id, name, task_id
            );
        }

        let info = Arc::new(TaskInfo::new(id, task_id, name));
        let handle = TaskNodeHandle {
            sender: self.sender.clone(),
            info: info.clone(),
            yield_sender: self.yield_sender.clone(),
        };
        if let Some(init) = &init {
            init(&handle);
        }
        let node = Node {
            info,
            paused: vec![],
            init,
        };
        self.nodes.lock().unwrap().insert(id, node);
        handle
    }

    pub fn delete_node(&self, id: NodeId) {
        self.kill(id);
        let mut nodes = self.nodes.lock().unwrap();
        assert!(nodes.remove(&id).is_some());
    }

    /// Get the node handle.
    pub fn get_node(&self, id: NodeId) -> Option<TaskNodeHandle> {
        let nodes = self.nodes.lock().unwrap();
        let info = nodes.get(&id)?.info.clone();
        Some(TaskNodeHandle {
            sender: self.sender.clone(),
            info,
            yield_sender: self.yield_sender.clone(),
        })
    }
}

#[derive(Clone)]
pub(crate) struct TaskNodeHandle {
    sender: Sender,
    info: Arc<TaskInfo>,
    yield_sender: Sender, // bz:
}

assert_send_sync!(TaskNodeHandle);

impl TaskNodeHandle {
    pub fn current() -> Self {
        Self::try_current().unwrap()
    }

    pub fn try_current() -> Option<Self> {
        let info = crate::context::try_current_task()?;
        let sender = crate::context::try_current(|h| h.task.sender.clone())?;
        let yield_sender = crate::context::try_current(|h| h.task.yield_sender.clone())?;
        Some(TaskNodeHandle {
            sender,
            info,
            yield_sender,
        })
    }

    pub(crate) fn id(&self) -> NodeId {
        self.info.node()
    }

    /// bz: only tasks spawned by a node is called by this spawn function
    pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        // bz: this is a new task spawned by the node, create a new info
        let handle = runtime::Handle::current();
        let next_task_id = handle.task.next_task_id;
        let new_task_id = TaskId(next_task_id.fetch_add(1, Ordering::SeqCst));
        let new_info = Arc::new(TaskInfo::new(
            self.info.inner.node,
            new_task_id,
            self.info.inner.name.clone(),
        ));

        if DEBUG_DETAIL {
            info!(
                // bz: debug
                "creating task: node id = {:?} name = {:?} task id = {:?} (increment task id)",
                new_info.inner.node, new_info.inner.name, new_info.task_id
            );
        }

        self.spawn_local(future, Some(new_info))
    }

    pub fn spawn_local<F>(
        &self,
        future: F,
        new_info: Option<Arc<TaskInfo>>,
    ) -> JoinHandle<F::Output>
    where
        F: Future + 'static,
        F::Output: 'static,
    {
        let sender = self.sender.clone();
        let yield_sender = self.yield_sender.clone();
        let info = self.info.clone();
        let mut killed_rx = info.killed.subscribe();

        let future = async move {
            pin_mut!(future);
            loop {
                select! {
                    _ = killed_rx.changed() => {
                        if *killed_rx.borrow() {
                            // when a cancelled task is run by run_all_ready(), it is dropped rather
                            // than being executed. Therefore this should never run. However, we must
                            // poll killed_rx in order to force this task to wake up when its node is
                            // killed. (Otherwise the task will not be dropped until its next
                            // scheduled wakeup, which may be never if it is listening for network
                            // messages).
                            panic!("killed task must not run!");
                        }
                    }

                    output = &mut future => {
                        break output;
                    }
                }
            }
        };

        let (runnable, task) = unsafe {
            // Safety: The schedule is not Sync,
            // the task's Waker must be used and dropped on the original thread.
            async_task::spawn_unchecked(future, move |runnable| {
                // info!( // bz: debug
                //     "get backtrace:\n{}",
                //     std::backtrace::Backtrace::force_capture()
                // );
                match &new_info {
                    Some(_info) => {
                        // bz: from newly created tasks or resume pending tasks
                        let handle = runtime::Handle::current();
                        if handle.task.is_in_order(_info.clone()) {
                            if DEBUG_DETAIL {
                                // bz: debug
                                info!(
                                    "-> send in TaskNodeHandle (yield_sender): node id {:?}, name {:?}, task id {:?}",
                                    _info.inner.node, _info.inner.name, _info.task_id
                                );
                            }
                            let _ = yield_sender.send((runnable, _info.clone()));
                        } else {
                            if DEBUG_DETAIL && _info.inner.name != "main" {
                                // bz: debug
                                info!(
                                    "-> send in TaskNodeHandle: node id {:?}, name {:?}, task id {:?}",
                                    _info.inner.node, _info.inner.name, _info.task_id
                                );
                            }
                            let _ = sender.send((runnable, _info.clone()));
                        }
                    }
                    None => {
                        // bz: from create_node()
                        if DEBUG_DETAIL && info.inner.name != "main" {
                            // bz: debug
                            info!(
                                "-> send in TaskNodeHandle: node id {:?}, name {:?}, task id {:?}",
                                info.inner.node, info.inner.name, info.task_id
                            );
                        }

                        let _ = sender.send((runnable, info.clone()));
                    }
                }
            })
        };
        runnable.schedule();

        JoinHandle {
            id: runtime_task::next_task_id(),
            inner: Arc::new(InnerHandle::new(Mutex::new(Some(task.fallible())))),
        }
    }

    pub fn enter(&self) -> crate::context::TaskEnterGuard {
        crate::context::enter_task(self.info.clone())
    }

    pub async fn await_future_in_node<F: Future>(&self, fut: F) -> F::Output {
        let wrapped = TaskEnteringFuture::new(self.info.clone(), fut);
        wrapped.await
    }
}

// Polls a wrapped future, entering the given task before each poll().
struct TaskEnteringFuture<F: Future> {
    task: Arc<TaskInfo>,
    inner: Pin<Box<F>>,
}

impl<F: Future> TaskEnteringFuture<F> {
    fn new(task: Arc<TaskInfo>, inner: F) -> Self {
        Self {
            task,
            inner: Box::pin(inner),
        }
    }
}

impl<F> Future for TaskEnteringFuture<F>
where
    F: Future,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let _guard = crate::context::enter_task(self.task.clone());
        self.inner.as_mut().poll(cx)
    }
}

/// Spawns a new asynchronous task, returning a [`JoinHandle`] for it.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handle = TaskNodeHandle::current();
    handle.spawn(future)
}

/// Spawns a `!Send` future on the local task set.
pub fn spawn_local<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + 'static,
    F::Output: 'static,
{
    let handle = TaskNodeHandle::current();
    // TODO: NOTE: to be able to capture and udpate task id for spawned tasks by a node, i have to add a Option<Arc<TaskInfo>> here
    // as input parameter, but I did not see this function been triggered yet
    // tmp give it a None
    info!(
        "crate::task::spawn_local()\nbacktrace:\n{}",
        std::backtrace::Backtrace::force_capture()
    ); // bz: debug
    handle.spawn_local(future, None)
}

/// Runs the provided closure on a thread where blocking is acceptable.
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let handle = TaskNodeHandle::current();
    handle.spawn(async move { f() })
}

#[derive(Debug)]
struct InnerHandle<T> {
    task: Mutex<Option<FallibleTask<T>>>,
}

impl<T> InnerHandle<T> {
    fn new(task: Mutex<Option<FallibleTask<T>>>) -> Self {
        Self { task }
    }

    // Important: Because InnerHandle is type erased and then un-erased with T = (),
    // we can't call any on methods on FallibleTask that deal with T. The drop and is_finished
    // methods only access the task header pointer, and don't depend on the type.
    // TODO: this really needs support from async-task for abort handles.
    fn abort(&self) {
        self.task.lock().unwrap().take();
    }

    fn is_finished(&self) -> bool {
        self.task
            .lock()
            .unwrap()
            .as_ref()
            .map(|task| task.is_finished())
            .unwrap_or(true)
    }
}

/// An owned permission to join on a task (await its termination).
#[derive(Debug)]
pub struct JoinHandle<T> {
    id: runtime_task::Id,
    inner: Arc<InnerHandle<T>>,
}

impl<T> JoinHandle<T> {
    /// Abort the task associated with the handle.
    pub fn abort(&self) {
        self.inner.abort();
    }

    /// Check if the task associate with the handle is finished.
    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    /// Cancel the task when this handle is dropped.
    #[doc(hidden)]
    pub fn cancel_on_drop(self) -> FallibleTask<T> {
        self.inner.task.lock().unwrap().take().unwrap()
    }

    /// Return an AbortHandle corresponding for the task.
    pub fn abort_handle(&self) -> AbortHandle {
        let inner = ErasablePtr::erase(Box::new(self.inner.clone()));
        let id = self.id.clone();
        AbortHandle { id, inner }
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut lock = self.inner.task.lock().unwrap();
        let task = lock.as_mut();
        if task.is_none() {
            return std::task::Poll::Ready(Err(join_error::cancelled(self.id.clone())));
        }
        std::pin::Pin::new(task.unwrap()).poll(cx).map(|res| {
            // TODO: decide cancelled or panic
            res.ok_or(join_error::cancelled(self.id.clone()))
        })
    }
}

impl<T> Drop for JoinHandle<T> {
    fn drop(&mut self) {
        if let Some(task) = self.inner.task.lock().unwrap().take() {
            task.detach();
        }
    }
}

/// AbortHandle allows aborting, but not awaiting the return value.
pub struct AbortHandle {
    id: runtime_task::Id,
    inner: ErasedPtr,
}

unsafe impl Send for AbortHandle {}
unsafe impl Sync for AbortHandle {}

impl AbortHandle {
    /// abort the task
    pub fn abort(&self) {
        let inner = self.inner();
        inner.abort();
        std::mem::forget(inner);
    }

    /// Check if the task associate with the handle is finished.
    pub fn is_finished(&self) -> bool {
        let inner = self.inner();
        let ret = inner.is_finished();
        std::mem::forget(inner);
        ret
    }

    fn inner(&self) -> Box<Arc<InnerHandle<()>>> {
        unsafe { ErasablePtr::unerase(self.inner) }
    }
}

impl Drop for AbortHandle {
    fn drop(&mut self) {
        // must turn our erased pointer back into a Box and drop it.
        let inner = self.inner();
        std::mem::drop(inner);
    }
}

impl UnwindSafe for AbortHandle {}
impl RefUnwindSafe for AbortHandle {}

impl fmt::Debug for AbortHandle {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt.debug_struct("AbortHandle")
            .field("id", &self.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        runtime::{Handle, NodeHandle, Runtime},
        time,
    };
    use join_set::JoinSet;
    use std::{collections::HashSet, sync::atomic::AtomicUsize, time::Duration};

    #[test]
    fn spawn_in_block_on() {
        let runtime = Runtime::new();
        runtime.block_on(async {
            spawn(async { 1 }).await.unwrap();
            spawn_local(async { 2 }).await.unwrap();
        });
    }

    #[test]
    fn kill() {
        let runtime = Runtime::new();
        let node1 = runtime.create_node().build();
        let node2 = runtime.create_node().build();

        let flag1 = Arc::new(AtomicUsize::new(0));
        let flag2 = Arc::new(AtomicUsize::new(0));

        let flag1_ = flag1.clone();
        node1.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag1_.fetch_add(2, Ordering::SeqCst);
            }
        });

        let flag2_ = flag2.clone();
        node2.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag2_.fetch_add(2, Ordering::SeqCst);
            }
        });

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag1.load(Ordering::SeqCst), 2);
            assert_eq!(flag2.load(Ordering::SeqCst), 2);
            Handle::current().kill(node1.id());
            Handle::current().kill(node1.id());

            time::sleep_until(t0 + Duration::from_secs(5)).await;
            assert_eq!(flag1.load(Ordering::SeqCst), 2);
            assert_eq!(flag2.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn restart() {
        let runtime = Runtime::new();

        let flag = Arc::new(AtomicUsize::new(0));

        let flag_ = flag.clone();
        let node = runtime
            .create_node()
            .init(move || {
                let flag = flag_.clone();
                async move {
                    // set flag to 0, then +2 every 2s
                    flag.store(0, Ordering::SeqCst);
                    loop {
                        time::sleep(Duration::from_secs(2)).await;
                        flag.fetch_add(2, Ordering::SeqCst);
                    }
                }
            })
            .build();

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);
            Handle::current().kill(node.id());
            Handle::current().restart(node.id());

            time::sleep_until(t0 + Duration::from_secs(6)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);

            time::sleep_until(t0 + Duration::from_secs(8)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn pause_resume() {
        let runtime = Runtime::new();
        let node = runtime.create_node().build();

        let flag = Arc::new(AtomicUsize::new(0));
        let flag_ = flag.clone();
        node.spawn(async move {
            loop {
                time::sleep(Duration::from_secs(2)).await;
                flag_.fetch_add(2, Ordering::SeqCst);
            }
        });

        runtime.block_on(async move {
            let t0 = time::Instant::now();

            time::sleep_until(t0 + Duration::from_secs(3)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);
            Handle::current().pause(node.id());
            Handle::current().pause(node.id());

            time::sleep_until(t0 + Duration::from_secs(5)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 2);

            Handle::current().resume(node.id());
            Handle::current().resume(node.id());
            time::sleep_until(t0 + Duration::from_secs_f32(5.5)).await;
            assert_eq!(flag.load(Ordering::SeqCst), 4);
        });
    }

    #[test]
    fn random_select_from_ready_tasks() {
        let mut seqs = HashSet::new();
        for seed in 0..10 {
            let runtime = Runtime::with_seed_and_config(seed, crate::SimConfig::default());
            let seq = runtime.block_on(async {
                let (tx, rx) = std::sync::mpsc::channel();
                let mut tasks = vec![];
                for i in 0..3 {
                    let tx = tx.clone();
                    tasks.push(spawn(async move {
                        for j in 0..5 {
                            tx.send(i * 10 + j).unwrap();
                            tokio::task::yield_now().await;
                        }
                    }));
                }
                drop(tx);
                futures::future::join_all(tasks).await;
                rx.into_iter().collect::<Vec<_>>()
            });
            seqs.insert(seq);
        }
        assert_eq!(seqs.len(), 10);
    }

    #[test]
    fn await_future_in_node() {
        let runtime = Runtime::new();
        let node1 = runtime.create_node().build();
        let node2 = runtime.create_node().build();
        let node1_id = node1.id();

        runtime.block_on(async move {
            node1
                .spawn(async move {
                    let id = node2
                        .await_future_in_node(async move {
                            tokio::task::yield_now().await;
                            NodeHandle::current().id()
                        })
                        .await;

                    assert_eq!(id, node2.id());
                    assert_eq!(NodeHandle::current().id(), node1_id);
                })
                .await
                .unwrap();
        });
    }

    #[test]
    fn test_abort() {
        let runtime = Runtime::new();

        fn panic_int() -> i32 {
            panic!();
        }

        runtime.block_on(async move {
            let jh = spawn(async move {
                time::sleep(Duration::from_secs(5)).await;
                panic_int()
            });
            time::sleep(Duration::from_secs(1)).await;
            jh.abort();
            jh.await.unwrap_err();
        });

        runtime.block_on(async move {
            let jh = spawn(async move {
                time::sleep(Duration::from_secs(5)).await;
                panic_int()
            });
            time::sleep(Duration::from_secs(1)).await;
            let ah = jh.abort_handle();
            ah.abort();
            jh.await.unwrap_err();
        });
    }

    #[test]
    fn test_joinset() {
        let runtime = Runtime::new();

        // test joining
        runtime.block_on(async move {
            let mut join_set = JoinSet::new();

            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                3
            });
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(2)).await;
                2
            });
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(1)).await;
                1
            });

            let mut res = Vec::new();
            while let Some(next) = join_set.join_next().await {
                res.push(next.unwrap());
            }
            assert_eq!(res, vec![1, 2, 3]);
        });

        // test cancelling
        runtime.block_on(async move {
            let mut join_set = JoinSet::new();

            // test abort_all()
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                panic!();
            });
            time::sleep(Duration::from_secs(1)).await;
            join_set.abort_all();
            time::sleep(Duration::from_secs(5)).await;

            // test drop
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                panic!();
            });
            time::sleep(Duration::from_secs(1)).await;
            std::mem::drop(join_set);
            time::sleep(Duration::from_secs(5)).await;
        });

        // test detach
        runtime.block_on(async move {
            let flag = Arc::new(AtomicBool::new(false));
            let mut join_set = JoinSet::new();

            let flag1 = flag.clone();
            join_set.spawn(async move {
                time::sleep(Duration::from_secs(3)).await;
                flag1.store(true, Ordering::Relaxed);
            });
            time::sleep(Duration::from_secs(1)).await;
            join_set.detach_all();
            time::sleep(Duration::from_secs(5)).await;
            assert_eq!(flag.load(Ordering::Relaxed), true);
        });
    }
}

/// bz: mpsc for executor, remove generic types. NOTE: the following code should be separate from the file ... however, its hard ...
///! A multi-producer, single-consumer queue but allows
///! consumer to randomly choose an element from the queue.
// use crate::rand::GlobalRng;
// use rand::Rng;
use std::sync::Weak;

/// Creates a new asynchronous channel, returning the sender/receiver halves.
pub fn channel() -> (Sender, Receiver) {
    let inner = Arc::new(Inner {
        queue: Mutex::new(Vec::new()),
    });
    let sender = Sender {
        inner: Arc::downgrade(&inner),
    };
    let recver = Receiver { inner };
    (sender, recver)
}

/// The sending-half of Rust’s asynchronous [`channel`] type.
pub struct Sender {
    inner: Weak<Inner>,
}

/// The receiving half of Rust’s [`channel`] type.
pub struct Receiver {
    inner: Arc<Inner>,
}

struct Inner {
    queue: Mutex<Vec<(Runnable, Arc<TaskInfo>)>>,
}

impl Clone for Sender {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish_non_exhaustive()
    }
}

/// An error returned from the `Sender::send` function on channels.
pub struct SendError<T>(pub T);

impl Sender {
    /// Attempts to send a value on this channel, returning it back if it could not be sent.
    pub fn send(
        &self,
        value: (Runnable, Arc<TaskInfo>),
    ) -> Result<(), SendError<(Runnable, Arc<TaskInfo>)>> {
        if let Some(inner) = self.inner.upgrade() {
            inner.queue.lock().unwrap().push(value);
            Ok(())
        } else {
            Err(SendError(value))
        }
    }
}

/// This enumeration is the list of the possible reasons
/// that `try_recv_random` could not return data when called.
pub enum TryRecvError {
    /// empty
    Empty,
    /// disconnected
    Disconnected,
}

impl Receiver {
    /// Attempts to return a pending value on this receiver without blocking.
    pub fn try_recv_random(
        &self,
        rng: &GlobalRng,
    ) -> Result<Option<(Runnable, Arc<TaskInfo>)>, TryRecvError> {
        let mut queue = self.inner.queue.lock().unwrap();

        if DEBUG_DETAIL {
            info!("{}", "~~~~~".repeat(15));
            info!(
                "try_recv_random: #queue = {:?} #release = {:?}",
                queue.len(),
                RELEASE,
            );
            info!("{}", "~~~~~".repeat(15));
        }

        if !queue.is_empty() {
            let idx = rng.with(|rng| rng.gen_range(0..queue.len()));
            Ok(Some(queue.swap_remove(idx)))
        } else if Arc::weak_count(&self.inner) == 0 {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    /// bz: we send the yield tasks back in a reverse order of Executor.handle.order
    pub fn try_recv_yield(
        &self,
        rng: &GlobalRng,
    ) -> Result<Option<(Runnable, Arc<TaskInfo>)>, TryRecvError> {
        let mut queue = self.inner.queue.lock().unwrap();
        if !queue.is_empty() {
            let handle = runtime::Handle::current();
            let len_order = handle.task.size_of_order();

            if DEBUG {
                info!("{}", "~~~~~".repeat(15));
                info!(
                    "try_recv_yield: #queue = {:?} #order = {:?} #release = {:?}",
                    queue.len(),
                    len_order,
                    RELEASE,
                );
            }

            assert!(
                len_order > 0,
                "try_recv_yield: the order should not be empty."
            );

            let handle = runtime::Handle::current();
            let mut order = handle.task.order.lock().unwrap();

            // bz: we return yield tasks in reverse order firstly
            if let Some(last) = order.pop() {
                for (idx, e) in queue.iter().enumerate() {
                    // bz: find which idx in queue is the last element in order
                    let (_, info) = e;
                    if last == info.clone() {
                        if DEBUG {
                            info!(
                                "try_recv_yield: PICK YIELD TASK with idx = {:?}",
                                last.task_id
                            );
                            info!("{}", "~~~~~".repeat(15));
                        }

                        drop(order);
                        return Ok(Some(queue.swap_remove(idx)));
                    }
                }
            }

            // bz: when we have multiple calls of instrumented_yield()
            // randomly pick a yield task from order
            // TODO: better solution
            let idx = rng.with(|rng| rng.gen_range(0..order.len()));
            let pick = order.remove(idx);
            for (idx, e) in queue.iter().enumerate() {
                // bz: find which idx in queue is the last element in order
                let (_, info) = e;
                if pick == info.clone() {
                    if DEBUG {
                        info!(
                            "try_recv_yield: RABDOMLY PICK YIELD TASK with idx = {:?}",
                            pick.task_id
                        );
                        info!("{}", "~~~~~".repeat(15));
                    }

                    drop(order);
                    return Ok(Some(queue.swap_remove(idx)));
                }
            }

            panic!("panic in try_recv_yield: should not come to this point.");
            
        } else if Arc::weak_count(&self.inner) == 0 {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }

    // /// bz: a simplest try: we send the tasks back in a reverse order of Executor.handle.order
    // pub fn try_simple_schedule(
    //     &self,
    //     rng: &GlobalRng,
    // ) -> Result<Option<(Runnable, Arc<TaskInfo>)>, TryRecvError> {
    //     let mut queue = self.inner.queue.lock().unwrap();
    //     if !queue.is_empty() {
    //         let handle = runtime::Handle::current();
    //         let len_order = handle.task.size_of_order();
    //         if len_order == 0 {
    //             // bz: randomly choose one task, since RELEASE = false or order.len() == 0
    //             let idx = rng.with(|rng| rng.gen_range(0..queue.len()));

    //             if DEBUG_DETAIL {
    //                 info!("try_simple_schedule: RANDOM TASK with idx = {:?}", idx);
    //             }

    //             return Ok(Some(queue.swap_remove(idx)));
    //         }

    //         let release = get_release();

    //         if DEBUG {
    //             info!("{}", "~~~~~".repeat(15));
    //             info!(
    //                 "try_simple_schedule: #queue = {:?} #order = {:?} #release = {:?}",
    //                 queue.len(),
    //                 len_order,
    //                 release,
    //             );
    //         }

    //         let mut order = handle.task.order.lock().unwrap();
    //         if release {
    //             // bz: we return yield tasks in reverse order firstly
    //             if let Some(last) = order.pop() {
    //                 for (idx, e) in queue.iter().enumerate() {
    //                     // bz: find which idx in queue is the last element in order
    //                     let (_, info) = e;
    //                     if last.task_id == info.task_id {
    //                         if DEBUG {
    //                             info!(
    //                                 "try_simple_schedule: PICK YIELD TASK with idx = {:?}",
    //                                 last.task_id
    //                             );
    //                             info!("{}", "~~~~~".repeat(15));
    //                         }

    //                         drop(order);
    //                         return Ok(Some(queue.swap_remove(idx)));
    //                     }
    //                 }
    //             }

    //             assert!(
    //                 true,
    //                 "panic in try_simple_schedule: should not come to this point."
    //             )
    //         }

    //         // bz: random choose one task, but make sure do not release yield tasks
    //         for (idx, y) in queue.iter().enumerate() {
    //             let mut found_idx = true;
    //             for x in order.iter() {
    //                 match x.task_id == y.1.task_id {
    //                     true => {
    //                         if DEBUG_DETAIL {
    //                             info!("order={}, queue={}", x.task_id, y.1.task_id);
    //                         }
    //                         found_idx = false;
    //                         break;
    //                     }
    //                     false => {
    //                         if DEBUG_DETAIL {
    //                             info!("queue={}, no matching in order", y.1.task_id);
    //                         }
    //                     }
    //                 }
    //             }
    //             if found_idx {
    //                 // bz: tmp return this TODO: random this selection
    //                 if DEBUG {
    //                     info!(
    //                         "try_simple_schedule: PICK RANDOM NON-YIELD TASK with idx = {:?}",
    //                         queue[idx].1.task_id
    //                     );
    //                     info!("{}", "~~~~~".repeat(15));
    //                 }

    //                 drop(order);
    //                 return Ok(Some(queue.swap_remove(idx)));
    //             }
    //         }
    //         drop(order);

    //         // bz: cannot find any task in queue that is not in order
    //         if DEBUG {
    //             info!("try_simple_schedule: PICK RANDOM NON-YIELD TASK -> cannot find any task in queue that is not in order.");
    //             info!("{}", "~~~~~".repeat(15));
    //         }

    //         return Ok(None);
    //     } else if Arc::weak_count(&self.inner) == 0 {
    //         Err(TryRecvError::Disconnected)
    //     } else {
    //         Err(TryRecvError::Empty)
    //     }
    // }

    /// clear
    pub fn clear_inner(&self) {
        let mut old = Vec::new();
        {
            let mut queue = self.inner.queue.lock().unwrap();
            std::mem::swap(&mut old, &mut queue);
        }
        // must release lock before dropping queue.
    }
}
