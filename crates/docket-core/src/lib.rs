//! Headless work-queue engine: worker/topic/item/claim. No concept beyond
//! these four enters this crate — see docs/glossary.md.

pub mod domain;
pub mod storage;

pub use domain::{Item, Resolution, State, Worker};
pub use storage::{Store, StoreError};
