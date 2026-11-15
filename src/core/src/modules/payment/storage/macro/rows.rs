macro_rules! payment_rows {
    ($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
        impl $repository {
            async fn lock_intent_financial_scope(connection: &mut Connection, intent: &PaymentIntent) -> Result<()> {
                let wallet_id = intent
                    .wallet_id
                    .clone()
                    .ok_or_else(|| crate::Error::conflict("payment intent has no wallet authority"))?;
                LedgerRepository::lock_financial_scopes(
                    connection,
                    std::slice::from_ref(&intent.scenario_id),
                    std::slice::from_ref(&wallet_id),
                )
                .await
            }

            pub(crate) async fn load_intent_connection(
                connection: &mut Connection,
                intent_id: &str,
            ) -> Result<Option<PaymentIntent>> {
                let row = sqlx::query(concat!("SELECT * FROM pay_intent WHERE id = $1", $update))
                    .bind(intent_id)
                    .fetch_optional(connection)
                    .await
                    .map_err(|error| BaseRepository::storage_error("get payment intent", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn load_intent_by_run(
                connection: &mut Connection,
                scenario_id: &str,
                run_id: &str,
            ) -> Result<Option<PaymentIntent>> {
                let row = sqlx::query(concat!(
                    "SELECT * FROM pay_intent WHERE scenario_id = $1 AND run_id = $2",
                    $update
                ))
                .bind(scenario_id)
                .bind(run_id)
                .fetch_optional(connection)
                .await
                .map_err(|error| BaseRepository::storage_error("find payment intent run", error))?;
                row.as_ref().map(Self::decode_row).transpose()
            }

            async fn load_run_connection(
                connection: &mut Connection,
                scenario_id: &str,
                run_id: &str,
            ) -> Result<Option<AuthorizationRecord>> {
                let Some(intent) = Self::load_intent_by_run(connection, scenario_id, run_id).await? else {
                    return Ok(None);
                };
                let row =
                    sqlx::query("SELECT * FROM pay_approval WHERE intent_id = $1 ORDER BY created_at DESC, id LIMIT 1")
                        .bind(&intent.id)
                        .fetch_optional(connection)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load payment approval replay", error))?;
                let approval = row.as_ref().map(ApprovalRepository::decode_row).transpose()?;
                Ok(Some(AuthorizationRecord { intent, approval }))
            }

            async fn load_approval_pool(pool: &Pool, intent_id: &str) -> Result<Option<ApprovalRequest>> {
                let row =
                    sqlx::query("SELECT * FROM pay_approval WHERE intent_id = $1 ORDER BY created_at DESC, id LIMIT 1")
                        .bind(intent_id)
                        .fetch_optional(pool)
                        .await
                        .map_err(|error| BaseRepository::storage_error("load payment intent approval", error))?;
                row.as_ref().map(ApprovalRepository::decode_row).transpose()
            }

            pub(crate) async fn append_side_effects(
                connection: &mut Connection,
                events: &[crate::model::PaymentEvent],
                outbox: &[crate::storage::OutboxMessage],
            ) -> Result<()> {
                for event in events {
                    EventRepository::insert_event(connection, event).await?;
                }
                for message in outbox {
                    OutboxRepository::insert_outbox(connection, message).await?;
                }
                Ok(())
            }

            pub(crate) fn decode_row(row: &DbRow) -> Result<PaymentIntent> {
                sqlx::FromRow::from_row(row)
                    .map_err(|error| BaseRepository::storage_error("decode payment entity", error))
            }
        }
    };
}
pub(crate) use payment_rows;
