use crate::*;
use near_sdk::{
    json_types::{ValidAccountId, U128},
    log, AccountId, Balance, Promise, PromiseResult,
};

pub use crate::types::*;
pub use crate::utils::*;

const UNSTAKED_YOCTOS_TO_IGNORE:u128 = 100;

pub struct GSPRUResult {
    pub sp_inx:u16, 
    pub extra:u128, 
    pub count_unblocked:u16,
    pub count_with_stake:u16,
    pub total_extra:u128
}

/****************************/
/* general Internal methods */
/****************************/
impl MetaPool {
    /// Asserts that the method was called by the owner.
    pub fn assert_owner_calling(&self) {
        assert_eq!(
            &env::predecessor_account_id(),
            &self.owner_account_id,
            "Can only be called by the owner"
        )
    }
    pub fn assert_operator_or_owner(&self) {
        assert!(
            &env::predecessor_account_id() == &self.owner_account_id
                || &env::predecessor_account_id() == &self.operator_account_id,
            "Can only be called by the operator or the owner"
        );
    }

    pub fn assert_not_busy(&self) {
        assert!(!self.contract_busy, "Contract is busy. Try again later");
    }

    pub fn assert_min_deposit_amount(&self, amount: u128) {
        assert!(
            amount >= self.min_deposit_amount,
            "minimum deposit amount is {}",
            self.min_deposit_amount
        );
    }
}

/***************************************/
/* Internal methods staking-pool trait */
/***************************************/
impl MetaPool {
    pub(crate) fn internal_deposit(&mut self) {
        self.assert_min_deposit_amount(env::attached_deposit());
        self.internal_deposit_attached_near_into(env::predecessor_account_id());
    }

    pub(crate) fn internal_deposit_attached_near_into(&mut self, account_id: AccountId) {
        let amount = env::attached_deposit();

        let mut account = self.internal_get_account(&account_id);

        account.available += amount;
        self.total_available += amount;
        self.contract_account_balance += amount;

        self.internal_update_account(&account_id, &account);

        log!(
            "{} deposited into @{}'s account. New available balance is {}",
            amount,
            account_id,
            account.available
        );
    }

    //------------------------------
    // MIMIC staking-pool, if there are unstaked, it must be free to withdraw
    pub(crate) fn internal_withdraw_use_unstaked(&mut self, requested_amount: u128) -> Promise {
        let account_id = env::predecessor_account_id();
        let mut account = self.internal_get_account(&account_id);

        //MIMIC staking-pool, move 1st form unstaked->available, it must be free to withdraw
        account.in_memory_try_finish_unstaking(&account_id, requested_amount, self);

        // NOTE: While ability to withdraw close to all available helps, it prevents lockup contracts from using this in a replacement to a staking pool,
        // because the lockup contracts relies on exact precise amount being withdrawn.
        let amount = account.take_from_available(requested_amount, self);

        //commented: Remove min_account_balance requirements, increase liq-pool target to cover all storage requirements
        //2 reasons: a) NEAR storage was cut by 10x  b) in the simplified flow, users do not keep "available" balance
        // assert!( !acc.is_empty() || acc.available >= self.min_account_balance,
        //     "The min balance for an open account is {} NEAR. You need to close the account to remove all funds",
        //     self.min_account_balance/NEAR);

        self.internal_update_account(&account_id, &account);
        //transfer to user native near account
        self.native_transfer_to_predecessor(amount)
    }
    pub(crate) fn native_transfer_to_predecessor(&mut self, amount: u128) -> Promise {
        //transfer to user native near account
        self.contract_account_balance -= amount;
        Promise::new(env::predecessor_account_id()).transfer(amount)
    }

    //------------------------------
    /// takes from account.available and mints stNEAR for account_id
    /// actual stake in a staking-pool is made by the meta-pool-heartbeat before the end of the epoch
    pub(crate) fn internal_stake_from_account(
        &mut self,
        account_id: AccountId,
        near_amount: Balance,
    ) {
        self.assert_not_busy();

        self.assert_min_deposit_amount(near_amount);

        let mut acc = self.internal_get_account(&account_id);

        // take from the account "available" balance
        // also subs self.total_available
        let amount = acc.take_from_available(near_amount, self);

        // Calculate the number of st_near (stake shares) that the account will receive for staking the given amount.
        let num_shares = self.stake_shares_from_amount(amount);
        assert!(num_shares > 0);

        //add shares to user account
        acc.add_stake_shares(num_shares, amount);
        //contract totals
        self.total_stake_shares += num_shares;
        self.total_for_staking += amount;
        self.epoch_stake_orders += amount;

        //--SAVE ACCOUNT--
        self.internal_update_account(&account_id, &acc);

        //log event
        event!(
            r#"{{"event":"STAKE","account":"{}","amount":"{}"}}"#,
            account_id,
            amount
        );
    }

    //------------------------------
    /// delayed_unstake, amount_requested is in yoctoNEARs
    pub(crate) fn internal_unstake(&mut self, amount_requested: u128) {
        self.assert_not_busy();

        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);

        let valued_shares = self.amount_from_stake_shares(acc.stake_shares);

        let amount_to_unstake: u128;
        let stake_shares_to_burn: u128;
        // if the amount is close to user's total, remove user's total
        // to: a) do not leave less than 1/1000 NEAR in the account, b) Allow 10 yoctos of rounding, e.g. remove(100) removes 99.999993 without panicking
        if is_close(amount_requested, valued_shares) {
            // allow for rounding simplification
            amount_to_unstake = valued_shares;
            stake_shares_to_burn = acc.stake_shares; // close enough to all shares, burn-it all (avoid leaving "dust")
        } else {
            //use amount_requested
            amount_to_unstake = amount_requested;
            // Calculate the number shares that the account will burn based on the amount requested
            stake_shares_to_burn = self.stake_shares_from_amount(amount_requested);
        }

        assert!(
            valued_shares >= amount_to_unstake,
            "Not enough value {} to unstake the requested amount",
            valued_shares
        );
        assert!(stake_shares_to_burn > 0 && stake_shares_to_burn <= acc.stake_shares);
        //use this operation to realize meta pending rewards
        acc.stake_realize_meta(self);

        //remove acc stake shares
        acc.sub_stake_shares(stake_shares_to_burn, amount_to_unstake);
        //the amount is now "unstaked", i.e. the user has a claim to this amount, 4-8 epochs form now
        acc.unstaked += amount_to_unstake;
        acc.unstaked_requested_unlock_epoch =
            env::epoch_height() + self.internal_compute_current_unstaking_delay(amount_to_unstake); //when the unstake will be available
                                                                                                    //--contract totals
        self.epoch_unstake_orders += amount_to_unstake;
        self.total_unstake_claims += amount_to_unstake;
        self.total_stake_shares -= stake_shares_to_burn; //burn
        self.total_for_staking -= amount_to_unstake;

        //--SAVE ACCOUNT--
        self.internal_update_account(&account_id, &acc);

        event!(
            r#"{{"event":"D-UNSTK","account_id":"{}","amount":"{}","shares":"{}"}}"#,
            account_id,
            amount_to_unstake,
            stake_shares_to_burn
        );

        log!(
            "@{} unstaked {}. Has now {} unstaked and {} stNEAR. Epoch:{}",
            account_id,
            amount_to_unstake,
            acc.unstaked,
            acc.stake_shares,
            env::epoch_height()
        );
    }

    //--------------------------------------------------
    /// adds liquidity from deposited amount
    pub(crate) fn internal_nslp_add_liquidity(&mut self, amount_requested: u128) -> u16 {
        self.assert_not_busy();

        let account_id = env::predecessor_account_id();
        let mut acc = self.internal_get_account(&account_id);

        //take from the account "available" balance
        let amount = acc.take_from_available(amount_requested, self);

        //get NSLP account
        let mut nslp_account = self.internal_get_nslp_account();

        //use this LP operation to realize meta pending rewards
        acc.nslp_realize_meta(&nslp_account, self);

        // Calculate the number of "nslp" shares the account will receive for adding the given amount of near liquidity
        let num_shares = self.nslp_shares_from_amount(amount, &nslp_account);
        assert!(num_shares > 0);

        //register added liquidity to compute rewards correctly
        acc.lp_meter.stake(amount);

        //update user account
        acc.nslp_shares += num_shares;
        //update NSLP account & main
        nslp_account.available += amount;
        self.total_available += amount;
        nslp_account.nslp_shares += num_shares; //total nslp shares

        //compute the % the user now owns of the Liquidity Pool (in basis points)
        let result_bp = proportional(10_000, acc.nslp_shares, nslp_account.nslp_shares) as u16;

        //--SAVE ACCOUNTS
        self.internal_update_account(&account_id, &acc);
        self.internal_save_nslp_account(&nslp_account);

        event!(
            r#"{{"event":"ADD.L","account_id":"{}","amount":"{}"}}"#,
            account_id,
            amount
        );

        return result_bp;
    }

    //--------------------------------------------------
    /// computes unstaking delay on current situation
    pub fn internal_compute_current_unstaking_delay(&self, amount: u128) -> u64 {
        let mut total_staked: u128 = 0;
        let mut normal_wait_staked_available: u128 = 0;
        for (_, sp) in self.staking_pools.iter().enumerate() {
            //if the pool has no unstaking in process
            total_staked += sp.staked;
            if !sp.busy_lock && sp.staked > 0 && sp.wait_period_ended() {
                normal_wait_staked_available += sp.staked;
                if normal_wait_staked_available > amount {
                    return NUM_EPOCHS_TO_UNLOCK;
                }
            }
        }
        if total_staked == 0 {
            //initial stake, nothing staked, someone delay-unstaking in contract epoch 0
            return NUM_EPOCHS_TO_UNLOCK;
        };
        //all pools are in unstaking-delay, it will take double the time
        return 2 * NUM_EPOCHS_TO_UNLOCK;
    }

    //--------------------------------
    // fees are extracted by minting a small amount of extra stNEAR
    pub(crate) fn add_extra_minted_shares(&mut self, account_id: AccountId, num_shares: u128) {
        if num_shares > 0 {
            let account = &mut self.internal_get_account(&account_id);
            account.stake_shares += num_shares;
            self.internal_update_account(&account_id, &account);
            // Increasing the total amount of stake shares (reduces price)
            self.total_stake_shares += num_shares;
        }
    }

    /// Returns the number of stNEAR (stake shares) corresponding to the given near amount at current stNEAR price
    /// if the amount & the shares are incorporated, price remains the same
    pub(crate) fn stake_shares_from_amount(&self, amount: Balance) -> u128 {
        return shares_from_amount(amount, self.total_for_staking, self.total_stake_shares);
    }

    /// Returns the amount corresponding to the given number of stNEAR (stake shares).
    pub(crate) fn amount_from_stake_shares(&self, num_shares: u128) -> u128 {
        return amount_from_shares(num_shares, self.total_for_staking, self.total_stake_shares);
    }

    //-----------------------------
    // NSLP: NEAR/stNEAR Liquidity Pool
    //-----------------------------

    // NSLP shares are trickier to compute since the NSLP itself can have stNEAR
    pub(crate) fn nslp_shares_from_amount(&self, amount: u128, nslp_account: &Account) -> u128 {
        let total_pool_value: u128 =
            nslp_account.available + self.amount_from_stake_shares(nslp_account.stake_shares);
        return shares_from_amount(amount, total_pool_value, nslp_account.nslp_shares);
    }

    // NSLP shares are trickier to compute since the NSLP itself can have stNEAR
    pub(crate) fn amount_from_nslp_shares(&self, num_shares: u128, nslp_account: &Account) -> u128 {
        let total_pool_value: u128 =
            nslp_account.available + self.amount_from_stake_shares(nslp_account.stake_shares);
        return amount_from_shares(num_shares, total_pool_value, nslp_account.nslp_shares);
    }

    //----------------------------------
    // The LP acquires stNEAR providing the liquid-unstake service
    // The LP needs to remove stNEAR automatically, to recover liquidity and to keep a low fee
    // The LP can recover near by internal clearing.
    // returns true if it used internal clearing
    // ---------------------------------
    pub(crate) fn nslp_try_internal_clearing(&mut self, staked_amount: u128) -> bool {
        // the user has just staked staked_amount of NEAR, they got stNEAR already
        let mut nslp_account = self.internal_get_nslp_account();
        log!(
            "nslp internal clearing nslp_account.stake_shares {}",
            nslp_account.stake_shares
        );
        // should not happen
        assert!(
            self.epoch_stake_orders >= staked_amount,
            "ERR in nslp_try_internal_clearing"
        );

        if nslp_account.stake_shares > 0 {
            //how much stNEAR do the nslp has?
            let valued_stake_shares = self.amount_from_stake_shares(nslp_account.stake_shares);
            //how much can we liquidate?
            let (st_near_to_sell, near_value) = if staked_amount >= valued_stake_shares {
                (nslp_account.stake_shares, valued_stake_shares) //all of them
            } else {
                (self.stake_shares_from_amount(staked_amount), staked_amount) //the amount recently staked
            };

            log!("NSLP clearing {} {}", st_near_to_sell, near_value);
            //log event
            event!(
                r#"{{"event":"NSLP.clr","shares":"{}","amount":"{}"}}"#,
                st_near_to_sell,
                near_value
            );

            // users made a deposit+mint, and now we need to convert that into a 0-fee swap NEAR<->stNEAR
            // we take NEAR from the contract, but let the users keep their minted stNEAR
            // we also burn liq-pool's stNEAR compensating the mint,
            // so the users' "deposit+mint" gets converted to a "send NEAR to the liq-pool, get stNEAR"
            self.total_for_staking -= near_value; // nslp gets the NEAR
            self.epoch_stake_orders -= near_value; // (also reduce epoch stake orders, that was incremented by user stake action)
            nslp_account.available += near_value; // nslp has more available now
            self.total_available += near_value; // which must be reflected in contract totals

            nslp_account.sub_stake_shares(st_near_to_sell, near_value); //nslp delivers stNEAR in exchange for the NEAR
            self.total_stake_shares -= st_near_to_sell; //we burn them (but the users keep the ones we minted earlier)
                                                        //save nslp account
            self.internal_save_nslp_account(&nslp_account);

            return true;
        }
        return false;
    }

    /// computes swap_fee_basis_points for NEAR/stNEAR Swap based on NSLP Balance
    pub(crate) fn internal_get_discount_basis_points(
        &self,
        available_near: u128,
        nears_requested: u128,
    ) -> u16 {
        log!(
            "get_discount_basis_points available_near={}  max_nears_to_pay={}",
            available_near,
            nears_requested
        );

        if available_near <= nears_requested {
            return self.nslp_max_discount_basis_points;
        }
        //amount after the swap
        let near_after = available_near - nears_requested;
        if near_after >= self.nslp_liquidity_target {
            //still >= target
            return self.nslp_min_discount_basis_points;
        }

        //linear curve from max to min on target
        let range = self.nslp_max_discount_basis_points - self.nslp_min_discount_basis_points;
        //here 0<near_after<self.nslp_liquidity_target, so 0<proportional_bp<range
        let proportional_bp = proportional(range as u128, near_after, self.nslp_liquidity_target);

        return self.nslp_max_discount_basis_points - proportional_bp as u16;
    }

    /// NEAR/stNEAR SWAP functions
    /// return how much NEAR you can get by selling x stNEAR
    pub(crate) fn internal_get_near_amount_sell_stnear(
        &self,
        available_near: u128,
        st_near_to_sell: u128,
    ) -> u128 {
        //compute how many nears are the st_near valued at
        let nears_out = self.amount_from_stake_shares(st_near_to_sell);
        let swap_fee_basis_points =
            self.internal_get_discount_basis_points(available_near, nears_out);
        assert!(swap_fee_basis_points < 10000, "inconsistency d>1");
        let fee = apply_pct(swap_fee_basis_points, nears_out);
        return (nears_out - fee).into(); //when stNEAR is sold user pays a swap fee (the user skips the waiting period)

        // env::log(
        //     format!(
        //         "@{} withdrawing {}. New unstaked balance is {}",
        //         account_id, amount, account.unstaked
        //     )
        //     .as_bytes(),
        // );
    }

    /// Inner method to get the given account or a new default value account.
    pub(crate) fn internal_get_account(&self, account_id: &String) -> Account {
        self.accounts.get(account_id).unwrap_or_default()
    }

    pub(crate) fn account_exists(&self, account_id: &String) -> bool {
        self.accounts.get(account_id).is_some()
    }
    
    /// Inner method to save the given account for a given account ID.
    pub(crate) fn internal_update_account(&mut self, account_id: &String, account: &Account) {
        self.accounts.insert(account_id, &account); //insert_or_update
    }

    /// Inner method to get the given account or a new default value account.
    pub(crate) fn internal_get_nslp_account(&self) -> Account {
        self.accounts
            .get(&NSLP_INTERNAL_ACCOUNT.into())
            .unwrap_or_default()
    }
    pub(crate) fn internal_save_nslp_account(&mut self, nslp_account: &Account) {
        self.internal_update_account(&NSLP_INTERNAL_ACCOUNT.into(), &nslp_account);
    }

    /// finds a staking pool requiring some stake to get balanced
    /// WARN: (returns 0,0) if no pool requires staking/all are busy
    pub(crate) fn get_staking_pool_requiring_stake(&self) -> (usize, u128) {
        let mut selected_to_stake_amount: u128 = 0;
        let mut selected_sp_inx: usize = 0;

        for (sp_inx, sp) in self.staking_pools.iter().enumerate() {
            // if the pool is not busy, and this pool can stake
            if !sp.busy_lock && sp.weight_basis_points > 0 {
                // if this pool has an unbalance requiring staking
                let should_have = apply_pct(sp.weight_basis_points, self.total_for_staking);
                // this pool requires staking?
                if should_have > sp.staked {
                    // how much?
                    let require_amount = should_have - sp.staked;
                    // is this the most unbalanced pool so far?
                    if require_amount > selected_to_stake_amount {
                        selected_to_stake_amount = require_amount;
                        selected_sp_inx = sp_inx;
                    }
                }
            }
        }

        return (selected_sp_inx, selected_to_stake_amount);
    }

    /// finds a staking pool requiring some unstake to get balanced
    /// WARN: returns (0,0) if no pool requires unstaking/all are busy
    pub(crate) fn internal_get_staking_pool_requiring_unstake(&self) -> GSPRUResult {
        let mut selected_sp_inx: usize = 0;
        let mut selected_extra_amount: u128 = 0;
        let mut total_extra: u128 = 0;
        let mut count_unblocked: u16 = 0;
        let mut count_with_stake: u16 = 0;

        for (sp_inx, sp) in self.staking_pools.iter().enumerate() {
            // if the pool is not busy, has stake
            if !sp.busy_lock && sp.staked > 0 {
                // count how how many sps are unblocked, i.e. can receive an unstake request
                count_with_stake += 1;
                if sp.unstaked <= UNSTAKED_YOCTOS_TO_IGNORE { // 100 yoctos
                    count_unblocked += 1
                };
                // check if this pool has an unbalance requiring un-staking
                let should_have = apply_pct(sp.weight_basis_points, self.total_for_staking);
                debug!(
                    r#"{{"event":"gtp.req.unstk shld:{} extra:{} unstk:{} w:{} {} {}","sp":"{}","amount":"{}"}}"#,
                    should_have / NEAR,
                    sp.staked.saturating_sub(should_have) / NEAR,
                    sp.unstaked / NEAR,
                    sp.weight_basis_points,
                    sp.unstk_req_epoch_height,
                    env::epoch_height(),
                    sp_inx,
                    sp.staked
                );
                // if not waiting, or wait started in this same epoch (no harm in unstaking more)
                // NOTE: Unstaking in the same epoch is only an issue, if you hit the last block of the epoch.
                //       In this case the receipt may be executed at the next epoch.
                // NOTE2: core-contracts/staking-pool is imprecise when unstaking, some times 1 to 10 yoctos remain in "unstaked"
                //        The bot should synchronize unstaked yoctos before calling this function.
                // We assume that if sp.unstaked>100 yoctos, a new unstake will cause that amount to be blocked
                if sp.unstaked <= UNSTAKED_YOCTOS_TO_IGNORE || sp.unstk_req_epoch_height == env::epoch_height() {
                    // does this pool requires un-staking? (has too much staked?)
                    if sp.staked > should_have {
                        // how much?
                        let extra = sp.staked - should_have;
                        total_extra += extra;
                        // is this the most unbalanced pool so far?
                        if extra > selected_extra_amount {
                            selected_extra_amount = extra;
                            selected_sp_inx = sp_inx;
                        }
                    }
                }
            }
        }

        GSPRUResult {
            sp_inx: selected_sp_inx as u16, 
            extra: selected_extra_amount, 
            count_unblocked, 
            count_with_stake, 
            total_extra}
    }

    pub fn internal_st_near_transfer(
        &mut self,
        sender_id: &AccountId,
        receiver_id: &AccountId,
        amount: u128,
    ) {
        assert_ne!(
            sender_id, receiver_id,
            "Sender and receiver should be different"
        );
        assert!(amount > 0, "The amount should be a positive number");
        let mut sender_acc = self.internal_get_account(&sender_id);
        let mut receiver_acc = self.internal_get_account(&receiver_id);
        assert!(
            amount <= sender_acc.stake_shares,
            "@{} not enough stNEAR balance {}",
            sender_id,
            sender_acc.stake_shares
        );

        let near_amount = self.amount_from_stake_shares(amount); //amount is in stNEAR(aka shares), let's compute how many nears that is - for acc.staking_meter
        sender_acc.sub_stake_shares(amount, near_amount);
        receiver_acc.add_stake_shares(amount, near_amount);

        self.internal_update_account(&sender_id, &sender_acc);
        self.internal_update_account(&receiver_id, &receiver_acc);
    }

    // MULTI FUN TOKEN [NEP-138](https://github.com/near/NEPs/pull/138)
    /// Transfer `amount` of tok tokens from the caller of the contract (`predecessor_id`) to `receiver_id`.
    /// Requirements:
    /// * receiver_id must pre-exist
    /// LMT - commented, no longer used
    /*
    pub fn internal_multifuntok_transfer(
        &mut self,
        sender_id: &AccountId,
        receiver_id: &AccountId,
        symbol: &str,
        amount: u128,
    ) {
        assert_ne!(
            sender_id, receiver_id,
            "Sender and receiver should be different"
        );
        assert!(amount > 0, "The amount should be a positive number");
        let mut sender_acc = self.internal_get_account(&sender_id);
        let mut receiver_acc = self.internal_get_account(&receiver_id);
        match &symbol as &str {
            "NEAR" => {
                assert!(
                    sender_acc.available >= amount,
                    "@{} not enough NEAR available {}",
                    sender_id,
                    sender_acc.available
                );
                sender_acc.available -= amount;
                receiver_acc.available += amount;
            }
            STNEAR => {
                let max_stnear = sender_acc.stake_shares;
                assert!(
                    amount <= max_stnear,
                    "@{} not enough stNEAR balance {}",
                    sender_id,
                    max_stnear
                );
                let near_amount = self.amount_from_stake_shares(amount); //amount is in stNEAR(aka shares), let's compute how many nears that is
                sender_acc.sub_stake_shares(amount, near_amount);
                receiver_acc.add_stake_shares(amount, near_amount);
            }
            "META" => {
                sender_acc.stake_realize_meta(self);
                assert!(
                    sender_acc.realized_meta >= amount,
                    "@{} not enough $META balance {}",
                    sender_id,
                    sender_acc.realized_meta
                );
                sender_acc.realized_meta -= amount;
                receiver_acc.realized_meta += amount;
            }
            _ => panic!("invalid symbol"),
        }
        self.internal_update_account(&sender_id, &sender_acc);
        self.internal_update_account(&receiver_id, &receiver_acc);
    }
    */

    // ft_token, executed after ft_transfer_call,
    // resolves (maybe refunds)
    // TODO rename
    pub fn int_ft_resolve_transfer(
        &mut self,
        sender_id: &AccountId,
        receiver_id: ValidAccountId,
        amount: U128,
    ) -> (u128, u128) {
        let sender_id: AccountId = sender_id.into();
        let receiver_id: AccountId = receiver_id.into();
        let amount: Balance = amount.into();

        // Get the unused amount from the `ft_on_transfer` call result.
        let unused_amount = match env::promise_result(0) {
            PromiseResult::NotReady => unreachable!(),
            PromiseResult::Successful(value) => {
                if let Ok(unused_amount) = near_sdk::serde_json::from_slice::<U128>(&value) {
                    std::cmp::min(amount, unused_amount.0)
                } else {
                    amount
                }
            }
            PromiseResult::Failed => amount,
        };

        if unused_amount > 0 {
            let mut receiver_acc = self.internal_get_account(&receiver_id);
            let receiver_balance = receiver_acc.stake_shares;
            if receiver_balance > 0 {
                let refund_amount = std::cmp::min(receiver_balance, unused_amount);
                let near_amount = self.amount_from_stake_shares(refund_amount); //amount is in stNEAR(aka shares), let's compute how many nears that is
                receiver_acc.sub_stake_shares(refund_amount, near_amount);
                self.internal_update_account(&receiver_id, &receiver_acc);

                let mut sender_acc = self.internal_get_account(&sender_id);
                sender_acc.add_stake_shares(refund_amount, near_amount);
                self.internal_update_account(&sender_id, &sender_acc);

                log!(
                    "Refund {} from {} to {}",
                    refund_amount,
                    receiver_id,
                    sender_id
                );
                return (amount - refund_amount, 0);
            }
        }
        (amount, 0)
    }

    pub(crate) fn internal_end_of_epoch_clearing(&mut self) {
        self.assert_not_busy();
        // This method is called before any actual staking/unstaking.

        // if any one of the two is zero, we've a pure stake or pure unstake epoch, no clearing
        // just go and stake or unstake
        if self.epoch_stake_orders == 0 || self.epoch_unstake_orders == 0 {
            return 
        }

        // NOTE: `to_keep` can also be computed as `min(self.epoch_stake_orders, self.epoch_unstake_orders)`
        let to_keep = if self.epoch_stake_orders >= self.epoch_unstake_orders {
            // if more stake-orders than unstake-orders, we keep the NEAR corresponding to epoch_unstake_orders (delayed unstakes)
            // we keep it from now, so the users can withdraw in 4 epochs (clearing: no need to stake and then unstake)
            self.epoch_unstake_orders
        } else {
            // if more delayed-unstakes than stakes, we keep at least the stake-orders, the NEAR we have (clearing: no need to stake and then unstake)
            // and the rest (delta) will be unstakes before EOE
            self.epoch_stake_orders
        };

        // clear opposing orders
        self.epoch_stake_orders -= to_keep;
        self.epoch_unstake_orders -= to_keep;
        event!(r#"{{"event":"clr.ord","keep":"{}"}}"#, to_keep);

        // we will keep this NEAR (no need to go to the pools). We consider it reserved for unstake_claims, 4 epochs from now
        // or maybe some part could be put again in epoch_stake_orders to re-stake
        self.consider_retrieved_for_unstake_claims(to_keep);
    }

    /// if we have to add some funds to retrieved_for_unstake_claims
    /// this fn consider possible "extra" funds coming from rebalances and
    /// send those to epochs_stake_orders to be restaked
    pub(crate) fn consider_retrieved_for_unstake_claims(&mut self, amount: u128) {
        self.retrieved_for_unstake_claims += amount;

        // CONSIDER REBALANCE:
        // we retrieved funds and incremented "retrieved_for_unstake_claims"
        // BUT this retrieval could originate from a rebalance action,
        // so we check if we have more NEAR in the contract than the amount we need for total_unstake_claims,
        // and if we do, then put the extra NEAR to re-stake, thus completing the rebalance
        // Note: we're ignoring unstaked_and_waiting and current epoch_unstake_orders, because reserve_for_unstake_claims has priority
        if self.retrieved_for_unstake_claims > self.total_unstake_claims {
            // confirmed extra to rebalance
            let extra = self.retrieved_for_unstake_claims - self.total_unstake_claims;
            self.retrieved_for_unstake_claims -= extra; // remove extra from reserve
            self.epoch_stake_orders += extra; // put it in epoch_stake_orders, so the funds are re-staked before EOE
            self.unstaked_for_rebalance = self.unstaked_for_rebalance.saturating_sub(extra); // no longer waiting

            //log event
            event!(
                r#"{{"event":"rebalance","extra":"{}","retrieved_for_unstake_claims":"{}","total_unstake_claims":"{}","ufr":"{}"}}"#,
                extra,
                self.retrieved_for_unstake_claims,
                self.total_unstake_claims,
                self.unstaked_for_rebalance,
            );
        }
    }

}
