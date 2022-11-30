use crate::*;
use near_sdk::{log, near_bindgen, Promise};

#[near_bindgen]
impl MetaPool {
    //----------------------------------
    // Heartbeat & Talking to the pools
    // ---------------------------------

    //-----------------------------
    // DISTRIBUTE
    //-----------------------------

    /// operator method -------------------------------------------------
    /// distribute_staking(). Do staking in batches of at most 100Kn
    /// returns "true" if the operator needs to call this fn again
    pub fn distribute_staking(&mut self) -> bool {
        //Note: In order to make this contract independent from the operator
        //this fn is open to be called by anyone

        self.assert_not_busy();

        //do we need to stake?
        if self.total_for_staking <= self.total_actually_staked {
            log!("no staking needed");
            return false;
        }
        // here self.total_for_staking > self.total_actually_staked
        // do clearing
        self.internal_end_of_epoch_clearing();
        // here, if we have epoch_stake_orders, then we need to stake
        if self.epoch_stake_orders == 0  { 
            // that delta is from some manual-unstake
            log!("self.epoch_stake_orders == 0");
            return false;
        }

        //-------------------------------------
        //compute amount to stake
        //-------------------------------------
        // Note: there could be minor yocto corrections after sync_unstake, altering total_actually_staked, consider that
        // epoch_stake_orders are NEAR that were deposited by users, stNEAR minted, and are available in the contract for staking or to reserve (see epoch_unstake_orders)
        // epoch_unstake_orders are stNEAR that was burned, users started delayed-unstake, and so NEAR must be unstaked from the pools...
        // ... or some NEAR from epoch_stake_orders might remain as reserve, to avoid senseless stake + unstake
        // but in any case, does not make sense to stake more than delta: total_for_staking - total_actually_staked
        let total_amount_to_stake = std::cmp::min(
            self.epoch_stake_orders,
            self.total_for_staking - self.total_actually_staked,
        );
        if total_amount_to_stake < MIN_STAKE_AMOUNT {
            log!("amount too low {}", total_amount_to_stake);
            return false;
        }
        // find pool
        // the resulting "amount_to_stake" could be less than total_amount_to_stake, if the pool does not need that much
        let (sp_inx, stake_required) = self.get_staking_pool_requiring_stake();
        log!(
            "total_amount_to_stake:{} get_staking_pool_requiring_stake=>{},{}",
            total_amount_to_stake,
            sp_inx,
            stake_required
        );
        // schedule promise to stake
        let amount_to_stake = std::cmp::min(total_amount_to_stake, stake_required);
        self.launch_direct_stake(sp_inx, amount_to_stake);
        return amount_to_stake < total_amount_to_stake; //did some staking (promises scheduled), call again?
    }

    // prev fn continues here
    /// internal launch direct stake on a pool
    /// **schedules promises** to stake 
    /// Note: if the sp has some sizable unstake pending, the fn will re-stake the unstaked-and-waiting-amount
    /// that amount can be lower than the amount requested to stake
    fn launch_direct_stake(&mut self, sp_inx:usize, mut amount_to_stake:u128) {

        if amount_to_stake > 0 {
            //most unbalanced pool found & available

            self.contract_busy = true;
            let sp = &mut self.staking_pools[sp_inx];
            sp.busy_lock = true;

            //case 1. pool has unstaked amount (we could be at the unstaking delay waiting period)
            //NOTE: The amount to stake can't be so low as a few yoctos because the staking-pool
            // will panic with : "panicked at 'The calculated number of \"stake\" shares received for staking should be positive', src/internal.rs:79:9"
            // that's because after division, if the amount is a few yoctos, the amount for shares is 0
            if sp.unstaked >= TEN_NEAR {
                //at least 10 NEAR
                //pool has a sizable unstaked amount
                if sp.unstaked < amount_to_stake {
                    //re-stake the unstaked
                    amount_to_stake = sp.unstaked;
                }

                //schedule async stake to re-stake in the pool
                ext_staking_pool::stake(
                    amount_to_stake.into(),
                    &sp.account_id,
                    NO_DEPOSIT,
                    gas::staking_pool::STAKE,
                )
                .then(ext_self_owner::on_staking_pool_stake_maybe_deposit(
                    sp_inx,
                    amount_to_stake,
                    false,
                    &env::current_account_id(),
                    NO_DEPOSIT,
                    gas::owner_callbacks::ON_STAKING_POOL_DEPOSIT_AND_STAKE,
                ));
            } else {
                //here the sp has no sizable unstaked balance, we must deposit_and_stake on the sp from our balance

                // TODO: This may be too optimistic, why not compute the storage explicitly and add
                //    a safety margin on top of that. That's because the account state may
                //    potentially exceed the 35N (or 3.5M right now). But I guess it can happen
                //    only at the beginning of metapool before the liquidity is provided.
                assert!(
                    env::account_balance() - MIN_BALANCE_FOR_STORAGE >= amount_to_stake,
                    "env::account_balance()-MIN_BALANCE_FOR_STORAGE < amount_to_stake"
                );

                //schedule async stake or deposit_and_stake on that pool
                ext_staking_pool::deposit_and_stake(
                    &sp.account_id,
                    amount_to_stake.into(), //attached amount
                    gas::staking_pool::DEPOSIT_AND_STAKE,
                )
                .then(ext_self_owner::on_staking_pool_stake_maybe_deposit(
                    sp_inx,
                    amount_to_stake,
                    true,
                    &env::current_account_id(),
                    NO_DEPOSIT,
                    gas::owner_callbacks::ON_STAKING_POOL_DEPOSIT_AND_STAKE,
                ));
            }
        }

        //Here we did some staking (the promises are scheduled for exec after this fn completes)
        self.total_actually_staked += amount_to_stake; //preventively consider the amount staked (undoes if async fails)
        self.epoch_stake_orders -= amount_to_stake; //preventively reduce stake orders
    }

    //prev fn continues here
    /// Called after amount is staked into a staking-pool
    /// This method needs to update staking pool status.
    pub fn on_staking_pool_stake_maybe_deposit(
        &mut self,
        sp_inx: usize,
        amount: u128,
        included_deposit: bool,
    ) -> bool {
        assert_callback_calling();

        let sp = &mut self.staking_pools[sp_inx];
        let sp_account_id = sp.account_id.clone();

        //WARN: This is a callback after-cross-contract-call method
        //busy locks must be saved false in the state, this method SHOULD NOT PANIC
        sp.busy_lock = false;
        self.contract_busy = false;

        let stake_succeeded = is_promise_success();

        let result: &str;
        if stake_succeeded {
            // STAKED OK
            result = "succeeded";
            // move into staked
            sp.staked += amount;
            // update accums based on the source of the funds
            let event: &str;
            if included_deposit {
                // we sent NEAR from the contract into the staking-pool
                event = "dist.stak"; //stake in the pools (including transfer)
                self.contract_account_balance -= amount; // we took from contract balance (transfer)
            } else {
                // stake the unstaked in the pool, no-transfer
                event = "dist.stak.nt"; //not deposited first, so staked funds came from unstaked funds already in the staking-pool
                sp.unstaked -= amount; //we've now less unstaked in this sp
                self.total_unstaked_and_waiting -= amount; // contract total of all unstaked & waiting, now there's less there.
                                                           // We kept the NEAR in the contract and took from unstaked_and_waiting
                                                           // ... unstaked_and_waiting was in their way to be converted in retrieved_for_unstake_claims
                self.consider_retrieved_for_unstake_claims(amount); // so this is a special case: the NEAR to stake was taken from total_unstaked_and_waiting,
                                                             // so we compensate and take the NEAR in the contract and consider it reserved for_unstake_claims
            }
            //log event
            event!(
                r#"{{"event":"{}","sp":"{}","amount":"{}"}}"#,
                event,
                sp_account_id,
                amount
            );
        } else {
            //STAKE FAILED
            result = "has failed";
            self.total_actually_staked -= amount; //undo preventive action considering the amount staked
            self.epoch_stake_orders += amount; //undo preventively reduce stake orders
        }
        log!("Staking of {} at @{} {}", amount, sp_account_id, result);

        return stake_succeeded;
    }

    // execute stake on sp[inx] by amount
    // used by operator if a validator requires stake to keep a seat
    // Note: this fn stakes from current epochs_stake_orders,
    // consider that the scheduled promise-to-stake/restake can fail
    pub fn manual_stake(&mut self, inx: u16, amount: U128String) {
        self.assert_operator_or_owner();
        self.assert_not_busy();

        assert!(self.epoch_stake_orders > MIN_STAKE_AMOUNT,
            "self.epoch_stake_orders too low {}", 
            self.epoch_stake_orders
        );
        assert!(amount.0 <= self.epoch_stake_orders,
            "self.epoch_stake_orders is {} you cant manual stake {}", 
            self.epoch_stake_orders, amount.0
        );

        let sp_inx = inx as usize;
        assert!(sp_inx < self.staking_pools.len(), "invalid index");
        let sp = &self.staking_pools[sp_inx];
        assert!(!sp.busy_lock, "sp busy");
        // schedule promise to direct stake
        self.launch_direct_stake(sp_inx, amount.0);
        // Note: if the pool has some sizable unstake pending, the fn will re-stake the unstaked-and-waiting-amount
        // that amount can be lower than the amount requested to stake
    }

    /// Start a forced rebalance unstake
    /// used by operator when a validator goes offline, to not wait and unstake immediately even over the max-rebalance-cap
    /// the stake of the sp is adjusted to weight, if weight==0, the sp is fully unstaked
    pub fn force_rebalance_unstake(&mut self, inx: u16) {
        self.assert_operator_or_owner();
        self.assert_not_busy();
        let sp_inx = inx as usize;
        assert!(sp_inx < self.staking_pools.len(), "invalid index");
        let sp = &self.staking_pools[sp_inx];
        assert!(!sp.busy_lock, "sp busy");
        // can not unstake while unstake pending (if it was done on previous epochs) 
        // because it will extend the waiting period
        assert!(
            sp.unstaked == 0 || sp.unstk_req_epoch_height == env::epoch_height(),
            "can not force rebalance-unstake while unstake pending. sp.unstake={}, sp.unstk_req_epoch_height={}, env::epoch_height()={}",
            sp.unstaked, sp.unstk_req_epoch_height, env::epoch_height()
        );
        // limit for rebalance_unstaking is the should_have of the pool
        let should_have = apply_pct(sp.weight_basis_points, self.total_for_staking);
        // if staked, (unstaked in this epoch or unstaked==0) and extra
        assert!(sp.staked > should_have, 
            "the sp has not extra stake. assigned weight_bp:{}, stake:{}",sp.weight_basis_points, sp.staked
        );
        // has extra, can be unstaked, start rebalance
        let extra = sp.staked - should_have;
        // Next call affects:
        // total_actually_staked, sp.stake & sp.unstake and total_unstaked_and_waiting, 
        // but it DOES NOT not affect reserve_for_unstake_claims and also DOES NOT change total_for_stake
        // this means that eventually we will retrieve from the pools, more than required for reserve_for_unstake_claims
        // at that point, the extra amount is set for restake (see fn retrieve_funds_from_a_pool), completing the rebalance
        self.perform_unstake(sp_inx, 0, extra); // unstake for rebalance
    }


    /// Start a rebalance unstake if needed, returns true if you should call again
    /// it calls get_staking_pool_requiring_unstake(), to get an index and amount
    /// used by operator when periodically rebalancing the pool 
    /// The amount to unstake is added to unstaked_for_rebalance.
    /// When these funds are finally withdrawn, 4 epochs from now, the extra NEAR received 
    /// (any amount reserved exceeding total_unstake_claims) will be added to epoch_stake_orders, thus completing the rebalance
    /// Note: It could happen that some users perform delayed-unstake during those epochs, that amount will be preserved in the contract,...
    /// ... because total_unstake_claims has priority over rebalance.
    pub fn do_rebalance_unstake(&mut self) -> bool {
        self.assert_operator_or_owner();

        // check for max x% unstaked
        let max_unstake_for_rebalance = self.max_unstake_for_rebalance() ;

        if self.unstaked_for_rebalance < max_unstake_for_rebalance {

            let unstake_rebalance_left = max_unstake_for_rebalance - self.unstaked_for_rebalance;

            if unstake_rebalance_left >= MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT {
            
                let gspru = self.internal_get_staking_pool_requiring_unstake();
                debug!(
                    r#"{{"event":"do_rebal_usntk", "extra":{}, "sp_inx":{}, "totExtra":{}, "unblocked":{}, "with_stake":{}}}"#,
                    gspru.extra/NEAR, gspru.sp_inx, gspru.total_extra/NEAR, gspru.count_unblocked, gspru.count_with_stake
                );
                // continue only if at least 40% of the pools with stake are "unblocked", i.e. not already waiting for unstake-period and so ready for more unstakes.
                // and if amount left for rebalance (total_extra) is more than already unstaked_for_rebalance
                // and if selected sp assigned=0 OR what's to rebalance is at least 0.05% of TFS (if there's some unbalance worth solving)
                if gspru.count_unblocked as u64 >= gspru.count_with_stake as u64 * 40 / 100 && 
                    ( self.staking_pools[gspru.sp_inx as usize].weight_basis_points == 0 || 
                        gspru.total_extra - self.unstaked_for_rebalance > self.total_for_staking / 2000 )
                {
                    let to_unstake_for_rebal = std::cmp::min(gspru.extra, unstake_rebalance_left);
                    // Next call affects:
                    // total_actually_staked, sp.stake & sp.unstake and total_unstaked_and_waiting, 
                    // but it DOES NOT not affect reserve_for_unstake_claims and also DOES NOT change total_for_stake
                    // this means that eventually we will retrieve from the pools, more than required for reserve_for_unstake_claims
                    // at that point, the extra amount is set for restake (see fn retrieve_funds_from_a_pool), completing the rebalance
                    self.perform_unstake(gspru.sp_inx as usize, 0, to_unstake_for_rebal); // unstake for rebalance
                    
                    return unstake_rebalance_left - to_unstake_for_rebal >= MIN_STAKE_UNSTAKE_AMOUNT_MOVEMENT; // call again?
                }
            }
        }
        false // default return
    }

    // Operator method, but open to anyone
    /// distribute_unstaking(). Do unstaking
    /// returns "true" if needs to be called again
    pub fn distribute_unstaking(&mut self) -> bool {
        //Note: In order to make this contract independent from the operator
        //this fn is open to be called by anyone

        self.assert_not_busy();
        // clearing first
        self.internal_end_of_epoch_clearing();
        // after clearing, epoch_unstake_orders is the amount to unstake
        // check if the amount justifies tx-fee / can be unstaked really
        // TODO: It's better to move `10 * TGAS as u128` since it's incorrect to compare GAS to NEAR.
        //    GAS has a gas_price, and if gas price goes higher, then the comparison might be out of
        //    date. Since the operator is paying for all gas, it's probably fine to execute anyway.
        if self.epoch_unstake_orders <= 10 * TGAS as u128 {
            return false;
        }

        let mut unstake_from_orders = self.epoch_unstake_orders;
        let mut unstake_from_rebalance = 0;

        // resulting "amount_to_unstake" can be lower than total_to_unstake, according to conditions in get_staking_pool_requiring_unstake 
        let gspru = self.internal_get_staking_pool_requiring_unstake();

        debug!(
            r#"{{"event":"sp_has_extra","sp":"{}","amount":"{}"}}"#,
            gspru.sp_inx,
            gspru.extra
        );

        if gspru.extra > 0 {
            if gspru.extra <= self.epoch_unstake_orders {
                unstake_from_orders = gspru.extra // no more than what the pool has extra
            }
            else { // more extra in the sp that the amount ordered
                // check if we can seize this opportunity to also unstake for rebalance
                let max_unstake_for_rebalance = self.max_unstake_for_rebalance();
                if self.unstaked_for_rebalance < max_unstake_for_rebalance {
                    let cap_to_extra_unstake_for_rebalance = max_unstake_for_rebalance - self.unstaked_for_rebalance;
                    if cap_to_extra_unstake_for_rebalance > 1*NEAR {
                        unstake_from_rebalance = std::cmp::min(cap_to_extra_unstake_for_rebalance, gspru.extra - unstake_from_orders);
                    }
                }
            }
        }

        if unstake_from_orders + unstake_from_rebalance > 10 * TGAS as u128 {
            // only if the amount justifies tx-fee
            // most unbalanced pool found & available
            // continue with generateing the promise for async cross-contract call to unstake
            self.perform_unstake(gspru.sp_inx as usize, unstake_from_orders, unstake_from_rebalance);
            return self.epoch_unstake_orders > 0; // if needs to be called again
        } else {
            return false;
        }
    }

    // two prev fns continue here
    // execute unstake on sp[inx] by amount
    // if is_rebalance then it does no consider this unstake originated in epoch_unstake_orders
    fn perform_unstake(&mut self, 
        sp_inx: usize, 
        amount_from_unstake_orders: u128, 
        amount_from_rebalance: u128,
    )
    {
        let total_amount = amount_from_unstake_orders + amount_from_rebalance;
        if total_amount == 0 {
            return;
        }
        self.assert_not_busy();

        assert!(self.total_actually_staked >= total_amount, "IUN");
        assert!(sp_inx < self.staking_pools.len(), "invalid index");
        let sp = &mut self.staking_pools[sp_inx];
        assert!(!sp.busy_lock,"sp is busy");
        assert!(
            sp.staked >= total_amount,
            "only {} staked can not unstake {}",
            sp.staked,
            total_amount,
        );
        
        self.contract_busy = true;
        sp.busy_lock = true;

        // preventively consider the amount un-staked (undoes if promise fails)
        self.total_actually_staked -= total_amount;
        self.epoch_unstake_orders -= amount_from_unstake_orders; // preventively consider the unstake_order fulfilled

        //launch async to un-stake from the pool
        ext_staking_pool::unstake(
            total_amount.into(),
            &sp.account_id,
            NO_DEPOSIT,
            gas::staking_pool::UNSTAKE,
        )
        .then(ext_self_owner::on_staking_pool_unstake(
            sp_inx,
            amount_from_unstake_orders.into(),
            amount_from_rebalance.into(),
            //extra async call args
            &env::current_account_id(),
            NO_DEPOSIT,
            gas::owner_callbacks::ON_STAKING_POOL_UNSTAKE,
        ));
    }
    /// The prev fn continues here
    /// Called after the given amount was unstaked at the staking pool contract.
    /// This method needs to update staking pool status.
    pub fn on_staking_pool_unstake(&mut self, 
        sp_inx: usize, 
        amount_from_unstake_orders: U128String, 
        amount_from_rebalance: U128String, 
    ) 
    {
        assert_callback_calling();

        let sp = &mut self.staking_pools[sp_inx];
        let total_amount = amount_from_unstake_orders.0 + amount_from_rebalance.0;

        let unstake_succeeded = is_promise_success();

        let result: &str;
        if unstake_succeeded {
            result = "succeeded";
            sp.staked -= total_amount;
            sp.unstaked += total_amount;
            sp.unstk_req_epoch_height = env::epoch_height();
            self.total_unstaked_and_waiting += total_amount; // contract total unstaked_and_waiting
            self.unstaked_for_rebalance += amount_from_rebalance.0; // total unstaked_and_waiting for rebalance
            event!(
                r#"{{"event":"unstk","sp":"{}","amount_fuo":"{}","amount_fr":"{}" }}"#,
                sp.account_id,
                amount_from_unstake_orders.0,
                amount_from_rebalance.0
            );
        } else {
            result = "has failed";
            self.total_actually_staked += total_amount; //undo preventive action considering the amount unstaked
            self.epoch_unstake_orders += amount_from_unstake_orders.0; //undo preventive action considering the order fulfiled
        }

        log!("Unstaking of {} at @{} {}", total_amount, sp.account_id, result);

        //WARN: This is a callback after-cross-contract-call method
        //busy locks must be saved false in the state, this method SHOULD NOT PANIC
        sp.busy_lock = false;
        self.contract_busy = false;
    }

    //utility to set contract busy flag manually by operator.
    #[payable]
    pub fn set_busy(&mut self, value: bool) {
        assert_one_yocto();
        self.assert_operator_or_owner();
        assert!(self.contract_busy != value,"contract_busy is already {}",value);
        self.contract_busy = value;
    }
    //operator manual set sp.busy_lock
    #[payable]
    pub fn sp_busy(&mut self, sp_inx: u16, value: bool) {
        assert_one_yocto();
        self.assert_operator_or_owner();

        let inx = sp_inx as usize;
        assert!(inx < self.staking_pools.len());

        let sp = &mut self.staking_pools[inx];
        assert!(sp.busy_lock != value,"sp[{}].busy_lock is already {}",inx,value);
        sp.busy_lock = value;
    }

    //-- check If extra balance has accumulated (30% of tx fees by near-protocol)
    pub fn extra_balance_accumulated(&self) -> U128String {
        return env::account_balance()
            .saturating_sub(self.contract_account_balance)
            .into();
    }

    //-- If extra balance has accumulated (30% of tx fees by near-protocol)
    // transfer to self.operator_account_id
    pub fn transfer_extra_balance_accumulated(&mut self) -> U128String {
        let extra_balance = self.extra_balance_accumulated().0;
        if extra_balance >= ONE_NEAR {
            //only if there's more than one near, and left 10 cents (consider transfer fees)
            Promise::new(self.operator_account_id.clone()).transfer(extra_balance - 10 * NEAR_CENT);
            return extra_balance.into();
        }
        return 0.into();
    }

    //-------------------------
    /// sync_unstaked_balance: should be called before `retrieve_funds_from_a_pool`
    /// when you unstake, core-contracts/staking-pool does some share calculation *rounding*, so the real unstaked amount is not exactly
    /// the same amount requested (a minor, few yoctoNEARS difference)
    /// this fn syncs sp.unstaked with the real, current unstaked amount informed by the sp
    pub fn sync_unstaked_balance(&mut self, sp_inx: u16) -> Promise {
        // Note: We avoid locking the contract here (busy_flag), to close the possibility of someone spamming this method
        //  to prevent operator from issuing a command. Assuming there will be a way to front-run a transaction, it can
        //    block the contract. We do not lock the pool and the contract at all, but if the callback
        //    is called at the moment when the pool or the contract is locked, the result is ignored.

        let inx = sp_inx as usize;
        assert!(inx < self.staking_pools.len());

        self.assert_not_busy();
        let sp = &mut self.staking_pools[inx];
        assert!(!sp.busy_lock, "sp is busy");

        // SUGGESTION: Maybe better to call `get_account` to get information about `staked` and
        //    `unstaked` balance at the same time. Sometimes the staking pool may throw yoctoNEAR
        //    for rounding errors. So in case this pool may accidentally get 1 yocto and then
        //    overflow when subtracting staked from unstaked or vise-versa.

        //query our current unstaked amount
        return ext_staking_pool::get_account_unstaked_balance(
            env::current_account_id(),
            //promise params
            &sp.account_id,
            NO_DEPOSIT,
            gas::staking_pool::GET_ACCOUNT_TOTAL_BALANCE,
        )
        .then(ext_self_owner::on_get_sp_unstaked_balance(
            inx,
            //promise params
            &env::current_account_id(),
            NO_DEPOSIT,
            gas::owner_callbacks::ON_GET_SP_UNSTAKED_BALANCE,
        ));
    }

    /// prev fn continues here - sync_unstaked_balance
    //------------------------------
    pub fn on_get_sp_unstaked_balance(
        &mut self,
        sp_inx: usize,
        #[callback] unstaked_balance: U128String,
    ) {
        // NOTE: be careful on `#[callback]` here. If the pool view call fails for some
        //    reason this call will not be entered, because #[callback] fails for failed_promises
        //    So *never* add a pair of lock/unlock if the callback uses #[callback] params
        //    because the entire contract will be locked until the owner calls make non-busy.
        //    E.g. if owner makes a mistake adding a new pool and adds an invalid pool.

        //we enter here after asking the staking-pool how much do we have *unstaked*
        //unstaked_balance: U128String contains the answer from the staking-pool

        assert_callback_calling();

        let sp = &mut self.staking_pools[sp_inx];

        // real unstaked amount for this pool
        let real_unstaked_balance: u128 = unstaked_balance.0;

        log!(
            "inx:{} sp:{} old_unstaked_balance:{} new_unstaked_balance:{}",
            sp_inx,
            sp.account_id,
            sp.unstaked,
            real_unstaked_balance
        );
        // we're not locking at the start, so we check there's no in-flight transaction if we need to
        // adjust the unstaked in a few yoctos
        if real_unstaked_balance != sp.unstaked && (self.contract_busy || sp.busy_lock) {
            // do not proceed to update if another operation is in mid-flight
            panic!("cant not update unstaked, contract or sp is busy, another operation is in mid-flight");
        }

        if real_unstaked_balance > sp.unstaked {
            //positive difference
            let difference = real_unstaked_balance - sp.unstaked;
            log!("positive difference {}", difference);
            sp.unstaked = real_unstaked_balance;
            sp.staked = sp.staked.saturating_sub(difference); //the difference was in "our" record of "staked"
        } else if real_unstaked_balance < sp.unstaked {
            //negative difference
            let difference = sp.unstaked - real_unstaked_balance;
            log!("negative difference {}", difference);
            sp.unstaked = real_unstaked_balance;
            sp.staked += difference; //the difference was in "our" record of "staked"
        }
    }

    //------------------------------------------------------------------------
    //-- COMPUTE AND DISTRIBUTE STAKING REWARDS for a specific staking-pool --
    //------------------------------------------------------------------------
    // Operator method, but open to anyone. Should be called once per epoch per sp, after sp rewards distribution (ping)
    /// Ask total balance from the staking pool and remembers it internally.
    /// Also computes and distributes rewards for operator and stakers
    /// this fn queries the staking pool (makes a cross-contract call)
    pub fn distribute_rewards(&mut self, sp_inx: u16) {
        //Note: In order to make this contract independent from the operator
        //this fn is open to be called by anyone
        //self.assert_operator_or_owner();

        self.assert_not_busy();

        let inx = sp_inx as usize;
        assert!(inx < self.staking_pools.len());

        let sp = &mut self.staking_pools[inx];
        assert!(!sp.busy_lock, "sp is busy");

        let epoch_height = env::epoch_height();

        if sp.staked == 0 && sp.unstaked == 0 {
            return;
        }

        if sp.last_asked_rewards_epoch_height == epoch_height {
            return;
        }

        log!(
            "Fetching total balance from the staking pool @{}",
            sp.account_id
        );

        self.contract_busy = true;
        sp.busy_lock = true;

        //query our current balance (includes staked+unstaked+staking rewards)
        ext_staking_pool::get_account_total_balance(
            env::current_account_id(),
            //promise params
            &sp.account_id,
            NO_DEPOSIT,
            gas::staking_pool::GET_ACCOUNT_TOTAL_BALANCE,
        )
        .then(ext_self_owner::on_get_sp_total_balance(
            inx,
            //promise params
            &env::current_account_id(),
            NO_DEPOSIT,
            gas::owner_callbacks::ON_GET_SP_TOTAL_BALANCE,
        ));
    }

    /// prev fn continues here
    /*
    Note: what does the tag #[callback] applied to a fn in parameter do?
    #[callback] parses the previous promise's result into the param
        Check out https://nomicon.io/RuntimeSpec/Components/BindingsSpec/PromisesAPI.html
        1. check promise_results_count() == 1
        2  check the execution status of the first promise and write the result into the register using promise_result(0, register_id) == 1
            Let's say that you used register_id == 0
        3. read register using register_len and read_register into Wasm memory
        4. parse the data using: let total_balance: WrappedBalance = serde_json::from_slice(&buf).unwrap();

    it has be last argument? can you add another argument for the on_xxx callback ?
    before that
    for example:
        /// Called after the request to get the current total balance from the staking pool.
        pub fn on_get_account_total_balance(&mut self, staking_pool_account: AccountId, #[callback] total_balance: WrappedBalance) {
            assert_self();
            self.set_staking_pool_status(TransactionStatus::Idle);
            ...
        and in the call
            ext_staking_pool::get_account_total_balance(
                env::current_account_id(),
                staking_pool_account_id,
                NO_DEPOSIT,
                gas::staking_pool::GET_ACCOUNT_TOTAL_BALANCE,
            )
            .then(ext_self_owner::on_get_account_total_balance(
                staking_pool_account_id,
                &env::current_account_id(),
                NO_DEPOSIT,
                gas::owner_callbacks::ON_GET_ACCOUNT_TOTAL_BALANCE,
            ))

    #[callback] marked-arguments are parsed in order. The position within arguments are not important, but the order is.
    If you have 2 arguments marked as #[callback] then you need to expect 2 promise results joined with promise_and
    */

    pub fn on_get_sp_total_balance(
        &mut self,
        sp_inx: usize,
        #[callback] total_balance: U128String,
    ) {
        //we enter here after asking the staking-pool how much do we have staked (plus rewards)
        //total_balance: U128String contains the answer from the staking-pool

        assert_callback_calling();

        //new_total_balance has the new staked amount for this pool
        let new_total_balance: u128;
        let sp = &mut self.staking_pools[sp_inx];

        //WARN: This is a callback after-cross-contract-call method
        //busy locks must be saved false in the state, this method SHOULD NOT PANIC
        sp.busy_lock = false;
        self.contract_busy = false;

        sp.last_asked_rewards_epoch_height = env::epoch_height();

        //total_balance informed is staking-pool.staked + staking-pool.unstaked
        new_total_balance = total_balance.0;

        let rewards: u128;
        if new_total_balance < sp.total_balance() {
            log!(
                "INCONSISTENCY @{} says new_total_balance < our info sp.total_balance()",
                sp.account_id
            );
            rewards = 0;
        } else {
            //compute rewards, as new balance minus old balance
            rewards = new_total_balance - sp.total_balance();
        }

        log!(
            "sp:{} old_balance:{} new_balance:{} rewards:{} unstaked:{}",
            sp.account_id,
            sp.total_balance(),
            new_total_balance,
            rewards,
            sp.unstaked
        );

        //updated accumulated_staked_rewards value for the contract
        self.accumulated_staked_rewards += rewards;
        //updated new "staked" value for this pool
        sp.staked = new_total_balance - sp.unstaked;

        if rewards > 0 {
            //add to total_for_staking & total_actually_staked, increasing share value for all stNEAR holders
            self.total_actually_staked += rewards;
            self.total_for_staking += rewards;

            // mint extra stNEAR representing fees for operator & developers
            // The fee the operator takes from rewards (0.5%)
            let operator_fee = apply_pct(self.operator_rewards_fee_basis_points, rewards);
            let operator_fee_shares = self.stake_shares_from_amount(operator_fee);
            // The fee the contract authors take from rewards (0.2%)
            let developers_fee = apply_pct(DEVELOPERS_REWARDS_FEE_BASIS_POINTS, rewards);
            let developers_fee_shares = self.stake_shares_from_amount(developers_fee);
            // Now add the newly minted shares. The fee is taken by making share price increase slightly smaller
            self.add_extra_minted_shares(self.operator_account_id.clone(), operator_fee_shares);
            self.add_extra_minted_shares(DEVELOPERS_ACCOUNT_ID.into(), developers_fee_shares);

            // estimate $META rewards to stakers
            self.est_meta_rewards_stakers += damp_multiplier(
                rewards,
                self.staker_meta_mult_pct,
                self.est_meta_rewards_stakers,
                self.max_meta_rewards_stakers,
            );
        }
    }

    //----------------------------------------------------------------------
    // Operator method, but open to anyone
    //----------------------------------------------------------------------
    /// finds a pool with the unstake delay completed and some unstake ready for retrieve
    /// Returns `sp_index` or:
    /// -1 if there are funds ready to retrieve but the pool is busy
    /// -2 if there are funds unstaked, but not ready in this epoch
    /// -3 if there are no unstaked funds
    pub fn get_staking_pool_requiring_retrieve(&self) -> i32 {
        let mut not_found_result_code: i32 = -3;

        for (sp_inx, sp) in self.staking_pools.iter().enumerate() {
            if sp.unstaked > 0 {
                if not_found_result_code == -3 {
                    not_found_result_code = -2
                };
                if sp.wait_period_ended() {
                    if not_found_result_code == -2 {
                        not_found_result_code = -1
                    };
                    if !sp.busy_lock {
                        // if this pool has unstaked and the waiting period has ended
                        return sp_inx as i32;
                    }
                }
            }
        }
        return not_found_result_code;
    }

    // Operator method, but open to anyone
    //----------------------------------------------------------------------
    //  WITHDRAW FROM ONE OF THE POOLS ONCE THE WAITING PERIOD HAS ELAPSED
    //----------------------------------------------------------------------
    /// launches a withdrawal call
    /// returns the amount withdrawn
    /// you MUST call get_staking_pool_requiring_retrieve() first, to obtain a valid inx
    /// and you MUST call sync_unstaked_balance(inx) before this, to get the exact amount to the yocto stored in sp.unstaked
    pub fn retrieve_funds_from_a_pool(&mut self, inx: u16) -> Promise {
        //Note: In order to make fund-recovering independent from the operator
        //this fn is open to be called by anyone

        assert!(inx < self.staking_pools.len() as u16, "invalid index");

        self.assert_not_busy();

        let sp = &mut self.staking_pools[inx as usize];
        assert!(!sp.busy_lock, "sp is busy");
        assert!(sp.unstaked > 0, "sp unstaked == 0");
        if !sp.wait_period_ended() {
            panic!(
                "unstaking-delay ends at {}, now is {}",
                sp.unstk_req_epoch_height + NUM_EPOCHS_TO_UNLOCK,
                env::epoch_height()
            );
        }

        // if we're here, the pool is not busy, and we unstaked and the waiting period has elapsed

        self.contract_busy = true;
        sp.busy_lock = true;

        //return promise
        return ext_staking_pool::withdraw_all(
            //promise params:
            &sp.account_id,
            NO_DEPOSIT,
            gas::staking_pool::WITHDRAW,
        )
        .then(ext_self_owner::on_retrieve_from_staking_pool(
            inx,
            //promise params:
            &env::current_account_id(),
            NO_DEPOSIT,
            gas::owner_callbacks::ON_STAKING_POOL_WITHDRAW,
        ));
    }
    //prev fn continues here
    /// This method needs to update staking pool busyLock
    pub fn on_retrieve_from_staking_pool(&mut self, inx: u16) -> U128String {
        assert_callback_calling();

        let sp = &mut self.staking_pools[inx as usize];
        let sp_account_id = sp.account_id.clone();
        let amount = sp.unstaked; // we retrieved all

        //WARN: This is a callback after-cross-contract-call method
        //busy locks must be saved false in the state, this method SHOULD NOT PANIC
        sp.busy_lock = false;
        self.contract_busy = false;

        let retrieve_succeeded = is_promise_success();

        let result: &str;
        let retrieved_amount: u128;
        if retrieve_succeeded {
            result = "succeeded";
            retrieved_amount = amount;
            sp.unstaked -= amount; // is no longer in the pool as "unstaked"
            self.total_unstaked_and_waiting = // contract total_unstaked_and_waiting decremented...
                self.total_unstaked_and_waiting.saturating_sub(amount); // ... because is no longer waiting
            self.contract_account_balance += amount; // the amount is now in the contract balance
            //log event
            event!(
                r#"{{"event":"retrieve","sp":"{}","amount":"{}"}}"#,
                sp_account_id,
                amount
            );
            // the amount retrieved should be considered "retrieved_for_unstake_claims" until the user calls withdraw_unstaked
            self.consider_retrieved_for_unstake_claims(amount);

        } else {
            result = "has failed";
            retrieved_amount = 0;
        }
        log!(
            "The withdrawal of {} from @{} {}",
            amount,
            sp_account_id,
            result
        );


        return retrieved_amount.into();
    }

    // Operator method, but open to anyone. No need to be called, is auto called before distribute stake/unstake
    //----------------------------------------------------------------------
    // End of Epoch clearing of STAKE_ORDERS vs UNSTAKE_ORDERS
    //----------------------------------------------------------------------
    // At the end of the epoch, only the delta between stake & unstake orders needs to be actually staked
    // if there are more in the stake orders than the unstake orders, some NEAR will not be sent to the pools
    // e.g. stake-orders: 1200, unstake-orders:1000 => net: stake 200 and keep 1000 to fulfill unstake claims after 4 epochs.
    // if there was more in the unstake orders than in the stake orders, a real unstake was initiated with one or more pools,
    // the rest should also be kept to fulfill unstake claims after 4 epochs.
    // e.g. stake-orders: 700, unstake-orders:1000 => net: start-unstake 300 and keep 700 to fulfill unstake claims after 4 epochs
    // if the delta is 0, there's no real stake-unstake, but the amount should be kept to fulfill unstake claims after 4 epochs
    // e.g. stake-orders: 500, unstake-orders:500 => net: 0 so keep 500 to fulfill unstake claims after 4 epochs.
    //
    pub fn end_of_epoch_clearing(&mut self) {
        self.internal_end_of_epoch_clearing() 
    }

    /// compute max cap for the unstakes-for-rebalance
    /// default unstake_for_rebalance_cap_bp = 100, so max_unstake_for_rebalance = 1%
    fn max_unstake_for_rebalance(&self) -> u128 {
        apply_pct(self.unstake_for_rebalance_cap_bp, self.total_for_staking)
    }

}
