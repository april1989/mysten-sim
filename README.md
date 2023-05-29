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
We want to:

1. Replace `register_fail_point_async` with `tokio.yield()`
2. Implement necessary steps in the scheduler of msim

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



















