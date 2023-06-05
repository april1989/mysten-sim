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

#### async messages in test_create_advance_epoch_tx_race
We simulate the async messages in the test, where we run a jsonrpsee dummy server instead of running a sui validator like `test_create_advance_epoch_tx_race`,
and use `real_tokio::task::spawn` instead of `register_fail_point_async`.

```shell
                               TestClusterBuilder                   
                                      | [create]
                                      ⌄
                                    node2 (node id = 2)
                                      | [add to queue]
                                      ⌄
                          msim::sim::task::run_all_ready() 
                                      | [whenever it's node2's turn, let it await for broadcast msg]
                                      ⌄
panic!("safe mode recorded"); <---- node2 (fail_point!("record_checkpoint_builder_is_safe_mode_metric"); @sui/crates/sui-core/src/authority/authority_per_epoch_store.rs)                                     
                                      |
                                      ⌄
register_fail_point_async     <---- node2 (fail_point_async!("change_epoch_tx_delay"); @sui/crates/sui-core/src/authority.rs) 
                                      |
                                      ⌄
register_fail_point_async     <---- node2 (fail_point_async!("reconfig_delay"); @sui/crates/sui-node/src/lib.rs)
                                      |
                                      ⌄
                                     ... (test sends txs to stop the async fns of register_fail_point_async)
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
  -> rewrite `mpsc::channel()`
- manipulate the order by resuming/pausing a task through `TaskHandle`

- implement task id of type `TaskId` for `TaskInfo`




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














