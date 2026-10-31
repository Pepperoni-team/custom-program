//! `sell_exact_in`, the USDC curve's sale, run against the built program
//! (docs/solana/PEPPER-CURVE-PROGRAM.md §5, §5.2; usdc-curve design §10.3.6).
//!
//! The handler calls the math crate's `quote_sell` and moves exactly what it returns: every sale of
//! the golden vectors is replayed through the program to the base unit (the source, the
//! destination, the fee account, `usdc_vault`, `token_vault`, the counters and `TradeUsdc` with the
//! gross amount). Then the shapes a router sends (a destination of any owner), the source bound to
//! account 0 (a stranger's or a delegate's refused), the reserve bounded by the counters whatever
//! the vault holds, the slippage on the net, each pause and the phase, the curve's own vaults and
//! mints, a frozen fee account, a buy and its sale through the program, and the refusals across
//! the two kinds of curve (3002 both ways). The curves are written into the bank
//! (`write_curve_usdc`) until `create_curve_usdc` exists.

mod common;

use anchor_lang::error::ErrorCode;
use anchor_lang::solana_program::program_option::COption;
use anchor_lang::AccountSerialize;
use anchor_spl::associated_token::get_associated_token_address as ata;
use anchor_spl::associated_token::spl_associated_token_account;
use anchor_spl::token::spl_token::{self, error::TokenError};
use common::*;
use pepper_curve::constants::*;
use pepper_curve::errors::FanexError;
use pepper_curve::events::{GraduationReadyUsdc, TradeUsdc};
use pepper_curve::state::{CurveUsdc, UsdcConfig};
use pepper_curve_math::{quote_buy, quote_sell, CurveState, SellQuote};
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::{Transaction, TransactionError},
};

/// The sale the behaviour tests start from: a curve after a 100 USDC buy, selling a million tokens.
const SALE: &str = "equity, after a 100 USDC buy: a sale of 1,000,000 tokens";
const SALE_OF_EVERYTHING: &str = "equity, after a 100 USDC buy: a sale of every token it sold";

/// One token, in base units.
const TOKEN: u64 = 1_000_000;

fn anchor(code: ErrorCode) -> u32 {
    code.into()
}

fn token(code: TokenError) -> u32 {
    code as u32
}

/// A wallet that sells: SOL for its fees, its associated token account of the curve's mint holding
/// `tokens` (written: a launch revoked the mint authority), and its USDC account, opened beforehand
/// (the sale opens nothing).
struct Seller {
    wallet: Keypair,
    source: Pubkey,
    destination: Pubkey,
}

fn seller(env: &mut Env, mint: &Pubkey, tokens: u64) -> Seller {
    let wallet = env.funded_wallet(SOL);
    let source = ata(&wallet.pubkey(), mint);
    env.put_token_account(source, *mint, wallet.pubkey(), tokens, COption::None);
    let destination = env.usdc_account(&wallet.pubkey());
    Seller { wallet, source, destination }
}

/// `sell_exact_in` as a router builds it (§10.5): the 11 accounts in the table's order.
fn sell_ix(
    env: &Env,
    curve: &CurveUsdc,
    authority: &Pubkey,
    source: &Pubkey,
    destination: &Pubkey,
    tokens_in: u64,
    min_usdc_out: u64,
) -> Instruction {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let address = curve_pda(&curve.mint);
    ix(
        pepper_curve::accounts::SellExactIn {
            user_transfer_authority: *authority,
            user_source_token: *source,
            user_destination_usdc: *destination,
            usdc_config: usdc_config_pda(),
            curve: address,
            usdc_vault: usdc_vault_pda(&curve.mint),
            token_vault: ata(&address, &curve.mint),
            fee_usdc_account: config.fee_usdc_account,
            usdc_mint: config.usdc_mint,
            token_mint: curve.mint,
            token_program: spl_token::ID,
        },
        pepper_curve::instruction::SellExactIn { tokens_in, min_usdc_out },
    )
}

/// `seller`'s own sale: it signs, sells from its own ATA and is paid into its own USDC account.
fn own_sale(env: &Env, curve: &CurveUsdc, seller: &Seller, tokens_in: u64) -> Instruction {
    sell_ix(env, curve, &seller.wallet.pubkey(), &seller.source, &seller.destination, tokens_in, 0)
}

/// `ix` with the account at `position` replaced. The list keeps its 11 positions.
fn with_account(mut ix: Instruction, position: usize, meta: AccountMeta) -> Instruction {
    assert_eq!(ix.accounts.len(), 11);
    ix.accounts[position] = meta;
    ix
}

fn send(env: &mut Env, signer: &Keypair, ix: Instruction) -> Result<Vec<String>, TransactionError> {
    let signer = signer.insecure_clone();
    env.send_logged_paid_by(&signer, &[ix], &[])
}

fn unit(result: Result<Vec<String>, TransactionError>) -> Result<(), TransactionError> {
    result.map(|_| ())
}

fn curve_now(env: &Env, mint: &Pubkey) -> CurveUsdc {
    env.fetch(&curve_pda(mint))
}

fn curve_bytes(env: &Env, mint: &Pubkey) -> Vec<u8> {
    env.svm.get_account(&curve_pda(mint)).unwrap().data
}

/// A written config and curve.
fn curve_with(state: &CurveState) -> (Env, CurveUsdc) {
    let mut env = Env::new();
    env.write_usdc_config();
    let curve = env.write_curve_usdc(state);
    (env, curve)
}

/// The curve of `SALE`, a seller holding a million tokens, and the pauser funded.
fn trading() -> (Env, CurveUsdc, Seller) {
    let v = vector(SALE);
    let (mut env, curve) = curve_with(&v.state);
    let s = seller(&mut env, &curve.mint, 3_000_000 * TOKEN);
    let pauser = env.pauser.pubkey();
    env.svm.airdrop(&pauser, SOL).unwrap();
    (env, curve, s)
}

fn sale_of(v: &Vector) -> (u64, SellQuote) {
    let (VectorOp::Sell { amount }, VectorOutcome::Sold(q)) = (v.op, v.outcome) else { panic!("{}: a sale", v.name) };
    (amount, q)
}

/// What every party of a sale holds, in base units: the seller's tokens and USDC, the fee account,
/// `usdc_vault` and `token_vault`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Holdings {
    source: u64,
    destination: u64,
    fee: u64,
    usdc_vault: u64,
    token_vault: u64,
}

fn holdings(env: &Env, curve: &CurveUsdc, source: &Pubkey, destination: &Pubkey) -> Holdings {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    Holdings {
        source: env.token_amount(source),
        destination: env.token_amount(destination),
        fee: env.token_amount(&config.fee_usdc_account),
        usdc_vault: env.token_amount(&usdc_vault_pda(&curve.mint)),
        token_vault: env.token_amount(&ata(&curve_pda(&curve.mint), &curve.mint)),
    }
}

/// The fields of a `TradeUsdc`, in its order, for one comparison.
fn trade_fields(t: &TradeUsdc) -> (Pubkey, Pubkey, Pubkey, bool, [u64; 7]) {
    (
        t.mint,
        t.authority,
        t.token_owner,
        t.is_buy,
        [t.usdc_amount, t.token_amount, t.creator_fee, t.fanex_fee, t.platform_fee, t.real_usdc_reserves, t.real_token_reserves],
    )
}

/// Asserts that a sale of `tokens_in` for `quote` landed exactly: each party moved by the quote's
/// figure, the curve holds the quote's counters and no other byte of it moved (the phase stays
/// Bonding: a sale never graduates), and the logs carry one `TradeUsdc` with the gross amount.
fn assert_sold(
    env: &Env,
    before: (&CurveUsdc, Holdings),
    after: Holdings,
    (tokens_in, quote): (u64, &SellQuote),
    logs: &[String],
    authority: &Pubkey,
) {
    let (start, held) = before;
    let what = format!("{tokens_in} tokens in: {quote:?}");
    assert_eq!(held.source - after.source, tokens_in, "{what}");
    assert_eq!(after.token_vault - held.token_vault, tokens_in, "{what}");
    assert_eq!(after.destination - held.destination, quote.proceeds, "the seller gets the net: {what}");
    assert_eq!(after.fee - held.fee, quote.platform_fee, "{what}");
    assert_eq!(held.usdc_vault - after.usdc_vault, quote.reserve_out, "the vault pays the gross: {what}");
    assert_eq!(quote.proceeds + quote.platform_fee, quote.reserve_out, "{what}");

    let curve = curve_now(env, &start.mint);
    assert_eq!(curve.swap_state(), quote.after, "{what}");
    let mut expected = start.clone();
    expected.set_swap_counters(&quote.after);
    let mut want = Vec::new();
    expected.try_serialize(&mut want).unwrap();
    assert_eq!(&curve_bytes(env, &start.mint)[..want.len()], &want[..], "no other byte of the curve moves: {what}");
    assert_eq!(curve.phase, PHASE_BONDING, "{what}");

    assert!(events_of::<GraduationReadyUsdc>(logs).is_empty(), "{what}");
    let trades = events_of::<TradeUsdc>(logs);
    assert_eq!(trades.len(), 1, "{what}");
    // `token_owner` is the source's owner, which the program holds to account 0.
    assert_eq!(
        trade_fields(&trades[0]),
        (
            start.mint,
            *authority,
            *authority,
            false,
            [
                quote.reserve_out,
                tokens_in,
                0,
                0,
                quote.platform_fee,
                quote.after.real_reserve,
                quote.after.real_token_reserves,
            ],
        ),
        "{what}"
    );
}

// ── The golden vectors, replayed (§10.9) ─────────────────────────────────────

#[test]
fn every_sell_vector_moves_exactly_what_the_crate_quotes() {
    let mut replayed = 0;
    let mut refused = 0;
    for v in vectors() {
        let VectorOp::Sell { amount } = v.op else { continue };
        let (mut env, curve) = curve_with(&v.state);
        assert_eq!(curve.swap_state(), v.state, "{}", v.name);
        // The vault holds the curve's real reserve, as a launch and its buys leave it.
        assert_eq!(env.token_amount(&usdc_vault_pda(&curve.mint)), v.state.real_reserve, "{}", v.name);
        let held_tokens = amount.max(TOKEN) + TOKEN;
        let s = seller(&mut env, &curve.mint, held_tokens);
        let sale = own_sale(&env, &curve, &s, amount);
        let held = holdings(&env, &curve, &s.source, &s.destination);
        let raw = curve_bytes(&env, &curve.mint);

        match v.outcome {
            VectorOutcome::Sold(q) => {
                let logs = send(&mut env, &s.wallet, sale).unwrap_or_else(|e| panic!("{}: {e:?}", v.name));
                let after = holdings(&env, &curve, &s.source, &s.destination);
                assert_sold(&env, (&curve, held), after, (amount, &q), &logs, &s.wallet.pubkey());
                assert_eq!(after.source, held_tokens - amount, "{}", v.name);
                replayed += 1;
            }
            VectorOutcome::Refused(e) => {
                expect_err(unit(send(&mut env, &s.wallet, sale)), code(FanexError::from(e)));
                assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held, "{}", v.name);
                assert_eq!(curve_bytes(&env, &curve.mint), raw, "{}", v.name);
                refused += 1;
            }
            VectorOutcome::Bought(_) => unreachable!("{}: a sale's outcome", v.name),
        }
    }
    assert_eq!((replayed, refused), (2, 3), "the 5 sales of curve-v1.json");
}

#[test]
fn the_sale_of_everything_the_curve_sold_leaves_one_base_unit_in_its_reserve() {
    // Floored twice, once per direction: the curve keeps the rounding, never the seller.
    let v = vector(SALE_OF_EVERYTHING);
    let (amount, q) = sale_of(&v);
    let (mut env, curve) = curve_with(&v.state);
    let s = seller(&mut env, &curve.mint, amount);
    let sale = own_sale(&env, &curve, &s, amount);
    send(&mut env, &s.wallet, sale).unwrap();
    assert_eq!(q.after.real_reserve, 1);
    assert_eq!(curve_now(&env, &curve.mint).real_usdc_reserves, 1);
    assert_eq!(env.token_amount(&usdc_vault_pda(&curve.mint)), 1);
    assert_eq!(env.token_amount(&s.source), 0);
}

// ── A router's shape (§10.5) ─────────────────────────────────────────────────

#[test]
fn a_router_sells_into_a_destination_of_any_owner_inside_the_launch_window_too() {
    let v = vector(SALE);
    let (amount, q) = sale_of(&v);
    let (mut env, curve) = curve_with(&v.state);
    // A sale has no launch rule: ten seconds after the launch, the proceeds go anywhere.
    env.set_time(curve.created_at + 10);
    // The router's program authority signs and sells its own tokens; the USDC goes to the user.
    let router = seller(&mut env, &curve.mint, 2 * amount);
    let user = Keypair::new();
    let user_usdc = env.usdc_account(&user.pubkey());

    let held = holdings(&env, &curve, &router.source, &user_usdc);
    let sale = sell_ix(&env, &curve, &router.wallet.pubkey(), &router.source, &user_usdc, amount, q.proceeds);
    let logs = send(&mut env, &router.wallet, sale).unwrap();
    let after = holdings(&env, &curve, &router.source, &user_usdc);
    // `authority` and `token_owner` are both the router's: neither is the user of a routed sale.
    assert_sold(&env, (&curve, held), after, (amount, &q), &logs, &router.wallet.pubkey());
    assert_eq!(env.token_amount(&router.destination), 0);

    // Any USDC account, an ATA or not: a router's intermediate one.
    let intermediate = Pubkey::new_unique();
    env.put_token_account(intermediate, env.usdc.mint, Pubkey::new_unique(), 0, COption::None);
    let now_curve = curve_now(&env, &curve.mint);
    let sale = sell_ix(&env, &now_curve, &router.wallet.pubkey(), &router.source, &intermediate, amount, 1);
    send(&mut env, &router.wallet, sale).unwrap();
    assert!(env.token_amount(&intermediate) > 0);
}

#[test]
fn a_wallet_with_no_sol_sells_while_another_key_pays_the_fee() {
    // The app's sponsored sale, and a router's program authority under its CPI: account 0 signs
    // read-only and pays nothing, the transaction's fee payer does.
    let v = vector(SALE);
    let (amount, q) = sale_of(&v);
    let (mut env, curve) = curve_with(&v.state);
    let wallet = Keypair::new();
    let source = ata(&wallet.pubkey(), &curve.mint);
    env.put_token_account(source, curve.mint, wallet.pubkey(), amount, COption::None);
    let destination = env.usdc_account(&wallet.pubkey());
    assert_eq!(env.lamports(&wallet.pubkey()), 0);

    let sale = sell_ix(&env, &curve, &wallet.pubkey(), &source, &destination, amount, q.proceeds);
    assert!(sale.accounts[0].is_signer && !sale.accounts[0].is_writable);
    let held = holdings(&env, &curve, &source, &destination);
    // `send_logged`: the admin pays the fee, the wallet only signs.
    let logs = env.send_logged(&[sale], &[&wallet]).unwrap();
    let after = holdings(&env, &curve, &source, &destination);
    assert_sold(&env, (&curve, held), after, (amount, &q), &logs, &wallet.pubkey());
    assert_eq!(env.lamports(&wallet.pubkey()), 0);
}

// ── The source, bound to account 0 ───────────────────────────────────────────

#[test]
fn the_source_must_belong_to_the_signer_and_a_delegate_is_refused() {
    let (mut env, curve, owner) = trading();
    let thief = seller(&mut env, &curve.mint, 0);
    let held = holdings(&env, &curve, &owner.source, &thief.destination);
    let raw = curve_bytes(&env, &curve.mint);
    let stolen = sell_ix(&env, &curve, &thief.wallet.pubkey(), &owner.source, &thief.destination, TOKEN, 0);
    expect_err(unit(send(&mut env, &thief.wallet, stolen.clone())), anchor(ErrorCode::ConstraintTokenOwner));

    // Approved by the owner, a delegate could move the tokens through SPL Token, never through the
    // sale: account 0 must own the source.
    let approve =
        spl_token::instruction::approve(&spl_token::ID, &owner.source, &thief.wallet.pubkey(), &owner.wallet.pubkey(), &[], TOKEN)
            .unwrap();
    let owner_wallet = owner.wallet.insecure_clone();
    env.send(&[approve], &[&owner_wallet]).unwrap();
    assert_eq!(env.token_account(&owner.source).delegated_amount, TOKEN);
    expect_err(unit(send(&mut env, &thief.wallet, stolen)), anchor(ErrorCode::ConstraintTokenOwner));

    assert_eq!(holdings(&env, &curve, &owner.source, &thief.destination), held);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
}

#[test]
fn a_seller_sells_no_more_than_it_holds() {
    let (mut env, curve, s) = trading();
    let holding = env.token_amount(&s.source);
    let held = holdings(&env, &curve, &s.source, &s.destination);
    let raw = curve_bytes(&env, &curve.mint);
    // The quote prices it, then SPL Token refuses the token leg, and nothing moves.
    assert!(quote_sell(&curve.swap_state(), holding + 1).is_ok());
    let over = own_sale(&env, &curve, &s, holding + 1);
    expect_err(unit(send(&mut env, &s.wallet, over)), token(TokenError::InsufficientFunds));
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
}

// ── The reserve: the counters, never the balance (§10.2) ─────────────────────

#[test]
fn a_sale_past_the_curve_s_real_reserve_is_refused_whatever_the_vault_holds() {
    let v = vector(SALE);
    let (amount, q) = sale_of(&v);
    let (mut env, curve) = curve_with(&v.state);
    // USDC sent straight to the vault: enough to pay any sale, and it counts for nothing.
    let usdc_vault = usdc_vault_pda(&curve.mint);
    env.mint_usdc(&usdc_vault, 10_000 * USDC);
    let s = seller(&mut env, &curve.mint, 100_000_000 * TOKEN);

    // The whole real supply sold back would pay more than the 99 USDC the counters hold.
    let everything = curve.real_token_reserves;
    let gross = quote_sell(&CurveState { real_reserve: u64::MAX, ..curve.swap_state() }, everything).unwrap().reserve_out;
    assert!(gross > curve.real_usdc_reserves && gross < env.token_amount(&usdc_vault));
    let held = holdings(&env, &curve, &s.source, &s.destination);
    let raw = curve_bytes(&env, &curve.mint);
    let over = own_sale(&env, &curve, &s, everything);
    expect_err(unit(send(&mut env, &s.wallet, over)), code(FanexError::InsufficientReserves));
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);

    // A sale within the counters is paid the vector's own quote, to the base unit: the donation
    // moved neither price nor reserve, and stays in the vault.
    let within = own_sale(&env, &curve, &s, amount);
    let logs = send(&mut env, &s.wallet, within).unwrap();
    let after = holdings(&env, &curve, &s.source, &s.destination);
    assert_sold(&env, (&curve, held), after, (amount, &q), &logs, &s.wallet.pubkey());
    assert_eq!(env.token_amount(&usdc_vault), 10_000 * USDC + v.state.real_reserve - q.reserve_out);
}

// ── The slippage, on the net ─────────────────────────────────────────────────

#[test]
fn the_slippage_is_checked_against_what_the_seller_receives() {
    let (mut env, curve, s) = trading();
    let q = quote_sell(&curve.swap_state(), 1_000_000 * TOKEN).unwrap();
    let held = holdings(&env, &curve, &s.source, &s.destination);
    // The gross, or one µUSDC more than the net: refused before anything moves.
    for min_usdc_out in [q.reserve_out, q.proceeds + 1] {
        let greedy = sell_ix(&env, &curve, &s.wallet.pubkey(), &s.source, &s.destination, 1_000_000 * TOKEN, min_usdc_out);
        expect_err(unit(send(&mut env, &s.wallet, greedy)), code(FanexError::SlippageExceeded));
    }
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);
    let exact = sell_ix(&env, &curve, &s.wallet.pubkey(), &s.source, &s.destination, 1_000_000 * TOKEN, q.proceeds);
    send(&mut env, &s.wallet, exact).unwrap();
    assert_eq!(env.token_amount(&s.destination), q.proceeds);
}

// ── The pauses and the phase (§10.3.3) ───────────────────────────────────────

fn pause_ix(signer: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetUsdcPaused { authority: *signer, usdc_config: usdc_config_pda() },
        pepper_curve::instruction::SetUsdcPaused { paused },
    )
}

fn curve_pause_ix(signer: &Pubkey, mint: &Pubkey, paused: bool) -> Instruction {
    ix(
        pepper_curve::accounts::SetCurveUsdcPaused { authority: *signer, usdc_config: usdc_config_pda(), curve: curve_pda(mint) },
        pepper_curve::instruction::SetCurveUsdcPaused { paused },
    )
}

#[test]
fn the_usdc_kill_switch_stops_the_sale_until_the_admin_unpauses() {
    let (mut env, curve, s) = trading();
    let pauser = env.pauser.insecure_clone();
    send(&mut env, &pauser, pause_ix(&pauser.pubkey(), true)).unwrap();
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    expect_err(unit(send(&mut env, &s.wallet, sale.clone())), code(FanexError::GloballyPaused));

    let admin = env.admin.insecure_clone();
    send(&mut env, &admin, pause_ix(&admin.pubkey(), false)).unwrap();
    send(&mut env, &s.wallet, sale).unwrap();
}

#[test]
fn a_curve_s_pause_stops_its_sales_and_no_other_curve_s() {
    let (mut env, curve, s) = trading();
    let other = env.write_curve_usdc(&curve.swap_state());
    let pauser = env.pauser.insecure_clone();
    send(&mut env, &pauser, curve_pause_ix(&pauser.pubkey(), &curve.mint, true)).unwrap();
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    expect_err(unit(send(&mut env, &s.wallet, sale)), code(FanexError::CurvePaused));

    let t = seller(&mut env, &other.mint, 1_000_000 * TOKEN);
    let sale = own_sale(&env, &other, &t, 1_000_000 * TOKEN);
    send(&mut env, &t.wallet, sale).unwrap();
}

#[test]
fn the_sol_pause_leaves_the_usdc_sale_running() {
    let (mut env, curve, s) = trading();
    env.set_paused(true);
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    send(&mut env, &s.wallet, sale).unwrap();
}

#[test]
fn a_curve_out_of_bonding_takes_no_sale() {
    let (mut env, curve, s) = trading();
    for phase in [PHASE_GRADUATING, PHASE_GRADUATED, PHASE_POOL_CREATED] {
        let mut moved = curve.clone();
        moved.phase = phase;
        env.put(curve_pda(&curve.mint), account_data(&moved, <CurveUsdc as anchor_lang::Space>::INIT_SPACE), pepper_curve::ID);
        let sale = own_sale(&env, &moved, &s, 1_000_000 * TOKEN);
        expect_err(unit(send(&mut env, &s.wallet, sale)), code(FanexError::NotBonding));
    }
}

// ── The curve's own vaults and the config's fee account ──────────────────────

/// The curve's vesting vault, `["vesting_vault", mint]`: a token account of the mint whose authority
/// is the curve PDA, as `token_vault`'s and `usdc_vault`'s are.
fn vesting_vault_pda(mint: &Pubkey) -> Pubkey {
    pda(&[VESTING_VAULT_SEED, mint.as_ref()])
}

#[test]
fn a_sale_moves_through_the_curve_s_own_vaults_alone() {
    let (mut env, curve, s) = trading();
    // The curve PDA signs for any USDC account it is the authority of: one in `usdc_vault`'s place
    // would pay the sale while the counters said the reserve had.
    let decoy = Pubkey::new_unique();
    env.put_token_account(decoy, env.usdc.mint, curve_pda(&curve.mint), 0, COption::None);
    env.mint_usdc(&decoy, 1_000 * USDC);
    // Another curve's vault: its own PDA's, which this curve cannot sign for anyway.
    let other = env.write_curve_usdc(&curve.swap_state());
    // And the vesting vault in `token_vault`'s place would take the tokens sold into the creator's
    // vesting while the counters said they were back on sale.
    let vesting_vault = vesting_vault_pda(&curve.mint);
    env.put_token_account(vesting_vault, curve.mint, curve_pda(&curve.mint), 2_000_000 * TOKEN, COption::None);

    let held = holdings(&env, &curve, &s.source, &s.destination);
    let raw = curve_bytes(&env, &curve.mint);
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    assert_eq!(sale.accounts[7].pubkey, config.fee_usdc_account);
    let stand_ins = [
        (5, AccountMeta::new(decoy, false), anchor(ErrorCode::ConstraintSeeds)),
        (5, AccountMeta::new(usdc_vault_pda(&other.mint), false), anchor(ErrorCode::ConstraintSeeds)),
        (6, AccountMeta::new(vesting_vault, false), anchor(ErrorCode::ConstraintAssociated)),
        (7, AccountMeta::new(s.destination, false), code(FanexError::AccountMismatch)),
    ];
    for (position, meta, expected) in stand_ins {
        let forged = with_account(sale.clone(), position, meta);
        expect_err(unit(send(&mut env, &s.wallet, forged)), expected);
    }
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);
    assert_eq!(env.token_amount(&decoy), 1_000 * USDC);
    assert_eq!(env.token_amount(&vesting_vault), 2_000_000 * TOKEN);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
}

#[test]
fn a_sale_takes_the_curve_s_own_token_and_the_config_s_usdc() {
    let (mut env, curve, s) = trading();
    let address = curve_pda(&curve.mint);
    // A stranger holding none of the curve's tokens, and a worthless mint of its own: its tokens in
    // the stranger's account, and the curve PDA's associated account of it, which anyone may open.
    // With `token_mint` unbound, the sale would price them on this curve and pay for them out of
    // its `usdc_vault`.
    let thief = seller(&mut env, &curve.mint, 0);
    let worthless = Pubkey::new_unique();
    env.put_launched_mint(worthless);
    let thief_tokens = ata(&thief.wallet.pubkey(), &worthless);
    env.put_token_account(thief_tokens, worthless, thief.wallet.pubkey(), 1_000_000 * TOKEN, COption::None);
    let curve_worthless = ata(&address, &worthless);
    env.put_token_account(curve_worthless, worthless, address, 0, COption::None);
    // And a USDC look-alike, six decimals too, for a seller of real tokens to be paid in: SPL Token
    // would refuse it against the vault's mint (3), and the program refuses it first.
    let look_alike = Pubkey::new_unique();
    env.put_launched_mint(look_alike);
    let look_alike_account = Pubkey::new_unique();
    env.put_token_account(look_alike_account, look_alike, s.wallet.pubkey(), 0, COption::None);

    let held = holdings(&env, &curve, &thief.source, &thief.destination);
    let held_by_s = holdings(&env, &curve, &s.source, &s.destination);
    let raw = curve_bytes(&env, &curve.mint);
    let worthless_sale = [
        (1, AccountMeta::new(thief_tokens, false)),
        (6, AccountMeta::new(curve_worthless, false)),
        (9, AccountMeta::new_readonly(worthless, false)),
    ]
    .into_iter()
    .fold(
        sell_ix(&env, &curve, &thief.wallet.pubkey(), &thief.source, &thief.destination, 1_000_000 * TOKEN, 0),
        |ix, (position, meta)| with_account(ix, position, meta),
    );
    expect_err(unit(send(&mut env, &thief.wallet, worthless_sale)), code(FanexError::AccountMismatch));
    let look_alike_sale = [(2, AccountMeta::new(look_alike_account, false)), (8, AccountMeta::new_readonly(look_alike, false))]
        .into_iter()
        .fold(own_sale(&env, &curve, &s, 1_000_000 * TOKEN), |ix, (position, meta)| with_account(ix, position, meta));
    expect_err(unit(send(&mut env, &s.wallet, look_alike_sale)), code(FanexError::AccountMismatch));

    assert_eq!(holdings(&env, &curve, &thief.source, &thief.destination), held);
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held_by_s);
    assert_eq!(env.token_amount(&thief_tokens), 1_000_000 * TOKEN);
    assert_eq!(env.token_amount(&curve_worthless), 0);
    assert_eq!(env.token_amount(&look_alike_account), 0);
    assert_eq!(curve_bytes(&env, &curve.mint), raw);
}

#[test]
fn a_frozen_fee_account_stops_every_sale_that_owes_it_a_fee() {
    let (mut env, curve, s) = trading();
    let fee = env.fetch::<UsdcConfig>(&usdc_config_pda()).fee_usdc_account;
    env.freeze_usdc(&fee);
    let held = holdings(&env, &curve, &s.source, &s.destination);
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    expect_err(unit(send(&mut env, &s.wallet, sale.clone())), token(TokenError::AccountFrozen));
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);

    // A sale whose gross is under 100 µUSDC owes no fee, so nothing is sent to the frozen account.
    let small = quote_sell(&curve.swap_state(), TOKEN).unwrap();
    assert!(small.reserve_out > 0 && small.platform_fee == 0);
    let small_sale = own_sale(&env, &curve, &s, TOKEN);
    send(&mut env, &s.wallet, small_sale).unwrap();
    assert_eq!(env.token_amount(&s.destination), small.proceeds);

    env.thaw_usdc(&fee);
    let now_curve = curve_now(&env, &curve.mint);
    let thawed = own_sale(&env, &now_curve, &s, 1_000_000 * TOKEN);
    send(&mut env, &s.wallet, thawed).unwrap();
}

#[test]
fn the_fee_wallet_cannot_stop_the_swaps_by_closing_its_fee_account() {
    // The internal audit (2026-10-29): every buy and sale names the fee account, and its owner, the
    // receive-only fee wallet, could sweep and close it, stopping every USDC trade until a 48-hour
    // proposal. Hardened as `initialize_usdc_config` now requires (its close authority the config's
    // PDA), it can be swept but never closed, and the sales go on.
    let (mut env, curve, s) = trading();
    let fee = env.fetch::<UsdcConfig>(&usdc_config_pda()).fee_usdc_account;
    let fee_wallet = env.fee_wallet.insecure_clone();
    let first = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    send(&mut env, &s.wallet, first).unwrap();
    let earned = env.token_amount(&fee);
    assert!(earned > 0);
    // The fee wallet sweeps what it earned, then tries to close the empty account.
    let sweep_to = env.usdc_account(&Keypair::new().pubkey());
    let sweep = spl_token::instruction::transfer(&spl_token::ID, &fee, &sweep_to, &fee_wallet.pubkey(), &[], earned).unwrap();
    env.send(&[sweep], &[&fee_wallet]).unwrap();
    assert_eq!(env.token_amount(&fee), 0);
    let close = spl_token::instruction::close_account(&spl_token::ID, &fee, &fee_wallet.pubkey(), &fee_wallet.pubkey(), &[]).unwrap();
    expect_err(env.send(&[close], &[&fee_wallet]), token(TokenError::OwnerMismatch));
    assert!(env.exists(&fee));

    // A sale owing a fee and one owing none both land.
    let now_curve = curve_now(&env, &curve.mint);
    let sale = own_sale(&env, &now_curve, &s, 1_000_000 * TOKEN);
    send(&mut env, &s.wallet, sale).unwrap();
    assert!(env.token_amount(&fee) > 0);
    let now_curve = curve_now(&env, &curve.mint);
    let small = own_sale(&env, &now_curve, &s, TOKEN);
    send(&mut env, &s.wallet, small).unwrap();
}

#[test]
fn a_frozen_destination_fails_only_its_own_sale() {
    let (mut env, curve, s) = trading();
    env.freeze_usdc(&s.destination);
    let held = holdings(&env, &curve, &s.source, &s.destination);
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    expect_err(unit(send(&mut env, &s.wallet, sale)), token(TokenError::AccountFrozen));
    assert_eq!(holdings(&env, &curve, &s.source, &s.destination), held);
    // The same seller, paid into another account.
    let elsewhere = env.usdc_account(&Keypair::new().pubkey());
    let sale = sell_ix(&env, &curve, &s.wallet.pubkey(), &s.source, &elsewhere, 1_000_000 * TOKEN, 1);
    send(&mut env, &s.wallet, sale).unwrap();
}

// ── A buy and its sale, both through the program ─────────────────────────────

fn buy_ix(env: &Env, curve: &CurveUsdc, buyer: &Pubkey, source: &Pubkey, destination: &Pubkey, usdc_in: u64) -> Instruction {
    let config: UsdcConfig = env.fetch(&usdc_config_pda());
    let address = curve_pda(&curve.mint);
    ix(
        pepper_curve::accounts::BuyExactIn {
            user_transfer_authority: *buyer,
            user_source_usdc: *source,
            user_destination_token: *destination,
            usdc_config: usdc_config_pda(),
            curve: address,
            usdc_vault: usdc_vault_pda(&curve.mint),
            token_vault: ata(&address, &curve.mint),
            fee_usdc_account: config.fee_usdc_account,
            // An equity curve takes no creator share: account 8 absent.
            creator_usdc_account: None,
            usdc_mint: config.usdc_mint,
            token_mint: curve.mint,
            launch_guard_waiver: None,
            token_program: spl_token::ID,
        },
        pepper_curve::instruction::BuyExactIn { usdc_in, min_tokens_out: 0 },
    )
}

#[test]
fn a_buy_then_its_sale_returns_the_curve_to_where_it_was_but_for_rounding() {
    let v = vector("equity, fresh: a 100 USDC buy");
    let VectorOp::Buy { amount, now } = v.op else { panic!("a buy") };
    let (mut env, curve) = curve_with(&v.state);
    env.set_time(now);
    let wallet = env.funded_wallet(SOL);
    let usdc = env.usdc_account(&wallet.pubkey());
    env.mint_usdc(&usdc, amount);
    let open = spl_associated_token_account::instruction::create_associated_token_account_idempotent(
        &env.admin.pubkey(),
        &wallet.pubkey(),
        &curve.mint,
        &spl_token::ID,
    );
    env.send(&[open], &[]).unwrap();
    let tokens = ata(&wallet.pubkey(), &curve.mint);

    let buy = buy_ix(&env, &curve, &wallet.pubkey(), &usdc, &tokens, amount);
    send(&mut env, &wallet, buy).unwrap();
    let bought = env.token_amount(&tokens);
    let after_buy = curve_now(&env, &curve.mint);
    assert_eq!(after_buy.swap_state(), quote_buy(&curve.swap_state(), amount, now).unwrap().after);

    let q = quote_sell(&after_buy.swap_state(), bought).unwrap();
    let sale = sell_ix(&env, &after_buy, &wallet.pubkey(), &tokens, &usdc, bought, q.proceeds);
    let logs = send(&mut env, &wallet, sale).unwrap();
    let back = curve_now(&env, &curve.mint);
    assert_eq!(back.swap_state(), q.after);
    // Every token back on sale; the reserve keeps what rounding left it, never less than nothing.
    assert_eq!(back.real_token_reserves, curve.real_token_reserves);
    assert!(back.real_usdc_reserves <= 2);
    assert_eq!(env.token_amount(&usdc_vault_pda(&curve.mint)), back.real_usdc_reserves);
    // Two 1% fees and the rounding: the seller gets back less than it paid.
    assert!(env.token_amount(&usdc) < amount);
    assert_eq!(event::<TradeUsdc>(&logs).usdc_amount, q.reserve_out);
}

// ── The two kinds of curve apart (§10.1) ─────────────────────────────────────

#[test]
fn the_sol_sell_refuses_a_usdc_curve() {
    let (mut env, curve, s) = trading();
    let address = curve_pda(&curve.mint);
    let sol_sell = ix(
        pepper_curve::accounts::Sell {
            seller: s.wallet.pubkey(),
            config: config_pda(),
            curve: address,
            mint: curve.mint,
            sol_vault: pda(&[SOL_VAULT_SEED, curve.mint.as_ref()]),
            token_vault: ata(&address, &curve.mint),
            seller_token_account: s.source,
            fanex: env.fee_wallet.pubkey(),
            token_program: spl_token::ID,
            system_program: system_program::id(),
        },
        pepper_curve::instruction::Sell { token_in: 1_000_000 * TOKEN, min_sol_out: 0 },
    );
    expect_err(unit(send(&mut env, &s.wallet, sol_sell)), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

#[test]
fn sell_exact_in_refuses_a_sol_curve() {
    let mut env = Env::new();
    env.write_usdc_config();
    env.write_tradeable_curve(OCT_5_2026 - DAY);
    let sol_mint = env.mint;
    // A `CurveUsdc` shaped like it, never written: the instruction names the SOL curve's address.
    let shape = env.curve_usdc(sol_mint, &vector(SALE).state);
    let s = seller(&mut env, &sol_mint, 1_000_000 * TOKEN);
    let sale = own_sale(&env, &shape, &s, 1_000_000 * TOKEN);
    assert_eq!(sale.accounts[4].pubkey, curve_pda(&sol_mint));
    expect_err(unit(send(&mut env, &s.wallet, sale)), ACCOUNT_DISCRIMINATOR_MISMATCH);
}

// ── What a sale costs (§10.5: Jupiter's routes budget compute) ───────────────

#[test]
fn a_sale_with_its_three_transfers_fits_a_router_s_budget() {
    let (mut env, curve, s) = trading();
    let q = quote_sell(&curve.swap_state(), 1_000_000 * TOKEN).unwrap();
    assert!(q.platform_fee > 0);
    let sale = own_sale(&env, &curve, &s, 1_000_000 * TOKEN);
    let tx = Transaction::new_signed_with_payer(&[sale], Some(&s.wallet.pubkey()), &[&s.wallet], env.svm.latest_blockhash());
    let meta = env.svm.send_transaction(tx).expect("the sale lands");
    let units = meta.compute_units_consumed;
    println!("sell_exact_in with three transfers: {units} compute units");
    assert!(units < 100_000, "{units} compute units");
}
