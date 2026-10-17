PRAGMA foreign_keys = ON;
BEGIN IMMEDIATE;

-- Canonical SQLite initialization DDL.

-- Epoch timestamps are seconds since 1970-01-01 UTC. Monetary values are
-- canonical unsigned base-unit integers encoded as TEXT. The 39-digit limit
-- covers u128 and avoids SQLite's signed-64-bit and REAL coercions.
-- Budget adapters must parse and checked-add these strings while holding the
-- same BEGIN IMMEDIATE transaction that writes a reservation; SQLite SUM()
-- would coerce large values and is not financially authoritative.

-- Keep the same explicit schema marker as PostgreSQL. PRAGMA user_version is
-- retained only as SQLite's fast startup guard; this table is the canonical
-- cross-dialect migration identity.
CREATE TABLE pay_schema_version (
    version             INTEGER PRIMARY KEY CHECK (version > 0),
    applied_at          INTEGER NOT NULL CHECK (applied_at >= 0)
);

INSERT INTO pay_schema_version (version, applied_at)
VALUES (1, CAST(strftime('%s', 'now') AS INTEGER));

CREATE TABLE pay_wallet (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    custody_type        TEXT NOT NULL CHECK (custody_type IN ('local', 'external')),
    algorithm           TEXT NOT NULL CHECK (length(trim(algorithm)) > 0),
    address             TEXT NOT NULL CHECK (length(trim(address)) > 0),
    public_key          TEXT,
    budget_asset        TEXT,
    budget_decimals     INTEGER,
    max_per_payment     TEXT CHECK (
                            max_per_payment IS NULL OR (
                                length(max_per_payment) BETWEEN 1 AND 39
                                AND max_per_payment NOT GLOB '*[^0-9]*'
                                AND (max_per_payment = '0' OR substr(max_per_payment, 1, 1) BETWEEN '1' AND '9')
                                AND max_per_payment <> '0'
                                AND (
                                    length(max_per_payment) < 39
                                    OR max_per_payment <= '340282366920938463463374607431768211455'
                                )
                            )
                        ),
    max_per_day         TEXT CHECK (
                            max_per_day IS NULL OR (
                                length(max_per_day) BETWEEN 1 AND 39
                                AND max_per_day NOT GLOB '*[^0-9]*'
                                AND (max_per_day = '0' OR substr(max_per_day, 1, 1) BETWEEN '1' AND '9')
                                AND max_per_day <> '0'
                                AND (
                                    length(max_per_day) < 39
                                    OR max_per_day <= '340282366920938463463374607431768211455'
                                )
                            )
                        ),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    CHECK (
        (custody_type = 'local' AND public_key IS NOT NULL AND length(trim(public_key)) > 0)
        OR custody_type = 'external'
    ),
    CHECK (public_key IS NULL OR length(trim(public_key)) > 0),
    CHECK (
        (budget_asset IS NULL AND budget_decimals IS NULL)
        OR (
            budget_asset IS NOT NULL
            AND length(trim(budget_asset)) > 0
            AND budget_decimals IS NOT NULL
            AND budget_decimals BETWEEN 0 AND 255
        )
    ),
    CHECK ((max_per_payment IS NULL AND max_per_day IS NULL) OR budget_asset IS NOT NULL),
    CHECK (
        max_per_payment IS NULL
        OR max_per_day IS NULL
        OR length(max_per_payment) < length(max_per_day)
        OR (length(max_per_payment) = length(max_per_day) AND max_per_payment <= max_per_day)
    ),
    CHECK (
        lower(algorithm) NOT IN ('secp256k1', 'evm', 'ethereum', 'eip155')
        OR (
            length(address) = 42
            AND substr(address, 1, 2) = '0x'
            AND address = lower(address)
            AND substr(address, 3) NOT GLOB '*[^0-9a-f]*'
        )
    ),
    -- External custody is not an address family. Canonicalize every value
    -- that is unambiguously an EVM address, regardless of algorithm label.
    CHECK (
        NOT (
            length(address) = 42
            AND lower(substr(address, 1, 2)) = '0x'
            AND substr(lower(address), 3) NOT GLOB '*[^0-9a-f]*'
        )
        OR address = lower(address)
    ),
    UNIQUE (algorithm, address),
    UNIQUE (id, budget_asset, budget_decimals)
);

-- One EVM account is one wallet-wide budget identity, regardless of the
-- custody/algorithm label. Opaque external addresses retain their existing
-- semantics; this partial index covers only strict 20-byte EVM addresses.
CREATE UNIQUE INDEX uq_pay_wallet_evm_address
    ON pay_wallet(lower(address))
    WHERE length(address) = 42
      AND lower(substr(address, 1, 2)) = '0x'
      AND substr(lower(address), 3) NOT GLOB '*[^0-9a-f]*';

-- Key material uses AES-256-GCM envelope encryption with full UUID v4 nonces.
-- XChaCha20-Poly1305 was the pre-release design and is intentionally unsupported.
-- Rotation appends a version and retires the old row; encrypted material is immutable.
CREATE TABLE wallet_key_version (
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    version             INTEGER NOT NULL CHECK (version BETWEEN 1 AND 4294967295),
    algorithm           TEXT NOT NULL CHECK (length(trim(algorithm)) > 0),
    address             TEXT NOT NULL CHECK (length(trim(address)) > 0),
    public_key          TEXT NOT NULL CHECK (length(trim(public_key)) > 0),
    encryption_scheme   TEXT NOT NULL CHECK (encryption_scheme = 'aes-256-gcm-uuid-v4-nonce-envelope-v1'),
    kek_id              TEXT NOT NULL CHECK (length(trim(kek_id)) > 0),
    wrapped_data_key    BLOB NOT NULL CHECK (length(wrapped_data_key) > 0),
    wrap_nonce          BLOB NOT NULL CHECK (length(wrap_nonce) = 16),
    key_ciphertext      BLOB NOT NULL CHECK (length(key_ciphertext) > 0),
    key_nonce           BLOB NOT NULL CHECK (length(key_nonce) = 16),
    aad                 BLOB NOT NULL CHECK (length(aad) > 0),
    status              TEXT NOT NULL CHECK (status IN ('active', 'retired', 'compromised')),
    activated_at        INTEGER NOT NULL CHECK (activated_at >= 0),
    retired_at          INTEGER,
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (wallet_id, version),
    CHECK (retired_at IS NULL OR retired_at >= activated_at),
    CHECK (
        lower(algorithm) NOT IN ('secp256k1', 'evm', 'ethereum', 'eip155')
        OR (
            length(address) = 42
            AND substr(address, 1, 2) = '0x'
            AND address = lower(address)
            AND substr(address, 3) NOT GLOB '*[^0-9a-f]*'
        )
    ),
    CHECK (
        NOT (
            length(address) = 42
            AND lower(substr(address, 1, 2)) = '0x'
            AND substr(lower(address), 3) NOT GLOB '*[^0-9a-f]*'
        )
        OR address = lower(address)
    ),
    CHECK (
        (status = 'active' AND retired_at IS NULL)
        OR (status IN ('retired', 'compromised') AND retired_at IS NOT NULL)
    )
);

CREATE UNIQUE INDEX uq_wallet_key_version_active
    ON wallet_key_version(wallet_id)
    WHERE status = 'active';

-- AES-GCM nonce reuse under one KEK is rejected even if UUID generation regresses.
CREATE UNIQUE INDEX uq_wallet_key_kek_wrap_nonce
    ON wallet_key_version(kek_id, wrap_nonce);

CREATE TABLE pay_client (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
    auth_principal      TEXT NOT NULL CHECK (length(trim(auth_principal)) > 0),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    UNIQUE (auth_principal)
);

CREATE TABLE pay_scenario (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    client_id           TEXT NOT NULL REFERENCES pay_client(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    external_ref        TEXT NOT NULL CHECK (length(trim(external_ref)) > 0),
    name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    UNIQUE (client_id, external_ref)
);

CREATE INDEX ix_pay_scenario_client_enabled
    ON pay_scenario(client_id, enabled, created_at);

-- Explicit scenario-scoped administrator authorization. The principal comes
-- from trusted authentication middleware; request payloads cannot nominate an
-- approver. Lifecycle checks also require the owning client and scenario to be
-- enabled at decision time.
CREATE TABLE pay_scenario_approver (
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    auth_principal      TEXT NOT NULL CHECK (length(trim(auth_principal)) > 0),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY (scenario_id, auth_principal)
);

CREATE INDEX ix_pay_scenario_approver_principal
    ON pay_scenario_approver(auth_principal, enabled, scenario_id);

CREATE TABLE pay_policy (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    protocols           TEXT NOT NULL CHECK (json_valid(protocols) AND json_type(protocols) = 'array'),
    networks            TEXT NOT NULL CHECK (json_valid(networks) AND json_type(networks) = 'array'),
    assets              TEXT NOT NULL CHECK (json_valid(assets) AND json_type(assets) = 'array'),
    budget_asset        TEXT NOT NULL CHECK (length(trim(budget_asset)) > 0),
    budget_decimals     INTEGER NOT NULL CHECK (budget_decimals BETWEEN 0 AND 255),
    allowed_recipients  TEXT NOT NULL DEFAULT '[]' CHECK (
                            json_valid(allowed_recipients) AND json_type(allowed_recipients) = 'array'
                        ),
    allowed_origins     TEXT NOT NULL DEFAULT '[]' CHECK (
                            json_valid(allowed_origins) AND json_type(allowed_origins) = 'array'
                        ),
    inbound_max_per_payment TEXT CHECK (
                            inbound_max_per_payment IS NULL OR (
                                length(inbound_max_per_payment) BETWEEN 1 AND 39
                                AND inbound_max_per_payment NOT GLOB '*[^0-9]*'
                                AND substr(inbound_max_per_payment, 1, 1) BETWEEN '1' AND '9'
                                AND (length(inbound_max_per_payment) < 39 OR inbound_max_per_payment <= '340282366920938463463374607431768211455')
                            )
                        ),
    inbound_max_per_day TEXT CHECK (
                            inbound_max_per_day IS NULL OR (
                                length(inbound_max_per_day) BETWEEN 1 AND 39
                                AND inbound_max_per_day NOT GLOB '*[^0-9]*'
                                AND substr(inbound_max_per_day, 1, 1) BETWEEN '1' AND '9'
                                AND (length(inbound_max_per_day) < 39 OR inbound_max_per_day <= '340282366920938463463374607431768211455')
                            )
                        ),
    inbound_approval_threshold TEXT CHECK (
                            inbound_approval_threshold IS NULL OR (
                                length(inbound_approval_threshold) BETWEEN 1 AND 39
                                AND inbound_approval_threshold NOT GLOB '*[^0-9]*'
                                AND substr(inbound_approval_threshold, 1, 1) BETWEEN '1' AND '9'
                                AND (length(inbound_approval_threshold) < 39 OR inbound_approval_threshold <= '340282366920938463463374607431768211455')
                            )
                        ),
    inbound_approval_enabled INTEGER NOT NULL DEFAULT 1 CHECK (inbound_approval_enabled IN (0, 1)),
    inbound_approval_overage TEXT CHECK (
                            inbound_approval_overage IS NULL OR (
                                length(inbound_approval_overage) BETWEEN 1 AND 39
                                AND inbound_approval_overage NOT GLOB '*[^0-9]*'
                                AND substr(inbound_approval_overage, 1, 1) BETWEEN '1' AND '9'
                                AND (length(inbound_approval_overage) < 39 OR inbound_approval_overage <= '340282366920938463463374607431768211455')
                            )
                        ),
    outbound_max_per_payment TEXT CHECK (
                            outbound_max_per_payment IS NULL OR (
                                length(outbound_max_per_payment) BETWEEN 1 AND 39
                                AND outbound_max_per_payment NOT GLOB '*[^0-9]*'
                                AND substr(outbound_max_per_payment, 1, 1) BETWEEN '1' AND '9'
                                AND (length(outbound_max_per_payment) < 39 OR outbound_max_per_payment <= '340282366920938463463374607431768211455')
                            )
                        ),
    outbound_max_per_day TEXT CHECK (
                            outbound_max_per_day IS NULL OR (
                                length(outbound_max_per_day) BETWEEN 1 AND 39
                                AND outbound_max_per_day NOT GLOB '*[^0-9]*'
                                AND substr(outbound_max_per_day, 1, 1) BETWEEN '1' AND '9'
                                AND (length(outbound_max_per_day) < 39 OR outbound_max_per_day <= '340282366920938463463374607431768211455')
                            )
                        ),
    outbound_approval_threshold TEXT CHECK (
                            outbound_approval_threshold IS NULL OR (
                                length(outbound_approval_threshold) BETWEEN 1 AND 39
                                AND outbound_approval_threshold NOT GLOB '*[^0-9]*'
                                AND substr(outbound_approval_threshold, 1, 1) BETWEEN '1' AND '9'
                                AND (length(outbound_approval_threshold) < 39 OR outbound_approval_threshold <= '340282366920938463463374607431768211455')
                            )
                        ),
    outbound_approval_enabled INTEGER NOT NULL DEFAULT 1 CHECK (outbound_approval_enabled IN (0, 1)),
    outbound_approval_overage TEXT CHECK (
                            outbound_approval_overage IS NULL OR (
                                length(outbound_approval_overage) BETWEEN 1 AND 39
                                AND outbound_approval_overage NOT GLOB '*[^0-9]*'
                                AND substr(outbound_approval_overage, 1, 1) BETWEEN '1' AND '9'
                                AND (length(outbound_approval_overage) < 39 OR outbound_approval_overage <= '340282366920938463463374607431768211455')
                            )
                        ),
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    CHECK (inbound_max_per_payment IS NOT NULL OR outbound_max_per_payment IS NOT NULL),
    CHECK (
        (inbound_max_per_payment IS NULL AND inbound_max_per_day IS NULL
         AND inbound_approval_threshold IS NULL AND inbound_approval_overage IS NULL)
        OR (inbound_max_per_payment IS NOT NULL AND inbound_max_per_day IS NOT NULL
            AND (length(inbound_max_per_payment) < length(inbound_max_per_day)
                 OR (length(inbound_max_per_payment) = length(inbound_max_per_day)
                     AND inbound_max_per_payment <= inbound_max_per_day))
            AND (inbound_approval_enabled = 1 OR inbound_approval_overage IS NULL))
    ),
    CHECK (
        (outbound_max_per_payment IS NULL AND outbound_max_per_day IS NULL
         AND outbound_approval_threshold IS NULL AND outbound_approval_overage IS NULL)
        OR (outbound_max_per_payment IS NOT NULL AND outbound_max_per_day IS NOT NULL
            AND (length(outbound_max_per_payment) < length(outbound_max_per_day)
                 OR (length(outbound_max_per_payment) = length(outbound_max_per_day)
                     AND outbound_max_per_payment <= outbound_max_per_day))
            AND (outbound_approval_enabled = 1 OR outbound_approval_overage IS NULL))
    ),
    UNIQUE (id, scenario_id)
);

CREATE TRIGGER trg_pay_policy_budget_unit_insert
BEFORE INSERT ON pay_policy
WHEN EXISTS (
    SELECT 1
    FROM json_each(NEW.assets)
    WHERE json_type(value) IS NOT 'object'
       OR json_type(value, '$.asset') IS NOT 'text'
       OR json_extract(value, '$.asset') IS NOT NEW.budget_asset
       OR json_type(value, '$.decimals') IS NOT 'integer'
       OR json_extract(value, '$.decimals') IS NOT NEW.budget_decimals
)
BEGIN
    SELECT RAISE(ABORT, 'policy assets must share the declared budget unit');
END;

CREATE TRIGGER trg_pay_policy_budget_unit_update
BEFORE UPDATE ON pay_policy
WHEN EXISTS (
    SELECT 1
    FROM json_each(NEW.assets)
    WHERE json_type(value) IS NOT 'object'
       OR json_type(value, '$.asset') IS NOT 'text'
       OR json_extract(value, '$.asset') IS NOT NEW.budget_asset
       OR json_type(value, '$.decimals') IS NOT 'integer'
       OR json_extract(value, '$.decimals') IS NOT NEW.budget_decimals
)
BEGIN
    SELECT RAISE(ABORT, 'policy assets must share the declared budget unit');
END;

CREATE TRIGGER trg_pay_policy_identity_immutable
BEFORE UPDATE ON pay_policy
WHEN OLD.scenario_id IS NOT NEW.scenario_id
  OR OLD.budget_asset IS NOT NEW.budget_asset
  OR OLD.budget_decimals IS NOT NEW.budget_decimals
BEGIN
    SELECT RAISE(ABORT, 'policy scenario and budget unit are immutable');
END;

-- Explicit association: one policy authorizes one or more wallets.
CREATE TABLE pay_policy_wallet (
    policy_id           TEXT NOT NULL REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (policy_id, wallet_id)
);

CREATE TRIGGER trg_pay_policy_wallet_validate
BEFORE INSERT ON pay_policy_wallet
BEGIN
    UPDATE pay_wallet
       SET budget_asset = (SELECT budget_asset FROM pay_policy WHERE id = NEW.policy_id),
           budget_decimals = (SELECT budget_decimals FROM pay_policy WHERE id = NEW.policy_id),
           updated_at = max(updated_at + 1, NEW.created_at)
     WHERE id = NEW.wallet_id
       AND budget_asset IS NULL
       AND budget_decimals IS NULL;
    SELECT RAISE(ABORT, 'policy budget unit does not match its wallet')
     WHERE NOT EXISTS (
        SELECT 1
          FROM pay_wallet wallet
          JOIN pay_policy policy ON policy.id = NEW.policy_id
         WHERE wallet.id = NEW.wallet_id
           AND wallet.budget_asset IS policy.budget_asset
           AND wallet.budget_decimals IS policy.budget_decimals
    );
    SELECT RAISE(ABORT, 'scenario wallet already has an enabled payment policy')
     WHERE (SELECT enabled FROM pay_policy WHERE id = NEW.policy_id) = 1
       AND EXISTS (
            SELECT 1
              FROM pay_policy_wallet existing
              JOIN pay_policy other ON other.id = existing.policy_id
             WHERE existing.wallet_id = NEW.wallet_id
               AND other.enabled = 1
               AND other.id <> NEW.policy_id
               AND other.scenario_id = (SELECT scenario_id FROM pay_policy WHERE id = NEW.policy_id)
       );
    SELECT RAISE(ABORT, 'wallet shared across scenarios requires aggregate hard limits')
     WHERE (SELECT enabled FROM pay_policy WHERE id = NEW.policy_id) = 1
       AND EXISTS (
            SELECT 1 FROM pay_wallet
             WHERE id = NEW.wallet_id AND (max_per_payment IS NULL OR max_per_day IS NULL)
       )
       AND EXISTS (
            SELECT 1
              FROM pay_policy_wallet existing
              JOIN pay_policy other ON other.id = existing.policy_id
             WHERE existing.wallet_id = NEW.wallet_id
               AND other.enabled = 1
               AND other.scenario_id <> (SELECT scenario_id FROM pay_policy WHERE id = NEW.policy_id)
       );
END;

CREATE TRIGGER trg_pay_policy_wallet_immutable_update
BEFORE UPDATE ON pay_policy_wallet
BEGIN
    SELECT RAISE(ABORT, 'policy wallet membership is immutable; replace the policy');
END;

CREATE TRIGGER trg_pay_policy_wallet_immutable_delete
BEFORE DELETE ON pay_policy_wallet
BEGIN
    SELECT RAISE(ABORT, 'policy wallet membership is immutable; replace the policy');
END;

CREATE TRIGGER trg_pay_policy_enable_validate
BEFORE UPDATE OF enabled ON pay_policy
WHEN NEW.enabled = 1
BEGIN
    SELECT RAISE(ABORT, 'scenario wallet already has an enabled payment policy')
     WHERE EXISTS (
        SELECT 1
          FROM pay_policy_wallet membership
          JOIN pay_policy_wallet existing ON existing.wallet_id = membership.wallet_id
          JOIN pay_policy other ON other.id = existing.policy_id
         WHERE membership.policy_id = NEW.id
           AND other.enabled = 1
           AND other.id <> NEW.id
           AND other.scenario_id = NEW.scenario_id
    );
    SELECT RAISE(ABORT, 'wallet shared across scenarios requires aggregate hard limits')
     WHERE EXISTS (
        SELECT 1
          FROM pay_policy_wallet membership
          JOIN pay_wallet wallet ON wallet.id = membership.wallet_id
         WHERE membership.policy_id = NEW.id
           AND (wallet.max_per_payment IS NULL OR wallet.max_per_day IS NULL)
           AND EXISTS (
                SELECT 1
                  FROM pay_policy_wallet existing
                  JOIN pay_policy other ON other.id = existing.policy_id
                 WHERE existing.wallet_id = membership.wallet_id
                   AND other.enabled = 1
                   AND other.id <> NEW.id
                   AND other.scenario_id <> NEW.scenario_id
           )
    );
END;

CREATE INDEX ix_pay_policy_wallet_wallet
    ON pay_policy_wallet(wallet_id, policy_id);

CREATE TABLE pay_offer (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    policy_id           TEXT NOT NULL REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    resource            TEXT NOT NULL CHECK (length(trim(resource)) > 0),
    protocols           TEXT NOT NULL CHECK (
                            json_valid(protocols)
                            AND json_type(protocols) = 'array'
                            AND json_array_length(protocols) > 0
                        ),
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT,
    asset_decimals      INTEGER NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    amount              TEXT NOT NULL CHECK (
                            length(amount) BETWEEN 1 AND 39
                            AND amount NOT GLOB '*[^0-9]*'
                            AND substr(amount, 1, 1) BETWEEN '1' AND '9'
                            AND (
                                length(amount) < 39
                                OR amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    enabled             INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    CHECK (token_contract IS NULL OR length(trim(token_contract)) > 0),
    UNIQUE (id, scenario_id, policy_id, wallet_id),
    FOREIGN KEY (policy_id, scenario_id)
        REFERENCES pay_policy(id, scenario_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (policy_id, wallet_id)
        REFERENCES pay_policy_wallet(policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT
);

CREATE TRIGGER trg_pay_offer_policy_insert
BEFORE INSERT ON pay_offer
WHEN NOT EXISTS (
    SELECT 1 FROM pay_policy policy
     WHERE policy.id = NEW.policy_id
       AND policy.inbound_max_per_payment IS NOT NULL
       AND policy.budget_asset IS NEW.asset
       AND policy.budget_decimals IS NEW.asset_decimals
       AND (
            length(NEW.amount) < length(policy.inbound_max_per_payment)
            OR (length(NEW.amount) = length(policy.inbound_max_per_payment)
                AND NEW.amount <= policy.inbound_max_per_payment)
       )
       AND EXISTS (
            SELECT 1 FROM json_each(policy.assets) allowed
             WHERE json_extract(allowed.value, '$.network') IS NEW.network
               AND json_extract(allowed.value, '$.asset') IS NEW.asset
               AND json_extract(allowed.value, '$.decimals') IS NEW.asset_decimals
               AND json_extract(allowed.value, '$.token_contract') IS NEW.token_contract
       )
       AND NOT EXISTS (
            SELECT 1 FROM json_each(NEW.protocols) offered
             WHERE NOT EXISTS (
                SELECT 1 FROM json_each(policy.protocols) allowed_protocol
                 WHERE allowed_protocol.value IS offered.value
             )
       )
)
BEGIN
    SELECT RAISE(ABORT, 'offer policy does not authorize this inbound budget unit');
END;

CREATE TRIGGER trg_pay_offer_policy_update
BEFORE UPDATE ON pay_offer
WHEN NOT EXISTS (
    SELECT 1 FROM pay_policy policy
     WHERE policy.id = NEW.policy_id
       AND policy.inbound_max_per_payment IS NOT NULL
       AND policy.budget_asset IS NEW.asset
       AND policy.budget_decimals IS NEW.asset_decimals
       AND (
            length(NEW.amount) < length(policy.inbound_max_per_payment)
            OR (length(NEW.amount) = length(policy.inbound_max_per_payment)
                AND NEW.amount <= policy.inbound_max_per_payment)
       )
       AND EXISTS (
            SELECT 1 FROM json_each(policy.assets) allowed
             WHERE json_extract(allowed.value, '$.network') IS NEW.network
               AND json_extract(allowed.value, '$.asset') IS NEW.asset
               AND json_extract(allowed.value, '$.decimals') IS NEW.asset_decimals
               AND json_extract(allowed.value, '$.token_contract') IS NEW.token_contract
       )
       AND NOT EXISTS (
            SELECT 1 FROM json_each(NEW.protocols) offered
             WHERE NOT EXISTS (
                SELECT 1 FROM json_each(policy.protocols) allowed_protocol
                 WHERE allowed_protocol.value IS offered.value
             )
       )
)
BEGIN
    SELECT RAISE(ABORT, 'offer policy does not authorize this inbound budget unit');
END;

-- One active fixed-price offer owns one scenario resource. Its protocols array
-- lists alternate wire adapters for that same price and recipient; allowing a
-- second active row would make resolve(scenario, resource, protocol)
-- nondeterministic.
CREATE UNIQUE INDEX uq_pay_offer_active_resource
    ON pay_offer(scenario_id, resource)
    WHERE enabled = 1;
CREATE INDEX ix_pay_offer_scenario_enabled
    ON pay_offer(scenario_id, enabled, created_at);

CREATE TABLE pay_intent (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    run_id              TEXT NOT NULL CHECK (length(trim(run_id)) > 0),
    wallet_id           TEXT REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    policy_id           TEXT REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    offer_id            TEXT REFERENCES pay_offer(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    direction           TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    protocol            TEXT NOT NULL CHECK (length(trim(protocol)) > 0),
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT,
    asset_decimals      INTEGER NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    recipient           TEXT NOT NULL CHECK (length(trim(recipient)) > 0),
    amount              TEXT NOT NULL CHECK (
                            length(amount) BETWEEN 1 AND 39
                            AND amount NOT GLOB '*[^0-9]*'
                            AND substr(amount, 1, 1) BETWEEN '1' AND '9'
                            AND (
                                length(amount) < 39
                                OR amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    settled_amount      TEXT NOT NULL DEFAULT '0' CHECK (
                            length(settled_amount) BETWEEN 1 AND 39
                            AND settled_amount NOT GLOB '*[^0-9]*'
                            AND (settled_amount = '0' OR substr(settled_amount, 1, 1) BETWEEN '1' AND '9')
                            AND (
                                length(settled_amount) < 39
                                OR settled_amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    resource            TEXT NOT NULL CHECK (length(trim(resource)) > 0),
    challenge_hash      TEXT CHECK (challenge_hash IS NULL OR length(trim(challenge_hash)) > 0),
    credential_hash     TEXT CHECK (credential_hash IS NULL OR length(trim(credential_hash)) > 0),
    request_hash        TEXT NOT NULL CHECK (length(trim(request_hash)) > 0),
    status              TEXT NOT NULL CHECK (status IN (
                            'created', 'pending_approval', 'authorized', 'signed',
                            'submitted', 'pending_settlement', 'settled', 'failed',
                            'expired', 'unknown'
                        )),
    reservation_status  TEXT NOT NULL DEFAULT 'unreserved' CHECK (reservation_status IN (
                            'unreserved', 'reserved', 'consumed', 'released'
                        )),
    reserved_amount     TEXT NOT NULL DEFAULT '0' CHECK (
                            length(reserved_amount) BETWEEN 1 AND 39
                            AND reserved_amount NOT GLOB '*[^0-9]*'
                            AND (reserved_amount = '0' OR substr(reserved_amount, 1, 1) BETWEEN '1' AND '9')
                            AND (
                                length(reserved_amount) < 39
                                OR reserved_amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    tx_hash             TEXT,
    settlement_ref      TEXT,
    next_reconcile_at   INTEGER,
    reconcile_attempts  INTEGER NOT NULL DEFAULT 0 CHECK (reconcile_attempts BETWEEN 0 AND 4294967295),
    lease_owner         TEXT,
    lease_expires_at    INTEGER,
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at          INTEGER NOT NULL CHECK (updated_at >= created_at),
    expires_at          INTEGER CHECK (expires_at IS NULL OR expires_at >= created_at),
    settled_at          INTEGER,
    UNIQUE (scenario_id, run_id),
    UNIQUE (id, scenario_id, wallet_id),
    UNIQUE (id, run_id, request_hash),
    FOREIGN KEY (policy_id, scenario_id)
        REFERENCES pay_policy(id, scenario_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (policy_id, wallet_id)
        REFERENCES pay_policy_wallet(policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (offer_id, scenario_id, policy_id, wallet_id)
        REFERENCES pay_offer(id, scenario_id, policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (
        length(settled_amount) < length(amount)
        OR (length(settled_amount) = length(amount) AND settled_amount <= amount)
    ),
    CHECK (
        length(reserved_amount) < length(amount)
        OR (length(reserved_amount) = length(amount) AND reserved_amount <= amount)
    ),
    CHECK (
        (reservation_status = 'unreserved' AND reserved_amount = '0')
        OR (reservation_status IN ('reserved', 'consumed', 'released') AND reserved_amount <> '0')
    ),
    CHECK (
        wallet_id IS NOT NULL AND policy_id IS NOT NULL
        AND (
            (direction = 'outbound' AND offer_id IS NULL)
            OR (direction = 'inbound' AND offer_id IS NOT NULL)
        )
    ),
    CHECK (
        (status = 'settled' AND settled_at IS NOT NULL)
        OR (status <> 'settled' AND settled_at IS NULL)
    ),
    CHECK (settled_at IS NULL OR settled_at >= created_at),
    CHECK (next_reconcile_at IS NULL OR next_reconcile_at >= 0),
    CHECK (lease_expires_at IS NULL OR lease_expires_at >= 0),
    CHECK (
        (lease_owner IS NULL AND lease_expires_at IS NULL)
        OR (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)
    ),
    CHECK (token_contract IS NULL OR length(trim(token_contract)) > 0),
    CHECK (tx_hash IS NULL OR length(trim(tx_hash)) > 0),
    CHECK (settlement_ref IS NULL OR length(trim(settlement_ref)) > 0)
);

CREATE TRIGGER trg_pay_intent_policy_insert
BEFORE INSERT ON pay_intent
WHEN NEW.status <> 'failed' AND NOT EXISTS (
    SELECT 1 FROM pay_policy policy
     WHERE policy.id = NEW.policy_id
       AND policy.budget_asset IS NEW.asset
       AND policy.budget_decimals IS NEW.asset_decimals
       AND (
            (NEW.direction = 'inbound' AND policy.inbound_max_per_payment IS NOT NULL)
            OR (NEW.direction = 'outbound' AND policy.outbound_max_per_payment IS NOT NULL)
       )
       AND EXISTS (SELECT 1 FROM json_each(policy.protocols) WHERE value IS NEW.protocol)
       AND EXISTS (
            SELECT 1 FROM json_each(policy.assets) allowed
             WHERE json_extract(allowed.value, '$.network') IS NEW.network
               AND json_extract(allowed.value, '$.asset') IS NEW.asset
               AND json_extract(allowed.value, '$.decimals') IS NEW.asset_decimals
               AND json_extract(allowed.value, '$.token_contract') IS NEW.token_contract
       )
       AND (
            NEW.direction = 'inbound'
            AND (
                length(NEW.amount) < length(policy.inbound_max_per_payment)
                OR (length(NEW.amount) = length(policy.inbound_max_per_payment)
                    AND NEW.amount <= policy.inbound_max_per_payment)
            )
            OR NEW.direction = 'outbound'
            AND (
                length(NEW.amount) < length(policy.outbound_max_per_payment)
                OR (length(NEW.amount) = length(policy.outbound_max_per_payment)
                    AND NEW.amount <= policy.outbound_max_per_payment)
            )
       )
)
BEGIN
    SELECT RAISE(ABORT, 'policy does not authorize intent direction or budget unit');
END;

-- State transitions may change lifecycle, reservation, settlement, and lease
-- fields only. The authorized business identity is immutable after creation.
CREATE TRIGGER trg_pay_intent_identity_immutable
BEFORE UPDATE ON pay_intent
WHEN OLD.scenario_id IS NOT NEW.scenario_id
  OR OLD.run_id IS NOT NEW.run_id
  OR OLD.wallet_id IS NOT NEW.wallet_id
  OR OLD.policy_id IS NOT NEW.policy_id
  OR OLD.offer_id IS NOT NEW.offer_id
  OR OLD.direction IS NOT NEW.direction
  OR OLD.protocol IS NOT NEW.protocol
  OR OLD.network IS NOT NEW.network
  OR OLD.asset IS NOT NEW.asset
  OR OLD.token_contract IS NOT NEW.token_contract
  OR OLD.asset_decimals IS NOT NEW.asset_decimals
  OR OLD.recipient IS NOT NEW.recipient
  OR OLD.amount IS NOT NEW.amount
  OR OLD.resource IS NOT NEW.resource
  OR OLD.challenge_hash IS NOT NEW.challenge_hash
  OR OLD.request_hash IS NOT NEW.request_hash
  OR OLD.created_at IS NOT NEW.created_at
  OR OLD.expires_at IS NOT NEW.expires_at
BEGIN
    SELECT RAISE(ABORT, 'payment intent identity is immutable');
END;

CREATE TRIGGER trg_pay_intent_credential_forward_only
BEFORE UPDATE OF credential_hash ON pay_intent
WHEN OLD.credential_hash IS NOT NULL
 AND OLD.credential_hash IS NOT NEW.credential_hash
BEGIN
    SELECT RAISE(ABORT, 'payment credential binding is immutable once set');
END;

CREATE UNIQUE INDEX uq_pay_intent_credential
    ON pay_intent(protocol, direction, credential_hash)
    WHERE credential_hash IS NOT NULL;
CREATE UNIQUE INDEX uq_pay_intent_challenge
    ON pay_intent(scenario_id, challenge_hash)
    WHERE direction = 'inbound' AND challenge_hash IS NOT NULL;
CREATE INDEX ix_pay_intent_scenario_direction_time
    ON pay_intent(scenario_id, direction, created_at DESC);
CREATE INDEX ix_pay_intent_wallet_direction_time
    ON pay_intent(wallet_id, direction, created_at DESC);
-- Pending reservations remain authoritative across UTC-day boundaries until
-- released or atomically consumed with ledger posting. No created_at cutoff,
-- network, or token_contract participates in these indexes.
CREATE INDEX ix_pay_intent_scenario_budget
    ON pay_intent(scenario_id, direction, reservation_status, asset, asset_decimals, reserved_amount)
    WHERE reservation_status = 'reserved';
CREATE INDEX ix_pay_intent_wallet_budget
    ON pay_intent(wallet_id, direction, reservation_status, asset, asset_decimals, reserved_amount)
    WHERE reservation_status = 'reserved';
CREATE INDEX ix_pay_intent_reconciliation_due
    ON pay_intent(next_reconcile_at, lease_expires_at)
    WHERE status IN ('signed', 'submitted', 'pending_settlement', 'unknown');
CREATE INDEX ix_pay_intent_created_expiry_due
    ON pay_intent(expires_at, lease_expires_at)
    WHERE status = 'created' AND expires_at IS NOT NULL;
CREATE INDEX ix_pay_intent_inbound_challenge_expiry
    ON pay_intent(expires_at)
    WHERE direction = 'inbound' AND status = 'created' AND reservation_status = 'reserved';

CREATE TABLE pay_event (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    intent_id           TEXT NOT NULL REFERENCES pay_intent(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    event_type          TEXT NOT NULL CHECK (length(trim(event_type)) > 0),
    attempt_no          INTEGER NOT NULL DEFAULT 0 CHECK (attempt_no BETWEEN 0 AND 4294967295),
    signature_ref       TEXT,
    tx_hash             TEXT,
    payload             TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(payload)),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    UNIQUE (intent_id, event_type, attempt_no),
    CHECK (signature_ref IS NULL OR length(trim(signature_ref)) > 0),
    CHECK (tx_hash IS NULL OR length(trim(tx_hash)) > 0)
);

CREATE INDEX ix_pay_event_intent_time
    ON pay_event(intent_id, created_at, id);

CREATE TABLE pay_approval (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    intent_id           TEXT NOT NULL,
    run_id              TEXT NOT NULL CHECK (length(trim(run_id)) > 0),
    requested_amount    TEXT NOT NULL CHECK (
                            length(requested_amount) BETWEEN 1 AND 39
                            AND requested_amount NOT GLOB '*[^0-9]*'
                            AND substr(requested_amount, 1, 1) BETWEEN '1' AND '9'
                            AND (
                                length(requested_amount) < 39
                                OR requested_amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    approved_amount     TEXT CHECK (
                            approved_amount IS NULL OR (
                                length(approved_amount) BETWEEN 1 AND 39
                                AND approved_amount NOT GLOB '*[^0-9]*'
                                AND (approved_amount = '0' OR substr(approved_amount, 1, 1) BETWEEN '1' AND '9')
                                AND (
                                    length(approved_amount) < 39
                                    OR approved_amount <= '340282366920938463463374607431768211455'
                                )
                            )
                        ),
    requested_by        TEXT NOT NULL CHECK (length(trim(requested_by)) > 0),
    approved_by         TEXT,
    status              TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'expired', 'consumed')),
    intent_hash         TEXT NOT NULL CHECK (length(trim(intent_hash)) > 0),
    grant_jti           TEXT,
    grant_claims_hash   TEXT,
    sealed_grant_token  TEXT,
    expires_at          INTEGER NOT NULL,
    consumed_at         INTEGER,
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    approved_at         INTEGER,
    FOREIGN KEY (intent_id, run_id, intent_hash)
        REFERENCES pay_intent(id, run_id, request_hash) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (
        approved_amount IS NULL
        OR length(approved_amount) < length(requested_amount)
        OR (length(approved_amount) = length(requested_amount) AND approved_amount <= requested_amount)
    ),
    CHECK (expires_at > created_at),
    CHECK (
        status NOT IN ('approved', 'consumed')
        OR (
            approved_amount IS NOT NULL
            AND approved_amount <> '0'
            AND approved_by IS NOT NULL
            AND grant_jti IS NOT NULL
            AND grant_claims_hash IS NOT NULL
            AND sealed_grant_token IS NOT NULL
            AND approved_at IS NOT NULL
        )
    ),
    CHECK (
        (status = 'consumed' AND consumed_at IS NOT NULL)
        OR (status <> 'consumed' AND consumed_at IS NULL)
    ),
    CHECK (
        (grant_jti IS NULL AND grant_claims_hash IS NULL AND sealed_grant_token IS NULL)
        OR (grant_jti IS NOT NULL AND grant_claims_hash IS NOT NULL AND sealed_grant_token IS NOT NULL)
    ),
    CHECK (status <> 'pending' OR (
        approved_amount IS NULL AND approved_by IS NULL AND approved_at IS NULL AND grant_jti IS NULL
    )),
    CHECK (status <> 'rejected' OR (
        approved_amount IS NULL AND approved_by IS NOT NULL AND grant_jti IS NULL
    )),
    CHECK (grant_jti IS NULL OR (approved_amount IS NOT NULL AND approved_amount <> '0' AND approved_at IS NOT NULL)),
    CHECK (approved_by IS NULL OR length(trim(approved_by)) > 0),
    CHECK (grant_jti IS NULL OR length(trim(grant_jti)) > 0),
    CHECK (grant_claims_hash IS NULL OR (
        length(grant_claims_hash) = 64 AND grant_claims_hash NOT GLOB '*[^0-9a-f]*'
    )),
    CHECK (grant_jti IS NULL OR expires_at > approved_at),
    CHECK (sealed_grant_token IS NULL OR (
        json_valid(sealed_grant_token)
        AND json_type(sealed_grant_token) = 'object'
        AND json_type(sealed_grant_token, '$.version') = 'integer'
        AND json_extract(sealed_grant_token, '$.version') > 0
        AND json_type(sealed_grant_token, '$.key_id') = 'text'
        AND length(trim(json_extract(sealed_grant_token, '$.key_id'))) > 0
        AND json_type(sealed_grant_token, '$.nonce') = 'text'
        AND length(json_extract(sealed_grant_token, '$.nonce')) > 0
        AND json_type(sealed_grant_token, '$.ciphertext') = 'text'
        AND length(json_extract(sealed_grant_token, '$.ciphertext')) > 0
    )),
    CHECK (approved_at IS NULL OR approved_at >= created_at),
    CHECK (consumed_at IS NULL OR consumed_at >= approved_at)
);

CREATE UNIQUE INDEX uq_pay_approval_active_intent
    ON pay_approval(intent_id)
    WHERE status IN ('pending', 'approved');
CREATE UNIQUE INDEX uq_pay_approval_grant_jti
    ON pay_approval(grant_jti)
    WHERE grant_jti IS NOT NULL;
CREATE UNIQUE INDEX uq_pay_approval_grant_claims_hash
    ON pay_approval(grant_claims_hash)
    WHERE grant_claims_hash IS NOT NULL;
CREATE INDEX ix_pay_approval_pending_expiry
    ON pay_approval(expires_at, created_at)
    WHERE status IN ('pending', 'approved');

CREATE TABLE pay_ledger (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    intent_id           TEXT REFERENCES pay_intent(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    scenario_id         TEXT REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    entry_type          TEXT NOT NULL CHECK (entry_type IN (
                            'payment', 'fee', 'refund', 'deposit', 'transfer', 'reversal'
                        )),
    direction           TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT,
    asset_decimals      INTEGER NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    amount              TEXT NOT NULL CHECK (
                            length(amount) BETWEEN 1 AND 39
                            AND amount NOT GLOB '*[^0-9]*'
                            AND substr(amount, 1, 1) BETWEEN '1' AND '9'
                            AND (
                                length(amount) < 39
                                OR amount <= '340282366920938463463374607431768211455'
                            )
                        ),
    posting_key         TEXT NOT NULL CHECK (length(trim(posting_key)) > 0),
    tx_hash             TEXT,
    settlement_ref      TEXT,
    effect_ref          TEXT,
    transfer_ref        TEXT,
    reversal_of         TEXT REFERENCES pay_ledger(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    effective_at        INTEGER NOT NULL CHECK (effective_at >= 0),
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    UNIQUE (wallet_id, posting_key),
    FOREIGN KEY (intent_id, scenario_id, wallet_id)
        REFERENCES pay_intent(id, scenario_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (intent_id IS NULL OR scenario_id IS NOT NULL),
    CHECK ((entry_type = 'reversal') = (reversal_of IS NOT NULL)),
    CHECK (reversal_of IS NULL OR reversal_of <> id),
    CHECK (token_contract IS NULL OR length(trim(token_contract)) > 0),
    CHECK (tx_hash IS NULL OR length(trim(tx_hash)) > 0),
    CHECK (settlement_ref IS NULL OR length(trim(settlement_ref)) > 0),
    CHECK (effect_ref IS NULL OR length(trim(effect_ref)) BETWEEN 1 AND 256),
    CHECK (transfer_ref IS NULL OR length(trim(transfer_ref)) > 0),
    CHECK (
        tx_hash IS NOT NULL OR settlement_ref IS NOT NULL
    )
);

CREATE UNIQUE INDEX uq_pay_ledger_reversal_once
    ON pay_ledger(reversal_of)
    WHERE reversal_of IS NOT NULL;

CREATE TRIGGER trg_pay_ledger_validate_reversal
BEFORE INSERT ON pay_ledger
WHEN NEW.reversal_of IS NOT NULL
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
          FROM pay_ledger AS original
         WHERE original.id = NEW.reversal_of
           AND original.entry_type <> 'reversal'
           AND original.wallet_id = NEW.wallet_id
           AND original.intent_id IS NEW.intent_id
           AND original.scenario_id IS NEW.scenario_id
           AND original.network = NEW.network
           AND original.asset = NEW.asset
           AND original.token_contract IS NEW.token_contract
           AND original.asset_decimals = NEW.asset_decimals
           AND original.amount = NEW.amount
           AND original.direction <> NEW.direction
           AND original.effect_ref IS NEW.effect_ref
           AND original.transfer_ref IS NEW.transfer_ref
    ) THEN RAISE(ABORT, 'ledger reversal must exactly negate one unreversed wallet fact') END;
END;

CREATE INDEX ix_pay_ledger_scenario_direction_time
    ON pay_ledger(scenario_id, direction, effective_at DESC);
CREATE INDEX ix_pay_ledger_wallet_direction_time
    ON pay_ledger(wallet_id, direction, effective_at DESC);
-- Daily confirmed budget usage is derived from immutable ledger effective
-- time. These covering indexes intentionally omit network/token_contract.
CREATE INDEX ix_pay_ledger_scenario_budget
    ON pay_ledger(scenario_id, direction, asset, asset_decimals, effective_at, amount, intent_id);
CREATE INDEX ix_pay_ledger_wallet_budget
    ON pay_ledger(wallet_id, direction, asset, asset_decimals, effective_at, amount, intent_id);
CREATE INDEX ix_pay_ledger_asset_time
    ON pay_ledger(network, asset, token_contract, asset_decimals, effective_at DESC);
CREATE INDEX ix_pay_ledger_intent
    ON pay_ledger(intent_id, effective_at, id);
CREATE INDEX ix_pay_ledger_transfer
    ON pay_ledger(transfer_ref)
    WHERE transfer_ref IS NOT NULL;

-- SQLite workers claim work with BEGIN IMMEDIATE plus a conditional update of
-- the lease fields. Expired leases can be reclaimed after process failure.
CREATE TABLE pay_outbox (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    aggregate_type      TEXT NOT NULL CHECK (length(trim(aggregate_type)) > 0),
    aggregate_id        TEXT NOT NULL CHECK (length(trim(aggregate_id)) > 0),
    event_type          TEXT NOT NULL CHECK (length(trim(event_type)) > 0),
    destination         TEXT NOT NULL CHECK (length(trim(destination)) > 0),
    correlation_id      TEXT NOT NULL CHECK (length(trim(correlation_id)) > 0),
    run_id              TEXT NOT NULL CHECK (length(trim(run_id)) > 0),
    payload             TEXT NOT NULL CHECK (json_valid(payload)),
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'dead_letter')),
    attempt_count       INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 4294967295),
    available_at        INTEGER NOT NULL CHECK (available_at >= 0),
    lease_owner         TEXT,
    lease_expires_at    INTEGER,
    last_error          TEXT,
    created_at          INTEGER NOT NULL CHECK (created_at >= 0),
    delivered_at        INTEGER,
    UNIQUE (destination, event_type, run_id),
    CHECK (
        (lease_owner IS NULL AND lease_expires_at IS NULL)
        OR (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)
    ),
    CHECK (lease_expires_at IS NULL OR lease_expires_at >= 0),
    CHECK (
        (status = 'delivered' AND delivered_at IS NOT NULL)
        OR (status <> 'delivered' AND delivered_at IS NULL)
    ),
    CHECK (delivered_at IS NULL OR delivered_at >= created_at)
);

CREATE INDEX ix_pay_outbox_claim
    ON pay_outbox(available_at, lease_expires_at, created_at)
    WHERE status = 'pending';
CREATE INDEX ix_pay_outbox_correlation
    ON pay_outbox(correlation_id, created_at);

CREATE TRIGGER trg_pay_event_no_update
BEFORE UPDATE ON pay_event
BEGIN
    SELECT RAISE(ABORT, 'pay_event is append-only');
END;

CREATE TRIGGER trg_pay_event_no_delete
BEFORE DELETE ON pay_event
BEGIN
    SELECT RAISE(ABORT, 'pay_event is append-only');
END;

CREATE TRIGGER trg_pay_ledger_no_update
BEFORE UPDATE ON pay_ledger
BEGIN
    SELECT RAISE(ABORT, 'pay_ledger is append-only');
END;

CREATE TRIGGER trg_pay_ledger_no_delete
BEFORE DELETE ON pay_ledger
BEGIN
    SELECT RAISE(ABORT, 'pay_ledger is append-only');
END;

CREATE TRIGGER trg_wallet_key_version_no_delete
BEFORE DELETE ON wallet_key_version
BEGIN
    SELECT RAISE(ABORT, 'wallet key versions cannot be deleted');
END;

CREATE TRIGGER trg_wallet_key_version_protect_material
BEFORE UPDATE OF
    wallet_id, version, algorithm, address, public_key, encryption_scheme,
    kek_id, wrapped_data_key, wrap_nonce, key_ciphertext, key_nonce, aad,
    activated_at, created_at
ON wallet_key_version
WHEN OLD.wallet_id IS NOT NEW.wallet_id
    OR OLD.version IS NOT NEW.version
    OR OLD.algorithm IS NOT NEW.algorithm
    OR OLD.address IS NOT NEW.address
    OR OLD.public_key IS NOT NEW.public_key
    OR OLD.encryption_scheme IS NOT NEW.encryption_scheme
    OR OLD.kek_id IS NOT NEW.kek_id
    OR OLD.wrapped_data_key IS NOT NEW.wrapped_data_key
    OR OLD.wrap_nonce IS NOT NEW.wrap_nonce
    OR OLD.key_ciphertext IS NOT NEW.key_ciphertext
    OR OLD.key_nonce IS NOT NEW.key_nonce
    OR OLD.aad IS NOT NEW.aad
    OR OLD.activated_at IS NOT NEW.activated_at
    OR OLD.created_at IS NOT NEW.created_at
BEGIN
    SELECT RAISE(ABORT, 'wallet key version identity and encrypted material are immutable');
END;

CREATE TRIGGER trg_wallet_key_version_forward_only
BEFORE UPDATE OF status, retired_at ON wallet_key_version
WHEN NOT (
    (
        OLD.status = 'active'
        AND (
            (NEW.status = 'active' AND NEW.retired_at IS NULL)
            OR (NEW.status IN ('retired', 'compromised') AND NEW.retired_at IS NOT NULL)
        )
    )
    OR (
        OLD.status = 'retired'
        AND NEW.status IN ('retired', 'compromised')
        AND OLD.retired_at IS NEW.retired_at
    )
    OR (
        OLD.status = 'compromised'
        AND NEW.status = 'compromised'
        AND OLD.retired_at IS NEW.retired_at
    )
)
BEGIN
    SELECT RAISE(ABORT, 'wallet key version lifecycle transitions are forward-only');
END;

CREATE TRIGGER trg_wallet_key_version_validate_insert
BEFORE INSERT ON wallet_key_version
WHEN NOT EXISTS (
        SELECT 1
        FROM pay_wallet
        WHERE id = NEW.wallet_id AND custody_type = 'local'
    )
    OR (
        NEW.status = 'active'
        AND NOT EXISTS (
            SELECT 1
            FROM pay_wallet
            WHERE id = NEW.wallet_id
              AND algorithm = NEW.algorithm
              AND address = NEW.address
              AND public_key = NEW.public_key
        )
    )
BEGIN
    SELECT RAISE(ABORT, 'key version does not match a local-custody wallet');
END;

CREATE TRIGGER trg_pay_wallet_custody_type_immutable
BEFORE UPDATE OF custody_type ON pay_wallet
WHEN OLD.custody_type IS NOT NEW.custody_type
BEGIN
    SELECT RAISE(ABORT, 'wallet custody type is immutable');
END;

CREATE TRIGGER trg_pay_wallet_budget_unit_immutable
BEFORE UPDATE OF budget_asset, budget_decimals ON pay_wallet
WHEN (
        OLD.budget_asset IS NOT NEW.budget_asset
        OR OLD.budget_decimals IS NOT NEW.budget_decimals
    )
    AND EXISTS (SELECT 1 FROM pay_policy_wallet WHERE wallet_id = OLD.id)
BEGIN
    SELECT RAISE(ABORT, 'wallet budget unit is immutable after policy binding');
END;

CREATE TRIGGER trg_pay_wallet_shared_limits_required
BEFORE UPDATE OF max_per_payment, max_per_day ON pay_wallet
WHEN (NEW.max_per_payment IS NULL OR NEW.max_per_day IS NULL)
 AND (
        SELECT count(DISTINCT policy.scenario_id)
          FROM pay_policy_wallet membership
          JOIN pay_policy policy ON policy.id = membership.policy_id
         WHERE membership.wallet_id = OLD.id
           AND policy.enabled = 1
     ) > 1
BEGIN
    SELECT RAISE(ABORT, 'wallet shared across scenarios requires aggregate hard limits');
END;

CREATE TRIGGER trg_pay_wallet_active_key_consistency
BEFORE UPDATE OF algorithm, address, public_key ON pay_wallet
WHEN EXISTS (
    SELECT 1
    FROM wallet_key_version
    WHERE wallet_id = NEW.id
      AND status = 'active'
      AND (
          algorithm IS NOT NEW.algorithm
          OR address IS NOT NEW.address
          OR public_key IS NOT NEW.public_key
      )
)
BEGIN
    SELECT RAISE(ABORT, 'wallet metadata must match its active key version');
END;

COMMIT;
