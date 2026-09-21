//! Coinbase Advanced Trade.
//!
//! Crypto only, and therefore always open — no session arithmetic, no
//! extended-hours flag, no clock endpoint.

pub mod auth;
pub mod models;
pub mod rest;
mod venue;

pub use venue::{CoinbaseConfig, CoinbaseVenue, BASE_URL, DEFAULT_VENUE_ID};
