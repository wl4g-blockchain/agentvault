-- Canonical PostgreSQL initialization DDL.
BEGIN;

-- Epoch timestamps are seconds since 1970-01-01 UTC. Monetary values are
-- unsigned base-unit integers. The domain rejects fractional, negative, and
-- above-u128 values without introducing floating-point rounding.

CREATE DOMAIN pay_uint128 AS NUMERIC
    CHECK (
        VALUE >= 0
        AND VALUE <= 340282366920938463463374607431768211455
        AND scale(VALUE) = 0
    );

-- This marker makes bootstrap fail closed when a database contains a partial
-- or unsupported AgentVault schema. It is operational metadata, not a
-- financial aggregate.
CREATE TABLE pay_schema_version (
    version             INTEGER PRIMARY KEY CHECK (version > 0),
    applied_at          BIGINT NOT NULL CHECK (applied_at >= 0)
);

INSERT INTO pay_schema_version (version, applied_at)
VALUES (1, EXTRACT(EPOCH FROM clock_timestamp())::BIGINT);

CREATE TABLE pay_wallet (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    custody_type        TEXT NOT NULL CHECK (custody_type IN ('local', 'external')),
    algorithm           TEXT NOT NULL CHECK (length(trim(algorithm)) > 0),
    address             TEXT NOT NULL CHECK (length(trim(address)) > 0),
    public_key          TEXT,
    budget_asset        TEXT,
    budget_decimals     SMALLINT,
    max_per_payment     pay_uint128 CHECK (max_per_payment IS NULL OR max_per_payment > 0),
    max_per_day         pay_uint128 CHECK (max_per_day IS NULL OR max_per_day > 0),
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
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
    CHECK (max_per_payment IS NULL OR max_per_day IS NULL OR max_per_payment <= max_per_day),
    CHECK (
        lower(algorithm) NOT IN ('secp256k1', 'evm', 'ethereum', 'eip155')
        OR (address = lower(address) AND address ~ '^0x[0-9a-f]{40}$')
    ),
    -- External custody is not an address family. Canonicalize every value
    -- that is unambiguously an EVM address, regardless of algorithm label.
    CHECK (address !~* '^0x[0-9a-f]{40}$' OR address = lower(address)),
    UNIQUE (algorithm, address),
    UNIQUE (id, budget_asset, budget_decimals)
);

-- One EVM account is one wallet-wide budget identity, regardless of the
-- custody/algorithm label. Opaque external addresses retain their existing
-- semantics; this index applies only to strict 20-byte EVM addresses.
CREATE UNIQUE INDEX uq_pay_wallet_evm_address
    ON pay_wallet(lower(address))
    WHERE address ~* '^0x[0-9a-f]{40}$';

-- Periodic, non-authoritative chain observations used by management,
-- alerting, and capacity planning. Signing still performs a fresh trusted
-- balance check immediately before using a key.
CREATE TABLE pay_wallet_balance (
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT NOT NULL DEFAULT '',
    asset_decimals      SMALLINT NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    asset_available     pay_uint128 NOT NULL,
    native_available    pay_uint128 NOT NULL,
    observed_at         BIGINT NOT NULL CHECK (observed_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= observed_at),
    PRIMARY KEY (wallet_id, network, asset, token_contract, asset_decimals)
);

CREATE INDEX ix_pay_wallet_balance_observed
    ON pay_wallet_balance(observed_at, wallet_id);

-- Key material uses AES-256-GCM envelope encryption with full UUID v4 nonces.
-- XChaCha20-Poly1305 was the pre-release design and is intentionally unsupported.
-- Rotation appends a version and retires the old row; encrypted material is immutable.
CREATE TABLE wallet_key_version (
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    version             BIGINT NOT NULL CHECK (version BETWEEN 1 AND 4294967295),
    algorithm           TEXT NOT NULL CHECK (length(trim(algorithm)) > 0),
    address             TEXT NOT NULL CHECK (length(trim(address)) > 0),
    public_key          TEXT NOT NULL CHECK (length(trim(public_key)) > 0),
    encryption_scheme   TEXT NOT NULL CHECK (encryption_scheme = 'aes-256-gcm-uuid-v4-nonce-envelope-v1'),
    kek_id              TEXT NOT NULL CHECK (length(trim(kek_id)) > 0),
    wrapped_data_key    BYTEA NOT NULL CHECK (octet_length(wrapped_data_key) > 0),
    wrap_nonce          BYTEA NOT NULL CHECK (octet_length(wrap_nonce) = 16),
    key_ciphertext      BYTEA NOT NULL CHECK (octet_length(key_ciphertext) > 0),
    key_nonce           BYTEA NOT NULL CHECK (octet_length(key_nonce) = 16),
    aad                 BYTEA NOT NULL CHECK (octet_length(aad) > 0),
    status              TEXT NOT NULL CHECK (status IN ('active', 'retired', 'compromised')),
    activated_at        BIGINT NOT NULL CHECK (activated_at >= 0),
    retired_at          BIGINT,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (wallet_id, version),
    CHECK (retired_at IS NULL OR retired_at >= activated_at),
    CHECK (
        lower(algorithm) NOT IN ('secp256k1', 'evm', 'ethereum', 'eip155')
        OR (address = lower(address) AND address ~ '^0x[0-9a-f]{40}$')
    ),
    CHECK (address !~* '^0x[0-9a-f]{40}$' OR address = lower(address)),
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
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
    UNIQUE (auth_principal)
);

CREATE TABLE pay_scenario (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    client_id           TEXT NOT NULL REFERENCES pay_client(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    name                TEXT NOT NULL CHECK (length(trim(name)) > 0),
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at)
);

CREATE INDEX ix_pay_scenario_client_enabled
    ON pay_scenario(client_id, enabled, created_at);

-- Explicit scenario-scoped administrator authorization. The principal comes
-- from trusted authentication; a request body cannot nominate an approver.
CREATE TABLE pay_scenario_approver (
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    auth_principal      TEXT NOT NULL CHECK (length(trim(auth_principal)) > 0),
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY (scenario_id, auth_principal)
);

CREATE INDEX ix_pay_scenario_approver_principal
    ON pay_scenario_approver(auth_principal, enabled, scenario_id);

CREATE TABLE pay_policy (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    protocols           JSONB NOT NULL CHECK (jsonb_typeof(protocols) = 'array'),
    networks            JSONB NOT NULL CHECK (jsonb_typeof(networks) = 'array'),
    assets              JSONB NOT NULL CHECK (jsonb_typeof(assets) = 'array'),
    budget_asset        TEXT NOT NULL CHECK (length(trim(budget_asset)) > 0),
    budget_decimals     SMALLINT NOT NULL CHECK (budget_decimals BETWEEN 0 AND 255),
    allowed_recipients  JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(allowed_recipients) = 'array'),
    allowed_origins     JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(allowed_origins) = 'array'),
    inbound_max_per_payment    pay_uint128,
    inbound_max_per_day        pay_uint128,
    inbound_approval_threshold pay_uint128,
    inbound_approval_enabled   BOOLEAN NOT NULL DEFAULT TRUE,
    inbound_approval_overage   pay_uint128 CHECK (inbound_approval_overage IS NULL OR inbound_approval_overage > 0),
    outbound_max_per_payment    pay_uint128,
    outbound_max_per_day        pay_uint128,
    outbound_approval_threshold pay_uint128,
    outbound_approval_enabled   BOOLEAN NOT NULL DEFAULT TRUE,
    outbound_approval_overage   pay_uint128 CHECK (outbound_approval_overage IS NULL OR outbound_approval_overage > 0),
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
    CHECK (inbound_max_per_payment IS NOT NULL OR outbound_max_per_payment IS NOT NULL),
    CHECK (
        (inbound_max_per_payment IS NULL AND inbound_max_per_day IS NULL
         AND inbound_approval_threshold IS NULL AND inbound_approval_overage IS NULL)
        OR (inbound_max_per_payment > 0 AND inbound_max_per_day > 0
            AND inbound_max_per_payment <= inbound_max_per_day
            AND (inbound_approval_threshold IS NULL OR inbound_approval_threshold > 0)
            AND (inbound_approval_enabled OR inbound_approval_overage IS NULL)
            AND inbound_max_per_day + COALESCE(inbound_approval_overage, 0)
                <= 340282366920938463463374607431768211455)
    ),
    CHECK (
        (outbound_max_per_payment IS NULL AND outbound_max_per_day IS NULL
         AND outbound_approval_threshold IS NULL AND outbound_approval_overage IS NULL)
        OR (outbound_max_per_payment > 0 AND outbound_max_per_day > 0
            AND outbound_max_per_payment <= outbound_max_per_day
            AND (outbound_approval_threshold IS NULL OR outbound_approval_threshold > 0)
            AND (outbound_approval_enabled OR outbound_approval_overage IS NULL)
            AND outbound_max_per_day + COALESCE(outbound_approval_overage, 0)
                <= 340282366920938463463374607431768211455)
    ),
    UNIQUE (id, scenario_id)
);

-- Policy value and transition invariants are validated by the model and the
-- explicit policy -> wallet storage transaction. The canonical schema keeps
-- only declarative constraints; it contains no hidden business procedures.

-- Explicit association: one policy authorizes one or more wallets. This table
-- is relational membership, not a ninth financial aggregate.
CREATE TABLE pay_policy_wallet (
    policy_id           TEXT NOT NULL REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY (policy_id, wallet_id)
);

CREATE INDEX ix_pay_policy_wallet_wallet
    ON pay_policy_wallet(wallet_id, policy_id);

CREATE TABLE pay_price (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    policy_id           TEXT NOT NULL REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    resource            TEXT NOT NULL CHECK (length(trim(resource)) > 0),
    protocols           JSONB NOT NULL CHECK (
                            jsonb_typeof(protocols) = 'array' AND jsonb_array_length(protocols) > 0
                        ),
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT,
    asset_decimals      SMALLINT NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    amount              pay_uint128 NOT NULL CHECK (amount > 0),
    wallet_id           TEXT NOT NULL REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    enabled             BOOLEAN NOT NULL DEFAULT TRUE,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
    CHECK (token_contract IS NULL OR length(trim(token_contract)) > 0),
    UNIQUE (id, scenario_id, policy_id, wallet_id),
    FOREIGN KEY (policy_id, scenario_id)
        REFERENCES pay_policy(id, scenario_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (policy_id, wallet_id)
        REFERENCES pay_policy_wallet(policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT
);

-- One active fixed-price offer owns one scenario resource. Its protocols array
-- lists alternate wire adapters for that same price and recipient; allowing a
-- second active row would make resolve(scenario, resource, protocol)
-- nondeterministic.
CREATE UNIQUE INDEX uq_pay_price_active_resource
    ON pay_price(scenario_id, resource)
    WHERE enabled;
CREATE INDEX ix_pay_price_scenario_enabled
    ON pay_price(scenario_id, enabled, created_at);

CREATE TABLE pay_intent (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    scenario_id         TEXT NOT NULL REFERENCES pay_scenario(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    run_id              TEXT NOT NULL CHECK (length(trim(run_id)) > 0),
    wallet_id           TEXT REFERENCES pay_wallet(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    policy_id           TEXT REFERENCES pay_policy(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    offer_id            TEXT REFERENCES pay_price(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    direction           TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    protocol            TEXT NOT NULL CHECK (length(trim(protocol)) > 0),
    network             TEXT NOT NULL CHECK (length(trim(network)) > 0),
    asset               TEXT NOT NULL CHECK (length(trim(asset)) > 0),
    token_contract      TEXT,
    asset_decimals      SMALLINT NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    recipient           TEXT NOT NULL CHECK (length(trim(recipient)) > 0),
    amount              pay_uint128 NOT NULL CHECK (amount > 0),
    settled_amount      pay_uint128 NOT NULL DEFAULT 0,
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
    reserved_amount     pay_uint128 NOT NULL DEFAULT 0,
    tx_hash             TEXT,
    settlement_ref      TEXT,
    next_reconcile_at   BIGINT,
    reconcile_attempts  BIGINT NOT NULL DEFAULT 0 CHECK (reconcile_attempts BETWEEN 0 AND 4294967295),
    lease_owner         TEXT,
    lease_expires_at    BIGINT,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    updated_at          BIGINT NOT NULL CHECK (updated_at >= created_at),
    expires_at          BIGINT CHECK (expires_at IS NULL OR expires_at >= created_at),
    settled_at          BIGINT,
    UNIQUE (scenario_id, run_id),
    UNIQUE (id, scenario_id, wallet_id),
    UNIQUE (id, run_id, request_hash),
    FOREIGN KEY (policy_id, scenario_id)
        REFERENCES pay_policy(id, scenario_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (policy_id, wallet_id)
        REFERENCES pay_policy_wallet(policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    FOREIGN KEY (offer_id, scenario_id, policy_id, wallet_id)
        REFERENCES pay_price(id, scenario_id, policy_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (settled_amount <= amount),
    CHECK (reserved_amount <= amount),
    CHECK (
        (reservation_status = 'unreserved' AND reserved_amount = 0)
        OR (reservation_status IN ('reserved', 'consumed', 'released') AND reserved_amount > 0)
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

-- State transitions may change lifecycle, reservation, settlement, and lease
-- fields only. Storage compares the full immutable identity and applies every
-- lifecycle transition with an optimistic revision predicate.

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
    ON pay_intent(scenario_id, direction, reservation_status, asset, asset_decimals)
    INCLUDE (reserved_amount)
    WHERE reservation_status = 'reserved';
CREATE INDEX ix_pay_intent_wallet_budget
    ON pay_intent(wallet_id, direction, reservation_status, asset, asset_decimals)
    INCLUDE (reserved_amount)
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
    attempt_no          BIGINT NOT NULL DEFAULT 0 CHECK (attempt_no BETWEEN 0 AND 4294967295),
    signature_ref       TEXT,
    tx_hash             TEXT,
    payload             JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
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
    requested_amount    pay_uint128 NOT NULL CHECK (requested_amount > 0),
    approved_amount     pay_uint128,
    requested_by        TEXT NOT NULL CHECK (length(trim(requested_by)) > 0),
    approved_by         TEXT,
    status              TEXT NOT NULL CHECK (status IN ('pending', 'approved', 'rejected', 'expired', 'consumed')),
    intent_hash         TEXT NOT NULL CHECK (length(trim(intent_hash)) > 0),
    grant_jti           TEXT,
    grant_claims_hash   TEXT,
    sealed_grant_token  JSONB,
    expires_at          BIGINT NOT NULL,
    consumed_at         BIGINT,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    approved_at         BIGINT,
    FOREIGN KEY (intent_id, run_id, intent_hash)
        REFERENCES pay_intent(id, run_id, request_hash) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (approved_amount IS NULL OR approved_amount <= requested_amount),
    CHECK (expires_at > created_at),
    CHECK (
        status NOT IN ('approved', 'consumed')
        OR (
            approved_amount IS NOT NULL
            AND approved_amount > 0
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
    CHECK (grant_jti IS NULL OR (approved_amount IS NOT NULL AND approved_amount > 0 AND approved_at IS NOT NULL)),
    CHECK (approved_by IS NULL OR length(trim(approved_by)) > 0),
    CHECK (grant_jti IS NULL OR length(trim(grant_jti)) > 0),
    CHECK (grant_claims_hash IS NULL OR grant_claims_hash ~ '^[0-9a-f]{64}$'),
    CHECK (grant_jti IS NULL OR expires_at > approved_at),
    CHECK (sealed_grant_token IS NULL OR (
        jsonb_typeof(sealed_grant_token) = 'object'
        AND jsonb_typeof(sealed_grant_token->'version') = 'number'
        AND (sealed_grant_token->>'version')::INTEGER > 0
        AND jsonb_typeof(sealed_grant_token->'key_id') = 'string'
        AND length(trim(sealed_grant_token->>'key_id')) > 0
        AND jsonb_typeof(sealed_grant_token->'nonce') = 'string'
        AND length(sealed_grant_token->>'nonce') > 0
        AND jsonb_typeof(sealed_grant_token->'ciphertext') = 'string'
        AND length(sealed_grant_token->>'ciphertext') > 0
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
    asset_decimals      SMALLINT NOT NULL CHECK (asset_decimals BETWEEN 0 AND 255),
    amount              pay_uint128 NOT NULL CHECK (amount > 0),
    posting_key         TEXT NOT NULL CHECK (length(trim(posting_key)) > 0),
    tx_hash             TEXT,
    settlement_ref      TEXT,
    effect_ref          TEXT,
    transfer_ref        TEXT,
    reversal_of         TEXT REFERENCES pay_ledger(id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    effective_at        BIGINT NOT NULL CHECK (effective_at >= 0),
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    UNIQUE (wallet_id, posting_key),
    FOREIGN KEY (intent_id, scenario_id, wallet_id)
        REFERENCES pay_intent(id, scenario_id, wallet_id) ON DELETE RESTRICT ON UPDATE RESTRICT,
    CHECK (intent_id IS NULL OR scenario_id IS NOT NULL),
    CHECK ((entry_type = 'reversal') = (reversal_of IS NOT NULL)),
    CHECK (reversal_of IS NULL OR reversal_of <> id),
    CHECK (token_contract IS NULL OR length(trim(token_contract)) > 0),
    CHECK (tx_hash IS NULL OR length(trim(tx_hash)) > 0),
    CHECK (settlement_ref IS NULL OR length(trim(settlement_ref)) > 0),
    CHECK (effect_ref IS NULL OR (length(trim(effect_ref)) BETWEEN 1 AND 256)),
    CHECK (transfer_ref IS NULL OR length(trim(transfer_ref)) > 0),
    CHECK (
        tx_hash IS NOT NULL OR settlement_ref IS NOT NULL
    )
);

CREATE UNIQUE INDEX uq_pay_ledger_reversal_once
    ON pay_ledger(reversal_of)
    WHERE reversal_of IS NOT NULL;

CREATE INDEX ix_pay_ledger_scenario_direction_time
    ON pay_ledger(scenario_id, direction, effective_at DESC);
CREATE INDEX ix_pay_ledger_wallet_direction_time
    ON pay_ledger(wallet_id, direction, effective_at DESC);
-- Daily confirmed budget usage is derived from immutable ledger effective
-- time. These covering indexes intentionally omit network/token_contract.
CREATE INDEX ix_pay_ledger_scenario_budget
    ON pay_ledger(scenario_id, direction, asset, asset_decimals, effective_at)
    INCLUDE (amount, intent_id);
CREATE INDEX ix_pay_ledger_wallet_budget
    ON pay_ledger(wallet_id, direction, asset, asset_decimals, effective_at)
    INCLUDE (amount, intent_id);
CREATE INDEX ix_pay_ledger_asset_time
    ON pay_ledger(network, asset, token_contract, asset_decimals, effective_at DESC);
CREATE INDEX ix_pay_ledger_intent
    ON pay_ledger(intent_id, effective_at, id);
CREATE INDEX ix_pay_ledger_transfer
    ON pay_ledger(transfer_ref)
    WHERE transfer_ref IS NOT NULL;

-- Transactional outbox rows are inserted in the same transaction as the
-- aggregate change. Workers claim due rows with FOR UPDATE SKIP LOCKED and
-- persist the lease so a crash can be recovered safely.
CREATE TABLE pay_outbox (
    id                  TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    aggregate_type      TEXT NOT NULL CHECK (length(trim(aggregate_type)) > 0),
    aggregate_id        TEXT NOT NULL CHECK (length(trim(aggregate_id)) > 0),
    event_type          TEXT NOT NULL CHECK (length(trim(event_type)) > 0),
    destination         TEXT NOT NULL CHECK (length(trim(destination)) > 0),
    run_id              TEXT NOT NULL CHECK (length(trim(run_id)) > 0),
    payload             JSONB NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'delivered', 'dead_letter')),
    attempt_count       BIGINT NOT NULL DEFAULT 0 CHECK (attempt_count BETWEEN 0 AND 4294967295),
    available_at        BIGINT NOT NULL CHECK (available_at >= 0),
    lease_owner         TEXT,
    lease_expires_at    BIGINT,
    last_error          TEXT,
    created_at          BIGINT NOT NULL CHECK (created_at >= 0),
    delivered_at        BIGINT,
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
-- Events, ledger facts, and encrypted key material are exposed only through
-- append/forward-transition storage ports. Production PostgreSQL deployments
-- use a migration owner and a least-privilege runtime role so these tables do
-- not need procedural mutation guards in the canonical schema.

COMMIT;
