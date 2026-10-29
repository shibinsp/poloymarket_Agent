//! US market trading domain module.
//!
//! Provides instruments, transaction cost schedules (SEC/FINRA), and historical backtesting engine.

pub mod backtest;
pub mod chart;
pub mod costs;
pub mod instruments;
pub mod market_feed;
pub mod session;
pub mod sma_crossover;
