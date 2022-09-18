#!/bin/bash
set -e

RUSTFLAGS='-C link-arg=-s' cargo +stable build --all --target wasm32-unknown-unknown --release
mkdir -p res
cp -u target/wasm32-unknown-unknown/release/metapool.wasm res/
cp -u target/wasm32-unknown-unknown/release/get_epoch_contract.wasm res/

