use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds};
use near_sdk::json_types::{ValidAccountId, U128};
use near_sdk::{env, near_bindgen};

use crate::*;

// --------------------------------------------------------------------------
// Storage Management
// storage is 384 bytes per account, at price = 1 NEAR/100Kib => 0.00384 NEAR per account 
// --------------------------------------------------------------------------
const STORAGE_COST_YOCTOS: u128 = ONE_NEAR / 100_000 * 384;
// storage is fixed, if the account is registered, STORAGE_COST_YOCTOS was received, 
// when the account is unregistered, STORAGE_COST_YOCTOS are returned 

#[near_bindgen]
impl MetaPool {
    // `registration_only` doesn't affect the implementation for vanilla fungible token.
    #[allow(unused_variables)]
    #[payable]
    pub fn storage_deposit(
        &mut self,
        account_id: Option<ValidAccountId>,
        registration_only: Option<bool>,
    ) -> StorageBalance {
        // get account_id
        let account_id:String = if account_id.is_some() {account_id.unwrap().into()} else {env::predecessor_account_id()};
        // if already exists, no more yoctos required
        let required = if self.account_exists(&account_id) {0} else {STORAGE_COST_YOCTOS};
        assert!(env::attached_deposit() >= required, "not enough attached for storage");
        // if user sent more than required, return it, keep only required
        if env::attached_deposit() > required {
            Promise::new(env::predecessor_account_id()).transfer(env::attached_deposit() - required);
        }
        // return current balance state
        StorageBalance {
            total: U128::from(STORAGE_COST_YOCTOS),
            available: U128::from(0),
        }
    }

    /// storage cost is fixed, excess amount is always 0, no storage_withdraw possible 
    #[allow(unused_variables)]
    pub fn storage_withdraw(&mut self, amount: Option<U128>) -> StorageBalance {
        panic!("storage excess amount is 0");
    }

    #[allow(unused_variables)]
    #[payable]
    pub fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        assert_one_yocto();
        if let Some(account) = self.accounts.get(&env::predecessor_account_id()) {
            // account exists
            if !account.can_be_closed() {
                panic!("cannot close account with balance in stNEAR or LP-NEAR-stNEAR");
            }
            // remove account, make sure something is removed
            assert!(
                self.accounts.remove(&env::predecessor_account_id()).is_some()
                ,"INCONSISTENCY - account does not exists now"
            );
            // return storage yoctos
            Promise::new(env::predecessor_account_id()).transfer(STORAGE_COST_YOCTOS);
        };
        true
    }

    // max & min total storage balance
    pub fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        StorageBalanceBounds {
            min: U128::from(STORAGE_COST_YOCTOS),
            max: Some(U128::from(STORAGE_COST_YOCTOS))
        }
    }

    pub fn storage_balance_of(&self, account_id: ValidAccountId) -> Option<StorageBalance> {
        if self.account_exists(&account_id.into()) {
            // if account exists
            Some(StorageBalance {
                total: U128::from(STORAGE_COST_YOCTOS),
                available: U128::from(0),
            })
        }
        else { 
            None
        }
    }
}
