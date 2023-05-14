use crate::*;

pub use crate::types::*;
pub use crate::utils::*;

// -----------------
// Reward meter utility
// -----------------
#[derive(BorshDeserialize, BorshSerialize, Debug, PartialEq)]
pub struct RewardMeter {
    ///added with staking
    ///subtracted on unstaking. WARN: Since unstaking can include rewards, delta_staked *CAN BECOME NEGATIVE*
    pub delta_staked: i128, //i128 changing this requires accounts migration
    pub last_multiplier_pct: u16, // (pct: 100 => x1, 200 => x2)
}

impl Default for RewardMeter {
    fn default() -> Self {
        Self {
            delta_staked: 0,
            last_multiplier_pct: 100,
        }
    }
}

impl RewardMeter {
    ///register a stake (to be able to compute rewards later)
    pub fn stake(&mut self, value: u128) {
        assert!(value <= std::i128::MAX as u128);
        self.delta_staked += value as i128;
    }
    ///register a unstake (to be able to compute rewards later)
    pub fn unstake(&mut self, value: u128) {
        assert!(value <= std::i128::MAX as u128);
        self.delta_staked -= value as i128;
    }

    #[inline]
    pub fn reset(&mut self, valued_shares: u128) {
        assert!(valued_shares <= std::i128::MAX as u128);
        self.delta_staked = valued_shares as i128; // reset meter to Zero difference
    }
}
