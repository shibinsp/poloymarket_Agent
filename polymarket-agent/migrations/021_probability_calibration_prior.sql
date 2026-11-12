-- Renumbered from 009 to 021 when merging master into feature/us-paper-strategy-worker
-- (12 Nov 2026): this branch already used 009-020, which live databases have applied.
-- Persist the model's raw forecast separately from the calibrated forecast
-- and the YES-side market prior so market-relative calibration can be learned.
ALTER TABLE confidence_calibration ADD COLUMN market_yes_price TEXT;
ALTER TABLE confidence_calibration ADD COLUMN model_probability TEXT;
