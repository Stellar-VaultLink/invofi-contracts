//! Cross-crate integration tests for the InvoFi Soroban protocol.
//!
//! This crate deploys all five contracts (registry, financing, repayment,
//! insurance, reputation) in a shared Soroban test environment and drives the
//! full invoice-lifecycle across contract boundaries. It proves that
//! cross-contract calls (auth propagation, event emission, status sync)
//! work correctly end-to-end.
//!
//! The per-crate unit tests remain untouched — this harness is additive.

#![no_std]

#[cfg(test)]
mod test;

#[cfg(test)]
mod proptest;
