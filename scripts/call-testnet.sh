set -e
NETWORK=testnet
OWNER=lucio.$NETWORK
MASTER_ACC=pool.$NETWORK
OPERATOR_ACC_SUFFIX=.meta.pool.$NETWORK
OPERATOR_ACC=operator$OPERATOR_ACC_SUFFIX
CONTRACT_ACC=meta-v2.$MASTER_ACC
GOV_TOKEN=token.meta.$MASTER_ACC

export NEAR_ENV=$NETWORK

set -ex
near call $CONTRACT_ACC test_method '{}' --accountId $OPERATOR_ACC --depositYocto 1
