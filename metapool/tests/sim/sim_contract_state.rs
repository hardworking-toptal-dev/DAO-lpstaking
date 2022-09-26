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

use crate::sim_setup::*;
use crate::sim_utils::*;
use metapool::*;

///
/// https://docs.google.com/spreadsheets/d/1VYynsw2yOGIE_0bFdy4CabnI1fnTXDEEffDVbYZSq6Q/edit?usp=sharing
///
#[derive(Debug, Serialize, Clone)]
#[serde(crate = "near_sdk::serde")]
pub struct State {
    pub epoch: u64,

    pub contract_account_balance: u128,
    pub retrieved_for_unstake_claims: u128,
    pub total_available: u128,

    pub epoch_stake_orders: u128,
    pub epoch_unstake_orders: u128,

    pub total_for_staking: u128,
    pub total_actually_staked: u128,
    pub to_stake_delta: i128,

    pub total_unstaked_and_waiting: u128,

    pub unstake_claims: u128,
    pub unstake_claims_available_long_term: u128, //how much we have to fulfill unstake claims

    pub staked_in_pools: u128,
    pub unstaked_in_pools: u128,
    pub total_in_pools: u128,

    pub unstaked_for_rebalance: u128,

    pub sps: Vec<Value>,
}

#[derive(Debug, Serialize)]
#[serde(crate = "near_sdk::serde")]
pub struct StateDiff {
    pub contract_account_balance: i128,
    pub retrieved_for_unstake_claims: i128,
    pub total_available: i128,

    pub epoch_stake_orders: i128,
    pub epoch_unstake_orders: i128,

    pub total_for_staking: i128,
    pub total_actually_staked: i128,
    pub to_stake_delta: i128,

    pub total_unstaked_and_waiting: i128,

    pub unstake_claims: i128,
    pub unstake_claims_available_long_term: i128, // how much we have to fulfill unstake claims long-term

    pub staked_in_pools: i128,
    pub unstaked_in_pools: i128,
    pub total_in_pools: i128,

    pub unstaked_for_rebalance: i128,
}
impl StateDiff {
    pub fn has_data(&self) -> bool {
        self.contract_account_balance != 0
            || self.retrieved_for_unstake_claims != 0
            || self.total_available != 0
            || self.epoch_stake_orders != 0
            || self.epoch_unstake_orders != 0
            || self.total_for_staking != 0
            || self.total_actually_staked != 0
            || self.to_stake_delta != 0
            || self.total_unstaked_and_waiting != 0
            || self.unstake_claims != 0
            || self.unstake_claims_available_long_term != 0
            || self.staked_in_pools != 0
            || self.unstaked_in_pools != 0
            || self.total_in_pools != 0
            || self.unstaked_for_rebalance != 0
    }
}

pub fn set_unstake_for_rebalance_cap_bp(bp:u16, sim:&Simulation){
    let metapool_contract = &sim.metapool;
    let mut contract_params = view!(metapool_contract.get_contract_params()).unwrap_json_value();
    contract_params["unstake_for_rebalance_cap_bp"] = bp.into();
    let mut args = json!({ "params":{} });
    args["params"] = contract_params;
    let res = sim.operator.call(
        sim.metapool.account_id(),
        "set_contract_params",
        args.to_string().as_bytes(),
        10*TGAS,
        0
    );
    print_logs(&res);
    if !res.is_ok() {
        //println!("res.is_ok()={} {:?}", &res.is_ok(), &res);
        print_exec_result(&res);
        panic!("set_unstake_for_rebalance_cap_bp failed")
    }
}

pub fn set_staking_pools_weight(weights_bp: Vec<u16>, sim:&Simulation){
    
    // set staking pools weight
    let mut pools:Vec<StakingPoolArgItem> = Vec::with_capacity(4);
    //---- prepare vector with test names
    for n in 0..=3 {
        let acc_id = &format!("sp{}.testnet", n);
        // prepare weight
        pools.push ( StakingPoolArgItem {
            account_id: acc_id.clone(),
            weight_basis_points: weights_bp[n]
        });
    }
    set_staking_pools(pools,&sim);
}
    
pub fn set_staking_pools(pools: Vec<StakingPoolArgItem>, sim:&Simulation){
    let metapool_contract = &sim.metapool;
    let res = call!(sim.operator,
        metapool_contract.set_staking_pools(pools),
        1,
        125 * TGAS
    );
    print_exec_result(&res);
    check_exec_result(&res);
}


pub fn build_state(sim: &Simulation) -> State {
    let metapool = &sim.metapool;
    let contract_state = view!(metapool.get_contract_state()).unwrap_json_value();

    let total_for_staking = as_u128(&contract_state["total_for_staking"]);
    let total_actually_staked = as_u128(&contract_state["total_actually_staked"]);

    let epoch_unstake_orders = as_u128(&contract_state["epoch_unstake_orders"]);

    let retrieved_for_unstake_claims = as_u128(&contract_state["retrieved_for_unstake_claims"]);
    let total_unstaked_and_waiting = as_u128(&contract_state["total_unstaked_and_waiting"]);

    let view_result = view!(metapool.get_staking_pool_list());
    let sps: Vec<Value> =
        near_sdk::serde_json::from_slice(&view_result.unwrap()).unwrap_or_default();

    let mut sum_staked: u128 = 0;
    let mut sum_unstaked: u128 = 0;
    for sp in &sps {
        sum_staked += as_u128(&sp["staked"]);
        sum_unstaked += as_u128(&sp["unstaked"]);
    }

    let to_stake_delta = total_for_staking as i128 - total_actually_staked as i128;

    let unstaked_for_rebalance =  as_u128(&contract_state["unstaked_for_rebalance"]);

    return State {
        epoch: as_u128(&contract_state["env_epoch_height"]) as u64,

        contract_account_balance: as_u128(&contract_state["contract_account_balance"]),
        retrieved_for_unstake_claims,
        total_available: as_u128(&contract_state["total_available"]),

        epoch_stake_orders: as_u128(&contract_state["epoch_stake_orders"]),
        epoch_unstake_orders,

        total_for_staking,
        total_actually_staked,
        to_stake_delta,

        total_unstaked_and_waiting,

        unstake_claims: as_u128(&contract_state["total_unstake_claims"]),
        unstake_claims_available_long_term: retrieved_for_unstake_claims
            + total_unstaked_and_waiting
            - unstaked_for_rebalance
            + epoch_unstake_orders , // recent delayed-unstake that will be converted to retrieved_for_unstake_claims or total_unstaked_and_waiting

        staked_in_pools: sum_staked,
        unstaked_in_pools: sum_unstaked,
        total_in_pools: sum_staked + sum_unstaked,

        unstaked_for_rebalance,

        sps,
    };
}

pub fn state_diff(pre: &State, post: &State) -> StateDiff {
    return StateDiff {
        contract_account_balance: post.contract_account_balance as i128
            - pre.contract_account_balance as i128,
        retrieved_for_unstake_claims: post.retrieved_for_unstake_claims as i128 - pre.retrieved_for_unstake_claims as i128,
        total_available: post.total_available as i128 - pre.total_available as i128,

        epoch_stake_orders: post.epoch_stake_orders as i128 - pre.epoch_stake_orders as i128,
        epoch_unstake_orders: post.epoch_unstake_orders as i128 - pre.epoch_unstake_orders as i128,

        total_for_staking: post.total_for_staking as i128 - pre.total_for_staking as i128,
        total_actually_staked: post.total_actually_staked as i128
            - pre.total_actually_staked as i128,
        to_stake_delta: post.to_stake_delta as i128 - pre.to_stake_delta as i128,

        total_unstaked_and_waiting: post.total_unstaked_and_waiting as i128
            - pre.total_unstaked_and_waiting as i128,

        unstake_claims: post.unstake_claims as i128 - pre.unstake_claims as i128,
        unstake_claims_available_long_term: post.unstake_claims_available_long_term as i128
            - pre.unstake_claims_available_long_term as i128, //how much we have to fulfill unstake claims

        staked_in_pools: post.staked_in_pools as i128 - pre.staked_in_pools as i128,
        unstaked_in_pools: post.unstaked_in_pools as i128 - pre.unstaked_in_pools as i128,
        total_in_pools: post.total_in_pools as i128 - pre.total_in_pools as i128,

        unstaked_for_rebalance: post.unstaked_for_rebalance as i128 - pre.unstaked_for_rebalance as i128,
    };
}

//-----------
impl State {
    pub fn test_invariants(&self) -> Result<u8, String> {
        // if rebalance_unstake was executed, TAS can be lower than TFS, *and the delta different than epoch_orders_delta*
        // rebalance_unstake works by waiting, retrieving, and then restaking if there's an extra over retrieved_for_unstake_claims
        if self.total_for_staking > self.total_actually_staked {
            // no invariant here because rebalance_unstake can cause this
        }
        else if self.total_for_staking < self.total_actually_staked {
            // this can only be caused by delayed-unstaked (reduces total_for_staking and adds to epoch_unstake_orders)
            let delta_stake = self.total_actually_staked - self.total_for_staking;
            if self.epoch_unstake_orders < self.epoch_stake_orders {
                return Err(
                    "(1) delta-stake<0 but self.epoch_stake_orders > self.epoch_unstake_orders".into(),
                );
            }
            let delta_orders = self.epoch_unstake_orders - self.epoch_stake_orders;
            // since the option is for TAS to be lower because rebalance_unstake, delta-TAS 
            // delta-TAS must be smaller or equal than delta-orders
            if !(delta_stake <= delta_orders) {
                return Err(
                    "(2) invariant delta_stake <= delta_orders, violated".into(),
                );
            }
        }

        if self.contract_account_balance
            != self.total_available + self.retrieved_for_unstake_claims + self.epoch_stake_orders
        {
            return Err(
                "CAB != self.total_available + self.retrieved_for_unstake_claims + self.epoch_stake_orders"
                    .into(),
            );
        }

        if self.unstake_claims != self.unstake_claims_available_long_term
        {
            return Err(
                "self.unstake_claims != self.unstake_claims_available_long_term"
                    .into(),
            );
        }

        if !(self.retrieved_for_unstake_claims <= self.unstake_claims)
        {
            println!("self.retrieved_for_unstake_claims {} should be <= self.unstake_claims {}. Rest is extra for rebalance",
                self.retrieved_for_unstake_claims, 
                self.unstake_claims
            );
            return Err(
                "self.retrieved_for_unstake_claims should be <= self.unstake_claims. Rest is extra for rebalance"
                    .into(),
            );
        }

        return Ok(0);
    }

    pub fn assert_rest_state(&self) {
        //we've just cleared orders
        assert_eq!(self.epoch_stake_orders, 0);
        assert_eq!(self.epoch_unstake_orders, 0);

        assert_eq!(self.total_for_staking, self.total_actually_staked);
        assert_eq!(self.total_for_staking, self.staked_in_pools);

        assert_eq!(self.total_unstaked_and_waiting, self.unstaked_in_pools);

        assert_eq!(
            self.unstake_claims,
            self.retrieved_for_unstake_claims + self.unstaked_in_pools
        );
    }
}
