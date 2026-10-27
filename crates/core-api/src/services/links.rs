//! Checks shared by the records that point at the CRM (enquiries, bookings,
//! tasks): that each linked organisation, contact and talent exists.

use core_db::Tx;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use super::crm::Check;
use crate::error::ApiResult;

/// The most talent one enquiry or booking names.
pub const MAX_TALENT: usize = 20;

/// A linked record's id and name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Named {
    pub id: Uuid,
    pub name: String,
}

/// Notes an issue for each linked organisation or contact that does not exist.
pub(crate) async fn check_crm(
    tx: &mut Tx,
    check: &mut Check,
    organisation_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    venue_id: Option<Uuid>,
) -> ApiResult<()> {
    for (field, id) in [("organisation_id", organisation_id), ("venue_id", venue_id)] {
        if let Some(id) = id
            && core_db::organisations::get(tx, id).await?.is_none()
        {
            check.issue(field, "there is no such organisation");
        }
    }
    if let Some(id) = contact_id
        && core_db::contacts::get(tx, id).await?.is_none()
    {
        check.issue("contact_id", "there is no such contact");
    }
    Ok(())
}

/// The talent ids without repeats, each checked to exist.
pub(crate) async fn check_talent(
    tx: &mut Tx,
    check: &mut Check,
    ids: &[Uuid],
) -> ApiResult<Vec<Uuid>> {
    let mut out: Vec<Uuid> = Vec::new();
    for id in ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    if out.len() > MAX_TALENT {
        check.issue("talent_ids", format!("name at most {MAX_TALENT} talent"));
        return Ok(out);
    }
    for (i, id) in out.iter().enumerate() {
        if core_db::talent::get(tx, *id).await?.is_none() {
            check.issue(format!("talent_ids[{i}]"), "there is no such talent");
        }
    }
    Ok(out)
}

/// Groups `(owner, talent, name)` rows by owner.
pub(crate) fn talent_by_owner(rows: Vec<(Uuid, Uuid, String)>, owner: Uuid) -> Vec<Named> {
    rows.into_iter()
        .filter(|(o, _, _)| *o == owner)
        .map(|(_, id, name)| Named { id, name })
        .collect()
}
