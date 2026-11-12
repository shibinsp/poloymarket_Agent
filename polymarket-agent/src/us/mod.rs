//! US market trading domain module.
//!
//! Provides instruments, transaction cost schedules (SEC/FINRA), and historical backtesting engine.

pub mod backtest;
pub mod chart;
pub mod costs;
pub mod ibs;
pub mod instruments;
pub mod ladder;
pub mod market_feed;
pub mod desk;
pub mod news_stream;
pub mod options;
pub mod orb;
pub mod session;
pub mod sma_crossover;
pub mod prefomc;
pub mod spreads;
pub mod turnaround;
