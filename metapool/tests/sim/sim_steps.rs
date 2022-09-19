#![allow(unused_imports)]
#![allow(unused_variables)]
#![allow(dead_code)]
use near_sdk::{
    borsh::{self, BorshDeserialize, BorshSerialize},
    json_types::{Base58PublicKey, U128},
    serde::{Deserialize, Serialize},
    serde_json::json,
    serde_json::Value,
    *,
};
use near_sdk_sim::{
    account::AccessKey,
    call, deploy, init_simulator,
    near_crypto::{KeyType, SecretKey, Signer},
    to_yocto, view, ContractAccount, ExecutionResult, UserAccount, ViewResult, DEFAULT_GAS,
    STORAGE_AMOUNT,
};

//
// desk check aka algorithm test plan
//
// Mechanism:
// ---------
// for each step {
//   retrieve state
//   execute method
//   retrieve state
//   compute diffs
//   assert on diffs (according to method)
//   assert on invariants (general)
// }
//
// https://docs.google.com/spreadsheets/d/1VYynsw2yOGIE_0bFdy4CabnI1fnTXDEEffDVbYZSq6Q/edit?usp=sharing
//

use crate::sim_contract_state::*;
use crate::sim_setup::*;
use crate::sim_utils::*;
use metapool::*;

//-----------------
pub fn bot_end_of_epoch_clearing(sim: &Simulation, start: &State) -> Result<StateAndDiff, String> {
    let result = step_call(
        sim,
        &sim.operator,
        "end_of_epoch_clearing",
        json!({}),
        50 * TGAS,
        NO_DEPOSIT,
        start,
    );

    //after end_of_epoch_clearing check invariants
    if let Ok(res) = &result {
        if res.state.unstake_claims_available_sum < res.state.unstake_claims {
            panic!(
                "unstake_claims_available_sum {} < unstake_claims {}",
                res.state.unstake_claims_available_sum, res.state.unstake_claims
            )
        }
        if res.state.epoch_stake_orders != 0 && res.state.epoch_unstake_orders != 0 {
            //at least on (or both) must be 0 after end_of_epoch_clearing
            panic!(
                "after end_of_epoch_clearing epoch_stake_orders {} epoch_unstake_orders {}",
                res.state.epoch_stake_orders, res.state.epoch_unstake_orders
            )
        }
    }

    return result;
}

//-----------------
pub fn bot_distributes(sim: &Simulation, start: &State) -> Result<StateAndDiff, String> {
    let mut state = start.clone();

    let metapool = &sim.metapool;

    // END_OF_EPOCH Task 1: check if there is the need to stake
    let mut more_work: bool = state.total_for_staking > state.total_actually_staked;
    while more_work {
        println!("--CALL metapool.distribute_staking");
        match step_call(
            sim,
            &sim.operator,
            "distribute_staking",
            json!({}),
            150 * TGAS,
            NO_DEPOSIT,
            &state,
        ) {
            Err(x) => return Err(x),
            Ok(data) => {
                state = data.state;
                more_work = data.res.unwrap().unwrap_json();
                println!("--result {}", more_work);
                if let Err(err) = state.test_invariants() {
                    panic!("invariant check {}", err);
                    //return Err(err)
                }
            }
        }
    }

    // END_OF_EPOCH Task 1: check if there is the need to unstake
    more_work = state.total_actually_staked > state.total_for_staking;
    while more_work {
        println!("--CALL metapool.distribute_unstaking");
        match step_call(
            sim,
            &sim.operator,
            "distribute_unstaking",
            json!({}),
            150 * TGAS,
            NO_DEPOSIT,
            &state,
        ) {
            Err(x) => return Err(x),
            Ok(data) => {
                state = data.state;
                more_work = data.res.unwrap().unwrap_json();
                println!("--result {}", more_work);
                if let Err(err) = state.test_invariants() {
                    panic!("invariant check {}", err);
                    //return Err(err)
                }
            }
        }
    }

    let diff = state_diff(&start, &state);
    return Ok(StateAndDiff {
        state,
        diff,
        res: None,
    });
}

pub fn apply_pct(basis_points: u16, amount: u128) -> u128 {
    return (U256::from(basis_points) * U256::from(amount) / U256::from(10_000)).as_u128();
}

/// finds a staking pool requiring some stake to get balanced
/// WARN: returns (0,0) if no pool requires staking/all are busy
fn get_staking_pool_requiring_unstake(
    state: &State,
    total_to_unstake: u128,
) -> (usize, u128) {
    let mut selected_to_unstake_amount: u128 = 0;
    let mut selected_stake: u128 = 0;
    let mut selected_sp_inx: usize = 0;

    for (sp_inx, sp) in state.sps.iter().enumerate() {
        // if the pool is not busy, has stake
        let staked = as_u128(&sp["staked"]);
        let unstaked = as_u128(&sp["unstaked"]);
        let w = as_u32(&sp["weight_basis_points"]);
        let should_have = state.total_for_staking * w as u128 / 10_000;
        if staked > 0 {
            //if has not unstaked balance waiting for withdrawal, or wait started in this same epoch (no harm in unstaking more)
            if unstaked == 0 || as_u128(&sp["unstaked_requested_epoch_height"]) == state.epoch as u128 {
                // if this pool has an unbalance requiring un-staking
                let should_have = apply_pct(w as u16, state.total_for_staking);
                // does this pool requires un-staking? (has too much staked?)
                if staked > should_have {
                    // how much?
                    let unstake_amount = staked - should_have;
                    // is this the most unbalanced pool so far?
                    if unstake_amount > selected_to_unstake_amount {
                        selected_to_unstake_amount = unstake_amount;
                        selected_stake = staked;
                        selected_sp_inx = sp_inx;
                    }
                }
            }
        }
    }

    if selected_to_unstake_amount > 0 {
        if selected_to_unstake_amount > total_to_unstake {
            selected_to_unstake_amount = total_to_unstake
        };
        //to avoid moving small amounts, if the remainder is less than 5K and this pool can accommodate the unstaking, increase amount
        let remainder = total_to_unstake - selected_to_unstake_amount;
        if remainder <= MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT
            && selected_stake
                > selected_to_unstake_amount + remainder + 2 * MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT
        {
            selected_to_unstake_amount += remainder
        };
    }
    return (selected_sp_inx, selected_to_unstake_amount);
}

//-----------------
pub fn bot_rebalance_unstake(sim: &Simulation, start: &State) -> Result<StateAndDiff, String> {

    let mut state = start.clone();

    let metapool = &sim.metapool;

    // before END_OF_EPOCH: rebalance up to 10%

    // check for max x% unstaked
    const MAX_UNSTAKED_PCT:u8 = 10;
    let max_allowed_unstaked_amount = state.total_for_staking * MAX_UNSTAKED_PCT as u128 / 100;

    let unstake_rebalance_cap =
        if state.total_unstaked_and_waiting > max_allowed_unstaked_amount {
            0 // do not unstake more if we have x% unstaked already
        }
        else {
            max_allowed_unstaked_amount - state.total_unstaked_and_waiting
        };

    if unstake_rebalance_cap > 2*MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT {
        
        // find aa pool with extra stake
        let (inx, extra) = get_staking_pool_requiring_unstake(&state, unstake_rebalance_cap -  MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT);
        
        //println!("about to call start_rebalance_unstake on {}, extra={}", pool.to_string(), extra);
        //let pool_id = pool["account_id"].as_str().unwrap().to_string();
        let result = step_call(
            sim,
            &sim.operator,
            "start_rebalance_unstake",
            json!({ "sp_inx": inx, "amount": extra.to_string() }),
            200 * TGAS,
            NO_DEPOSIT,
            &state,
        )?;
        state = result.state;

    }


    let diff = state_diff(&start, &state);
    return Ok(StateAndDiff {
        state,
        diff,
        res: None,
    });
}

//-----------------
pub fn bot_ping_rewards(sim: &Simulation, start: &State) -> Result<StateAndDiff, String> {
    // COMPUTE REWARDS
    //if the epoch is recently started -- ping the pools so they compute rewards and do the same in the meta-pool

    let mut state = start.clone();

    for inx in 0..state.sps.len() {
        let pool = &state.sps[inx];
        let staked = as_u128(&pool["staked"]);
        let unstaked = as_u128(&pool["unstaked"]);
        if (staked > 0 || unstaked > 0) && &pool["last_asked_rewards_epoch_height"] != state.epoch {
            //ping on the pool so it calculates rewards
            println!("about to call PING & DISTRIBUTE on {}", pool.to_string());
            let pool_id = pool["account_id"].as_str().unwrap().to_string();
            let ping = sim
                .operator
                .call(pool_id, "ping", &[], 200 * TGAS, NO_DEPOSIT);
            check_exec_result(&ping);
            //await near.call(pool.account_id, "ping", {}, OPERATOR_ACCOUNT, credentials.private_key, 200);
            //calculates rewards now in the meta for that pool
            //pub fn distribute_rewards(&mut self, sp_inx: u16) -> void
            println!("meta.DISTR");
            let result = step_call(
                sim,
                &sim.operator,
                "distribute_rewards",
                json!({ "sp_inx": inx }),
                200 * TGAS,
                NO_DEPOSIT,
                &state,
            )?;
            state = result.state;
        }
    }

    let diff = state_diff(&start, &state);
    return Ok(StateAndDiff {
        state,
        diff,
        res: None,
    });
}

//-----------------
pub fn bot_retrieve(sim: &Simulation, start: &State) -> Result<StateAndDiff, String> {
    let mut state = start.clone();
    // RETRIEVE UNSTAKED FUNDS
    for inx in 0..state.sps.len() {
        let pool = &state.sps[inx];
        let staked = as_u128(&pool["staked"]);
        let unstaked = as_u128(&pool["unstaked"]);
        if unstaked > 0 && &pool["unstaked_requested_epoch_height"] != "0" {
            println!("about to try RETRIEVE UNSTAKED FUNDS on pool {:?}", pool);
            let now = state.epoch;
            let mut when =
                as_u128(&pool["unstaked_requested_epoch_height"]) as u64 + NUM_EPOCHS_TO_UNLOCK;
            if when > now + 30 {
                when = now
            }; //bad data or hard-fork
            if when <= now {
                //try RETRIEVE UNSTAKED FUNDS
                match step_call(
                    sim,
                    &sim.operator,
                    "retrieve_funds_from_a_pool",
                    json!({ "inx": inx }),
                    200 * TGAS,
                    NO_DEPOSIT,
                    &state,
                ) {
                    Err(x) => return Err(x),
                    Ok(data) => state = data.state,
                }
            }
        }
    }

    let diff = state_diff(&start, &state);
    return Ok(StateAndDiff {
        state,
        diff,
        res: None,
    });
}

pub struct StateAndDiff {
    pub state: State,
    pub diff: StateDiff,
    pub res: Option<ExecutionResult>,
}

//-----------
pub fn step_call(
    sim: &Simulation,
    acc: &UserAccount,
    method: &str,
    args: Value,
    gas: u64,
    attached_near: u128,
    pre: &State,
) -> Result<StateAndDiff, String> {
    println!("step_call {}", method);
    let res = acc.call(
        sim.metapool.account_id(),
        method,
        args.to_string().as_bytes(),
        gas,
        attached_near,
    ); // call!(pepe, metapool.nslp_add_liquidity(),10_000*NEAR,200*TGAS);
       //print_exec_result(&res);
    print_logs(&res);
    if res.is_ok() {
        let post = build_state(&sim);
        let diff = state_diff(&pre, &post);
        println!(
            "--DIFF {}",
            serde_json::to_string(&diff).unwrap_or_default()
        );
        println!(
            "--POST {}",
            serde_json::to_string(&post).unwrap_or_default()
        );

        if let Err(err) = post.test_invariants() {
            panic!("invariant check {}", err);
            //return Err(err)
        }
        return Ok(StateAndDiff {
            state: post,
            diff,
            res: Some(res),
        });
    } else {
        let msg = format!("Txn Failed, {}.{}", sim.metapool.account_id(), method);
        println!("step_call failed {}", msg);
        //println!("res.is_ok()={} {:?}", &res.is_ok(), &res);
        print_exec_result(&res);
        return Err(msg);
    }
}
