use crate::error::ErrorCode;
use crate::libraries::liquidity_math;
use crate::libraries::tick_math;
use crate::states::*;
use crate::util::*;
use anchor_lang::prelude::*;
use anchor_lang::solana_program;
use anchor_lang::system_program::{transfer, Transfer};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::metadata::create_metadata_accounts_v3;
use anchor_spl::metadata::mpl_token_metadata::types::Creator;
use anchor_spl::metadata::Metadata;
use anchor_spl::token::{Mint, Token, TokenAccount};
use anchor_spl::token_2022::spl_token_2022::extension::{
    BaseStateWithExtensions, StateWithExtensions,
};
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_2022::{
    self,
    spl_token_2022::{self, instruction::AuthorityType},
};
use anchor_spl::token_interface;
use std::cell::RefMut;
#[cfg(feature = "enable-log")]
use std::convert::identity;
use std::ops::Deref;
use std::ops::DerefMut;

#[derive(Accounts)]
#[instruction(tick_lower_index: i32, tick_upper_index: i32,tick_array_lower_start_index:i32,tick_array_upper_start_index:i32)]
pub struct OpenPosition<'info> {
    /// Pays to mint the position
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: Receives the position NFT
    pub position_nft_owner: UncheckedAccount<'info>,

    /// Unique token mint address
    #[account(
        init,
        mint::decimals = 0,
        mint::authority = pool_state.key(),
        payer = payer,
    )]
    pub position_nft_mint: Box<Account<'info, Mint>>,

    /// Token account where position NFT will be minted
    /// This account created in the contract by cpi to avoid large stack variables
    #[account(
        init,
        associated_token::mint = position_nft_mint,
        associated_token::authority = position_nft_owner,
        payer = payer,
    )]
    pub position_nft_account: Box<Account<'info, TokenAccount>>,

    /// To store metaplex metadata
    /// CHECK: Safety check performed inside function body
    #[account(mut)]
    pub metadata_account: UncheckedAccount<'info>,

    /// Add liquidity for this pool
    #[account(mut)]
    pub pool_state: AccountLoader<'info, PoolState>,

    /// Store the information of market marking in range
    #[account(
        init_if_needed,
        seeds = [
            POSITION_SEED.as_bytes(),
            pool_state.key().as_ref(),
            &tick_lower_index.to_be_bytes(),
            &tick_upper_index.to_be_bytes(),
        ],
        bump,
        payer = payer,
        space = ProtocolPositionState::LEN
    )]
    pub protocol_position: Box<Account<'info, ProtocolPositionState>>,

    /// CHECK: Account to store data for the position's lower tick
    #[account(
        mut,
        seeds = [
            TICK_ARRAY_SEED.as_bytes(),
            pool_state.key().as_ref(),
            &tick_array_lower_start_index.to_be_bytes(),
        ],
        bump,
    )]
    pub tick_array_lower: UncheckedAccount<'info>,

    /// CHECK: Account to store data for the position's upper tick
    #[account(
        mut,
        seeds = [
            TICK_ARRAY_SEED.as_bytes(),
            pool_state.key().as_ref(),
            &tick_array_upper_start_index.to_be_bytes(),
        ],
        bump,
    )]
    pub tick_array_upper: UncheckedAccount<'info>,

    /// personal position state
    #[account(
        init,
        seeds = [POSITION_SEED.as_bytes(), position_nft_mint.key().as_ref()],
        bump,
        payer = payer,
        space = PersonalPositionState::LEN
    )]
    pub personal_position: Box<Account<'info, PersonalPositionState>>,

    /// The token_0 account deposit token to the pool
    #[account(
        mut,
        token::mint = token_vault_0.mint
    )]
    pub token_account_0: Box<Account<'info, TokenAccount>>,

    /// The token_1 account deposit token to the pool
    #[account(
        mut,
        token::mint = token_vault_1.mint
    )]
    pub token_account_1: Box<Account<'info, TokenAccount>>,

    /// The address that holds pool tokens for token_0
    #[account(
        mut,
        constraint = token_vault_0.key() == pool_state.load()?.token_vault_0
    )]
    pub token_vault_0: Box<Account<'info, TokenAccount>>,

    /// The address that holds pool tokens for token_1
    #[account(
        mut,
        constraint = token_vault_1.key() == pool_state.load()?.token_vault_1
    )]
    pub token_vault_1: Box<Account<'info, TokenAccount>>,

    /// Sysvar for token mint and ATA creation
    pub rent: Sysvar<'info, Rent>,

    /// Program to create the position manager state account
    pub system_program: Program<'info, System>,

    /// Program to create mint account and mint tokens
    pub token_program: Program<'info, Token>,
    /// Program to create an ATA for receiving position NFT
    pub associated_token_program: Program<'info, AssociatedToken>,

    /// Program to create NFT metadata
    /// CHECK: Metadata program address constraint applied
    pub metadata_program: Program<'info, Metadata>,
    // remaining account
    // #[account(
    //     seeds = [
    //         POOL_TICK_ARRAY_BITMAP_SEED.as_bytes(),
    //         pool_state.key().as_ref(),
    //     ],
    //     bump
    // )]
    // pub tick_array_bitmap: AccountLoader<'info, TickArrayBitmapExtension>,
}

/// Add liquidity to an initialized pool
pub fn add_liquidity<'b, 'c: 'info, 'info>(
    payer: &'b Signer<'info>,
    token_account_0: &'b AccountInfo<'info>,
    token_account_1: &'b AccountInfo<'info>,
    token_vault_0: &'b AccountInfo<'info>,
    token_vault_1: &'b AccountInfo<'info>,
    tick_array_lower_loader: &'b AccountLoad<'info, TickArrayState>,
    tick_array_upper_loader: &'b AccountLoad<'info, TickArrayState>,
    protocol_position: &mut ProtocolPositionState,
    token_program_2022: Option<&Program<'info, Token2022>>,
    token_program: &'b Program<'info, Token>,
    vault_0_mint: Option<Box<InterfaceAccount<'info, token_interface::Mint>>>,
    vault_1_mint: Option<Box<InterfaceAccount<'info, token_interface::Mint>>>,
    tick_array_bitmap_extension: Option<&'c AccountInfo<'info>>,
    pool_state: &mut RefMut<PoolState>,
    liquidity: &mut u128,
    amount_0_max: u64,
    amount_1_max: u64,
    tick_lower_index: i32,
    tick_upper_index: i32,
    base_flag: Option<bool>,
) -> Result<(u64, u64, u64, u64)> {
    if *liquidity == 0 {
        if base_flag.is_none() {
            // when establishing a new position , liquidity allows for further additions
            return Ok((0, 0, 0, 0));
        }
        if base_flag.unwrap() {
            // must deduct transfer fee before calculate liquidity
            // because only v2 instruction support token_2022, vault_0_mint must be exist
            let amount_0_transfer_fee =
                get_transfer_fee(vault_0_mint.clone().unwrap(), amount_0_max).unwrap();
            *liquidity = liquidity_math::get_liquidity_from_single_amount_0(
                pool_state.sqrt_price_x64,
                tick_math::get_sqrt_price_at_tick(tick_lower_index)?,
                tick_math::get_sqrt_price_at_tick(tick_upper_index)?,
                amount_0_max.checked_sub(amount_0_transfer_fee).unwrap(),
            );
            #[cfg(feature = "enable-log")]
            msg!(
                "liquidity: {}, amount_0_max:{}, amount_0_transfer_fee:{}",
                *liquidity,
                amount_0_max,
                amount_0_transfer_fee
            );
        } else {
            // must deduct transfer fee before calculate liquidity
            // because only v2 instruction support token_2022, vault_1_mint must be exist
            let amount_1_transfer_fee =
                get_transfer_fee(vault_1_mint.clone().unwrap(), amount_1_max).unwrap();
            *liquidity = liquidity_math::get_liquidity_from_single_amount_1(
                pool_state.sqrt_price_x64,
                tick_math::get_sqrt_price_at_tick(tick_lower_index)?,
                tick_math::get_sqrt_price_at_tick(tick_upper_index)?,
                amount_1_max.checked_sub(amount_1_transfer_fee).unwrap(),
            );
            #[cfg(feature = "enable-log")]
            msg!(
                "liquidity: {}, amount_1_max:{}, amount_1_transfer_fee:{}",
                *liquidity,
                amount_1_max,
                amount_1_transfer_fee
            );
        }
    }
    assert!(*liquidity > 0);
    let liquidity_before = pool_state.liquidity;
    require_keys_eq!(tick_array_lower_loader.load()?.pool_id, pool_state.key());
    require_keys_eq!(tick_array_upper_loader.load()?.pool_id, pool_state.key());

    // get tick_state
    let mut tick_lower_state = *tick_array_lower_loader
        .load_mut()?
        .get_tick_state_mut(tick_lower_index, pool_state.tick_spacing)?;
    let mut tick_upper_state = *tick_array_upper_loader
        .load_mut()?
        .get_tick_state_mut(tick_upper_index, pool_state.tick_spacing)?;
    if tick_lower_state.tick == 0 {
        tick_lower_state.tick = tick_lower_index;
    }
    if tick_upper_state.tick == 0 {
        tick_upper_state.tick = tick_upper_index;
    }
    let clock = Clock::get()?;
    let (amount_0, amount_1, flip_tick_lower, flip_tick_upper) = modify_position(
        i128::try_from(*liquidity).unwrap(),
        pool_state,
        protocol_position,
        &mut tick_lower_state,
        &mut tick_upper_state,
        clock.unix_timestamp as u64,
    )?;

    // update tick_state
    tick_array_lower_loader.load_mut()?.update_tick_state(
        tick_lower_index,
        pool_state.tick_spacing,
        tick_lower_state,
    )?;
    tick_array_upper_loader.load_mut()?.update_tick_state(
        tick_upper_index,
        pool_state.tick_spacing,
        tick_upper_state,
    )?;

    if flip_tick_lower {
        let mut tick_array_lower = tick_array_lower_loader.load_mut()?;
        let before_init_tick_count = tick_array_lower.initialized_tick_count;
        tick_array_lower.update_initialized_tick_count(true)?;

        if before_init_tick_count == 0 {
            pool_state.flip_tick_array_bit(
                tick_array_bitmap_extension,
                tick_array_lower.start_tick_index,
            )?;
        }
    }
    if flip_tick_upper {
        let mut tick_array_upper = tick_array_upper_loader.load_mut()?;
        let before_init_tick_count = tick_array_upper.initialized_tick_count;
        tick_array_upper.update_initialized_tick_count(true)?;

        if before_init_tick_count == 0 {
            pool_state.flip_tick_array_bit(
                tick_array_bitmap_extension,
                tick_array_upper.start_tick_index,
            )?;
        }
    }
    require!(
        amount_0 > 0 || amount_1 > 0,
        ErrorCode::ForbidBothZeroForSupplyLiquidity
    );

    let mut amount_0_transfer_fee = 0;
    let mut amount_1_transfer_fee = 0;
    if vault_0_mint.is_some() {
        amount_0_transfer_fee =
            get_transfer_inverse_fee(vault_0_mint.clone().unwrap(), amount_0).unwrap();
    };
    if vault_1_mint.is_some() {
        amount_1_transfer_fee =
            get_transfer_inverse_fee(vault_1_mint.clone().unwrap(), amount_1).unwrap();
    }
    emit!(LiquidityCalculateEvent {
        pool_liquidity: liquidity_before,
        pool_sqrt_price_x64: pool_state.sqrt_price_x64,
        pool_tick: pool_state.tick_current,
        calc_amount_0: amount_0,
        calc_amount_1: amount_1,
        trade_fee_owed_0: 0,
        trade_fee_owed_1: 0,
        transfer_fee_0: amount_0_transfer_fee,
        transfer_fee_1: amount_1_transfer_fee,
    });
    #[cfg(feature = "enable-log")]
    msg!(
        "amount_0: {}, amount_0_transfer_fee: {}, amount_1: {}, amount_1_transfer_fee: {}",
        amount_0,
        amount_0_transfer_fee,
        amount_1,
        amount_1_transfer_fee
    );
    require_gte!(
        amount_0_max,
        amount_0 + amount_0_transfer_fee,
        ErrorCode::PriceSlippageCheck
    );
    require_gte!(
        amount_1_max,
        amount_1 + amount_1_transfer_fee,
        ErrorCode::PriceSlippageCheck
    );
    let mut token_2022_program_opt: Option<AccountInfo> = None;
    if token_program_2022.is_some() {
        token_2022_program_opt = Some(token_program_2022.clone().unwrap().to_account_info());
    }
    transfer_from_user_to_pool_vault(
        payer,
        token_account_0,
        token_vault_0,
        vault_0_mint,
        &token_program,
        token_2022_program_opt.clone(),
        amount_0 + amount_0_transfer_fee,
    )?;

    transfer_from_user_to_pool_vault(
        payer,
        token_account_1,
        token_vault_1,
        vault_1_mint,
        &token_program,
        token_2022_program_opt.clone(),
        amount_1 + amount_1_transfer_fee,
    )?;
    emit!(LiquidityChangeEvent {
        pool_state: pool_state.key(),
        tick: pool_state.tick_current,
        tick_lower: tick_lower_index,
        tick_upper: tick_upper_index,
        liquidity_before: liquidity_before,
        liquidity_after: pool_state.liquidity,
    });
    Ok((
        amount_0,
        amount_1,
        amount_0_transfer_fee,
        amount_1_transfer_fee,
    ))
}

pub fn modify_position(
    liquidity_delta: i128,
    pool_state: &mut RefMut<PoolState>,
    protocol_position_state: &mut ProtocolPositionState,
    tick_lower_state: &mut TickState,
    tick_upper_state: &mut TickState,
    timestamp: u64,
) -> Result<(u64, u64, bool, bool)> {
    let (flip_tick_lower, flip_tick_upper) = update_position(
        liquidity_delta,
        pool_state,
        protocol_position_state,
        tick_lower_state,
        tick_upper_state,
        timestamp,
    )?;
    let mut amount_0 = 0;
    let mut amount_1 = 0;

    if liquidity_delta != 0 {
        (amount_0, amount_1) = liquidity_math::get_delta_amounts_signed(
            pool_state.tick_current,
            pool_state.sqrt_price_x64,
            tick_lower_state.tick,
            tick_upper_state.tick,
            liquidity_delta,
        )?;
        if pool_state.tick_current >= tick_lower_state.tick
            && pool_state.tick_current < tick_upper_state.tick
        {
            pool_state.liquidity =
                liquidity_math::add_delta(pool_state.liquidity, liquidity_delta)?;
        }
    }

    Ok((amount_0, amount_1, flip_tick_lower, flip_tick_upper))
}

/// Updates a position with the given liquidity delta and tick
pub fn update_position(
    liquidity_delta: i128,
    pool_state: &mut RefMut<PoolState>,
    protocol_position_state: &mut ProtocolPositionState,
    tick_lower_state: &mut TickState,
    tick_upper_state: &mut TickState,
    timestamp: u64,
) -> Result<(bool, bool)> {
    let updated_reward_infos = pool_state.update_reward_infos(timestamp)?;

    let mut flipped_lower = false;
    let mut flipped_upper = false;

    // update the ticks if liquidity delta is non-zero
    if liquidity_delta != 0 {
        // Update tick state and find if tick is flipped
        flipped_lower = tick_lower_state.update(
            pool_state.tick_current,
            liquidity_delta,
            pool_state.fee_growth_global_0_x64,
            pool_state.fee_growth_global_1_x64,
            false,
            &updated_reward_infos,
        )?;
        flipped_upper = tick_upper_state.update(
            pool_state.tick_current,
            liquidity_delta,
            pool_state.fee_growth_global_0_x64,
            pool_state.fee_growth_global_1_x64,
            true,
            &updated_reward_infos,
        )?;
        #[cfg(feature = "enable-log")]
        msg!(
            "tick_upper.reward_growths_outside_x64:{:?}, tick_lower.reward_growths_outside_x64:{:?}",
            identity(tick_upper_state.reward_growths_outside_x64),
            identity(tick_lower_state.reward_growths_outside_x64)
        );
    }

    // Update fees
    let (fee_growth_inside_0_x64, fee_growth_inside_1_x64) = tick_array::get_fee_growth_inside(
        tick_lower_state.deref(),
        tick_upper_state.deref(),
        pool_state.tick_current,
        pool_state.fee_growth_global_0_x64,
        pool_state.fee_growth_global_1_x64,
    );

    // Update reward outside if needed
    let reward_growths_inside = tick_array::get_reward_growths_inside(
        tick_lower_state.deref(),
        tick_upper_state.deref(),
        pool_state.tick_current,
        &updated_reward_infos,
    );

    protocol_position_state.update(
        tick_lower_state.tick,
        tick_upper_state.tick,
        liquidity_delta,
        fee_growth_inside_0_x64,
        fee_growth_inside_1_x64,
        reward_growths_inside,
    )?;
    if liquidity_delta < 0 {
        if flipped_lower {
            tick_lower_state.clear();
        }
        if flipped_upper {
            tick_upper_state.clear();
        }
    }
    Ok((flipped_lower, flipped_upper))
}

fn get_metadata_data(personal_position_id: Pubkey) -> (String, String, String) {
    return (
        String::from("Raydium Concentrated Liquidity"),
        String::from("RCL"),
        format!(
            "https://dynamic-ipfs.raydium.io/clmm/position?id={}",
            personal_position_id.to_string()
        ),
    );
}

pub fn initialize_token_metadata_extension<'info>(
    payer: &Signer<'info>,
    position_nft_mint: &AccountInfo<'info>,
    mint_authority: &AccountInfo<'info>,
    metadata_update_authority: &AccountInfo<'info>,
    token_2022_program: &Program<'info, Token2022>,
    name: String,
    symbol: String,
    uri: String,
    signers_seeds: &[&[&[u8]]],
) -> Result<()> {
    let metadata = spl_token_metadata_interface::state::TokenMetadata {
        name,
        symbol,
        uri,
        ..Default::default()
    };

    let mint_data = position_nft_mint.try_borrow_data()?;
    let mint_state_unpacked =
        StateWithExtensions::<spl_token_2022::state::Mint>::unpack(&mint_data)?;
    let new_account_len = mint_state_unpacked
        .try_get_new_account_len::<spl_token_metadata_interface::state::TokenMetadata>(&metadata)?;
    let new_rent_exempt_lamports = Rent::get()?.minimum_balance(new_account_len);
    let additional_lamports = new_rent_exempt_lamports.saturating_sub(position_nft_mint.lamports());
    // CPI call will borrow the account data
    drop(mint_data);

    let cpi_context = CpiContext::new(
        token_2022_program.to_account_info(),
        Transfer {
            from: payer.to_account_info(),
            to: position_nft_mint.to_account_info(),
        },
    );
    transfer(cpi_context, additional_lamports)?;

    solana_program::program::invoke_signed(
        &spl_token_metadata_interface::instruction::initialize(
            token_2022_program.key,
            position_nft_mint.key,
            metadata_update_authority.key,
            position_nft_mint.key,
            &mint_authority.key(),
            metadata.name,
            metadata.symbol,
            metadata.uri,
        ),
        &[
            position_nft_mint.to_account_info(),
            mint_authority.to_account_info(),
            metadata_update_authority.to_account_info(),
            token_2022_program.to_account_info(),
        ],
        signers_seeds,
    )?;

    Ok(())
}

#[cfg(test)]
mod modify_position_test {
    use super::modify_position;
    use crate::error::ErrorCode;
    use crate::libraries::tick_math;
    use crate::states::oracle::block_timestamp_mock;
    use crate::states::pool_test::build_pool;
    use crate::states::protocol_position::*;
    use crate::states::tick_array_test::build_tick;

    #[test]
    fn liquidity_delta_zero_empty_liquidity_not_allowed_test() {
        let pool_state_ref = build_pool(1, 10, 1000, 10000);
        let pool_state = &mut pool_state_ref.borrow_mut();
        let tick_lower_state = &mut build_tick(1, 10, 10).take();
        let tick_upper_state = &mut build_tick(2, 10, -10).take();

        let result = modify_position(
            0,
            pool_state,
            &mut ProtocolPositionState::default(),
            tick_lower_state,
            tick_upper_state,
            block_timestamp_mock(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), ErrorCode::InvaildLiquidity.into());
    }

    #[test]
    fn init_position_in_range_test() {
        let liquidity = 10000;
        let tick_current = 1;
        let pool_state_ref = build_pool(
            tick_current,
            10,
            tick_math::get_sqrt_price_at_tick(tick_current).unwrap(),
            liquidity,
        );
        let pool_state = &mut pool_state_ref.borrow_mut();

        let tick_lower_index = 0;
        let tick_upper_index = 2;
        let tick_lower_state = &mut build_tick(tick_lower_index, 0, 0).take();
        let tick_upper_state = &mut build_tick(tick_upper_index, 0, 0).take();

        let liquidity_delta = 10000;
        let protocol_position = &mut ProtocolPositionState::default();
        let (amount_0_int, amount_1_int, flip_tick_lower, flip_tick_upper) = modify_position(
            liquidity_delta,
            pool_state,
            protocol_position,
            tick_lower_state,
            tick_upper_state,
            block_timestamp_mock(),
        )
        .unwrap();
        assert!(amount_0_int != 0);
        assert!(amount_1_int != 0);
        assert_eq!(flip_tick_lower, true);
        assert_eq!(flip_tick_upper, true);

        // check pool active liquidity
        let new_liquidity = pool_state.liquidity;
        assert_eq!(new_liquidity, liquidity + (liquidity_delta as u128));

        // check tick state
        assert!(tick_lower_state.is_initialized());
        assert!(tick_lower_state.liquidity_gross == 10000);
        assert!(tick_upper_state.liquidity_gross == 10000);

        assert!(tick_lower_state.liquidity_net == 10000);
        assert!(tick_upper_state.liquidity_net == -10000);

        assert!(tick_lower_state.fee_growth_outside_0_x64 == pool_state.fee_growth_global_0_x64);
        assert!(tick_lower_state.fee_growth_outside_1_x64 == pool_state.fee_growth_global_1_x64);
        assert!(tick_upper_state.fee_growth_outside_0_x64 == 0);
        assert!(tick_upper_state.fee_growth_outside_1_x64 == 0);

        // check protocol position
        let fee_growth_inside_0_last_x64 = pool_state.fee_growth_global_0_x64
            - tick_lower_state.fee_growth_outside_0_x64
            - tick_upper_state.fee_growth_outside_0_x64;
        let fee_growth_inside_1_last_x64 = pool_state.fee_growth_global_1_x64
            - tick_lower_state.fee_growth_outside_1_x64
            - tick_upper_state.fee_growth_outside_1_x64;
        assert!(protocol_position.fee_growth_inside_0_last_x64 == fee_growth_inside_0_last_x64);
        assert!(protocol_position.fee_growth_inside_1_last_x64 == fee_growth_inside_1_last_x64);
        assert!(protocol_position.token_fees_owed_0 == 0);
        assert!(protocol_position.token_fees_owed_1 == 0);
        assert!(protocol_position.tick_lower_index == tick_lower_index);
        assert!(protocol_position.tick_upper_index == tick_upper_index);

        // check protocol position state
    }

    #[test]
    fn init_position_in_left_of_current_tick_test() {
        let liquidity = 10000;
        let tick_current = 1;
        let pool_state_ref = build_pool(
            tick_current,
            10,
            tick_math::get_sqrt_price_at_tick(tick_current).unwrap(),
            liquidity,
        );
        let pool_state = &mut pool_state_ref.borrow_mut();

        let tick_lower_index = -1;
        let tick_upper_index = 0;
        let tick_lower_state = &mut build_tick(tick_lower_index, 0, 0).take();
        let tick_upper_state = &mut build_tick(tick_upper_index, 0, 0).take();

        let liquidity_delta = 10000;
        let protocol_position = &mut ProtocolPositionState::default();
        let (amount_0_int, amount_1_int, flip_tick_lower, flip_tick_upper) = modify_position(
            liquidity_delta,
            pool_state,
            protocol_position,
            tick_lower_state,
            tick_upper_state,
            block_timestamp_mock(),
        )
        .unwrap();
        assert!(amount_0_int == 0);
        assert!(amount_1_int != 0);
        assert_eq!(flip_tick_lower, true);
        assert_eq!(flip_tick_upper, true);

        // check pool active liquidity
        let new_liquidity = pool_state.liquidity;
        assert_eq!(new_liquidity, liquidity_delta as u128);

        // check tick state
        assert!(tick_lower_state.is_initialized());
        assert!(tick_lower_state.liquidity_gross == 10000);
        assert!(tick_upper_state.liquidity_gross == 10000);

        assert!(tick_lower_state.liquidity_net == 10000);
        assert!(tick_upper_state.liquidity_net == -10000);

        assert!(tick_lower_state.fee_growth_outside_0_x64 == pool_state.fee_growth_global_0_x64);
        assert!(tick_lower_state.fee_growth_outside_1_x64 == pool_state.fee_growth_global_1_x64);
        assert!(tick_upper_state.fee_growth_outside_0_x64 == pool_state.fee_growth_global_0_x64);
        assert!(tick_upper_state.fee_growth_outside_1_x64 == pool_state.fee_growth_global_1_x64);

        // check protocol position
        let fee_growth_inside_0_last_x64 = pool_state.fee_growth_global_0_x64
            - tick_lower_state.fee_growth_outside_0_x64
            - (pool_state.fee_growth_global_0_x64 - tick_upper_state.fee_growth_outside_0_x64);
        let fee_growth_inside_1_last_x64 = pool_state.fee_growth_global_1_x64
            - tick_lower_state.fee_growth_outside_1_x64
            - (pool_state.fee_growth_global_1_x64 - tick_upper_state.fee_growth_outside_1_x64);
        assert!(protocol_position.fee_growth_inside_0_last_x64 == fee_growth_inside_0_last_x64);
        assert!(protocol_position.fee_growth_inside_1_last_x64 == fee_growth_inside_1_last_x64);
        assert!(protocol_position.token_fees_owed_0 == 0);
        assert!(protocol_position.token_fees_owed_1 == 0);
        assert!(protocol_position.tick_lower_index == tick_lower_index);
        assert!(protocol_position.tick_upper_index == tick_upper_index);
    }

    #[test]
    fn init_position_in_right_of_current_tick_test() {
        let liquidity = 10000;
        let tick_current = 1;
        let pool_state_ref = build_pool(
            tick_current,
            10,
            tick_math::get_sqrt_price_at_tick(tick_current).unwrap(),
            liquidity,
        );
        let pool_state = &mut pool_state_ref.borrow_mut();

        let tick_lower_index = 2;
        let tick_upper_index = 3;
        let tick_lower_state = &mut build_tick(tick_lower_index, 0, 0).take();
        let tick_upper_state = &mut build_tick(tick_upper_index, 0, 0).take();

        let liquidity_delta = 10000;
        let protocol_position = &mut ProtocolPositionState::default();
        let (amount_0_int, amount_1_int, flip_tick_lower, flip_tick_upper) = modify_position(
            liquidity_delta,
            pool_state,
            protocol_position,
            tick_lower_state,
            tick_upper_state,
            block_timestamp_mock(),
        )
        .unwrap();
        assert!(amount_0_int != 0);
        assert!(amount_1_int == 0);
        assert_eq!(flip_tick_lower, true);
        assert_eq!(flip_tick_upper, true);

        // check pool active liquidity
        let new_liquidity = pool_state.liquidity;
        assert_eq!(new_liquidity, liquidity_delta as u128);

        // check tick state
        assert!(tick_lower_state.is_initialized());
        assert!(tick_lower_state.liquidity_gross == 10000);
        assert!(tick_upper_state.liquidity_gross == 10000);

        assert!(tick_lower_state.liquidity_net == 10000);
        assert!(tick_upper_state.liquidity_net == -10000);

        assert!(tick_lower_state.fee_growth_outside_0_x64 == 0);
        assert!(tick_lower_state.fee_growth_outside_1_x64 == 0);
        assert!(tick_upper_state.fee_growth_outside_0_x64 == 0);
        assert!(tick_upper_state.fee_growth_outside_1_x64 == 0);

        // check protocol position
        let fee_growth_inside_0_last_x64 = pool_state.fee_growth_global_0_x64
            - (pool_state.fee_growth_global_0_x64 - tick_lower_state.fee_growth_outside_0_x64)
            - tick_upper_state.fee_growth_outside_0_x64;
        let fee_growth_inside_1_last_x64 = pool_state.fee_growth_global_1_x64
            - (pool_state.fee_growth_global_1_x64 - tick_lower_state.fee_growth_outside_1_x64)
            - tick_upper_state.fee_growth_outside_1_x64;
        assert!(protocol_position.fee_growth_inside_0_last_x64 == fee_growth_inside_0_last_x64);
        assert!(protocol_position.fee_growth_inside_1_last_x64 == fee_growth_inside_1_last_x64);
        assert!(protocol_position.token_fees_owed_0 == 0);
        assert!(protocol_position.token_fees_owed_1 == 0);
        assert!(protocol_position.tick_lower_index == tick_lower_index);
        assert!(protocol_position.tick_upper_index == tick_upper_index);

        // check protocol position state
    }
}
