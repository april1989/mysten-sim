# The Mysten Simulator for Sui (Clean Version)

**This is a fork of https://github.com/madsim-rs/madsim**

The madsim project is building a tokio-like (but not 100% compatible) deterministic runtime.
This fork modifies the original project to produce a drop-in replacement for tokio.

## Usage:

1. use `sui_macro::instrumented_yield_id()` to instrument each suspicious program location, where `id` is of type `usize`. 
We have instrumented the 2 program locations that exposes the failed test `test_create_advance_epoch_tx_race` with `id = 11, 22`.

2. run the following command to start an automatic test:
```shell
cd sui 
MSIM_TEST_SCHEDULE=11-22 LOCAL_MSIM_PATH=$path_to_our_msim cargo simtest test_create_advance_epoch_tx_race 
# MSIM_TEST_SCHEDULE=11-22 LOCAL_MSIM_PATH=/home/ubuntu/mysten-sim-x cargo simtest test_create_advance_epoch_tx_race 
```


### statically detected races in Sui

`/sui_race/races.json` and `sui_race/races-authority_store.json`

### instrumented points in sui 

```rust
    use sui_macros::{instrumented_yield_id};
    println!("instrumented_yield in xxx");
    instrumented_yield_id!(xx);
```

`authority_store`: instrument 5 program points in async functions for 6 race pairs
1. `crates/sui-core/src/checkpoints/checkpoint_executor/mod.rs:507` instrumented_yield in execute_change_epoch_tx (in for loop) (race 1:1) 
2. `crates/sui-core/src/consensus_handler.rs:385` instrumented_yield in AsyncTransactionScheduler::run (in while loop) (race 1:2 3:1 3:2 4:1 5:2)
3. `crates/sui-core/src/checkpoints/checkpoint_executor/mod.rs:516` instrumented_yield in acquire_shared_locks_from_effects (in for loop) (race 2:1)
4. `crates/sui-core/src/authority_server.rs:392` instrumented_yield in handle_certificate (race 2:2 4:2 6:1) -> *cannot reach* because `is_full_node=false`@`crates/sui-node/src/lib.rs` is not a validator; only node id = 1 is a validator
5. `crates/sui-core/src/checkpoints/mod.rs:834` instrumented_yield in create_checkpoints-true (in for loop) (race 5:1 6:2)

`MSIM_TEST_SCHEDULE=1-2,3-4,2-2,2-4,5-2,4-5` 

### run cargo simtest

default: 
- `cargo simtest --no-fail-fast`
- Summary [ 616.726s] 669 tests run

our: 
- `MSIM_TEST_SCHEDULE=1-2,3-4,2-2,2-4,5-2,4-5 LOCAL_MSIM_PATH=/home/ubuntu/mysten-sim-x cargo simtest --no-fail-fast --test-threads=1`
- `sui_race/log_all-simtest_ours10-debug.txt` and `sui_race/log_all-simtest_ours10-debug-trim.txt`



### difficulties
1. loop/context-insensitive: calling context/stack sensitive by backtrace 
2. the distance between instrumented point and racy point
3. even though there were a race, no way to know if it causes errors triggered by a race

