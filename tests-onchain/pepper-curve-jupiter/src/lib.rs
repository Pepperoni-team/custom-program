//! # pepper-curve-jupiter
//!
//! Jupiter's `Amm` interface (`jupiter-amm-interface` at `95bd184`) for Pepper's USDC bonding curve,
//! `pepper_curve`'s `CurveUsdc` (USDC curve design §7, §10.5; docs/solana/PEPPER-CURVE-PROGRAM.md §14).
//!
//! - **The market** is one curve: `key` its PDA `["curve", mint]`, label «Pepper», its reserve mints
//!   USDC and the token. `from_keyed_account` reads a `CurveUsdc` of layout version 1 owned by the
//!   program, at the address its own seeds and bump give, and refuses anything else.
//! - **The quote reads two accounts**, the curve and the USDC config (`get_accounts_to_update`): the
//!   counters suffice, no vault balance. It is the program's own arithmetic, `pepper_curve_math`'s
//!   `quote_buy` and `quote_sell`, which `buy_exact_in` and `sell_exact_in` call before moving exactly
//!   what they return. Exact-in only. On a buy `in_amount` is what the curve charges, below the
//!   amount asked when a buy at graduation is partly filled; `fee_amount` is in USDC, the 1% and, on
//!   a revenue curve, the creator's and Fanex's raise shares (on a sale, the 1% of the gross).
//! - **It is active** only in Bonding, with neither pause set and past the curve's five-minute launch
//!   window, whose 1%-per-wallet cap reads a holding no quote can see.
//! - **The swap** is the program's instruction over its fixed account list, account 0 the
//!   `token_transfer_authority`, the creator's USDC account passed exactly while the counters require
//!   it, the launch cap's waiver never (§10.3.5), as `Swap::Placeholder { data: Some([direction]) }`;
//!   [`swap::swap_instruction_data`] encodes it.
//! - No network call, no program dependency (the swap calls SPL Token alone).

pub mod layout;
pub mod quote;
pub mod swap;

use std::sync::atomic::Ordering;

use jupiter_amm_interface::{
    single_program_amm, AccountProvider, Amm, AmmContext, AmmError, AmmLabel, ClockRef,
    KeyedAccount, Quote, QuoteParams, SingleProgramAmm, SwapAndAccountMetas, SwapMode, SwapParams,
};
use rust_decimal::Decimal;
use solana_account::ReadableAccount;
use solana_pubkey::Pubkey;

use crate::layout::{
    CurveAddresses, CurveUsdc, LayoutError, UsdcConfig, PROGRAM_ID, USDC_CONFIG_ADDRESS,
};
use crate::quote::{check_tradeable, Direction, Refusal};
use crate::swap::{buy_account_metas, placeholder, sell_account_metas, UserAccounts, BUY_ACCOUNTS};

pub use crate::swap::swap_instruction_data;

impl From<LayoutError> for AmmError {
    fn from(error: LayoutError) -> Self {
        AmmError::Custom(error.to_string())
    }
}

impl From<Refusal> for AmmError {
    fn from(refusal: Refusal) -> Self {
        AmmError::Custom(refusal.to_string())
    }
}

/// One USDC curve of `pepper_curve`, as Jupiter routes it.
#[derive(Clone)]
pub struct PepperCurveAmm {
    addresses: CurveAddresses,
    curve: CurveUsdc,
    /// Read by `update`, with the curve; until then nothing is active or quoted.
    config: Option<UsdcConfig>,
    /// The chain's clock Jupiter keeps current, for the launch window.
    clock: ClockRef,
}

single_program_amm!(PepperCurveAmm, PROGRAM_ID, "Pepper");

fn read_curve(account: &impl ReadableAccount) -> Result<CurveUsdc, LayoutError> {
    if account.owner() != &PROGRAM_ID {
        return Err(LayoutError::WrongOwner {
            account: CurveUsdc::NAME,
            owner: *account.owner(),
        });
    }
    CurveUsdc::decode(account.data())
}

fn read_config(account: &impl ReadableAccount) -> Result<UsdcConfig, LayoutError> {
    if account.owner() != &PROGRAM_ID {
        return Err(LayoutError::WrongOwner {
            account: UsdcConfig::NAME,
            owner: *account.owner(),
        });
    }
    UsdcConfig::decode(account.data())
}

impl PepperCurveAmm {
    /// The curve as last read.
    pub fn curve(&self) -> &CurveUsdc {
        &self.curve
    }

    /// The USDC config as last read, or `None` before the first `update`.
    pub fn config(&self) -> Option<&UsdcConfig> {
        self.config.as_ref()
    }

    /// The curve's PDA, its USDC vault and its token vault.
    pub fn addresses(&self) -> &CurveAddresses {
        &self.addresses
    }

    fn now(&self) -> i64 {
        self.clock.unix_timestamp.load(Ordering::Relaxed)
    }

    fn loaded_config(&self) -> Result<&UsdcConfig, AmmError> {
        self.config
            .as_ref()
            .ok_or_else(|| AmmError::from("the USDC config is not read yet: update first"))
    }

    /// USDC into the curve's token is a buy, the reverse a sale; any other pair is not this market's.
    fn direction(
        &self,
        config: &UsdcConfig,
        input: &Pubkey,
        output: &Pubkey,
    ) -> Result<Direction, AmmError> {
        let mint = self.curve.mint;
        if *input == config.usdc_mint && *output == mint {
            Ok(Direction::Buy)
        } else if *input == mint && *output == config.usdc_mint {
            Ok(Direction::Sell)
        } else {
            Err(AmmError::from(format!(
                "{input} into {output} is not this curve's pair"
            )))
        }
    }
}

impl Amm for PepperCurveAmm {
    fn from_keyed_account(
        keyed_account: &KeyedAccount,
        amm_context: &AmmContext,
    ) -> Result<Self, AmmError> {
        let curve = read_curve(&keyed_account.account)?;
        let addresses = CurveAddresses::of(&curve)?;
        if addresses.curve != keyed_account.key {
            return Err(LayoutError::WrongAddress {
                account: CurveUsdc::NAME,
                expected: addresses.curve,
                found: keyed_account.key,
            }
            .into());
        }
        Ok(Self {
            addresses,
            curve,
            config: None,
            clock: amm_context.clock_ref.clone(),
        })
    }

    fn label(&self) -> AmmLabel {
        Self::LABEL
    }

    fn program_id(&self) -> Pubkey {
        Self::PROGRAM_ID
    }

    fn key(&self) -> Pubkey {
        self.addresses.curve
    }

    /// USDC and the token, once `update` has read the config (USDC's mint is the config's, set once):
    /// before it, the token alone.
    fn get_reserve_mints(&self) -> Vec<Pubkey> {
        match &self.config {
            Some(config) => vec![config.usdc_mint, self.curve.mint],
            None => vec![self.curve.mint],
        }
    }

    fn requires_update_for_reserve_mints(&self) -> bool {
        true
    }

    fn get_accounts_to_update(&self) -> Vec<Pubkey> {
        vec![self.addresses.curve, USDC_CONFIG_ADDRESS]
    }

    fn update(&mut self, account_provider: impl AccountProvider) -> Result<(), AmmError> {
        let curve = read_curve(&account_provider.try_get(&self.addresses.curve)?)?;
        if curve.mint != self.curve.mint
            || curve.bump != self.curve.bump
            || curve.usdc_vault_bump != self.curve.usdc_vault_bump
        {
            return Err(AmmError::from(
                "the curve's account now records another mint or bump",
            ));
        }
        let config = read_config(&account_provider.try_get(&USDC_CONFIG_ADDRESS)?)?;
        self.curve = curve;
        self.config = Some(config);
        Ok(())
    }

    fn quote(&self, quote_params: &QuoteParams) -> Result<Quote, AmmError> {
        if quote_params.swap_mode != SwapMode::ExactIn {
            return Err(AmmError::from("exact-in only"));
        }
        let config = self.loaded_config()?;
        let direction =
            self.direction(config, &quote_params.input_mint, &quote_params.output_mint)?;
        let quoted = quote::quote(
            &self.curve,
            config,
            direction,
            quote_params.amount,
            self.now(),
        )?;
        Ok(Quote {
            in_amount: quoted.in_amount,
            out_amount: quoted.out_amount,
            fee_amount: quoted.fee_amount,
            fee_mint: config.usdc_mint,
            fee_pct: Decimal::from(quoted.fee_amount)
                .checked_div(Decimal::from(quoted.fee_base))
                .unwrap_or(Decimal::ZERO),
        })
    }

    fn get_swap_and_account_metas(
        &self,
        swap_params: &SwapParams,
    ) -> Result<SwapAndAccountMetas, AmmError> {
        if swap_params.swap_mode != SwapMode::ExactIn {
            return Err(AmmError::from("exact-in only"));
        }
        let config = self.loaded_config()?;
        let direction = self.direction(
            config,
            &swap_params.source_mint,
            &swap_params.destination_mint,
        )?;
        let user = UserAccounts {
            authority: swap_params.token_transfer_authority,
            source: swap_params.source_token_account,
            destination: swap_params.destination_token_account,
        };
        let account_metas = match direction {
            Direction::Buy => buy_account_metas(&self.curve, &self.addresses, config, &user)?,
            Direction::Sell => sell_account_metas(&self.curve, &self.addresses, config, &user),
        };
        Ok(SwapAndAccountMetas {
            swap: placeholder(direction),
            account_metas,
        })
    }

    /// The longer of the two lists, a buy's 13 (a sale names 11).
    fn get_accounts_len(&self) -> usize {
        BUY_ACCOUNTS.len()
    }

    fn is_active(&self) -> bool {
        self.config
            .as_ref()
            .is_some_and(|config| check_tradeable(&self.curve, config, self.now()).is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::key;
    use crate::layout::{curve_usdc, find_curve_address, find_usdc_vault_address, usdc_config};
    use crate::quote::tests::{curve_from_state, open_config, vector_cases};
    use jupiter_amm_interface::FeeMode;
    use solana_account::Account;
    use solana_clock::Clock;
    use std::collections::HashMap;

    const IDL: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../lib/solana/pepper-curve/idl/pepper_curve.json"
    );
    const USDC: Pubkey = solana_pubkey::pubkey!("3KVFue8ug46TW2itUWis5hzEYfUqgraJCNy1RXT2Pfy1");

    fn program_account(data: Vec<u8>) -> Account {
        Account {
            lamports: 10_000_000,
            data,
            owner: PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
    }

    /// A vector's revenue curve at real addresses, its config naming `USDC`.
    fn market() -> (CurveUsdc, UsdcConfig) {
        let cases = vector_cases();
        let state = &cases
            .iter()
            .find(|c| c["name"] == "revenue, fresh: a buy inside both caps")
            .unwrap()["state"];
        let mint = key(55);
        let curve = CurveUsdc {
            mint,
            bump: find_curve_address(&mint).1,
            usdc_vault_bump: find_usdc_vault_address(&mint).1,
            ..curve_from_state(state)
        };
        (
            curve,
            UsdcConfig {
                usdc_mint: USDC,
                ..open_config()
            },
        )
    }

    fn clock_at(unix_timestamp: i64) -> AmmContext {
        AmmContext {
            clock_ref: ClockRef::from(Clock {
                unix_timestamp,
                ..Clock::default()
            }),
        }
    }

    fn provider(curve: &CurveUsdc, config: &UsdcConfig) -> HashMap<Pubkey, Box<Account>> {
        let (address, _) = find_curve_address(&curve.mint);
        HashMap::from([
            (address, Box::new(program_account(curve.to_account_data()))),
            (
                USDC_CONFIG_ADDRESS,
                Box::new(program_account(config.to_account_data())),
            ),
        ])
    }

    fn amm(curve: &CurveUsdc, config: &UsdcConfig, now: i64) -> PepperCurveAmm {
        let (address, _) = find_curve_address(&curve.mint);
        let keyed = KeyedAccount {
            key: address,
            account: program_account(curve.to_account_data()),
            params: None,
        };
        let mut amm = PepperCurveAmm::from_keyed_account(&keyed, &clock_at(now)).unwrap();
        amm.update(&provider(curve, config)).unwrap();
        amm
    }

    fn params(amount: u64, input: Pubkey, output: Pubkey) -> QuoteParams {
        QuoteParams {
            amount,
            input_mint: input,
            output_mint: output,
            swap_mode: SwapMode::ExactIn,
            fee_mode: FeeMode::Normal,
        }
    }

    #[test]
    fn the_market_is_the_curve_named_pepper_on_the_program() {
        let (curve, config) = market();
        let amm = amm(&curve, &config, curve.created_at + 600);
        let (address, _) = find_curve_address(&curve.mint);
        assert_eq!(amm.label(), "Pepper");
        // The kit loads tests/fixtures/<label, lowercased, spaces as underscores>.so.
        assert_eq!(amm.label().to_lowercase().replace(' ', "_"), "pepper");
        assert_eq!(amm.program_id(), PROGRAM_ID);
        assert_eq!(amm.key(), address);
        assert_eq!(amm.get_reserve_mints(), vec![USDC, curve.mint]);
        assert!(amm.requires_update_for_reserve_mints());
        assert_eq!(
            amm.get_accounts_to_update(),
            vec![address, USDC_CONFIG_ADDRESS]
        );
        assert_eq!(amm.get_accounts_len(), 13);
        assert!(!amm.supports_exact_out());
        assert!(!amm.has_dynamic_accounts());
        assert!(!amm.unidirectional());
        assert!(amm.program_dependencies().is_empty());
        assert!(amm.is_active());
    }

    #[test]
    fn a_buy_and_a_sale_quote_in_usdc_with_the_fee_as_a_fraction() {
        let (curve, config) = market();
        let curve = CurveUsdc {
            real_usdc_reserves: 50_000_000,
            ..curve
        };
        let amm = amm(&curve, &config, curve.created_at + 600);
        let buy = amm.quote(&params(100_000_000, USDC, curve.mint)).unwrap();
        let expected =
            pepper_curve_math::quote_buy(&curve.swap_state(), 100_000_000, curve.created_at + 600)
                .unwrap();
        assert_eq!(buy.in_amount, expected.charged);
        assert_eq!(buy.out_amount, expected.tokens_out);
        assert_eq!(
            buy.fee_amount,
            expected.platform_fee + expected.creator_amount + expected.fanex_amount
        );
        assert_eq!(buy.fee_mint, USDC);
        // 1% of the buy, then 37.6% and 2.4% of the rest to the creator and Fanex: 40.6%.
        assert_eq!(buy.fee_pct, Decimal::new(406, 3));

        let sale = amm
            .quote(&params(1_000_000_000_000, curve.mint, USDC))
            .unwrap();
        let expected =
            pepper_curve_math::quote_sell(&curve.swap_state(), 1_000_000_000_000).unwrap();
        assert_eq!(
            (sale.in_amount, sale.out_amount, sale.fee_amount),
            (1_000_000_000_000, expected.proceeds, expected.platform_fee)
        );
        assert_eq!(sale.fee_mint, USDC);
        assert_eq!(
            sale.fee_pct,
            Decimal::from(expected.platform_fee) / Decimal::from(expected.reserve_out)
        );
    }

    #[test]
    fn another_pair_exact_out_and_a_quote_before_the_config_are_refused() {
        let (curve, config) = market();
        let amm = amm(&curve, &config, curve.created_at + 600);
        assert!(amm.quote(&params(1, USDC, USDC)).is_err());
        assert!(amm.quote(&params(1, key(1), curve.mint)).is_err());
        let exact_out = QuoteParams {
            swap_mode: SwapMode::ExactOut,
            ..params(1, USDC, curve.mint)
        };
        assert!(amm.quote(&exact_out).is_err());

        let (address, _) = find_curve_address(&curve.mint);
        let keyed = KeyedAccount {
            key: address,
            account: program_account(curve.to_account_data()),
            params: None,
        };
        let fresh =
            PepperCurveAmm::from_keyed_account(&keyed, &clock_at(curve.created_at + 600)).unwrap();
        assert!(!fresh.is_active());
        assert_eq!(fresh.get_reserve_mints(), vec![curve.mint]);
        assert!(fresh.quote(&params(1_000_000, USDC, curve.mint)).is_err());
    }

    #[test]
    fn only_the_program_s_curve_at_its_own_address_is_a_market() {
        let (curve, _) = market();
        let (address, _) = find_curve_address(&curve.mint);
        let context = clock_at(0);
        let foreign = KeyedAccount {
            key: address,
            account: Account {
                owner: key(9),
                ..program_account(curve.to_account_data())
            },
            params: None,
        };
        assert!(PepperCurveAmm::from_keyed_account(&foreign, &context).is_err());
        let elsewhere = KeyedAccount {
            key: key(8),
            account: program_account(curve.to_account_data()),
            params: None,
        };
        assert!(PepperCurveAmm::from_keyed_account(&elsewhere, &context).is_err());
        let mut sol_curve = curve.to_account_data();
        sol_curve[..8].copy_from_slice(&[0; 8]);
        let sol = KeyedAccount {
            key: address,
            account: program_account(sol_curve),
            params: None,
        };
        assert!(PepperCurveAmm::from_keyed_account(&sol, &context).is_err());
    }

    #[test]
    fn update_follows_the_counters_and_the_pauses() {
        let (curve, config) = market();
        let now = curve.created_at + 600;
        let mut amm = amm(&curve, &config, now);
        assert!(amm.is_active());
        let paused = UsdcConfig {
            paused: true,
            ..config
        };
        amm.update(&provider(&curve, &paused)).unwrap();
        assert!(!amm.is_active());
        assert!(amm.quote(&params(1_000_000, USDC, curve.mint)).is_err());
        let graduating = CurveUsdc { phase: 1, ..curve };
        amm.update(&provider(&graduating, &config)).unwrap();
        assert!(!amm.is_active());
        let moved = CurveUsdc {
            real_usdc_reserves: 7,
            ..curve
        };
        amm.update(&provider(&moved, &config)).unwrap();
        assert!(amm.is_active());
        assert_eq!(amm.curve().real_usdc_reserves, 7);
        // A config another program owns is refused, and the last good one kept.
        let mut forged = provider(&curve, &config);
        forged.get_mut(&USDC_CONFIG_ADDRESS).unwrap().owner = key(3);
        assert!(amm.update(&forged).is_err());
        assert_eq!(amm.config(), Some(&config));
    }

    #[test]
    fn the_launch_window_turns_the_market_off_on_jupiter_s_clock() {
        let (curve, config) = market();
        let amm = amm(&curve, &config, curve.created_at + 299);
        assert!(!amm.is_active());
        assert!(amm.quote(&params(1_000_000, USDC, curve.mint)).is_err());
        amm.clock.update(Clock {
            unix_timestamp: curve.created_at + 300,
            ..Clock::default()
        });
        assert!(amm.is_active());
        assert!(amm.quote(&params(1_000_000, USDC, curve.mint)).is_ok());
    }

    #[test]
    fn the_metas_and_the_placeholder_follow_the_direction() {
        let (curve, config) = market();
        let amm = amm(&curve, &config, curve.created_at + 600);
        let jupiter = solana_pubkey::pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");
        let swap_params = |source_mint: Pubkey, destination_mint: Pubkey| SwapParams {
            swap_mode: SwapMode::ExactIn,
            in_amount: 1,
            out_amount: 1,
            source_mint,
            destination_mint,
            source_token_account: key(61),
            destination_token_account: key(62),
            token_transfer_authority: key(60),
            user: key(63),
            payer: key(64),
            quote_mint_to_referrer: None,
            jupiter_program_id: &jupiter,
            missing_dynamic_accounts_as_default: false,
        };
        let buy = amm
            .get_swap_and_account_metas(&swap_params(USDC, curve.mint))
            .unwrap();
        assert_eq!(buy.swap, placeholder(Direction::Buy));
        assert_eq!(buy.account_metas.len(), 13);
        assert_eq!(
            (buy.account_metas[0].pubkey, buy.account_metas[0].is_signer),
            (key(60), true)
        );
        assert_eq!(buy.account_metas[8].pubkey, curve.creator_usdc_account);
        // The absent option is the program's own id, never Jupiter's placeholder.
        assert_eq!(buy.account_metas[11].pubkey, PROGRAM_ID);
        assert!(buy
            .account_metas
            .iter()
            .all(|m| m.pubkey != jupiter && m.pubkey != key(63) && m.pubkey != key(64)));
        let sale = amm
            .get_swap_and_account_metas(&swap_params(curve.mint, USDC))
            .unwrap();
        assert_eq!(sale.swap, placeholder(Direction::Sell));
        assert_eq!(sale.account_metas.len(), 11);
        assert_eq!(
            (sale.account_metas[1].pubkey, sale.account_metas[2].pubkey),
            (key(61), key(62))
        );
        assert!(amm
            .get_swap_and_account_metas(&swap_params(USDC, USDC))
            .is_err());
    }

    fn idl() -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(IDL).expect("the committed IDL"))
            .expect("the IDL parses")
    }

    fn size_of(ty: &serde_json::Value) -> usize {
        match ty.as_str() {
            Some("u8" | "bool") => 1,
            Some("u16") => 2,
            Some("u64" | "i64") => 8,
            Some("pubkey") => 32,
            Some(other) => panic!("a field type this layout does not hold: {other}"),
            None => {
                let array = ty["array"].as_array().expect("an array type");
                size_of(&array[0]) * array[1].as_u64().unwrap() as usize
            }
        }
    }

    /// The offsets this SDK reads, held to the field list the program's IDL publishes: Borsh lays
    /// the fields out in order with no padding, right after the 8-byte discriminator.
    #[test]
    fn the_layouts_are_the_idl_s() {
        let idl = idl();
        for (name, fields, len, discriminator) in [
            (
                "CurveUsdc",
                &curve_usdc::FIELDS[..],
                curve_usdc::LEN,
                curve_usdc::DISCRIMINATOR,
            ),
            (
                "UsdcConfig",
                &usdc_config::FIELDS[..],
                usdc_config::LEN,
                usdc_config::DISCRIMINATOR,
            ),
        ] {
            let ty = idl["types"]
                .as_array()
                .unwrap()
                .iter()
                .find(|t| t["name"] == name)
                .unwrap();
            let mut offset = 8;
            let mut laid_out = Vec::new();
            for field in ty["type"]["fields"].as_array().unwrap() {
                laid_out.push((field["name"].as_str().unwrap().to_string(), offset));
                offset += size_of(&field["type"]);
            }
            assert_eq!(offset, len, "{name}'s size");
            let ours: Vec<(String, usize)> =
                fields.iter().map(|(n, o)| (n.to_string(), *o)).collect();
            assert_eq!(ours, laid_out, "{name}'s offsets");
            let account = idl["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|a| a["name"] == name)
                .unwrap();
            let idl_discriminator: Vec<u8> = account["discriminator"]
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b.as_u64().unwrap() as u8)
                .collect();
            assert_eq!(idl_discriminator, discriminator, "{name}'s discriminator");
        }
    }
}
