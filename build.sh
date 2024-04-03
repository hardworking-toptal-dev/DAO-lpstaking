#!/bin/bash
set -e

RUSTFLAGS='-C link-arg=-s' cargo build --package metapool --target wasm32-unknown-unknown --release
mkdir -p res
cp -u target/wasm32-unknown-unknown/release/metapool.wasm res/
cp -u target/wasm32-unknown-unknown/release/staking_pool.wasm res/
cp -u target/wasm32-unknown-unknown/release/get_epoch_contract.wasm res/

