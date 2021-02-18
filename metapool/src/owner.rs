use crate::*;
use near_sdk::{near_bindgen, Promise, PublicKey};

#[near_bindgen]
impl MetaPool {
    
    // OWNER'S METHODS and getters

    /// Adds full access key with the given public key to the account once the contract is empty
    /// (has no accounts)
    /// Requires 50 TGas (2 * BASE_GAS)
    pub fn add_full_access_key(&mut self, new_public_key: Base58PublicKey) -> Promise {
        
        self.assert_owner_calling();

        assert!(self.accounts.len() == 0, "contract still has accounts");

        env::log(b"Adding a full access key");

        let new_public_key: PublicKey = new_public_key.into();

        Promise::new(env::current_account_id()).add_full_access_key(new_public_key)
    }

    /// Owner's method.
    /// Pauses pool staking.
    pub fn pause_staking(&mut self) {
        self.assert_owner_calling();
        assert!(!self.staking_paused, "The staking is already paused");
        self.staking_paused = true;
    }

    //---------------------------------
    // staking-pools-list (SPL) management
    //---------------------------------

    /// get the current list of pools
    pub fn get_staking_pool_list(&self) -> Vec<StakingPoolJSONInfo> {
        let mut result = Vec::with_capacity(self.staking_pools.len());
        for elem in self.staking_pools.iter(){
            result.push(StakingPoolJSONInfo{
                account_id: elem.account_id.clone(),
                weight_basis_points: elem.weight_basis_points,
                staked: elem.staked.into(),
                unstaked: elem.unstaked.into(),
                last_asked_rewards_epoch_height: elem.last_asked_rewards_epoch_height.into(),
                unstaked_requested_epoch_height: elem.unstk_req_epoch_height.into(),
            })
        }
        return result;
    }

    ///remove staking pool from list *if it's empty*
    pub fn remove_staking_pool(&mut self, inx:u16 ){

        self.assert_owner_calling();

        let sp = &self.staking_pools[inx as usize];
        if !sp.is_empty() {
            panic!(b"sp is not empty")
        }
        self.staking_pools.remove(inx as usize);
    }

    ///update existing weight_basis_points
    pub fn set_staking_pool_weight(&mut self, inx:u16, weight_basis_points:u16 ){

        self.assert_owner_calling();

        let sp = &mut self.staking_pools[inx as usize];
        if sp.busy_lock {
            panic!(b"sp is busy")
        }
        sp.weight_basis_points = weight_basis_points;
        self.check_staking_pool_list_consistency();
    }
    
    ///add a new staking pool or update existing weight_basis_points
    pub fn set_staking_pool(&mut self, account_id:AccountId, weight_basis_points:u16 ){

        self.assert_owner_calling();

        //search the pools
        for sp_inx in 0..self.staking_pools.len() {
            if self.staking_pools[sp_inx].account_id==account_id {
                //found, set weight_basis_points
                self.set_staking_pool_weight(sp_inx as u16, weight_basis_points);
                return;
            }
        }
        //not found, it's a new pool
        self.staking_pools.push(  StakingPoolInfo::new(account_id, weight_basis_points) );
        self.check_staking_pool_list_consistency();
    }

    fn check_staking_pool_list_consistency(&self) {
        assert!(self.sum_staking_pool_list_weight_basis_points()<=10000,"sum(staking_pools.weight) can not be GT 100%");
    }

    pub fn sum_staking_pool_list_weight_basis_points(&self) -> u16 {
        let mut total_weight: u16 = 0;
        for sp in self.staking_pools.iter() {
            total_weight+=sp.weight_basis_points;
        }
        return total_weight;
    }

    //--------------------------------------------------
    /// computes unstaking delay on current situation
    pub fn compute_current_unstaking_delay(&self, amount:U128String) -> u16 {
        return self.internal_compute_current_unstaking_delay(amount.0) as u16;
    }


    //---------------------------------
    // owner & operator accounts
    //---------------------------------

    pub fn get_operator_account_id(&self) -> AccountId {
        return self.operator_account_id.clone();
    }
    pub fn set_operator_account_id(&self, account_id:AccountId) {
        assert_owner_calling();
        assert(is_valid_account_id(account_id));
        self.operator_account_id = account_id;
    }

    /// The amount of tokens that were deposited to the staking pool.
    /// NOTE: The actual balance can be larger than this known deposit balance due to staking
    /// rewards acquired on the staking pool.
    /// To refresh the amount the owner can call `refresh_staking_pool_balance`.
    pub fn get_known_deposited_balance(&self) -> U128String {
        return self.total_actually_staked.into();
    }

    /// full account info
    /// Returns JSON representation of the account for the given account ID.
    pub fn get_account_info(&self, account_id: AccountId) -> GetAccountInfoResult {
        let acc = self.internal_get_account(&account_id);
        let stnear = self.amount_from_stake_shares(acc.stake_shares);
        // trip_rewards = current_stnear + trip_accum_unstakes - trip_accum_stakes - trip_start_stnear;
        let trip_rewards = (stnear + acc.trip_accum_unstakes).saturating_sub(acc.trip_accum_stakes + acc.trip_start_stnear);
        //NLSP share value
        let mut nslp_share_value: u128 = 0;
        if acc.nslp_shares != 0 {
            let nslp_account = self.internal_get_nslp_account();
            nslp_share_value = acc.valued_nslp_shares(self, &nslp_account);
        }
        return GetAccountInfoResult {
            account_id,
            available: acc.available.into(),
            stnear: stnear.into(),
            unstaked: acc.unstaked.into(),
            unstaked_requested_unlock_epoch: acc.unstaked_requested_unlock_epoch.into(),
            can_withdraw: (env::epoch_height() >= acc.unstaked_requested_unlock_epoch),
            total: (acc.available + stnear + acc.unstaked).into(),
            //trip-meter
            trip_start: acc.trip_start.into(),
            trip_start_stnear: acc.trip_start_stnear.into(),
            trip_accum_stakes: acc.trip_accum_stakes.into(),
            trip_accum_unstakes: acc.trip_accum_unstakes.into(),
            trip_rewards: trip_rewards.into(),

            nslp_shares: acc.nslp_shares.into(),
            nslp_share_value: nslp_share_value.into(),

            meta: acc.total_meta(self).into(),
        };
    }


    /// NEP-129 get information about this contract
    /// returns JSON string according to [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
    pub fn get_contract_info(&self) -> NEP129Response {
        return NEP129Response {
            dataVersion:1,
            name: CONTRACT_NAME.into(),
            version:CONTRACT_VERSION.into(),
            source:"https://github.com/Narwallets/meta-pool".into(), 
            standards:vec!("NEP-138".into(),"NEP-141".into()),  
            webAppUrl:self.web_app_url.clone(),
            developersAccountId:DEVELOPERS_ACCOUNT_ID.into(),
            auditorAccountId: self.auditor_account_id.clone()
        }
    }

    /// sets confirgurable contract info [NEP-129](https://github.com/nearprotocol/NEPs/pull/129)
    // Note: params are not Option<String> so the user can not inadvertely set null to data by not including the argument
    pub fn set_contract_info(&mut self, web_app_url:String, auditor_account_id:String) {
        self.assert_owner_calling();
        self.web_app_url = if web_app_url.len()>0 { Some(web_app_url) } else { None };
        self.auditor_account_id = if auditor_account_id.len()>0 { Some(auditor_account_id) } else { None };
    }

    /// get contract totals 
    /// Returns JSON representation of the contract state
    pub fn get_contract_state(&self) -> GetContractStateResult {

        let lp_account = self.internal_get_nslp_account();

        return GetContractStateResult {
            total_available: self.total_available.into(),
            total_for_staking: self.total_for_staking.into(),
            total_actually_staked: self.total_actually_staked.into(),
            accumulated_staked_rewards: self.accumulated_staked_rewards.into(),
            total_unstaked_and_waiting: self.total_unstaked_and_waiting.into(),
            total_actually_unstaked_and_retrieved: self.total_actually_unstaked_and_retrieved.into(),
            total_stake_shares: self.total_stake_shares.into(),
            total_meta: self.total_meta.into(),
            accounts_count: self.accounts.len().into(),
            staking_pools_count: self.staking_pools.len() as u16,
            nslp_liquidity: lp_account.available.into(),
            nslp_current_discount_basis_points: self.internal_get_discount_basis_points(lp_account.available, TEN_NEAR)
        };
    }

    /// Returns JSON representation of contract parameters
    pub fn get_contract_params(&self) -> ContractParamsJSON {
        return ContractParamsJSON {
            staking_paused: self.staking_paused,
            min_account_balance: self.min_account_balance.into(),

            nslp_near_one_percent_target: self.nslp_near_one_percent_target.into(),
            nslp_max_discount_basis_points: self.nslp_max_discount_basis_points,
            nslp_min_discount_basis_points: self.nslp_min_discount_basis_points,

            staker_meta_mult_pct: self.staker_meta_mult_pct,
            stnear_sell_meta_mult_pct: self.stnear_sell_meta_mult_pct,
            lp_provider_meta_mult_pct: self.lp_provider_meta_mult_pct,
                    
            operator_rewards_fee_basis_points: self.operator_rewards_fee_basis_points,
            operator_swap_cut_basis_points: self.operator_swap_cut_basis_points,
            treasury_swap_cut_basis_points: self.treasury_swap_cut_basis_points,
            };
    }

    /// Sets contract parameters 
    pub fn set_contract_params(&mut self, params:ContractParamsJSON) {

        self.assert_owner_calling();

        self.min_account_balance = params.min_account_balance.0;

        self.nslp_near_one_percent_target = params.nslp_near_one_percent_target.0;
        self.nslp_max_discount_basis_points = params.nslp_max_discount_basis_points;
        self.nslp_min_discount_basis_points = params.nslp_min_discount_basis_points;

        self.staker_meta_mult_pct = params.staker_meta_mult_pct;
        self.stnear_sell_meta_mult_pct = params.stnear_sell_meta_mult_pct;
        self.lp_provider_meta_mult_pct = params.lp_provider_meta_mult_pct;
                    
        self.operator_rewards_fee_basis_points = params.operator_rewards_fee_basis_points;
        self.operator_swap_cut_basis_points = params.operator_swap_cut_basis_points;
        self.treasury_swap_cut_basis_points = params.treasury_swap_cut_basis_points;

    }
    
    /// get sp (staking-pool) info
    /// Returns JSON representation of sp recorded state
    pub fn get_sp_info(&self, sp_inx_i32: i32) -> StakingPoolJSONInfo {

        assert!(sp_inx_i32 > 0);

        let sp_inx = sp_inx_i32 as usize;
        assert!(sp_inx < self.staking_pools.len());

        let sp = &self.staking_pools[sp_inx];

        return StakingPoolJSONInfo {
            account_id: sp.account_id.clone(),
            weight_basis_points: sp.weight_basis_points,
            staked: sp.staked.into(),
            unstaked: sp.unstaked.into(),
            unstaked_requested_epoch_height: sp.unstk_req_epoch_height.into(),
            last_asked_rewards_epoch_height: sp.last_asked_rewards_epoch_height.into(),
        };
    }

}
