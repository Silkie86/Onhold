//! Finance and sales reports. Money is in the workspace's base currency,
//! using each invoice's own rate fixed at issue; credit notes count
//! against revenue.

use chrono::NaiveDate;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone)]
pub struct Row {
    /// The month, payer or talent.
    pub key: String,
    pub id: Option<Uuid>,
    pub invoiced_minor: i64,
    pub tax_minor: i64,
    pub count: i64,
}

/// Invoiced, net of credit notes, by month of issue.
pub async fn revenue_by_month(conn: &mut PgConnection, from: NaiveDate, to: NaiveDate) -> Result<Vec<Row>> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT to_char(date_trunc('month', issue_date), 'YYYY-MM') AS "key!", NULL::uuid AS id,
                  coalesce(sum(CASE WHEN kind = 'credit_note' THEN -base_total_minor ELSE base_total_minor END), 0)::bigint AS "invoiced_minor!",
                  coalesce(sum(CASE WHEN kind = 'credit_note' THEN -1 ELSE 1 END
                               * round(tax_minor::numeric * base_total_minor / nullif(total_minor, 0))), 0)::bigint AS "tax_minor!",
                  count(*) FILTER (WHERE kind = 'invoice') AS "count!"
           FROM invoices
           WHERE workspace_id = app_workspace_id() AND deleted_at IS NULL AND status NOT IN ('draft', 'void')
             AND issue_date BETWEEN $1 AND $2 AND base_total_minor IS NOT NULL
           GROUP BY 1 ORDER BY 1"#,
        from,
        to
    )
    .fetch_all(conn)
    .await?)
}

/// Invoiced, net of credit notes, by who was billed.
pub async fn revenue_by_payer(conn: &mut PgConnection, from: NaiveDate, to: NaiveDate) -> Result<Vec<Row>> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT coalesce(o.name, t.name, i.payer_name, '') AS "key!", coalesce(i.payer_organisation_id, i.payer_talent_id) AS id,
                  coalesce(sum(CASE WHEN i.kind = 'credit_note' THEN -i.base_total_minor ELSE i.base_total_minor END), 0)::bigint AS "invoiced_minor!",
                  coalesce(sum(CASE WHEN i.kind = 'credit_note' THEN -1 ELSE 1 END
                               * round(i.tax_minor::numeric * i.base_total_minor / nullif(i.total_minor, 0))), 0)::bigint AS "tax_minor!",
                  count(*) FILTER (WHERE i.kind = 'invoice') AS "count!"
           FROM invoices i
           LEFT JOIN organisations o ON o.id = i.payer_organisation_id
           LEFT JOIN talent t ON t.id = i.payer_talent_id
           WHERE i.workspace_id = app_workspace_id() AND i.deleted_at IS NULL AND i.status NOT IN ('draft', 'void')
             AND i.issue_date BETWEEN $1 AND $2 AND i.base_total_minor IS NOT NULL
           GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 200"#,
        from,
        to
    )
    .fetch_all(conn)
    .await?)
}

/// Invoiced net of tax, by the talent each line is for (lines for no one in
/// particular go to the invoice's booking's talent, else to no one).
pub async fn revenue_by_talent(conn: &mut PgConnection, from: NaiveDate, to: NaiveDate) -> Result<Vec<Row>> {
    Ok(sqlx::query_as!(
        Row,
        r#"WITH lines AS (
               SELECT coalesce((l->>'talent_id')::uuid,
                               (SELECT bt.talent_id FROM booking_talent bt WHERE bt.booking_id = i.booking_id
                                ORDER BY bt.position LIMIT 1),
                               i.issuer_talent_id) AS talent_id,
                      CASE WHEN i.kind = 'credit_note' THEN -1 ELSE 1 END
                        * round((l->>'amount_minor')::numeric * i.base_total_minor / nullif(i.total_minor, 0)) AS base_net,
                      i.id AS invoice_id, i.kind
               FROM invoices i, jsonb_array_elements(i.line_items) l
               WHERE i.workspace_id = app_workspace_id() AND i.deleted_at IS NULL AND i.status NOT IN ('draft', 'void')
                 AND i.issue_date BETWEEN $1 AND $2 AND i.base_total_minor IS NOT NULL
                 AND i.purpose <> 'commission'
           )
           SELECT coalesce(t.name, 'No talent') AS "key!", l.talent_id AS id,
                  coalesce(sum(l.base_net), 0)::bigint AS "invoiced_minor!", 0::bigint AS "tax_minor!",
                  count(DISTINCT l.invoice_id) FILTER (WHERE l.kind = 'invoice') AS "count!"
           FROM lines l LEFT JOIN talent t ON t.id = l.talent_id
           GROUP BY 1, 2 ORDER BY 3 DESC LIMIT 200"#,
        from,
        to
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Outstanding {
    pub id: Uuid,
    pub reference: Option<String>,
    pub payer: String,
    pub booking_id: Option<Uuid>,
    pub currency: String,
    pub outstanding_minor: i64,
    pub base_outstanding_minor: Option<i64>,
    pub issue_date: Option<NaiveDate>,
    pub due_date: Option<NaiveDate>,
    pub status: String,
}

/// Invoices still owed, oldest due first.
pub async fn outstanding(conn: &mut PgConnection) -> Result<Vec<Outstanding>> {
    Ok(sqlx::query_as!(
        Outstanding,
        r#"SELECT i.id, i.reference, coalesce(i.payer_name, o.name, t.name, '') AS "payer!", i.booking_id, i.currency,
                  (i.total_minor - i.paid_minor - i.credited_minor) AS "outstanding_minor!",
                  round((i.total_minor - i.paid_minor - i.credited_minor)::numeric * i.base_total_minor
                        / nullif(i.total_minor, 0))::bigint AS base_outstanding_minor,
                  i.issue_date, i.due_date, i.status::text AS "status!"
           FROM invoices i
           LEFT JOIN organisations o ON o.id = i.payer_organisation_id
           LEFT JOIN talent t ON t.id = i.payer_talent_id
           WHERE i.workspace_id = app_workspace_id() AND i.deleted_at IS NULL AND i.kind = 'invoice'
             AND i.status IN ('issued', 'part_paid', 'overdue')
           ORDER BY i.due_date NULLS LAST, i.issue_date
           LIMIT 1000"#
    )
    .fetch_all(conn)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Funnel {
    pub enquiries: i64,
    pub quoted: i64,
    pub won: i64,
    pub lost: i64,
    pub open: i64,
}

/// What became of the enquiries made in a period.
pub async fn funnel(conn: &mut PgConnection, from: NaiveDate, to: NaiveDate) -> Result<Funnel> {
    Ok(sqlx::query_as!(
        Funnel,
        r#"SELECT count(*) AS "enquiries!",
                  count(*) FILTER (WHERE EXISTS (
                      SELECT 1 FROM bookings b JOIN quotes q ON q.booking_id = b.id
                      WHERE b.enquiry_id = e.id AND q.sent_at IS NOT NULL)) AS "quoted!",
                  count(*) FILTER (WHERE s.outcome = 'won') AS "won!",
                  count(*) FILTER (WHERE s.outcome = 'lost') AS "lost!",
                  count(*) FILTER (WHERE s.outcome IS NULL OR s.outcome = 'open') AS "open!"
           FROM enquiries e LEFT JOIN pipeline_stages s ON s.id = e.stage_id
           WHERE e.workspace_id = app_workspace_id() AND e.deleted_at IS NULL
             AND e.created_at::date BETWEEN $1 AND $2"#,
        from,
        to
    )
    .fetch_one(conn)
    .await?)
}

/// Money received and paid out in a period, by currency.
#[derive(Debug, Clone)]
pub struct Cash {
    pub currency: String,
    pub received_minor: i64,
    pub refunded_minor: i64,
    pub paid_out_minor: i64,
}

pub async fn cash(conn: &mut PgConnection, from: NaiveDate, to: NaiveDate) -> Result<Vec<Cash>> {
    Ok(sqlx::query_as!(
        Cash,
        r#"SELECT currency AS "currency!",
                  coalesce(sum(amount_minor) FILTER (WHERE kind = 'payment'), 0)::bigint AS "received_minor!",
                  coalesce(sum(amount_minor) FILTER (WHERE kind = 'refund'), 0)::bigint AS "refunded_minor!",
                  coalesce(sum(amount_minor) FILTER (WHERE kind = 'payout'), 0)::bigint AS "paid_out_minor!"
           FROM payments
           WHERE workspace_id = app_workspace_id() AND voided_at IS NULL AND paid_on BETWEEN $1 AND $2
           GROUP BY currency ORDER BY currency"#,
        from,
        to
    )
    .fetch_all(conn)
    .await?)
}
