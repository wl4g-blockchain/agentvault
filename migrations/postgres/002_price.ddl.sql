-- Rename pricing in place. PostgreSQL retains all rows and automatically
-- retargets the existing intent foreign keys to the renamed relation.
ALTER TABLE pay_price RENAME TO pay_price;
ALTER INDEX uq_pay_price_active_resource RENAME TO uq_pay_price_active_resource;
ALTER INDEX ix_pay_price_scenario_enabled RENAME TO ix_pay_price_scenario_enabled;
INSERT INTO pay_schema_version (version, applied_at)
VALUES (2, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT);
