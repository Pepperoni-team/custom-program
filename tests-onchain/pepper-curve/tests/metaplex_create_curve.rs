//! Today's `create_curve` end to end, with the real Metaplex Token Metadata
//! program loaded from its mainnet fixture (docs/solana/PEPPER-CURVE-PROGRAM.md
//! §10 and §13; usdc-curve design §10.11, 2026-10-22). The other suites write
//! their curves into the bank, so until this file only localnet and devnet
//! reached Metaplex. Since the fee-wallet split (2026-10-24) the update
//! authority is `config.admin`, which the real Metaplex takes here, a key apart
//! from the fee wallet in this harness.

mod common;

use anchor_lang::solana_program::program_option::COption;
use anchor_lang::solana_program::program_pack::Pack;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::metadata::mpl_token_metadata::{
    self,
    accounts::Metadata,
    errors::MplTokenMetadataError,
    types::{Key, TokenStandard},
};
use anchor_spl::token::spl_token;
use common::*;
use pepper_curve::constants::*;
use pepper_curve::instructions::CreateCurveParams;
use pepper_curve::state::{CurvePhase, GlobalConfig};
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

const COMPUTE_BUDGET_ID: Pubkey = Pubkey::from_str_const("ComputeBudget111111111111111111111111111111");
const RENT_SYSVAR_ID: Pubkey = Pubkey::from_str_const("SysvarRent111111111111111111111111111111111");

/// The compute units the app asks for a launch (`CREATE_CU_LIMIT`,
/// `lib/solana/pepper-curve/launch.ts`).
const CREATE_CU_LIMIT: u32 = 500_000;

const NAME: &str = "Pepper Fixture Launch";
/// Ten bytes: Metaplex's own limit, tighter than the program's
/// `MAX_SYMBOL_LEN` (12).
const SYMBOL: &str = "PEPPERTEST";
const URI: &str = "https://pepper.fan/api/solana/metadata/peppertest";

/// An equity launch (no raise split): 80M tokens on the curve, 18M for the
/// pool, 2% vested with the `revenue` defaults.
const CURVE_SUPPLY: u64 = 80_000_000 * 1_000_000;
const MIGRATION_SUPPLY: u64 = 18_000_000 * 1_000_000;
const VESTING_SUPPLY: u64 = 2_000_000 * 1_000_000;

fn params(symbol: &str) -> CreateCurveParams {
    CreateCurveParams {
        creator_target_lamports: 0,
        cpmm_seed_lamports: MIN_CPMM_SEED_LAMPORTS,
        creator_split_bps: 0,
        fanex_split_bps: 0,
        creator_cap_lamports: 0,
        fanex_cap_lamports: 0,
        virtual_sol_reserves: 5 * SOL,
        virtual_token_reserves: TOKEN_TOTAL_SUPPLY,
        curve_token_supply: CURVE_SUPPLY,
        migration_token_supply: MIGRATION_SUPPLY,
        vesting_token_supply: VESTING_SUPPLY,
        vesting_cliff_seconds: VESTING_CLIFF_SECONDS,
        vesting_duration_seconds: VESTING_DURATION_SECONDS,
        name: NAME.to_string(),
        symbol: symbol.to_string(),
        uri: URI.to_string(),
    }
}

fn compute_unit_limit(units: u32) -> Instruction {
    let mut data = vec![2u8];
    data.extend_from_slice(&units.to_le_bytes());
    Instruction { program_id: COMPUTE_BUDGET_ID, accounts: vec![], data }
}

/// `create_curve` by `launcher` for itself (creator mode), the update
/// authority the admin the config names (the fee-wallet split, 2026-10-24).
fn create_curve_ix(launcher: &Pubkey, mint: &Pubkey, update_authority: &Pubkey, symbol: &str) -> Instruction {
    let curve = curve_pda(mint);
    ix(
        pepper_curve::accounts::CreateCurve {
            launcher: *launcher,
            creator: *launcher,
            config: config_pda(),
            mint: *mint,
            curve,
            sol_vault: pda(&[SOL_VAULT_SEED, mint.as_ref()]),
            vesting: pda(&[VESTING_SEED, mint.as_ref()]),
            token_vault: ata(&curve, mint),
            vesting_vault: pda(&[VESTING_VAULT_SEED, mint.as_ref()]),
            metadata: Metadata::find_pda(mint).0,
            metadata_update_authority: *update_authority,
            token_program: spl_token::ID,
            token_metadata_program: mpl_token_metadata::ID,
            associated_token_program: anchor_spl::associated_token::ID,
            system_program: system_program::id(),
            rent: RENT_SYSVAR_ID,
        },
        pepper_curve::instruction::CreateCurve { params: params(symbol) },
    )
}

struct Launch {
    env: Env,
    launcher: Keypair,
    mint: Keypair,
    admin: Pubkey,
}

fn launch_env() -> Launch {
    let mut env = Env::new();
    env.load_metaplex();
    let config = env.fetch::<GlobalConfig>(&config_pda());
    assert_ne!(config.admin, config.fanex_fee_wallet, "the harness keeps the two keys apart");
    let launcher = env.creator.insecure_clone();
    Launch { env, launcher, mint: Keypair::new(), admin: config.admin }
}

impl Launch {
    /// Sends the launch as the app does (the compute limit first), and
    /// answers the compute units it used.
    fn send(&mut self, symbol: &str) -> Result<u64, solana_sdk::transaction::TransactionError> {
        let ixs = [
            compute_unit_limit(CREATE_CU_LIMIT),
            create_curve_ix(&self.launcher.pubkey(), &self.mint.pubkey(), &self.admin, symbol),
        ];
        let tx = Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.launcher.pubkey()),
            &[&self.launcher, &self.mint],
            self.env.svm.latest_blockhash(),
        );
        let result = self.env.svm.send_transaction(tx);
        self.env.svm.expire_blockhash();
        match result {
            Ok(meta) => Ok(meta.compute_units_consumed),
            Err(failed) => {
                println!("{}", failed.meta.pretty_logs());
                Err(failed.err)
            }
        }
    }
}

#[test]
fn create_curve_writes_the_metaplex_metadata_and_revokes_the_mint_authority() {
    let mut launch = launch_env();
    let units = launch.send(SYMBOL).expect("the launch lands");
    println!("create_curve with the Metaplex CPI: {units} compute units");
    assert!(units < CREATE_CU_LIMIT as u64, "{units} compute units, over the app's {CREATE_CU_LIMIT}");

    let mint = launch.mint.pubkey();
    let account = launch.env.svm.get_account(&Metadata::find_pda(&mint).0).expect("the metadata exists");
    assert_eq!(account.owner, mpl_token_metadata::ID);
    let metadata = Metadata::safe_deserialize(&account.data).unwrap();
    assert_eq!(metadata.key, Key::MetadataV1);
    assert_eq!(metadata.mint, mint);
    // Metaplex pads the strings with NULs to their maximum length.
    assert_eq!(metadata.name.len(), mpl_token_metadata::MAX_NAME_LENGTH);
    assert_eq!(metadata.symbol.len(), mpl_token_metadata::MAX_SYMBOL_LENGTH);
    assert_eq!(metadata.uri.len(), mpl_token_metadata::MAX_URI_LENGTH);
    assert_eq!(metadata.name.trim_end_matches('\0'), NAME);
    assert_eq!(metadata.symbol.trim_end_matches('\0'), SYMBOL);
    assert_eq!(metadata.uri.trim_end_matches('\0'), URI);
    // The program's admin (config.admin) keeps the right to fix them, never the
    // receive-only fee wallet (the fee-wallet split, 2026-10-24).
    assert_eq!(metadata.update_authority, launch.admin);
    assert!(metadata.is_mutable);
    assert_eq!(metadata.seller_fee_basis_points, 0);
    assert_eq!(metadata.creators, None);
    assert_eq!(metadata.token_standard, Some(TokenStandard::Fungible));

    // The supply is fixed for good: no mint authority, no freeze authority.
    let mint_state = spl_token::state::Mint::unpack(&launch.env.svm.get_account(&mint).unwrap().data).unwrap();
    assert_eq!(mint_state.mint_authority, COption::None);
    assert_eq!(mint_state.freeze_authority, COption::None);
    assert_eq!(mint_state.supply, TOKEN_TOTAL_SUPPLY);
    assert_eq!(mint_state.decimals, TOKEN_DECIMALS);

    let curve = launch.env.fetch::<pepper_curve::state::Curve>(&curve_pda(&mint));
    assert_eq!(curve.phase, CurvePhase::Bonding);
    assert_eq!(curve.creator, launch.launcher.pubkey());
    let balance = |address: &Pubkey| {
        spl_token::state::Account::unpack(&launch.env.svm.get_account(address).unwrap().data).unwrap().amount
    };
    assert_eq!(balance(&ata(&curve_pda(&mint), &mint)), CURVE_SUPPLY + MIGRATION_SUPPLY);
    assert_eq!(balance(&pda(&[VESTING_VAULT_SEED, mint.as_ref()])), VESTING_SUPPLY);
}

/// The program takes a symbol of up to 12 bytes; Metaplex refuses one over
/// 10, and the launch is undone whole: no curve, no mint. The create route
/// caps the symbol at 10 for this reason (`app/api/solana/curve/create/route.ts`).
#[test]
fn metaplex_refuses_an_eleven_byte_symbol_the_program_accepts() {
    let eleven = "PEPPERTEST1";
    assert_eq!(eleven.len(), mpl_token_metadata::MAX_SYMBOL_LENGTH + 1);
    assert!(eleven.len() <= MAX_SYMBOL_LEN);

    let mut launch = launch_env();
    let refused = launch.send(eleven).map(|_| ());
    expect_err(refused, MplTokenMetadataError::SymbolTooLong as u32);
    let mint = launch.mint.pubkey();
    assert!(!launch.env.exists(&curve_pda(&mint)));
    assert!(!launch.env.exists(&mint));
}
