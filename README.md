# The Mysten Simulator for 

**This is a fork of https://github.com/madsim-rs/madsim**

The madsim project is building a tokio-like (but not 100% compatible) deterministic runtime.
This fork modifies the original project to produce a drop-in replacement for tokio.

## Usage:

### when running toy programs in `test-crates`

Run the following command in terminal:
```shell
cd test-crates/jsonrpsee-test
RUSTFLAGS="--cfg msim" cargo test 
```
or add `RUSTFLAGS` to environment variable.


### entry of simtest (macros)

`sim_test`@`msim_macros/src/lib.rs:158`, which has the main body of `parse_test`@`msim_macros/src/lib.rs:165`.


### toy_test

[toy_test](test-crates/toy-test) is a simple version of test_create_advance_epoch_tx_race. 

Run the following command:
```shell 
cd test-crates/toy-test
RUSTFLAGS="--cfg msim" cargo test
```



## Implementation

### Goals
1. Replace `register_fail_point_async` with `instrument_yield()`
2. Implement necessary steps in the scheduler of msim

### Things to do

`instrument_yield()` currently only captures the current stack trace, and `LAST_CAPTURE` is empty. 
We need to make the current task yield execution back to the scheduler (a.k.a., `Executor`) by: 

- wherever `instrument_yield()` is placed, we should pause the tasks there and wait for our scheduler. We can call `context::current_task()` to get the correponding `TaskInfo`, and push instances to `LAST_CAPTURE` in order to trigger yield by `Future::YieldToScheduler::poll()`

- `LAST_CAPTURE` is of type `Mutex<Option<Arc<(Arc<TaskInfo>, Waker, Backtrace)>>>`, which only stores one instance. We need a vec to maintain the order of received yield tasks, as well as a map between task id and its corrsponding `Arc<TaskInfo>, Waker, Backtrace`

- `Executor.handle.nodes` (of type `TaskHandle`) stores a map of node id (`NodeId`) and its node (a.k.a., runnable task), where `TaskHandle` controls a task's start/resume/pause.

- `TaskNodeHandle::spawn_local()` (called by `TaskEnteringFuture::spawn()`) is where a task has been add back to `Executor.sender` (is `TaskNodeHandle.sender` from `TaskHandle.sender` from `Executor.sender`), then get received and drained by `Executor.queue` in `Executor::run_all_ready()`.

- `Executor` currently randomly picks a task, where the random number is `Executor.rand` (created in function `Runtime::with_seed_and_config()`):
```Rust
let mut rand = rand::GlobalRng::new_with_seed(seed);
tokio::msim_adapter::util::reset_rng(rand.gen::<u64>());
```
and the task index is determined by `try_recv_random()`.

Another place to control the tasks is through `TaskHandle` (or `Handle`) to start/resume/pause a task directly. Meanwhile, `Executor` cooperates by checking `info.paused` or `info.killed` to avoid running a paused or killed tasks.

Hence, we can control the order of tasks in two ways:
- (use this) replace `try_recv_random()` to a scheduler
  -> rewrite `mpsc::channel()` to `task::channel()`
- manipulate the order by resuming/pausing a task through `TaskHandle`



### Scheduler
- step1, static analysis to identify potential points to insert instrumented_yield (manually insert a few instrumented_yield points)
  -> task-dependency analysis
- step2, profile to capture a trace => compute a set of schedules. *how to get a trace?*
- step3, enforce every schedule from the computed set of schedules. *for now, just assume we have a naive reverse order schedule*, e.g., 

consider we only have three `instrumented_yield` points: p1, p2, p3 running on task id 100, 200, 300. 
we emunerate all their orders:
100 -> 200 -> 300
100 -> 300 -> 200
...
rerun for 6 times





### Things already done

Currently, the scheduler only works when there is only one `instrumented_yield().await` called in a task.

- implement task id of type `TaskId` for `TaskInfo`
- implement `order` and `last_captures` in `TaskHandle` to record the order of yield tasks, wakers, and backtraces
- implement `RELEASE` a global flag of whether it is the time to release yield tasks
- logic: 
  * in `instrumented_yield()`, we record the task has been yield and store to `order`
  * in `YieldToScheduler::poll()`, we collect the corresponding yield taskinfo, waker and backtrace that has been just yielded in `instrumented_yield()`; the reason of why we have to collect the info here is because the waker is only available here under context
  * we count the number of called `instrumented_yield().await` from source code (i.e., increment `PASSED_INSTRUMENTED_YIELDS`); at the beginning of test, we randomly pick a number (i.e., `THRESHOLD`); whenever we have seen `THRESHOLD` of calls of `instrumented_yield().await`, we set `RELEASE` to true, wake all the yield tasks and send them to `Executor::run_all_ready()`
    - we might random a large number of `THRESHOLD` and we actually do not have that many yield tasks; if so, we release all yielded tasks in their reverse order in the loop of `Executor::block_on()`
  * the tasks are stored in two `channel`s: one for tasks from non-yield nodes, one for tasks from yield nodes (i.e., chosen by )
  * the next task is picked by `Receiver::try_simple_schedule()`: 
    - when there is no element in `order` or `RELEASE == false`, we randomly pick a task and return it to executor
    - when `RELEASE == true`, we return the yield tasks in their reverse order in `order`
    - when there are elements in `order` but `RELEASE == false`, we randomly pick a task that is not in `order` and return it to executor
  * during the waiting of release flag, other nodes might get deleted or killed by `Handle`; we pause those nodes, and resume their kills/deletes after executing yield tasks and returning to `Executor::block_on()`
- currently, we randomly choose a node to yield all its tasks; the chosen node id is `yield_node_id` in `TaskHandle`
- currently, for a task with two and more calls of `instrumented_yield()`, we randomly pick a task for those we have seen more than once

- NOTE: the code below `channel()` in `msim/src/sim/task.rs` should be in a separate file, however, we have to use the crate-private trait `TaskInfo` and for convenience of accessing the above fields, we put them in the `task.rs` file

- where to increment task id:
  * for a newly created node with the following trace, we increment the task id for the code executed by this node
  ```shell
   0: msim::sim::task::TaskNodeHandle::spawn_local::{{closure}}
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:797:45
   1: async_task::raw::RawTask<F,T,S>::schedule
             at /home/ubuntu/.cargo/git/checkouts/async-task-2c3ead35a682b15c/4e45b26/src/raw.rs:414:9
   2: async_task::runnable::Runnable::schedule
             at /home/ubuntu/.cargo/git/checkouts/async-task-2c3ead35a682b15c/4e45b26/src/runnable.rs:272:13
   3: msim::sim::task::TaskNodeHandle::spawn_local
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:802:9
   4: msim::sim::runtime::NodeBuilder::init::{{closure}} # -> creating a new node
             at /home/ubuntu/mysten-sim/msim/src/sim/runtime/mod.rs:404:13
   5: msim::sim::task::TaskHandle::create_node
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:684:13
   6: msim::sim::runtime::NodeBuilder::build
             at /home/ubuntu/mysten-sim/msim/src/sim/runtime/mod.rs:417:20
   7: toy_test::test::test_toy::{{closure}}::{{closure}}::{{closure}}
             at ./src/lib.rs:115:20
   8: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/9eb3afe9ebe9c7d2b84b71002d44f4a0edac95e0/library/core/src/future/future.rs:125:9
  ```
  * for a task spawned by a node with the following trace, we increment the task id for this task
  ```shell
   0: msim::sim::task::TaskNodeHandle::spawn_local::{{closure}}
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:797:45
   1: async_task::raw::RawTask<F,T,S>::schedule
             at /home/ubuntu/.cargo/git/checkouts/async-task-2c3ead35a682b15c/4e45b26/src/raw.rs:414:9
   2: async_task::runnable::Runnable::schedule
             at /home/ubuntu/.cargo/git/checkouts/async-task-2c3ead35a682b15c/4e45b26/src/runnable.rs:272:13
   3: msim::sim::task::TaskNodeHandle::spawn_local
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:802:9
   4: msim::sim::task::TaskNodeHandle::spawn # -> spawning a new task
             at /home/ubuntu/mysten-sim/msim/src/sim/task.rs:740:9
   5: msim::sim::runtime::NodeHandle::spawn
             at /home/ubuntu/mysten-sim/msim/src/sim/runtime/mod.rs:463:9
   6: toy_test::test::test_toy::{{closure}}::{{closure}}::{{closure}}
             at ./src/lib.rs:124:9
   7: <core::pin::Pin<P> as core::future::future::Future>::poll
             at /rustc/9eb3afe9ebe9c7d2b84b71002d44f4a0edac95e0/library/core/src/future/future.rs:125:9
  ```
  * to distinguish the two types of tasks, we update the task id in `msim::sim::task::TaskNodeHandle::spawn`

  * NOTE: uncertain change in `crate::task::spawn_local()`


#### Real scenario in test_create_advance_epoch_tx_race 

reproduce the race test with instrumented_yield:

- we have 4 validator nodes, each node spawns two tasks each called `instrumented_yield()`
- we only need to yield the two tasks in one validator node, as written in the test
- how can we panic when entering the safe mode without register fail_point?







### debug 
add code 
```rust
tracing::info!( // bz: debug
    "get backtrace:\n{}",
    std::backtrace::Backtrace::force_capture()
);
```