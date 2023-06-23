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


