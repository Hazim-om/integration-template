//! Omnipair on-chain account layouts and spot swap quote math.
//!

use anchor_lang::AnchorDeserialize;
use solana_pubkey::Pubkey;

use crate::trading_venue::error::TradingVenueError;

pub const BPS_DENOMINATOR: u128 = 10_000;

pub(crate) const RESERVE_VAULT_SEED_PREFIX: &[u8] = b"reserve_vault";
pub(crate) const FUTARCHY_AUTHORITY_SEED_PREFIX: &[u8] = b"futarchy_authority";

pub const OMNIPAIR_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("omnixgS8fnqHfCcTGKWj6JtKjzpJZ1Y5y9pyFkQDkYE");
pub const SPL_TOKEN_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
pub const TOKEN_2022_PROGRAM_ID: Pubkey =
    Pubkey::from_str_const("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");

/// Anchor `swap` instruction discriminator (`global:swap`).
pub const SWAP_INSTRUCTION_DISCRIMINATOR: [u8; 8] =
    [0xf8, 0xc6, 0x9e, 0x91, 0xe1, 0x75, 0x87, 0xc8];

pub fn ceil_div(numerator: u128, denominator: u128) -> Option<u128> {
    if denominator == 0 {
        return None;
    }
    Some((numerator + denominator - 1) / denominator)
}

#[derive(AnchorDeserialize, Debug, Clone, Copy, Default)]
pub struct VaultBumps {
    pub reserve0: u8,
    pub reserve1: u8,
    pub collateral0: u8,
    pub collateral1: u8,
}

#[derive(AnchorDeserialize, Debug, Clone, Copy, Default)]
pub struct LastPriceEMA {
    pub symmetric: u64,
    pub directional: u64,
}

#[derive(AnchorDeserialize, Debug, Clone)]
pub struct OmnipairPair {
    pub token0: Pubkey,
    pub token1: Pubkey,
    pub lp_mint: Pubkey,
    pub rate_model: Pubkey,
    pub swap_fee_bps: u16,
    pub half_life: u64,
    pub fixed_cf_bps: Option<u16>,
    pub reserve0: u64,
    pub reserve1: u64,
    pub cash_reserve0: u64,
    pub cash_reserve1: u64,
    pub last_price0_ema: LastPriceEMA,
    pub last_price1_ema: LastPriceEMA,
    pub last_update: u64,
    pub last_rate0: u64,
    pub last_rate1: u64,
    pub total_debt0: u64,
    pub total_debt1: u64,
    pub total_debt0_shares: u128,
    pub total_debt1_shares: u128,
    pub total_supply: u64,
    pub total_collateral0: u64,
    pub total_collateral1: u64,
    pub token0_decimals: u8,
    pub token1_decimals: u8,
    pub params_hash: [u8; 32],
    pub version: u8,
    pub bump: u8,
    pub vault_bumps: VaultBumps,
    pub reduce_only: bool,
}

#[derive(AnchorDeserialize, Debug, Clone)]
pub struct OmnipairRateModel {
    pub exp_rate: u64,
    pub target_util_start: u64,
    pub target_util_end: u64,
    pub half_life_ms: u64,
    pub min_rate: u64,
    pub max_rate: u64,
    pub initial_rate: u64,
}

#[derive(AnchorDeserialize, Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct RevenueShare {
    pub swap_bps: u16,
    pub interest_bps: u16,
}

#[derive(AnchorDeserialize, Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct RevenueRecipients {
    pub futarchy_treasury: Pubkey,
    pub buybacks_vault: Pubkey,
    pub team_treasury: Pubkey,
}

#[derive(AnchorDeserialize, Debug, Clone, Copy, Default)]
#[allow(dead_code)]
pub struct RevenueDistribution {
    pub futarchy_treasury_bps: u16,
    pub buybacks_vault_bps: u16,
    pub team_treasury_bps: u16,
}

#[derive(AnchorDeserialize, Debug, Clone)]
#[allow(dead_code)]
pub struct OmnipairFutarchyAuthority {
    pub version: u8,
    pub authority: Pubkey,
    pub recipients: RevenueRecipients,
    pub revenue_share: RevenueShare,
    pub revenue_distribution: RevenueDistribution,
    pub global_reduce_only: bool,
    pub bump: u8,
}

pub struct SwapQuote {
    pub amount_out: u64,
    pub fee_amount: u64,
}

impl OmnipairPair {
    /// Constant-product swap: Δy = (Δx * y) / (x + Δx)
    pub fn calculate_amount_out(
        reserve_in: u64,
        reserve_out: u64,
        amount_in: u64,
    ) -> Result<u64, TradingVenueError> {
        let denominator = (reserve_in as u128)
            .checked_add(amount_in as u128)
            .ok_or_else(|| TradingVenueError::MathError("reserve denominator overflow".into()))?;
        let amount_out = (amount_in as u128)
            .checked_mul(reserve_out as u128)
            .ok_or_else(|| TradingVenueError::MathError("amount out mul overflow".into()))?
            .checked_div(denominator)
            .ok_or_else(|| TradingVenueError::MathError("amount out div".into()))?;
        u64::try_from(amount_out).map_err(|_| TradingVenueError::MathError("amount out u64".into()))
    }

    /// Spot swap quote: fee on input, then constant product; enforces cash reserve on output.
    pub fn swap_quote(
        &self,
        amount_in: u64,
        input_mint: Pubkey,
    ) -> Result<SwapQuote, TradingVenueError> {
        let is_token0_in = input_mint == self.token0;
        if !is_token0_in && input_mint != self.token1 {
            return Err(TradingVenueError::InvalidMint(input_mint.into()));
        }

        let (reserve_in, reserve_out, cash_reserve_out) = if is_token0_in {
            (self.reserve0, self.reserve1, self.cash_reserve1)
        } else {
            (self.reserve1, self.reserve0, self.cash_reserve0)
        };

        if reserve_in == 0 || reserve_out == 0 {
            return Err(TradingVenueError::AmmMethodError("invalid reserves".into()));
        }

        let fee_amount = ceil_div(
            (amount_in as u128)
                .checked_mul(self.swap_fee_bps as u128)
                .ok_or_else(|| TradingVenueError::MathError("fee mul".into()))?,
            BPS_DENOMINATOR,
        )
        .ok_or_else(|| TradingVenueError::MathError("fee div".into()))? as u64;

        let amount_in_after_fee = amount_in.checked_sub(fee_amount).ok_or_else(|| {
            TradingVenueError::MathError("amount after fee underflow".into())
        })?;

        let amount_out =
            Self::calculate_amount_out(reserve_in, reserve_out, amount_in_after_fee)?;

        if amount_out > cash_reserve_out {
            return Err(TradingVenueError::AmmMethodError(
                "insufficient cash reserve for output".into(),
            ));
        }

        Ok(SwapQuote {
            amount_out,
            fee_amount,
        })
    }
}

#[derive(Debug, Clone)]
pub struct DerivedAccounts {
    pub reserve_vault0: Pubkey,
    pub reserve_vault1: Pubkey,
    pub futarchy_authority: Pubkey,
    pub event_authority: Pubkey,
}

impl DerivedAccounts {
    pub fn compute(pair_key: &Pubkey, state: &OmnipairPair) -> Self {
        let (reserve_vault0, _) = Pubkey::find_program_address(
            &[
                RESERVE_VAULT_SEED_PREFIX,
                pair_key.as_ref(),
                state.token0.as_ref(),
            ],
            &OMNIPAIR_PROGRAM_ID,
        );
        let (reserve_vault1, _) = Pubkey::find_program_address(
            &[
                RESERVE_VAULT_SEED_PREFIX,
                pair_key.as_ref(),
                state.token1.as_ref(),
            ],
            &OMNIPAIR_PROGRAM_ID,
        );
        let (futarchy_authority, _) = Pubkey::find_program_address(
            &[FUTARCHY_AUTHORITY_SEED_PREFIX],
            &OMNIPAIR_PROGRAM_ID,
        );
        let (event_authority, _) =
            Pubkey::find_program_address(&[b"__event_authority"], &OMNIPAIR_PROGRAM_ID);
        Self {
            reserve_vault0,
            reserve_vault1,
            futarchy_authority,
            event_authority,
        }
    }
}

pub fn deserialize_pair(data: &[u8]) -> Result<OmnipairPair, TradingVenueError> {
    if data.len() < 8 {
        return Err(TradingVenueError::DeserializationFailed(
            "omnipair pair data too short".into(),
        ));
    }
    OmnipairPair::deserialize(&mut &data[8..]).map_err(|_| {
        TradingVenueError::DeserializationFailed("omnipair pair anchor decode".into())
    })
}

pub fn deserialize_rate_model(data: &[u8]) -> Result<OmnipairRateModel, TradingVenueError> {
    if data.len() < 8 {
        return Err(TradingVenueError::DeserializationFailed(
            "omnipair rate model data too short".into(),
        ));
    }
    OmnipairRateModel::deserialize(&mut &data[8..]).map_err(|_| {
        TradingVenueError::DeserializationFailed("omnipair rate model anchor decode".into())
    })
}

pub fn deserialize_futarchy_authority(
    data: &[u8],
) -> Result<OmnipairFutarchyAuthority, TradingVenueError> {
    if data.len() < 8 {
        return Err(TradingVenueError::DeserializationFailed(
            "omnipair futarchy authority data too short".into(),
        ));
    }
    OmnipairFutarchyAuthority::deserialize(&mut &data[8..]).map_err(|_| {
        TradingVenueError::DeserializationFailed("omnipair futarchy anchor decode".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_product_matches_sdk() {
        let out = OmnipairPair::calculate_amount_out(1_000_000, 1_000_000, 100_000).unwrap();
        assert_eq!(out, 90_909);
    }

    #[test]
    fn swap_quote_with_fee_matches_sdk() {
        let token0 = Pubkey::new_unique();
        let token1 = Pubkey::new_unique();
        let pair = OmnipairPair {
            token0,
            token1,
            lp_mint: Pubkey::default(),
            rate_model: Pubkey::default(),
            swap_fee_bps: 30,
            half_life: 0,
            fixed_cf_bps: None,
            reserve0: 1_000_000_000,
            reserve1: 1_000_000_000,
            cash_reserve0: 1_000_000_000,
            cash_reserve1: 1_000_000_000,
            last_price0_ema: LastPriceEMA::default(),
            last_price1_ema: LastPriceEMA::default(),
            last_update: 0,
            last_rate0: 0,
            last_rate1: 0,
            total_debt0: 0,
            total_debt1: 0,
            total_debt0_shares: 0,
            total_debt1_shares: 0,
            total_supply: 0,
            total_collateral0: 0,
            total_collateral1: 0,
            token0_decimals: 9,
            token1_decimals: 9,
            params_hash: [0; 32],
            version: 1,
            bump: 0,
            vault_bumps: VaultBumps::default(),
            reduce_only: false,
        };

        let result = pair.swap_quote(1_000_000, token0).unwrap();
        assert_eq!(result.fee_amount, 3000);
        assert_eq!(result.amount_out, 996_006);
    }
}
