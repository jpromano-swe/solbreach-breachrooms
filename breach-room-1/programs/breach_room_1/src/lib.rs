use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke,
};

declare_id!("CZcRKbKx41T6ueFMDWpdz2PiLSPFaDoyKqqWVSdH2uhN");

#[program]
pub mod breach_room_1 {
    use super::*;

    pub fn initialize_protocol(ctx: Context<InitializeProtocol>, fee_bps: u16) -> Result<()> {
        require!(fee_bps <= 1_000, BreachRoomError::FeeTooHigh);

        let protocol = &mut ctx.accounts.protocol;
        protocol.admin = ctx.accounts.admin.key();
        protocol.treasury = ctx.accounts.treasury.key();
        protocol.trusted_reviewer = ctx.accounts.trusted_reviewer.key();
        protocol.active_market = ctx.accounts.market_config.key();
        protocol.fee_bps = fee_bps;
        protocol.bump = ctx.bumps.protocol;

        let market_config = &mut ctx.accounts.market_config;
        market_config.protocol = protocol.key();
        market_config.payout_mint = ctx.accounts.payout_mint.key();
        market_config.default_escrow_vault = ctx.accounts.default_escrow_vault.key();
        market_config.bump = ctx.bumps.market_config;

        Ok(())
    }

    pub fn create_task(
        ctx: Context<CreateTask>,
        task_id: u64,
        category: [u8; 16],
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, BreachRoomError::InvalidAmount);

        let task = &mut ctx.accounts.task;
        task.protocol = ctx.accounts.protocol.key();
        task.category_config = ctx.accounts.category_config.key();
        task.escrow_vault = ctx.accounts.escrow_vault.key();
        task.payout_mint = ctx.accounts.category_config.payout_mint;
        task.worker = Pubkey::default();
        task.amount = amount;
        task.task_id = task_id;
        task.generation = 1;
        task.status = TaskStatus::Open;
        task.bump = ctx.bumps.task;

        let category_config = &mut ctx.accounts.category_config;
        category_config.protocol = ctx.accounts.protocol.key();
        category_config.payout_mint = ctx.accounts.payout_mint.key();
        category_config.default_escrow_vault = ctx.accounts.escrow_vault.key();
        category_config.category = category;
        category_config.bump = ctx.bumps.category_config;

        Ok(())
    }

    pub fn accept_task(ctx: Context<AcceptTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        require!(task.status == TaskStatus::Open, BreachRoomError::InvalidStatus);

        task.worker = ctx.accounts.worker.key();
        task.status = TaskStatus::InReview;

        Ok(())
    }

    pub fn approve_task(ctx: Context<ApproveTask>) -> Result<()> {
        let task = &mut ctx.accounts.task;
        let protocol = &ctx.accounts.protocol;

        require_keys_eq!(
            ctx.accounts.reviewer.key(),
            protocol.trusted_reviewer,
            BreachRoomError::ReviewerRequired
        );
        require!(task.status == TaskStatus::InReview, BreachRoomError::InvalidStatus);

        // The individual accounts are valid, but the relationships between
        // task, category, escrow vault, and payout mint are not enforced here.
        task.status = TaskStatus::Approved;
        task.escrow_vault = ctx.accounts.provided_escrow_vault.key();
        task.payout_mint = ctx.accounts.provided_category_config.payout_mint;

        Ok(())
    }

    pub fn archive_receipt(ctx: Context<ArchiveReceipt>) -> Result<()> {
        let receipt = &mut ctx.accounts.receipt;
        require_keys_eq!(
            receipt.worker,
            ctx.accounts.worker.key(),
            BreachRoomError::WorkerRequired
        );

        receipt.status = ReceiptStatus::Archived;
        Ok(())
    }

    pub fn reopen_receipt(ctx: Context<ReopenReceipt>) -> Result<()> {
        let receipt = &mut ctx.accounts.receipt;

        // This uses stable seeds, then rewrites the same address back to Open
        // without proving a fresh generation or one-way lifecycle transition.
        receipt.task = ctx.accounts.task.key();
        receipt.worker = ctx.accounts.worker.key();
        receipt.amount = ctx.accounts.task.amount;
        receipt.generation = ctx.accounts.task.generation + 1;
        receipt.status = ReceiptStatus::Open;
        receipt.bump = ctx.bumps.receipt;

        Ok(())
    }

    pub fn execute_payout_via_cpi(
        ctx: Context<ExecutePayoutViaCpi>,
        payout_instruction_data: Vec<u8>,
    ) -> Result<()> {
        let task = &mut ctx.accounts.task;

        require!(task.status == TaskStatus::Approved, BreachRoomError::InvalidStatus);
        require_keys_eq!(
            task.worker,
            ctx.accounts.worker.key(),
            BreachRoomError::WorkerRequired
        );

        let account_metas = ctx
            .remaining_accounts
            .iter()
            .map(|account| {
                if account.is_writable {
                    AccountMeta::new(*account.key, account.is_signer)
                } else {
                    AccountMeta::new_readonly(*account.key, account.is_signer)
                }
            })
            .collect::<Vec<_>>();

        let instruction = Instruction {
            program_id: ctx.accounts.payout_program.key(),
            accounts: account_metas,
            data: payout_instruction_data,
        };

        invoke(&instruction, ctx.remaining_accounts)?;

        // Any successful CPI is treated as a completed payout.
        task.status = TaskStatus::Paid;

        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeProtocol<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: Stored as a protocol-owned treasury reference for the challenge.
    pub treasury: UncheckedAccount<'info>,
    /// CHECK: Stored as the reviewer authority used by the challenge.
    pub trusted_reviewer: UncheckedAccount<'info>,
    /// CHECK: Stored as a mint reference for the room state.
    pub payout_mint: UncheckedAccount<'info>,
    /// CHECK: Stored as an escrow reference for the room state.
    pub default_escrow_vault: UncheckedAccount<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + ProtocolConfig::INIT_SPACE,
        seeds = [b"protocol"],
        bump
    )]
    pub protocol: Account<'info, ProtocolConfig>,
    #[account(
        init,
        payer = admin,
        space = 8 + CategoryConfig::INIT_SPACE,
        seeds = [b"market", protocol.key().as_ref()],
        bump
    )]
    pub market_config: Account<'info, CategoryConfig>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(task_id: u64, category: [u8; 16])]
pub struct CreateTask<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(has_one = admin)]
    pub protocol: Account<'info, ProtocolConfig>,
    /// CHECK: Stored as the escrow route configured for this task category.
    pub escrow_vault: UncheckedAccount<'info>,
    /// CHECK: Stored as a mint reference for this task category.
    pub payout_mint: UncheckedAccount<'info>,
    #[account(
        init,
        payer = admin,
        space = 8 + CategoryConfig::INIT_SPACE,
        seeds = [b"category", protocol.key().as_ref(), category.as_ref()],
        bump
    )]
    pub category_config: Account<'info, CategoryConfig>,
    #[account(
        init,
        payer = admin,
        space = 8 + BountyTask::INIT_SPACE,
        seeds = [b"task", protocol.key().as_ref(), &task_id.to_le_bytes()],
        bump
    )]
    pub task: Account<'info, BountyTask>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AcceptTask<'info> {
    #[account(mut)]
    pub worker: Signer<'info>,
    #[account(mut)]
    pub task: Account<'info, BountyTask>,
}

#[derive(Accounts)]
pub struct ApproveTask<'info> {
    pub reviewer: Signer<'info>,
    pub protocol: Account<'info, ProtocolConfig>,
    #[account(mut, has_one = protocol)]
    pub task: Account<'info, BountyTask>,
    pub provided_category_config: Account<'info, CategoryConfig>,
    /// CHECK: The instruction intentionally accepts a caller-provided route.
    pub provided_escrow_vault: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ArchiveReceipt<'info> {
    pub worker: Signer<'info>,
    #[account(mut)]
    pub receipt: Account<'info, Receipt>,
}

#[derive(Accounts)]
pub struct ReopenReceipt<'info> {
    #[account(mut)]
    pub worker: Signer<'info>,
    pub task: Account<'info, BountyTask>,
    #[account(
        init_if_needed,
        payer = worker,
        space = 8 + Receipt::INIT_SPACE,
        seeds = [b"receipt", task.key().as_ref(), worker.key().as_ref()],
        bump
    )]
    pub receipt: Account<'info, Receipt>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExecutePayoutViaCpi<'info> {
    pub worker: Signer<'info>,
    #[account(mut)]
    pub task: Account<'info, BountyTask>,
    /// CHECK: Caller selects the program invoked by this instruction.
    pub payout_program: UncheckedAccount<'info>,
}

#[account]
#[derive(InitSpace)]
pub struct ProtocolConfig {
    pub admin: Pubkey,
    pub treasury: Pubkey,
    pub trusted_reviewer: Pubkey,
    pub active_market: Pubkey,
    pub fee_bps: u16,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct CategoryConfig {
    pub protocol: Pubkey,
    pub payout_mint: Pubkey,
    pub default_escrow_vault: Pubkey,
    pub category: [u8; 16],
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct BountyTask {
    pub protocol: Pubkey,
    pub category_config: Pubkey,
    pub escrow_vault: Pubkey,
    pub payout_mint: Pubkey,
    pub worker: Pubkey,
    pub amount: u64,
    pub task_id: u64,
    pub generation: u64,
    pub status: TaskStatus,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct Receipt {
    pub task: Pubkey,
    pub worker: Pubkey,
    pub amount: u64,
    pub generation: u64,
    pub status: ReceiptStatus,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq)]
pub enum TaskStatus {
    Open,
    InReview,
    Approved,
    Paid,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, PartialEq, Eq)]
pub enum ReceiptStatus {
    Open,
    Archived,
}

#[error_code]
pub enum BreachRoomError {
    #[msg("Fee is above the room limit.")]
    FeeTooHigh,
    #[msg("Amount must be greater than zero.")]
    InvalidAmount,
    #[msg("This instruction cannot be used from the current task state.")]
    InvalidStatus,
    #[msg("The configured reviewer must approve this task.")]
    ReviewerRequired,
    #[msg("The assigned worker must execute this action.")]
    WorkerRequired,
}
