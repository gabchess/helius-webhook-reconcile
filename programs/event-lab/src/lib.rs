use anchor_lang::prelude::*;

declare_id!("9LQwY9ThwtXFhpgMZ1MmRBfA5wc9YUSTUGDcmoKcKTZ6");

/// One instruction, one event. The event goes to the logs as a `Program data:` line,
/// which is what a webhook consumer has to decode after the fact.
#[program]
pub mod event_lab {
    use super::*;

    /// Emit one `TransferRecorded` event. The signer is the only account and pays nothing.
    pub fn record_transfer(
        ctx: Context<RecordTransfer>,
        id: u64,
        amount: u64,
        memo: String,
    ) -> Result<()> {
        emit!(TransferRecorded {
            id,
            amount,
            who: ctx.accounts.signer.key(),
            memo,
        });
        Ok(())
    }
}

/// Accounts of `record_transfer`.
#[derive(Accounts)]
pub struct RecordTransfer<'info> {
    pub signer: Signer<'info>,
}

/// The event a webhook consumer has to decode after the fact.
#[event]
pub struct TransferRecorded {
    pub id: u64,
    pub amount: u64,
    pub who: Pubkey,
    pub memo: String,
}
