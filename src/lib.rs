pub mod error;
pub mod instructions;
pub mod libraries;
pub mod states;
pub mod util;

use anchor_lang::prelude::*;
use core as core_;
use instructions::*;
use states::*;

solana_program::declare_id!("CAMMCzo5YL8w4VFF8KVHrK22GGUsp5VTaW7grrKgrWqK");
