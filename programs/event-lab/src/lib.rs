use anchor_lang::prelude::*;

declare_id!("9LQwY9ThwtXFhpgMZ1MmRBfA5wc9YUSTUGDcmoKcKTZ6");

/// Stub for chunk 2. It has no logic. It exists so `anchor build` runs in CI.
#[program]
pub mod event_lab {
    use super::*;

    pub fn ping(_ctx: Context<Ping>) -> Result<()> {
        Ok(())
    }
}

#[derive(Accounts)]
pub struct Ping {}
