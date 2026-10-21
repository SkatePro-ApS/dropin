//! Drop In's git work. Runs the real git.exe; every read of one
//! list is pinned to one HEAD, and the branch only ever moves by a checkout-like step.

mod branches;
mod changes;
mod ops;
mod parse;
mod run;
mod status;

pub use branches::*;
pub use changes::*;
pub use ops::*;
pub use parse::*;
pub use run::*;
pub use status::*;

#[cfg(test)]
mod tests;
