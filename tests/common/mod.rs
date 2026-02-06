//! Common test utilities and proptest strategies for libdictenstein.
//!
//! This module provides reusable proptest strategies for generating test data
//! across different dictionary implementations, as well as macros for
//! generalized trait-based testing.

#[macro_use]
pub mod macros;
pub mod strategies;
