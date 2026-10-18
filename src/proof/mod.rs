//! Code that Verus proves, with its specs and proofs
//! (`doc/08-fuzzing-verification.md`), which the rest of the crate calls.
//! `cargo verus verify` checks the proofs; a normal build erases every spec
//! and proof, leaving only the code.
//!
//! - [`ProvenInlineKey`]: the std key types' inline keys, which their
//!   `InlineKey` impls return.
//! - `std_specs.rs`: what the std functions that the code calls do, trusted.
mod inline_key;
mod std_specs;

pub(crate) use inline_key::ProvenInlineKey;
