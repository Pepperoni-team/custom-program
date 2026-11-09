//! The swap: the program's own instruction over its fixed account lists, `buy_exact_in`'s 13 and
//! `sell_exact_in`'s 11 in the order the program declares them (USDC curve design §10.3.5, §10.3.6),
//! no `remaining_accounts`, nothing opened, one signer. Until Jupiter gives the venue a `Swap`
//! variant of its own it travels as `Swap::Placeholder { data: Some([direction]) }`, and
//! [`swap_instruction_data`] is the encoder that turns it, with the amounts, into the instruction's
//! data (§10.5).

use jupiter_amm_interface::{AmmError, Swap};
use solana_instruction::AccountMeta;
use solana_pubkey::Pubkey;

use crate::layout::{
    CurveAddresses, CurveUsdc, UsdcConfig, PROGRAM_ID, TOKEN_PROGRAM_ID, USDC_CONFIG_ADDRESS,
};
use crate::quote::{Direction, Refusal};

/// The first 8 bytes of sha256("global:buy_exact_in"), as the committed IDL records them.
pub const BUY_EXACT_IN_DISCRIMINATOR: [u8; 8] = [250, 234, 13, 123, 213, 156, 19, 236];
/// The first 8 bytes of sha256("global:sell_exact_in").
pub const SELL_EXACT_IN_DISCRIMINATOR: [u8; 8] = [149, 39, 222, 155, 211, 124, 152, 26];

/// `Swap::Placeholder`'s one byte: the direction, all the encoder needs besides the amounts.
pub const DIRECTION_BUY: u8 = 0;
pub const DIRECTION_SELL: u8 = 1;

/// `buy_exact_in`'s accounts by their IDL names, in the instruction's order: never reordered.
pub const BUY_ACCOUNTS: [&str; 13] = [
    "user_transfer_authority",
    "user_source_usdc",
    "user_destination_token",
    "usdc_config",
    "curve",
    "usdc_vault",
    "token_vault",
    "fee_usdc_account",
    "creator_usdc_account",
    "usdc_mint",
    "token_mint",
    "launch_guard_waiver",
    "token_program",
];

/// `sell_exact_in`'s accounts by their IDL names, in the instruction's order.
pub const SELL_ACCOUNTS: [&str; 11] = [
    "user_transfer_authority",
    "user_source_token",
    "user_destination_usdc",
    "usdc_config",
    "curve",
    "usdc_vault",
    "token_vault",
    "fee_usdc_account",
    "usdc_mint",
    "token_mint",
    "token_program",
];

/// The swap's user side, from Jupiter's `SwapParams`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UserAccounts {
    /// Account 0, the only signer: the user, or a router's program authority on its shared accounts.
    /// Not writable: the transaction's fee payer pays.
    pub authority: Pubkey,
    /// Owned by `authority` (the program refuses a delegate on a sale).
    pub source: Pubkey,
    /// Any token account of the output mint outside the launch window, where alone a router routes.
    pub destination: Pubkey,
}

/// An optional account left out, as Anchor reads it: the program's own id, neither signer nor
/// writable, so no position ever moves. Not Jupiter's placeholder: the program would read that one as
/// an account passed.
fn absent() -> AccountMeta {
    AccountMeta::new_readonly(PROGRAM_ID, false)
}

/// `buy_exact_in`'s 13 metas. Account 8, the creator's USDC account, is passed exactly while the
/// curve's counters require it, else left out; account 11, the launch cap's waiver, is always left
/// out: the program reads it only inside the launch window, where the SDK does not route.
pub fn buy_account_metas(
    curve: &CurveUsdc,
    addresses: &CurveAddresses,
    config: &UsdcConfig,
    user: &UserAccounts,
) -> Result<Vec<AccountMeta>, Refusal> {
    let creator_usdc_account = if curve.creator_usdc_account_required() {
        if curve.creator_usdc_account == Pubkey::default() {
            return Err(Refusal::CreatorUsdcAccountUnset);
        }
        AccountMeta::new(curve.creator_usdc_account, false)
    } else {
        absent()
    };
    Ok(vec![
        AccountMeta::new_readonly(user.authority, true),
        AccountMeta::new(user.source, false),
        AccountMeta::new(user.destination, false),
        AccountMeta::new_readonly(USDC_CONFIG_ADDRESS, false),
        AccountMeta::new(addresses.curve, false),
        AccountMeta::new(addresses.usdc_vault, false),
        AccountMeta::new(addresses.token_vault, false),
        AccountMeta::new(config.fee_usdc_account, false),
        creator_usdc_account,
        AccountMeta::new_readonly(config.usdc_mint, false),
        AccountMeta::new_readonly(curve.mint, false),
        absent(),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ])
}

/// `sell_exact_in`'s 11 metas: no optional account.
pub fn sell_account_metas(
    curve: &CurveUsdc,
    addresses: &CurveAddresses,
    config: &UsdcConfig,
    user: &UserAccounts,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new_readonly(user.authority, true),
        AccountMeta::new(user.source, false),
        AccountMeta::new(user.destination, false),
        AccountMeta::new_readonly(USDC_CONFIG_ADDRESS, false),
        AccountMeta::new(addresses.curve, false),
        AccountMeta::new(addresses.usdc_vault, false),
        AccountMeta::new(addresses.token_vault, false),
        AccountMeta::new(config.fee_usdc_account, false),
        AccountMeta::new_readonly(config.usdc_mint, false),
        AccountMeta::new_readonly(curve.mint, false),
        AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
    ]
}

/// The swap as Jupiter carries it until the venue has a variant of its own.
pub fn placeholder(direction: Direction) -> Swap {
    let byte = match direction {
        Direction::Buy => DIRECTION_BUY,
        Direction::Sell => DIRECTION_SELL,
    };
    Swap::Placeholder {
        data: Some(vec![byte]),
    }
}

/// The direction a swap carries: this SDK's `Placeholder`, or nothing.
pub fn direction_of(swap: &Swap) -> Option<Direction> {
    match swap {
        Swap::Placeholder { data: Some(data) } if data.as_slice() == [DIRECTION_BUY] => {
            Some(Direction::Buy)
        }
        Swap::Placeholder { data: Some(data) } if data.as_slice() == [DIRECTION_SELL] => {
            Some(Direction::Sell)
        }
        _ => None,
    }
}

/// The instruction's data: its discriminator, then `amount_in` and `minimum_out` as Borsh u64s, the
/// arguments `buy_exact_in(usdc_in, min_tokens_out)` and `sell_exact_in(tokens_in, min_usdc_out)`
/// take. A sale's minimum is checked against the net the seller receives.
pub fn instruction_data(direction: Direction, amount_in: u64, minimum_out: u64) -> Vec<u8> {
    let discriminator = match direction {
        Direction::Buy => BUY_EXACT_IN_DISCRIMINATOR,
        Direction::Sell => SELL_EXACT_IN_DISCRIMINATOR,
    };
    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&discriminator);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&minimum_out.to_le_bytes());
    data
}

/// The encoder Jupiter's swap side, and the test kit, call with the `Swap` the `Amm` returned.
pub fn swap_instruction_data(
    swap: &Swap,
    amount_in: u64,
    minimum_out: u64,
) -> Result<Vec<u8>, AmmError> {
    let direction =
        direction_of(swap).ok_or_else(|| AmmError::from(format!("not a Pepper swap: {swap:?}")))?;
    Ok(instruction_data(direction, amount_in, minimum_out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::tests::{distinct_config, key};
    use crate::layout::{
        find_curve_address, find_usdc_vault_address, CURVE_SEED, USDC_CONFIG_SEED, USDC_VAULT_SEED,
    };
    use crate::quote::tests::{curve_from_state, vector_cases};
    use serde_json::Value;

    const IDL: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../lib/solana/pepper-curve/idl/pepper_curve.json"
    );

    fn idl() -> Value {
        serde_json::from_str(&std::fs::read_to_string(IDL).expect("the committed IDL"))
            .expect("the IDL parses")
    }

    fn instruction(idl: &Value, name: &str) -> Value {
        idl["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ix| ix["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}"))
    }

    fn bytes(value: &Value) -> Vec<u8> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b.as_u64().unwrap() as u8)
            .collect()
    }

    /// A revenue curve inside its raise (account 8 due), at its real addresses.
    fn revenue() -> (CurveUsdc, CurveAddresses) {
        let cases = vector_cases();
        let state = &cases
            .iter()
            .find(|c| c["name"] == "revenue, fresh: a buy inside both caps")
            .unwrap()["state"];
        let mint = key(77);
        let curve = CurveUsdc {
            mint,
            bump: find_curve_address(&mint).1,
            usdc_vault_bump: find_usdc_vault_address(&mint).1,
            ..curve_from_state(state)
        };
        let addresses = CurveAddresses::of(&curve).unwrap();
        (curve, addresses)
    }

    fn user() -> UserAccounts {
        UserAccounts {
            authority: key(90),
            source: key(91),
            destination: key(92),
        }
    }

    /// The program's instruction as the IDL records it: the names in order, each account's signer
    /// and writable flags, the fixed program, the PDA seeds. A meta of this SDK that disagrees fails
    /// every Jupiter route that takes it, so it fails here first.
    fn assert_metas_match_the_idl(
        name: &str,
        names: &[&str],
        metas: &[AccountMeta],
        absent_at: &[usize],
    ) {
        let ix = instruction(&idl(), name);
        let accounts = ix["accounts"].as_array().unwrap();
        assert_eq!(
            accounts
                .iter()
                .map(|a| a["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            names,
            "{name}'s order"
        );
        assert_eq!(metas.len(), accounts.len(), "{name}");
        for (i, (account, meta)) in accounts.iter().zip(metas).enumerate() {
            let label = format!("{name} account {i} ({})", account["name"]);
            if absent_at.contains(&i) {
                assert_eq!(account["optional"], true, "{label} may be left out");
                assert_eq!(
                    (meta.pubkey, meta.is_signer, meta.is_writable),
                    (PROGRAM_ID, false, false),
                    "{label}"
                );
                continue;
            }
            assert_eq!(meta.is_signer, account["signer"] == true, "{label}: signer");
            assert_eq!(
                meta.is_writable,
                account["writable"] == true,
                "{label}: writable"
            );
            if let Some(address) = account["address"].as_str() {
                assert_eq!(meta.pubkey.to_string(), address, "{label}");
            }
        }
    }

    #[test]
    fn a_buy_names_the_program_s_thirteen_accounts_with_account_8_while_it_is_due() {
        let (curve, addresses) = revenue();
        let config = distinct_config();
        assert!(curve.creator_usdc_account_required());
        let metas = buy_account_metas(&curve, &addresses, &config, &user()).unwrap();
        assert_metas_match_the_idl("buy_exact_in", &BUY_ACCOUNTS, &metas, &[11]);
        let keys: Vec<Pubkey> = metas.iter().map(|m| m.pubkey).collect();
        assert_eq!(
            keys,
            [
                key(90),
                key(91),
                key(92),
                USDC_CONFIG_ADDRESS,
                addresses.curve,
                addresses.usdc_vault,
                addresses.token_vault,
                config.fee_usdc_account,
                curve.creator_usdc_account,
                config.usdc_mint,
                curve.mint,
                PROGRAM_ID,
                TOKEN_PROGRAM_ID,
            ]
        );
        // The program's own test of the list (buy_exact_in.rs): account 0 alone signs, these write.
        let writable: Vec<usize> = (0..13).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(writable, [1, 2, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_buy_leaves_account_8_out_once_the_cap_is_full_or_on_an_equity_curve() {
        let (revenue, addresses) = revenue();
        for curve in [
            CurveUsdc {
                creator_received_usdc: revenue.creator_cap_usdc,
                ..revenue
            },
            CurveUsdc {
                creator_split_bps: 0,
                creator_usdc_account: Pubkey::default(),
                ..revenue
            },
        ] {
            let metas = buy_account_metas(&curve, &addresses, &distinct_config(), &user()).unwrap();
            assert_metas_match_the_idl("buy_exact_in", &BUY_ACCOUNTS, &metas, &[8, 11]);
        }
        // Due and unset: no list, since the program would refuse it.
        let unset = CurveUsdc {
            creator_usdc_account: Pubkey::default(),
            ..revenue
        };
        assert_eq!(
            buy_account_metas(&unset, &addresses, &distinct_config(), &user()),
            Err(Refusal::CreatorUsdcAccountUnset)
        );
    }

    #[test]
    fn a_sale_names_the_program_s_eleven_accounts() {
        let (curve, addresses) = revenue();
        let metas = sell_account_metas(&curve, &addresses, &distinct_config(), &user());
        assert_metas_match_the_idl("sell_exact_in", &SELL_ACCOUNTS, &metas, &[]);
        let writable: Vec<usize> = (0..11).filter(|&i| metas[i].is_writable).collect();
        assert_eq!(writable, [1, 2, 4, 5, 6, 7]);
        assert_eq!(metas[4].pubkey, addresses.curve);
        assert_eq!(metas[8].pubkey, distinct_config().usdc_mint);
    }

    /// The seeds the IDL records for the PDAs a swap names, against this SDK's derivations.
    #[test]
    fn the_pdas_are_the_ones_the_idl_records() {
        let idl = idl();
        let program = idl["address"].as_str().unwrap();
        assert_eq!(program, PROGRAM_ID.to_string());
        for name in ["buy_exact_in", "sell_exact_in"] {
            let ix = instruction(&idl, name);
            let account = |n: &str| {
                ix["accounts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|a| a["name"] == n)
                    .unwrap()
                    .clone()
            };
            let seeds = |n: &str| account(n)["pda"]["seeds"].as_array().unwrap().clone();
            assert_eq!(
                bytes(&seeds("usdc_config")[0]["value"]),
                USDC_CONFIG_SEED,
                "{name}"
            );
            let curve = seeds("curve");
            assert_eq!(bytes(&curve[0]["value"]), CURVE_SEED, "{name}");
            assert_eq!(curve[1]["path"], "curve.mint", "{name}");
            let vault = seeds("usdc_vault");
            assert_eq!(bytes(&vault[0]["value"]), USDC_VAULT_SEED, "{name}");
            assert_eq!(vault[1]["path"], "curve.mint", "{name}");
            // The token vault: the associated-token PDA of [curve, token program, token_mint].
            let token_vault = account("token_vault");
            let ata = token_vault["pda"]["seeds"].as_array().unwrap();
            assert_eq!(ata[0]["path"], "curve", "{name}");
            assert_eq!(
                bytes(&ata[1]["value"]),
                TOKEN_PROGRAM_ID.to_bytes(),
                "{name}"
            );
            assert_eq!(ata[2]["path"], "token_mint", "{name}");
            assert_eq!(
                bytes(&token_vault["pda"]["program"]["value"]),
                crate::layout::ASSOCIATED_TOKEN_PROGRAM_ID.to_bytes(),
                "{name}"
            );
        }
    }

    #[test]
    fn the_data_is_the_idl_s_discriminator_and_two_u64s() {
        let idl = idl();
        for (name, direction, args) in [
            (
                "buy_exact_in",
                Direction::Buy,
                ["usdc_in", "min_tokens_out"],
            ),
            (
                "sell_exact_in",
                Direction::Sell,
                ["tokens_in", "min_usdc_out"],
            ),
        ] {
            let ix = instruction(&idl, name);
            let discriminator = bytes(&ix["discriminator"]);
            let names: Vec<&str> = ix["args"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a["name"].as_str().unwrap())
                .collect();
            assert_eq!(names, args, "{name}");
            assert!(
                ix["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|a| a["type"] == "u64"),
                "{name}"
            );
            let data = instruction_data(direction, 0x0102_0304_0506_0708, u64::MAX - 1);
            assert_eq!(data.len(), 24);
            assert_eq!(&data[..8], discriminator.as_slice(), "{name}");
            assert_eq!(&data[8..16], &0x0102_0304_0506_0708u64.to_le_bytes());
            assert_eq!(&data[16..], &(u64::MAX - 1).to_le_bytes());
        }
    }

    #[test]
    fn the_placeholder_carries_the_direction_and_nothing_else_encodes() {
        for direction in [Direction::Buy, Direction::Sell] {
            let swap = placeholder(direction);
            assert_eq!(direction_of(&swap), Some(direction));
            assert_eq!(
                swap_instruction_data(&swap, 5, 7).unwrap(),
                instruction_data(direction, 5, 7)
            );
        }
        assert_eq!(
            placeholder(Direction::Buy),
            Swap::Placeholder {
                data: Some(vec![0])
            }
        );
        assert_eq!(
            placeholder(Direction::Sell),
            Swap::Placeholder {
                data: Some(vec![1])
            }
        );
        for other in [
            Swap::Placeholder { data: None },
            Swap::Placeholder {
                data: Some(vec![2]),
            },
            Swap::Placeholder {
                data: Some(vec![0, 0]),
            },
            Swap::TokenSwap,
        ] {
            assert_eq!(direction_of(&other), None);
            assert!(swap_instruction_data(&other, 1, 0).is_err());
        }
    }
}
