-- SQLite retargets existing foreign keys during the table rename. Preserve
-- every intent/event/ledger row; only pricing indexes need their new names.
ALTER TABLE pay_price RENAME TO pay_price;
DROP INDEX uq_pay_price_active_resource;
DROP INDEX ix_pay_price_scenario_enabled;
CREATE UNIQUE INDEX uq_pay_price_active_resource
    ON pay_price(scenario_id, resource) WHERE enabled = 1;
CREATE INDEX ix_pay_price_scenario_enabled
    ON pay_price(scenario_id, enabled, created_at);
INSERT INTO pay_schema_version (version, applied_at)
VALUES (2, CAST(strftime('%s', 'now') AS INTEGER));
