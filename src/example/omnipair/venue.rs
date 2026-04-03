//! `TradingVenue` for Omnipair pairs (spot swap).

use ahash::HashSet;
use async_trait::async_trait;
use solana_account::{Account, ReadableAccount};
use solana_instruction::{AccountMeta, Instruction};
use solana_pubkey::Pubkey;
use solana_sysvar::clock::Clock;

use crate::{
    account_caching::AccountsCache,
    example::omnipair::state::{
        deserialize_futarchy_authority, deserialize_pair, deserialize_rate_model, DerivedAccounts,
        OmnipairFutarchyAuthority, OmnipairPair, OmnipairRateModel, OMNIPAIR_PROGRAM_ID,
        SPL_TOKEN_PROGRAM_ID, SWAP_INSTRUCTION_DISCRIMINATOR, TOKEN_2022_PROGRAM_ID,
    },
    trading_venue::{
        AddressLookupTableTrait, FromAccount, QuoteRequest, QuoteResult, SwapType, TradingVenue,
        error::TradingVenueError,
        protocol::PoolProtocol,
        token_info::TokenInfo,
    },
};

/// Omnipair pool venue — `market_id` is the pair account address.
#[derive(Clone)]
pub struct OmnipairVenue {
    pub pair_key: Pubkey,
    pub state: OmnipairPair,
    pub derived: DerivedAccounts,
    pub rate_model_data: Option<OmnipairRateModel>,
    pub interest_bps: u16,
    pub current_slot: u64,
    required_state_pubkeys: HashSet<Pubkey>,
    found_all_pubkeys: bool,
    token_info: Vec<TokenInfo>,
}

impl FromAccount for OmnipairVenue {
    fn from_account(pubkey: &Pubkey, account: &Account) -> Result<Self, TradingVenueError> {
        if account.owner != OMNIPAIR_PROGRAM_ID {
            return Err(TradingVenueError::FromAccountError(
                "account owner is not Omnipair program".into(),
            ));
        }
        let state = deserialize_pair(account.data())?;
        let derived = DerivedAccounts::compute(pubkey, &state);
        let required_state_pubkeys = HashSet::from_iter([
            *pubkey,
            state.rate_model,
            derived.futarchy_authority,
            state.token0,
            state.token1,
            solana_sdk_ids::sysvar::clock::ID,
        ]);
        Ok(Self {
            pair_key: *pubkey,
            state,
            derived,
            rate_model_data: None,
            interest_bps: 0,
            current_slot: 0,
            required_state_pubkeys,
            found_all_pubkeys: false,
            token_info: Vec::new(),
        })
    }
}

#[async_trait]
impl TradingVenue for OmnipairVenue {
    fn initialized(&self) -> bool {
        self.found_all_pubkeys
    }

    fn market_id(&self) -> Pubkey {
        self.pair_key
    }

    fn program_id(&self) -> Pubkey {
        OMNIPAIR_PROGRAM_ID
    }

    fn program_dependencies(&self) -> Vec<Pubkey> {
        vec![
            OMNIPAIR_PROGRAM_ID,
            SPL_TOKEN_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
        ]
    }

    fn protocol(&self) -> PoolProtocol {
        PoolProtocol::Omnipair
    }

    fn tradable_mints(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        Ok(vec![self.state.token0, self.state.token1])
    }

    fn decimals(&self) -> Result<Vec<i32>, TradingVenueError> {
        Ok(vec![
            self.token_info
                .first()
                .ok_or_else(|| TradingVenueError::MissingState(self.state.token0.into()))?
                .decimals,
            self.token_info
                .get(1)
                .ok_or_else(|| TradingVenueError::MissingState(self.state.token1.into()))?
                .decimals,
        ])
    }

    fn get_token_info(&self) -> &[TokenInfo] {
        &self.token_info
    }

    async fn update_state(&mut self, cache: &dyn AccountsCache) -> Result<(), TradingVenueError> {
        let accounts_pubkeys = vec![
            self.pair_key,
            self.state.rate_model,
            self.derived.futarchy_authority,
            self.state.token0,
            self.state.token1,
            solana_sdk_ids::sysvar::clock::ID,
        ];

        self.required_state_pubkeys.extend(&accounts_pubkeys);

        let accounts = cache.get_accounts(&accounts_pubkeys).await?;

        let [pair_a, rate_a, futarchy_a, mint0_a, mint1_a, clock_a]: [Option<Account>; 6] = accounts
            .try_into()
            .map_err(|_| TradingVenueError::FailedToFetchMultipleAccountData)?;

        let pair_account = pair_a
            .as_ref()
            .ok_or_else(|| TradingVenueError::MissingState(self.pair_key.into()))?;
        self.state = deserialize_pair(pair_account.data())?;
        self.derived = DerivedAccounts::compute(&self.pair_key, &self.state);

        if let Some(ref acc) = rate_a {
            self.rate_model_data = Some(deserialize_rate_model(acc.data())?);
        } else {
            self.rate_model_data = None;
        }

        if let Some(ref acc) = futarchy_a {
            let fa: OmnipairFutarchyAuthority = deserialize_futarchy_authority(acc.data())?;
            self.interest_bps = fa.revenue_share.interest_bps;
        } else {
            self.interest_bps = 0;
        }

        if let Some(ref clock_acc) = clock_a {
            let clock: Clock = clock_acc
                .deserialize_data()
                .map_err(|_| TradingVenueError::DeserializationFailed("clock sysvar".into()))?;
            self.current_slot = clock.slot;
        }

        if let [Some(m0), Some(m1)] = [mint0_a, mint1_a] {
            self.token_info = vec![
                TokenInfo::new(&self.state.token0, &m0, u64::MAX)?,
                TokenInfo::new(&self.state.token1, &m1, u64::MAX)?,
            ];
        }

        self.found_all_pubkeys = true;
        Ok(())
    }

    fn quote(&self, request: QuoteRequest) -> Result<QuoteResult, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }

        if request.amount == 0 {
            return Ok(QuoteResult {
                input_mint: request.input_mint,
                output_mint: request.output_mint,
                amount: 0,
                expected_output: 0,
                not_enough_liquidity: false,
            });
        }

        if self.state.reduce_only {
            return Err(TradingVenueError::InactivePoolError(
                self.pair_key,
                PoolProtocol::Omnipair,
            ));
        }

        let mut projected = self.state.clone();
        if let Some(ref rate_model) = self.rate_model_data {
            projected.simulate_update(self.current_slot, rate_model, self.interest_bps);
        }

        let result = projected.swap_quote(request.amount, request.input_mint)?;

        Ok(QuoteResult {
            input_mint: request.input_mint,
            output_mint: request.output_mint,
            amount: request.amount,
            expected_output: result.amount_out,
            not_enough_liquidity: false,
        })
    }

    fn generate_swap_instruction(
        &self,
        request: QuoteRequest,
        user: Pubkey,
    ) -> Result<Instruction, TradingVenueError> {
        if request.swap_type == SwapType::ExactOut {
            return Err(TradingVenueError::ExactOutNotSupported);
        }

        let t0 = self
            .token_info
            .first()
            .ok_or_else(|| TradingVenueError::NotInitialized("token0 mint info".into()))?;
        let t1 = self
            .token_info
            .get(1)
            .ok_or_else(|| TradingVenueError::NotInitialized("token1 mint info".into()))?;

        let is_token0_in = request.input_mint == self.state.token0;
        if !is_token0_in && request.input_mint != self.state.token1 {
            return Err(TradingVenueError::InvalidMint(request.input_mint.into()));
        }
        if request.output_mint
            != (if is_token0_in {
                self.state.token1
            } else {
                self.state.token0
            })
        {
            return Err(TradingVenueError::InvalidMint(request.output_mint.into()));
        }

        let (token_in_mint, token_out_mint, token_in_vault, token_out_vault, user_in, user_out) =
            if is_token0_in {
                (
                    self.state.token0,
                    self.state.token1,
                    self.derived.reserve_vault0,
                    self.derived.reserve_vault1,
                    t0.get_associated_token_address(&user),
                    t1.get_associated_token_address(&user),
                )
            } else {
                (
                    self.state.token1,
                    self.state.token0,
                    self.derived.reserve_vault1,
                    self.derived.reserve_vault0,
                    t1.get_associated_token_address(&user),
                    t0.get_associated_token_address(&user),
                )
            };

        let mut data = Vec::with_capacity(8 + 16);
        data.extend_from_slice(&SWAP_INSTRUCTION_DISCRIMINATOR);
        data.extend_from_slice(&request.amount.to_le_bytes());
        data.extend_from_slice(&0u64.to_le_bytes());

        let accounts = vec![
            AccountMeta::new(self.pair_key, false),
            AccountMeta::new(self.state.rate_model, false),
            AccountMeta::new_readonly(self.derived.futarchy_authority, false),
            AccountMeta::new(token_in_vault, false),
            AccountMeta::new(token_out_vault, false),
            AccountMeta::new(user_in, false),
            AccountMeta::new(user_out, false),
            AccountMeta::new_readonly(token_in_mint, false),
            AccountMeta::new_readonly(token_out_mint, false),
            AccountMeta::new(user, true),
            AccountMeta::new_readonly(SPL_TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(self.derived.event_authority, false),
            AccountMeta::new_readonly(OMNIPAIR_PROGRAM_ID, false),
        ];

        Ok(Instruction {
            program_id: OMNIPAIR_PROGRAM_ID,
            accounts,
            data,
        })
    }

    fn get_required_pubkeys_for_update(&self) -> Result<Vec<Pubkey>, TradingVenueError> {
        if !self.found_all_pubkeys {
            return Err(TradingVenueError::NotInitialized(
                "State needs to be fully updated".into(),
            ));
        }
        Ok(self.required_state_pubkeys.iter().copied().collect())
    }
}

#[async_trait]
impl AddressLookupTableTrait for OmnipairVenue {
    async fn get_lookup_table_keys(
        &self,
        accounts_cache: Option<&dyn AccountsCache>,
    ) -> Result<Vec<Pubkey>, TradingVenueError> {
        let rpc_cache = accounts_cache
            .ok_or_else(|| TradingVenueError::SomethingWentWrong("RPC cache required".into()))?;

        let token_mint_accounts = rpc_cache
            .get_accounts(&[self.state.token0, self.state.token1])
            .await?;

        let mint0 = token_mint_accounts[0]
            .as_ref()
            .ok_or(TradingVenueError::MissingState("token0 mint".into()))?;
        let mint1 = token_mint_accounts[1]
            .as_ref()
            .ok_or(TradingVenueError::MissingState("token1 mint".into()))?;

        let p0 = mint0.owner;
        let p1 = mint1.owner;

        Ok(vec![
            self.state.token0,
            self.state.token1,
            p0,
            p1,
            self.pair_key,
            self.derived.reserve_vault0,
            self.derived.reserve_vault1,
            self.state.rate_model,
            self.derived.futarchy_authority,
            self.derived.event_authority,
            SPL_TOKEN_PROGRAM_ID,
            TOKEN_2022_PROGRAM_ID,
            OMNIPAIR_PROGRAM_ID,
        ])
    }
}
