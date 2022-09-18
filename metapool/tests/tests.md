### Glossary

USER DEPOSIT_AND_STAKE: add to epoch_stake_orders (maybe nspl_clearing), increment total_for_stake
USER UNSTAKE: delayed-unstake, add to epoch_unstake_orders, decrement total_for_stake

DISTRIBUTE STAKE: move from the contract to sps
DISTRIBUTE UNSTAKE: invoe unsktake in the sp, added to unstaked_and_waiting

RETRIEVE: Move from a sp to the contract
WITHDRAW: Move from the contract to the user

### Tests

- [x]  create contract, configure sps
- [x]  deposit and stake
- [x]  distribute staking
- [x]  delayed unstake
- [x]  withdraw unstaked
- [x]  distribute unstaking
- [x]  add liquidity
- [x]  liquid unstake
- [x]  get meta
- [x]  remove liquidity
- [x]  migration

instructions and variables

                                  epoch   epoch    total    total     retrieved    total        total    contract
                                  stake   unstake  for      actually  for_unstake  unstaked     unstake  account
                                  orders  orders   staking  staked    claims       and_waiting  claims   balance

deposit_and_stake                   +x               +x                                                     +x

deposit_and_stake+nslp_clearing   0:+x-x            0:+x-x                                                  +x (because more NEAR in nslp)
(ok,is a swap NEAR->stNEAR
so only affects the nslp)

liquid-unstake                                                                                              -x (NEAR sent to the user)
(user account and nslp)

delayed-unstake                              +x       -x                                           +x

end_of_epoch_clearing               p:0     p:0                         +delta

distribute_staking                   -x                       +x                                            -x
distribute_staking stake-from-unstk  -x                       +x         +x            -x                       (special case, as if instant retrieve)

distribute_unstaking                         -x               -x                       +x

start_rebalance_unstake                                       -x                       +x

retrieve_from_pool                                                        +x           -x                   +x
retrieve_from_pool REBAL             +y                                   +x-y         -x                   +x
(y depends on total 
unstake claims, extra)

user claim (withdraw)                                                     -x                       -x       -x
