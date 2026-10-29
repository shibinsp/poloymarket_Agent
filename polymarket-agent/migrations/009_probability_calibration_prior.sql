-- Persist the model's raw forecast separately from the calibrated forecast
-- and the YES-side market prior so market-relative calibration can be learned.
ALTER TABLE confidence_calibration ADD COLUMN market_yes_price TEXT;
ALTER TABLE confidence_calibration ADD COLUMN model_probability TEXT;
