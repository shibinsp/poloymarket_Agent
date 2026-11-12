-- Which strategy opened a venue trade. Exits differ by strategy: an
-- opening-range breakout is flat by the close, an SMA swing is held for weeks.
-- NULL for rows written before this column existed.
ALTER TABLE trades ADD COLUMN strategy TEXT;
