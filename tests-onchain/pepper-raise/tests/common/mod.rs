#![allow(dead_code)]

//! The LiteSVM harness of the raise, run against the built `pepper_curve.so` it lives in since N+3 (`PEPPER_CURVE_SO`).
//! Since the internal audit of 2026-10-29 the program is loaded under the upgradeable loader, as a
//! cluster runs it, with a ProgramData that records `Env::admin` as its upgrade authority: what
//! `initialize_raise_config` reads (`load_upgradeable_program`, ported from the `pepper_dividends` harness).

use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use pepper_curve::raise::constants::*;
use litesvm::LiteSVM;
use solana_sdk::{
    account::Account,
    clock::Clock,
    instruction::{Instruction, InstructionError},
    program_pack::Pack,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_instruction, system_program,
    transaction::{Transaction, TransactionError},
};

pub const Q: u64 = 1_000_000;
pub const USD: u64 = 1_000_000;
pub const DAY: i64 = 86_400;
/// 2026-10-01 00:00:00 UTC.
pub const OCT_1_2026: i64 = 1_790_812_800;
/// The pool fee every test raise trades at (1%, the devnet value).
pub const POOL_FEE_BPS: u16 = 100;

pub fn program_so() -> String {
    std::env::var("PEPPER_CURVE_SO").unwrap_or_else(|_| "../../target/deploy/pepper_curve.so".to_string())
}

/// The custom error code of a `FanexError`.
pub fn code(e: pepper_curve::errors::FanexError) -> u32 {
    anchor_lang::error::ERROR_CODE_OFFSET + e as u32
}

/// The number of one of Anchor's own errors (`ConstraintSeeds` 2006, `AccountNotSystemOwned` 3011,
/// `InstructionFallbackNotFound` 101, …), which a test pins like the program's.
pub fn anchor_code(e: anchor_lang::error::ErrorCode) -> u32 {
    e.into()
}

/// The system program's `AccountAlreadyInUse`: an `init` of an account that exists.
pub const SYSTEM_ACCOUNT_ALREADY_IN_USE: u32 = 0;

// ── The upgradeable loader (as the pepper-dividends harness, c11506fa2) ──────

/// A ProgramData's header, before the ELF: `UpgradeableLoaderState::size_of_programdata_metadata`.
pub const PROGRAM_DATA_HEADER: usize = 45;

/// The ProgramData address of `program_id`: the upgradeable loader's PDA of the program id alone.
pub fn program_data_pda(program_id: &Pubkey) -> Pubkey {
    bpf_loader_upgradeable::get_program_data_address(program_id)
}

/// A ProgramData's bytes as the loader writes them: the bincode of
/// `UpgradeableLoaderState::ProgramData` (the tag 3 as a u32, `slot` as a u64, then the authority
/// as an `Option<Pubkey>`: one byte, then its 32, left zero when there is none, so the header is 45
/// bytes either way), then the ELF.
pub fn program_data_bytes(slot: u64, upgrade_authority: Option<Pubkey>, elf: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(PROGRAM_DATA_HEADER + elf.len());
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&slot.to_le_bytes());
    match upgrade_authority {
        Some(authority) => {
            data.push(1);
            data.extend_from_slice(authority.as_ref());
        }
        None => data.extend_from_slice(&[0u8; 33]),
    }
    assert_eq!(data.len(), PROGRAM_DATA_HEADER);
    data.extend_from_slice(elf);
    data
}

/// Loads `elf` at `program_id` under the upgradeable loader, as a deploy leaves it, replacing
/// whatever ran there: the ProgramData first, since LiteSVM compiles the ELF from it when the
/// Program account arrives, then the Program account (the tag 2 as a u32 and the ProgramData's
/// address), executable. Returns the ProgramData's address.
pub fn load_upgradeable_program(svm: &mut LiteSVM, program_id: Pubkey, elf: &[u8], upgrade_authority: Option<Pubkey>) -> Pubkey {
    let loader = bpf_loader_upgradeable::id();
    let program_data = program_data_pda(&program_id);
    let slot = svm.get_sysvar::<Clock>().slot;
    let data = program_data_bytes(slot, upgrade_authority, elf);
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(program_data, Account { lamports, data, owner: loader, executable: false, rent_epoch: 0 }).unwrap();
    let mut program = 2u32.to_le_bytes().to_vec();
    program.extend_from_slice(program_data.as_ref());
    let lamports = svm.minimum_balance_for_rent_exemption(program.len());
    svm.set_account(program_id, Account { lamports, data: program, owner: loader, executable: true, rent_epoch: 0 })
        .expect("the program loads from its ProgramData");
    program_data
}

/// How `Env` loads `pepper_curve`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramLoad {
    /// Under the upgradeable loader, `Env::admin` its recorded upgrade authority (`Env::new`).
    UpgradeableByAdmin,
    /// Under the upgradeable loader, immutable: a header that records no authority.
    Immutable,
    /// Through LiteSVM's `add_program`: no ProgramData at all.
    WithoutProgramData,
}

pub fn expect_err(result: Result<(), TransactionError>, expected: u32) {
    match result {
        Err(TransactionError::InstructionError(_, InstructionError::Custom(c))) => {
            assert_eq!(c, expected, "custom error {c}, expected {expected}")
        }
        other => panic!("expected custom error {expected}, got {other:?}"),
    }
}

pub fn ix<A: ToAccountMetas, D: InstructionData>(accounts: A, data: D) -> Instruction {
    Instruction { program_id: pepper_curve::ID, accounts: accounts.to_account_metas(None), data: data.data() }
}

pub fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &pepper_curve::ID).0
}
pub fn config_pda() -> Pubkey {
    pda(&[RAISE_CONFIG_SEED])
}
pub fn raise_pda(creator: &Pubkey, id: u64) -> Pubkey {
    pda(&[RAISE_SEED, creator.as_ref(), &id.to_le_bytes()])
}
pub fn vault(seed: &[u8], raise: &Pubkey) -> Pubkey {
    pda(&[seed, raise.as_ref()])
}
pub fn position_pda(raise: &Pubkey, owner: &Pubkey) -> Pubkey {
    pda(&[POSITION_SEED, raise.as_ref(), owner.as_ref()])
}
pub fn month_pda(raise: &Pubkey, month: u32) -> Pubkey {
    pda(&[MONTH_SEED, raise.as_ref(), &month.to_le_bytes()])
}
/// The pool's position is owned by the raise PDA itself.
pub fn pool_position_pda(raise: &Pubkey) -> Pubkey {
    position_pda(raise, raise)
}

pub struct Env {
    pub svm: LiteSVM,
    /// Deploys (the program's recorded upgrade authority), administers and sponsors (pays fees and
    /// rent), like the Fanex raise sponsor: one key, as the upgrade Squad's vault is both the upgrade
    /// authority and the admin on mainnet.
    pub admin: Keypair,
    pub attester: Keypair,
    pub fee_wallet: Keypair,
    pub mint_authority: Keypair,
    pub mint: Pubkey,
    pub fee_usdc: Pubkey,
}

impl Env {
    pub fn new() -> Self {
        let mut env = Self::without_config(ProgramLoad::UpgradeableByAdmin);
        env.init_config(5 * USD, POOL_FEE_BPS);
        env
    }

    /// What a deploy leaves before `initialize_config`: the program loaded as `load` says, the
    /// clock, the test USDC and the fee wallet's account, and no `RaiseConfig` yet.
    pub fn without_config(load: ProgramLoad) -> Self {
        let mut svm = LiteSVM::new();
        let admin = Keypair::new();
        let path = program_so();
        let elf = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("{path}: {e} (build first: cargo-build-sbf --manifest-path programs/pepper_curve/Cargo.toml)"));
        match load {
            ProgramLoad::UpgradeableByAdmin => {
                load_upgradeable_program(&mut svm, pepper_curve::ID, &elf, Some(admin.pubkey()));
            }
            ProgramLoad::Immutable => {
                load_upgradeable_program(&mut svm, pepper_curve::ID, &elf, None);
            }
            ProgramLoad::WithoutProgramData => svm.add_program(pepper_curve::ID, &elf),
        }
        svm.airdrop(&admin.pubkey(), 1_000_000_000_000).unwrap();
        let mut env = Self {
            svm,
            admin,
            attester: Keypair::new(),
            fee_wallet: Keypair::new(),
            mint_authority: Keypair::new(),
            mint: Pubkey::default(),
            fee_usdc: Pubkey::default(),
        };
        env.set_time(OCT_1_2026);
        env.mint = env.create_mint();
        let fee_owner = env.fee_wallet.pubkey();
        env.fee_usdc = env.token_account(&fee_owner);
        env
    }

    /// Signs with the admin (fee payer) plus `extra`, then expires the blockhash
    /// so an identical transaction can be sent again.
    pub fn send(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let admin = self.admin.insecure_clone();
        self.send_as(&admin, ixs, extra)
    }

    /// Like `send`, but `payer` pays the fee and signs first: a transaction the
    /// sponsor neither pays for nor signs, unless it is in `extra`.
    pub fn send_as(&mut self, payer: &Keypair, ixs: &[Instruction], extra: &[&Keypair]) -> Result<(), TransactionError> {
        let mut signers: Vec<&Keypair> = vec![payer];
        signers.extend_from_slice(extra);
        let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|_| ()).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    pub fn set_time(&mut self, unix_timestamp: i64) {
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    pub fn now(&self) -> i64 {
        self.svm.get_sysvar::<Clock>().unix_timestamp
    }

    fn create_mint(&mut self) -> Pubkey {
        let mint = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(spl_token::state::Mint::LEN);
        let ixs = [
            system_instruction::create_account(
                &self.admin.pubkey(),
                &mint.pubkey(),
                rent,
                spl_token::state::Mint::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_mint2(&spl_token::id(), &mint.pubkey(), &self.mint_authority.pubkey(), None, 6)
                .unwrap(),
        ];
        self.send(&ixs, &[&mint]).unwrap();
        mint.pubkey()
    }

    /// Creates `owner`'s USDC associated token account.
    pub fn token_account(&mut self, owner: &Pubkey) -> Pubkey {
        let ata = spl_associated_token_account::get_associated_token_address(owner, &self.mint);
        let create = spl_associated_token_account::instruction::create_associated_token_account(
            &self.admin.pubkey(),
            owner,
            &self.mint,
            &spl_token::id(),
        );
        self.send(&[create], &[]).unwrap();
        ata
    }

    /// A USDC account of `owner` at a fresh address, not its associated one: what anyone can open
    /// for anyone, since `InitializeAccount3` needs no signature of the owner.
    pub fn token_account_elsewhere(&mut self, owner: &Pubkey) -> Pubkey {
        let account = Keypair::new();
        let rent = self.svm.minimum_balance_for_rent_exemption(spl_token::state::Account::LEN);
        let ixs = [
            system_instruction::create_account(
                &self.admin.pubkey(),
                &account.pubkey(),
                rent,
                spl_token::state::Account::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_account3(&spl_token::id(), &account.pubkey(), &self.mint, owner).unwrap(),
        ];
        self.send(&ixs, &[&account]).unwrap();
        account.pubkey()
    }

    pub fn mint_to(&mut self, to: &Pubkey, amount: u64) {
        let mint_to = spl_token::instruction::mint_to(&spl_token::id(), &self.mint, to, &self.mint_authority.pubkey(), &[], amount)
            .unwrap();
        let authority = self.mint_authority.insecure_clone();
        self.send(&[mint_to], &[&authority]).unwrap();
    }

    /// A funded wallet with `usdc` in its token account.
    pub fn user(&mut self, usdc: u64) -> (Keypair, Pubkey) {
        let wallet = Keypair::new();
        self.svm.airdrop(&wallet.pubkey(), 1_000_000_000).unwrap();
        let ata = self.token_account(&wallet.pubkey());
        if usdc > 0 {
            self.mint_to(&ata, usdc);
        }
        (wallet, ata)
    }

    pub fn balance(&self, token_account: &Pubkey) -> u64 {
        let account = self.svm.get_account(token_account).expect("token account exists");
        spl_token::state::Account::unpack(&account.data).unwrap().amount
    }

    pub fn fetch<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let account = self.svm.get_account(address).expect("account exists");
        T::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    pub fn exists(&self, address: &Pubkey) -> bool {
        self.svm.get_account(address).map(|a| a.lamports > 0).unwrap_or(false)
    }

    /// An `initialize_config` paid by `payer`, signed as the upgrade authority by `upgrade_authority`,
    /// naming `admin`, with the program's own ProgramData and the harness's attester, fee wallet and mint.
    pub fn init_config_ix(&self, payer: &Pubkey, upgrade_authority: &Pubkey, admin: &Pubkey, launch_fee: u64, pool_fee_bps: u16) -> Instruction {
        self.init_config_ix_with(payer, upgrade_authority, &program_data_pda(&pepper_curve::ID), admin, launch_fee, pool_fee_bps)
    }

    /// `init_config_ix` with any account as the ProgramData.
    pub fn init_config_ix_with(
        &self,
        payer: &Pubkey,
        upgrade_authority: &Pubkey,
        program_data: &Pubkey,
        admin: &Pubkey,
        launch_fee: u64,
        pool_fee_bps: u16,
    ) -> Instruction {
        ix(
            pepper_curve::accounts::InitializeRaiseConfig {
                payer: *payer,
                config: config_pda(),
                system_program: system_program::id(),
                upgrade_authority: *upgrade_authority,
                program_data: *program_data,
                admin: *admin,
            },
            pepper_curve::instruction::InitializeRaiseConfig {
                attester: self.attester.pubkey(),
                fee_wallet: self.fee_wallet.pubkey(),
                usdc_mint: self.mint,
                launch_fee,
                pool_fee_bps,
            },
        )
    }

    /// The config as the deploy session makes it: `Env::admin` pays, signs as the upgrade authority
    /// and is named the admin.
    pub fn init_config(&mut self, launch_fee: u64, pool_fee_bps: u16) {
        let admin = self.admin.pubkey();
        let init = self.init_config_ix(&admin, &admin, &admin, launch_fee, pool_fee_bps);
        self.send(&[init], &[]).unwrap();
    }

    /// The launch fee the config holds: what the harness builds a creator's `create_raise` on.
    pub fn launch_fee(&self) -> u64 {
        self.fetch::<pepper_curve::raise::state::RaiseConfig>(&config_pda()).launch_fee
    }

    pub fn set_paused(&mut self, paused: bool) -> Result<(), TransactionError> {
        let pause = ix(
            pepper_curve::accounts::UpdateRaiseConfig { config: config_pda(), admin: self.admin.pubkey() },
            pepper_curve::instruction::SetRaisePaused { paused },
        );
        self.send(&[pause], &[])
    }

    /// The curves' `UsdcConfig` written into the bank, as `initialize_usdc_config` stores it, naming
    /// `pauser`: all `usdc_pause_raise` reads of it (2026-11-05). The other keys are the admin's or
    /// all-zero; nothing in this suite reads them.
    pub fn put_usdc_config(&mut self, pauser: &Pubkey) {
        use anchor_lang::{AccountSerialize, Space};
        use pepper_curve::state::UsdcConfig;
        let (address, bump) = Pubkey::find_program_address(&[pepper_curve::constants::USDC_CONFIG_SEED], &pepper_curve::ID);
        let admin = self.admin.pubkey();
        let config = UsdcConfig {
            version: 1,
            bump,
            paused: false,
            admin,
            pauser: *pauser,
            migration_authority: Pubkey::default(),
            usdc_mint: self.mint,
            fee_usdc_account: self.fee_usdc,
            lp_fee_owner: admin,
            raydium_cpmm_program: Pubkey::default(),
            raydium_amm_config: Pubkey::default(),
            raydium_create_pool_fee: Pubkey::default(),
            raydium_lock_program: Pubkey::default(),
            raydium_lock_authority: Pubkey::default(),
            launcher: admin,
            claim_approver: Pubkey::default(),
            reserved: [0; 64],
        };
        let mut data = Vec::with_capacity(8 + UsdcConfig::INIT_SPACE);
        config.try_serialize(&mut data).unwrap();
        data.resize(8 + UsdcConfig::INIT_SPACE, 0);
        let lamports = self.svm.minimum_balance_for_rent_exemption(data.len());
        self.svm
            .set_account(address, Account { lamports, data, owner: pepper_curve::ID, executable: false, rent_epoch: 0 })
            .unwrap();
    }

    /// `usdc_pause_raise` signed by `pauser`, the admin paying the fee.
    pub fn usdc_pause_raise(&mut self, pauser: &Keypair) -> Result<(), TransactionError> {
        let pause = ix(
            pepper_curve::accounts::UsdcPauseRaise {
                pauser: pauser.pubkey(),
                usdc_config: pda(&[pepper_curve::constants::USDC_CONFIG_SEED]),
                raise_config: config_pda(),
            },
            pepper_curve::instruction::UsdcPauseRaise {},
        );
        self.send(&[pause], &[pauser])
    }
}

use pepper_curve::raise::{RaiseParams, TierInput};

/// One tier of 1,000 quotas at USD 1: fans pay at most USD 1,000, the creator's
/// part is 909.090909 USD, the minimum is 25% of it, 24-month term, 21 days.
pub fn small_params(id: u64, now: i64) -> RaiseParams {
    RaiseParams {
        raise_id: id,
        tier_count: 1,
        tiers: [TierInput { units: 1_000 * Q, price: 1_000_000 }, TierInput::default(), TierInput::default()],
        min_creator_part: 227_272_728,
        share_bps: 10_000,
        term_months: 24,
        deadline: now + 21 * DAY,
    }
}

impl Env {
    pub fn create_raise_as(
        &mut self,
        creator: &Keypair,
        creator_usdc: &Pubkey,
        params: RaiseParams,
        attester: &Keypair,
    ) -> Result<Pubkey, TransactionError> {
        let max_launch_fee = self.launch_fee();
        self.create_raise_capped(creator, creator_usdc, params, attester, max_launch_fee)
    }

    /// A `create_raise` whose creator agrees to pay at most `max_launch_fee`.
    pub fn create_raise_capped(
        &mut self,
        creator: &Keypair,
        creator_usdc: &Pubkey,
        params: RaiseParams,
        attester: &Keypair,
        max_launch_fee: u64,
    ) -> Result<Pubkey, TransactionError> {
        let raise = raise_pda(&creator.pubkey(), params.raise_id);
        let create = ix(
            pepper_curve::accounts::CreateRaise {
                payer: self.admin.pubkey(),
                creator: creator.pubkey(),
                attester: attester.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise,
                escrow: vault(ESCROW_SEED, &raise),
                guarantee_vault: vault(GUARANTEE_SEED, &raise),
                pool_vault: vault(POOL_SEED, &raise),
                payouts_vault: vault(PAYOUTS_SEED, &raise),
                creator_usdc: *creator_usdc,
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::CreateRaise { params, max_launch_fee },
        );
        self.send(&[create], &[creator, attester]).map(|_| raise)
    }

    pub fn create_raise(&mut self, creator: &Keypair, creator_usdc: &Pubkey, params: RaiseParams) -> Result<Pubkey, TransactionError> {
        let attester = self.attester.insecure_clone();
        self.create_raise_as(creator, creator_usdc, params, &attester)
    }

    /// A creator with USD 10 who launches `small_params(id)`: (creator, creator_usdc, raise).
    pub fn launch(&mut self, id: u64) -> (Keypair, Pubkey, Pubkey) {
        let (creator, creator_usdc) = self.user(10 * USD);
        let now = self.now();
        let raise = self.create_raise(&creator, &creator_usdc, small_params(id, now)).unwrap();
        (creator, creator_usdc, raise)
    }

    pub fn buy_as(
        &mut self,
        raise: &Pubkey,
        buyer: &Keypair,
        buyer_usdc: &Pubkey,
        max_cost: u64,
        min_units: u64,
        attester: &Keypair,
    ) -> Result<(), TransactionError> {
        let buy = ix(
            pepper_curve::accounts::BuyQuotas {
                payer: self.admin.pubkey(),
                buyer: buyer.pubkey(),
                attester: attester.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                position: position_pda(raise, &buyer.pubkey()),
                buyer_usdc: *buyer_usdc,
                escrow: vault(ESCROW_SEED, raise),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::BuyQuotas { max_cost, min_units },
        );
        self.send(&[buy], &[buyer, attester])
    }

    pub fn buy(&mut self, raise: &Pubkey, buyer: &Keypair, buyer_usdc: &Pubkey, max_cost: u64) -> Result<(), TransactionError> {
        let attester = self.attester.insecure_clone();
        self.buy_as(raise, buyer, buyer_usdc, max_cost, 0, &attester)
    }

    /// Ten buyers of 100 quotas each (the per-wallet limit) sell out `small_params`.
    pub fn sell_out(&mut self, raise: &Pubkey) -> Vec<(Keypair, Pubkey)> {
        let mut buyers = Vec::new();
        for _ in 0..10 {
            let (buyer, buyer_usdc) = self.user(100 * USD);
            self.buy(raise, &buyer, &buyer_usdc, 100 * USD).unwrap();
            buyers.push((buyer, buyer_usdc));
        }
        buyers
    }

    pub fn cancel(&mut self, creator: &Keypair, raise: &Pubkey) -> Result<(), TransactionError> {
        let cancel = ix(
            pepper_curve::accounts::CancelRaise { creator: creator.pubkey(), raise: *raise },
            pepper_curve::instruction::CancelRaise {},
        );
        self.send(&[cancel], &[creator])
    }
}

impl Env {
    pub fn finalize(&mut self, raise: &Pubkey, creator_usdc: &Pubkey) -> Result<(), TransactionError> {
        let finalize = ix(
            pepper_curve::accounts::Finalize {
                payer: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                pool_position: pool_position_pda(raise),
                escrow: vault(ESCROW_SEED, raise),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                pool_vault: vault(POOL_SEED, raise),
                creator_usdc: *creator_usdc,
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::Finalize {},
        );
        self.send(&[finalize], &[])
    }

    /// Refunds `owner`'s position into `destination` (which must be the owner's).
    pub fn refund(&mut self, raise: &Pubkey, owner: &Pubkey, destination: &Pubkey) -> Result<(), TransactionError> {
        let position = position_pda(raise, owner);
        let rent_payer = self.fetch::<pepper_curve::raise::state::Position>(&position).rent_payer;
        let refund = ix(
            pepper_curve::accounts::Refund {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                position,
                rent_payer,
                owner_usdc: *destination,
                escrow: vault(ESCROW_SEED, raise),
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::Refund {},
        );
        self.send(&[refund], &[])
    }

    /// Launch + sell out + finalize: (creator, creator_usdc, raise, buyers).
    pub fn active_raise(&mut self, id: u64) -> (Keypair, Pubkey, Pubkey, Vec<(Keypair, Pubkey)>) {
        let (creator, creator_usdc, raise) = self.launch(id);
        let buyers = self.sell_out(&raise);
        self.finalize(&raise, &creator_usdc).unwrap();
        (creator, creator_usdc, raise, buyers)
    }
}

pub const NOVEMBER_2026: u32 = 24_322;

impl Env {
    pub fn declare_as(&mut self, raise: &Pubkey, month: u32, due: u64, attester: &Keypair) -> Result<(), TransactionError> {
        let declare = ix(
            pepper_curve::accounts::DeclareMonth {
                payer: self.admin.pubkey(),
                attester: attester.pubkey(),
                config: config_pda(),
                raise: *raise,
                month_account: month_pda(raise, month),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::DeclareMonth { month, due, estimated: false },
        );
        self.send(&[declare], &[attester])
    }

    pub fn declare(&mut self, raise: &Pubkey, month: u32, due: u64) -> Result<(), TransactionError> {
        let attester = self.attester.insecure_clone();
        self.declare_as(raise, month, due, &attester)
    }

    /// `fund_month`'s instruction: `amount` is the whole payment, the guarantee's
    /// refill and the month's part (2026-10-19), so the guarantee vault rides last.
    pub fn fund_month_ix(&self, creator: &Pubkey, creator_usdc: &Pubkey, raise: &Pubkey, month: u32, amount: u64) -> Instruction {
        ix(
            pepper_curve::accounts::FundMonth {
                creator: *creator,
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                month_account: month_pda(raise, month),
                creator_usdc: *creator_usdc,
                payouts_vault: vault(PAYOUTS_SEED, raise),
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
            },
            pepper_curve::instruction::FundMonth { amount },
        )
    }

    pub fn fund_month(&mut self, creator: &Keypair, creator_usdc: &Pubkey, raise: &Pubkey, month: u32, amount: u64) -> Result<(), TransactionError> {
        let fund = self.fund_month_ix(&creator.pubkey(), creator_usdc, raise, month, amount);
        self.send(&[fund], &[creator])
    }

    /// `fund_month`, handing back the logs its events are written to.
    pub fn fund_month_logged(
        &mut self,
        creator: &Keypair,
        creator_usdc: &Pubkey,
        raise: &Pubkey,
        month: u32,
        amount: u64,
    ) -> Result<Vec<String>, TransactionError> {
        let fund = self.fund_month_ix(&creator.pubkey(), creator_usdc, raise, month, amount);
        self.send_logged(&[fund], &[creator])
    }

    /// `fund_direct`'s instruction, the guarantee vault last, as in `fund_month_ix`.
    pub fn fund_direct_ix(&self, creator: &Pubkey, creator_usdc: &Pubkey, raise: &Pubkey, amount: u64) -> Instruction {
        ix(
            pepper_curve::accounts::FundDirect {
                creator: *creator,
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                creator_usdc: *creator_usdc,
                payouts_vault: vault(PAYOUTS_SEED, raise),
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
            },
            pepper_curve::instruction::FundDirect { amount },
        )
    }

    pub fn fund_direct(&mut self, creator: &Keypair, creator_usdc: &Pubkey, raise: &Pubkey, amount: u64) -> Result<(), TransactionError> {
        let fund = self.fund_direct_ix(&creator.pubkey(), creator_usdc, raise, amount);
        self.send(&[fund], &[creator])
    }

    /// `fund_direct`, handing back the logs its events are written to.
    pub fn fund_direct_logged(&mut self, creator: &Keypair, creator_usdc: &Pubkey, raise: &Pubkey, amount: u64) -> Result<Vec<String>, TransactionError> {
        let fund = self.fund_direct_ix(&creator.pubkey(), creator_usdc, raise, amount);
        self.send_logged(&[fund], &[creator])
    }

    pub fn settle(&mut self, raise: &Pubkey, month: u32) -> Result<(), TransactionError> {
        let settle = ix(
            pepper_curve::accounts::SettleMonth {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                month_account: month_pda(raise, month),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::SettleMonth {},
        );
        self.send(&[settle], &[])
    }

    pub fn replenish(&mut self, creator: &Keypair, creator_usdc: &Pubkey, raise: &Pubkey, amount: u64) -> Result<(), TransactionError> {
        let replenish = ix(
            pepper_curve::accounts::ReplenishGuarantee {
                creator: creator.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                creator_usdc: *creator_usdc,
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::ReplenishGuarantee { amount },
        );
        self.send(&[replenish], &[creator])
    }

    pub fn release(&mut self, raise: &Pubkey, creator_usdc: &Pubkey) -> Result<(), TransactionError> {
        let release = ix(
            pepper_curve::accounts::ReleaseGuarantee {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                creator_usdc: *creator_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::ReleaseGuarantee {},
        );
        self.send(&[release], &[])
    }
}

impl Env {
    pub fn claim(&mut self, raise: &Pubkey, owner: &Pubkey, destination: &Pubkey) -> Result<(), TransactionError> {
        let claim = ix(
            pepper_curve::accounts::Claim {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                position: position_pda(raise, owner),
                owner_usdc: *destination,
                payouts_vault: vault(PAYOUTS_SEED, raise),
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::Claim {},
        );
        self.send(&[claim], &[])
    }

    /// Declares November 2026 and has the creator pay `amount` for it.
    pub fn pay_november(&mut self, creator: &Keypair, creator_usdc: &Pubkey, raise: &Pubkey, amount: u64) {
        self.mint_to(creator_usdc, amount);
        self.set_time(pepper_curve::raise::calendar::declare_open_ts(NOVEMBER_2026));
        self.declare(raise, NOVEMBER_2026, amount).unwrap();
        self.fund_month(creator, creator_usdc, raise, NOVEMBER_2026, amount).unwrap();
    }
}

impl Env {
    pub fn sell_to_pool_as(
        &mut self,
        raise: &Pubkey,
        seller: &Keypair,
        seller_usdc: &Pubkey,
        units: u64,
        min_out: u64,
        attester: &Keypair,
    ) -> Result<(), TransactionError> {
        let sell = ix(
            pepper_curve::accounts::SellToPool {
                seller: seller.pubkey(),
                attester: attester.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                position: position_pda(raise, &seller.pubkey()),
                pool_position: pool_position_pda(raise),
                pool_vault: vault(POOL_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                seller_usdc: *seller_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::SellToPool { units, min_out },
        );
        self.send(&[sell], &[seller, attester])
    }

    /// Sells at any price above zero.
    pub fn sell_to_pool(&mut self, raise: &Pubkey, seller: &Keypair, seller_usdc: &Pubkey, units: u64) -> Result<(), TransactionError> {
        let attester = self.attester.insecure_clone();
        self.sell_to_pool_as(raise, seller, seller_usdc, units, 1, &attester)
    }

    pub fn buy_from_pool(
        &mut self,
        raise: &Pubkey,
        buyer: &Keypair,
        buyer_usdc: &Pubkey,
        units: u64,
        max_in: u64,
    ) -> Result<(), TransactionError> {
        let attester = self.attester.insecure_clone();
        let buy = ix(
            pepper_curve::accounts::BuyFromPool {
                payer: self.admin.pubkey(),
                buyer: buyer.pubkey(),
                attester: attester.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                pool_position: pool_position_pda(raise),
                buyer_position: position_pda(raise, &buyer.pubkey()),
                pool_vault: vault(POOL_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                buyer_usdc: *buyer_usdc,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            },
            pepper_curve::instruction::BuyFromPool { units, max_in },
        );
        self.send(&[buy], &[buyer, &attester])
    }

    pub fn sweep_pool(&mut self, raise: &Pubkey) -> Result<(), TransactionError> {
        let sweep = ix(
            pepper_curve::accounts::SweepPoolEarnings {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                pool_position: pool_position_pda(raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                pool_vault: vault(POOL_SEED, raise),
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::SweepPoolEarnings {},
        );
        self.send(&[sweep], &[])
    }
}

impl Env {
    pub fn close_raise(&mut self, raise: &Pubkey, creator_usdc: &Pubkey) -> Result<(), TransactionError> {
        let close = ix(
            pepper_curve::accounts::CloseRaise {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                pool_position: pool_position_pda(raise),
                pool_vault: vault(POOL_SEED, raise),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                creator_usdc: *creator_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::CloseRaise {},
        );
        self.send(&[close], &[])
    }

    /// Closes `owner`'s position; what it is still owed goes to `destination`,
    /// which must be the owner's (for the pool's own position, a vault of the raise).
    pub fn close_position(&mut self, raise: &Pubkey, owner: &Pubkey, destination: &Pubkey) -> Result<(), TransactionError> {
        let position = position_pda(raise, owner);
        let rent_payer = self.fetch::<pepper_curve::raise::state::Position>(&position).rent_payer;
        let close = ix(
            pepper_curve::accounts::ClosePosition {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                position,
                rent_payer,
                owner_usdc: *destination,
                payouts_vault: vault(PAYOUTS_SEED, raise),
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::ClosePosition {},
        );
        self.send(&[close], &[])
    }

    /// Declares every month of the term with nothing due, then moves the clock
    /// to the last month's due date.
    pub fn declare_zero_months_to_term_end(&mut self, raise: &Pubkey) {
        let state: pepper_curve::raise::state::Raise = self.fetch(raise);
        for month in state.first_month..=state.term_end_month {
            self.set_time(pepper_curve::raise::calendar::declare_open_ts(month));
            self.declare(raise, month, 0).unwrap();
        }
        self.set_time(pepper_curve::raise::calendar::due_ts(state.term_end_month));
    }
}

impl Env {
    /// Like `send`, but hands back the transaction's logs, where `emit!` writes its events.
    pub fn send_logged(&mut self, ixs: &[Instruction], extra: &[&Keypair]) -> Result<Vec<String>, TransactionError> {
        let mut signers: Vec<&Keypair> = vec![&self.admin];
        signers.extend_from_slice(extra);
        let tx = Transaction::new_signed_with_payer(ixs, Some(&self.admin.pubkey()), &signers, self.svm.latest_blockhash());
        let result = self.svm.send_transaction(tx).map(|meta| meta.logs).map_err(|failed| failed.err);
        self.svm.expire_blockhash();
        result
    }

    /// `close_raise`, returning the `RaiseClosed` event it emitted.
    pub fn close_raise_logged(
        &mut self,
        raise: &Pubkey,
        creator_usdc: &Pubkey,
    ) -> Result<pepper_curve::raise::events::RaiseClosed, TransactionError> {
        let close = ix(
            pepper_curve::accounts::CloseRaise {
                caller: self.admin.pubkey(),
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                pool_position: pool_position_pda(raise),
                pool_vault: vault(POOL_SEED, raise),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                creator_usdc: *creator_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::CloseRaise {},
        );
        self.send_logged(&[close], &[]).map(|logs| event(&logs))
    }
}

impl Env {
    /// An account's lamports; zero once it is closed.
    pub fn lamports(&self, address: &Pubkey) -> u64 {
        self.svm.get_account(address).map(|a| a.lamports).unwrap_or(0)
    }

    /// Makes `raise` read as one written before 2026-10-21: no rent payer, and
    /// counters that never counted. Rewrites the account through its own types.
    pub fn forget_rent_payer(&mut self, raise: &Pubkey) {
        let mut state: pepper_curve::raise::state::Raise = self.fetch(raise);
        state.rent_payer = Pubkey::default();
        state.open_positions = 0;
        state.live_months = 0;
        let mut account = self.svm.get_account(raise).expect("the raise exists");
        let mut data = Vec::new();
        anchor_lang::AccountSerialize::try_serialize(&state, &mut data).unwrap();
        account.data = data;
        self.svm.set_account(*raise, account).unwrap();
    }

    pub fn close_month_ix(&self, raise: &Pubkey, month: u32, rent_payer: &Pubkey) -> Instruction {
        self.close_month_ix_by(&self.admin.pubkey(), raise, month, rent_payer)
    }

    /// `close_month_ix` with `caller` as the instruction's signer.
    pub fn close_month_ix_by(&self, caller: &Pubkey, raise: &Pubkey, month: u32, rent_payer: &Pubkey) -> Instruction {
        ix(
            pepper_curve::accounts::CloseMonth {
                caller: *caller,
                raise: *raise,
                month_account: month_pda(raise, month),
                rent_payer: *rent_payer,
            },
            pepper_curve::instruction::CloseMonth {},
        )
    }

    /// `close_month`, its rent to the payer the raise records.
    pub fn close_month(&mut self, raise: &Pubkey, month: u32) -> Result<(), TransactionError> {
        let payer = self.fetch::<pepper_curve::raise::state::Raise>(raise).rent_payer;
        let close = self.close_month_ix(raise, month, &payer);
        self.send(&[close], &[])
    }

    pub fn close_month_logged(&mut self, raise: &Pubkey, month: u32) -> Result<pepper_curve::raise::events::MonthClosed, TransactionError> {
        let payer = self.fetch::<pepper_curve::raise::state::Raise>(raise).rent_payer;
        let close = self.close_month_ix(raise, month, &payer);
        self.send_logged(&[close], &[]).map(|logs| event(&logs))
    }

    pub fn close_raise_accounts_ix(&self, raise: &Pubkey, rent_payer: &Pubkey) -> Instruction {
        self.close_raise_accounts_ix_by(&self.admin.pubkey(), raise, rent_payer)
    }

    /// `close_raise_accounts_ix` with `caller` as the instruction's signer.
    pub fn close_raise_accounts_ix_by(&self, caller: &Pubkey, raise: &Pubkey, rent_payer: &Pubkey) -> Instruction {
        ix(
            pepper_curve::accounts::CloseRaiseAccounts {
                caller: *caller,
                config: config_pda(),
                usdc_mint: self.mint,
                raise: *raise,
                rent_payer: *rent_payer,
                escrow: vault(ESCROW_SEED, raise),
                guarantee_vault: vault(GUARANTEE_SEED, raise),
                pool_vault: vault(POOL_SEED, raise),
                payouts_vault: vault(PAYOUTS_SEED, raise),
                fee_usdc: self.fee_usdc,
                token_program: spl_token::id(),
            },
            pepper_curve::instruction::CloseRaiseAccounts {},
        )
    }

    /// `close_raise_accounts`, its rent to the payer the raise records.
    pub fn close_raise_accounts(&mut self, raise: &Pubkey) -> Result<(), TransactionError> {
        let payer = self.fetch::<pepper_curve::raise::state::Raise>(raise).rent_payer;
        let close = self.close_raise_accounts_ix(raise, &payer);
        self.send(&[close], &[])
    }

    pub fn close_raise_accounts_logged(&mut self, raise: &Pubkey) -> Result<pepper_curve::raise::events::RaiseAccountsClosed, TransactionError> {
        let payer = self.fetch::<pepper_curve::raise::state::Raise>(raise).rent_payer;
        let close = self.close_raise_accounts_ix(raise, &payer);
        self.send_logged(&[close], &[]).map(|logs| event(&logs))
    }
}

/// The first `E` among a transaction's logs: `emit!` writes each event as
/// "Program data: " and the base64 of its discriminator and its borsh bytes.
pub fn event<E: anchor_lang::Event>(logs: &[String]) -> E {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| STANDARD.decode(data).ok())
        .find_map(|bytes| {
            let body = bytes.strip_prefix(E::DISCRIMINATOR)?;
            Some(E::try_from_slice(body).expect("the event decodes"))
        })
        .expect("the event was emitted")
}

/// Every `E` among a transaction's logs, in the order they were emitted: an
/// empty list where `event` would panic, so a test can say an event is absent.
pub fn events_of<E: anchor_lang::Event>(logs: &[String]) -> Vec<E> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    logs.iter()
        .filter_map(|line| line.strip_prefix("Program data: "))
        .filter_map(|data| STANDARD.decode(data).ok())
        .filter_map(|bytes| {
            let body = bytes.strip_prefix(E::DISCRIMINATOR)?;
            Some(E::try_from_slice(body).expect("the event decodes"))
        })
        .collect()
}
