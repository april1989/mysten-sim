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



### toy_test

[toy_test](test-crates/toy-test) is a simple version of test_create_advance_epoch_tx_race. 
We want to:

1. Replace `register_fail_point_async` with `tokio.yield()`
2. Implement necessary steps in the scheduler of msim

Run the following command if you do not install sui crates:
```shell 
cd test-crates/toy-test
RUSTFLAGS="--cfg msim" cargo test --manifest-path ../../../sui/Cargo.toml
```
BUT ALL FAIL COMPILE NOW.