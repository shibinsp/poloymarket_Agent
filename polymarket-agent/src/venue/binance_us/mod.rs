//! Binance.US.
//!
//! Spot crypto only, and therefore always open — no session arithmetic, no
//! extended-hours flag, no clock endpoint.
//!
//! Global Binance blocks US persons, so this targets `api.binance.us`, which
//! is a separate exchange with its own credentials, its own symbol list and
//! **no testnet**. `testnet.binance.vision` belongs to global Binance and
//! answers for symbols Binance.US does not list.

pub mod auth;
pub mod models;
pub mod rest;
mod venue;

pub use venue::{BinanceUsConfig, BinanceUsVenue, BASE_URL, DEFAULT_VENUE_ID};
