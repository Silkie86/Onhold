//! Roles, permissions, the request context and the one place a permission is
//! checked.
//!
//! A permission is `resource:action`. Roles map to permission sets in the
//! `role_permissions` table, seeded per workspace from [`Role::default_permissions`].
//! Handlers never look at roles: services call [`Ctx::authorize`] once per
//! operation. Row ownership (for example "only this talent's bookings") is a
//! SQL filter, not a Rust check, so `authorize` takes no object.

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ids::{UserId, WorkspaceId};

/// A workspace membership role. A fixed set in v1.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    utoipa::ToSchema,
)]
#[sqlx(type_name = "member_role", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Admin,
    Agent,
    Finance,
    Viewer,
    Talent,
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::Owner,
        Role::Admin,
        Role::Agent,
        Role::Finance,
        Role::Viewer,
        Role::Talent,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Agent => "agent",
            Role::Finance => "finance",
            Role::Viewer => "viewer",
            Role::Talent => "talent",
        }
    }

    /// Staff roles are everyone except talent; a workspace can require 2FA for them.
    pub fn is_staff(self) -> bool {
        !matches!(self, Role::Talent)
    }

    /// The permissions seeded for this role when a workspace is created.
    pub fn default_permissions(self) -> BTreeSet<Permission> {
        use Permission as P;
        let all: BTreeSet<Permission> = Permission::ALL.iter().copied().collect();
        match self {
            Role::Owner => all,
            // Everything except billing and closing the workspace.
            Role::Admin => all
                .into_iter()
                .filter(|p| !matches!(p, P::BillingRead | P::BillingWrite | P::WorkspaceClose))
                .collect(),
            // Finance sees the workspace's billing details and runs the money:
            // invoices, payments, reports and tax settings.
            Role::Finance => [
                P::WorkspaceRead,
                P::MemberRead,
                P::BillingRead,
                P::ThingRead,
                P::EventRead,
                P::TalentRead,
                P::OrganisationRead,
                P::ContactRead,
                P::EnquiryRead,
                P::BookingRead,
                P::QuoteRead,
                P::TaskRead,
                P::TaskWrite,
                P::ProjectRead,
                P::ProjectWrite,
                P::DocumentRead,
                P::DocumentWrite,
                P::InvoiceRead,
                P::InvoiceWrite,
                P::PaymentWrite,
                P::ReportRead,
                P::FinanceConfig,
                P::InboxRead,
                P::InboxWrite,
            ]
            .into(),
            Role::Agent => [
                P::WorkspaceRead,
                P::MemberRead,
                P::ThingRead,
                P::ThingWrite,
                P::EventRead,
                P::TalentRead,
                P::TalentWrite,
                P::OrganisationRead,
                P::OrganisationWrite,
                P::ContactRead,
                P::ContactWrite,
                P::EnquiryRead,
                P::EnquiryWrite,
                P::BookingRead,
                P::BookingWrite,
                P::QuoteRead,
                P::QuoteWrite,
                P::TaskRead,
                P::TaskWrite,
                P::ProjectRead,
                P::ProjectWrite,
                P::DocumentRead,
                P::DocumentWrite,
                P::InvoiceRead,
                P::InvoiceWrite,
                P::InboxRead,
                P::InboxWrite,
            ]
            .into(),
            Role::Viewer => [
                P::WorkspaceRead,
                P::MemberRead,
                P::ThingRead,
                P::EventRead,
                P::TalentRead,
                P::OrganisationRead,
                P::ContactRead,
                P::EnquiryRead,
                P::BookingRead,
                P::QuoteRead,
                P::TaskRead,
                P::ProjectRead,
                P::DocumentRead,
                P::InvoiceRead,
                P::InboxRead,
            ]
            .into(),
            Role::Talent => [P::WorkspaceRead, P::PortalUse].into(),
        }
    }
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

macro_rules! permissions {
    ($($variant:ident => $s:literal),+ $(,)?) => {
        /// A `resource:action` permission.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, utoipa::ToSchema)]
        pub enum Permission {
            $(#[serde(rename = $s)] #[schema(rename = $s)] $variant,)+
        }

        impl Permission {
            pub const ALL: &'static [Permission] = &[$(Permission::$variant,)+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Permission::$variant => $s,)+
                }
            }
        }

        impl FromStr for Permission {
            type Err = UnknownPermission;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $($s => Ok(Permission::$variant),)+
                    _ => Err(UnknownPermission(s.to_owned())),
                }
            }
        }
    };
}

permissions! {
    WorkspaceRead => "workspace:read",
    WorkspaceUpdate => "workspace:update",
    WorkspaceClose => "workspace:close",
    MemberRead => "member:read",
    MemberInvite => "member:invite",
    MemberUpdate => "member:update",
    MemberRemove => "member:remove",
    ApiKeyRead => "api_key:read",
    ApiKeyWrite => "api_key:write",
    AuditRead => "audit:read",
    BillingRead => "billing:read",
    BillingWrite => "billing:write",
    // Workspace configuration: custom fields, pipelines, templates, rules.
    ConfigWrite => "config:write",
    // The placeholder entity that exercises the engines until real modules land.
    ThingRead => "thing:read",
    ThingWrite => "thing:write",
    EventRead => "event:read",
    WebhookRead => "webhook:read",
    WebhookWrite => "webhook:write",
    JobRead => "job:read",
    JobWrite => "job:write",
    TalentRead => "talent:read",
    TalentWrite => "talent:write",
    OrganisationRead => "organisation:read",
    OrganisationWrite => "organisation:write",
    ContactRead => "contact:read",
    ContactWrite => "contact:write",
    EnquiryRead => "enquiry:read",
    EnquiryWrite => "enquiry:write",
    BookingRead => "booking:read",
    BookingWrite => "booking:write",
    // Confirming a booking despite a clash.
    BookingOverride => "booking:override",
    QuoteRead => "quote:read",
    QuoteWrite => "quote:write",
    TaskRead => "task:read",
    TaskWrite => "task:write",
    // Projects: tours, seasons, circuits, campaigns, with their dates, crew and costs.
    ProjectRead => "project:read",
    ProjectWrite => "project:write",
    DocumentRead => "document:read",
    DocumentWrite => "document:write",
    InvoiceRead => "invoice:read",
    // Drafting, issuing, sending, crediting and voiding invoices; settlements.
    InvoiceWrite => "invoice:write",
    // Recording payments, refunds and payouts.
    PaymentWrite => "payment:write",
    ReportRead => "report:read",
    // Tax codes, invoice numbering, bank details and the invoicing model.
    FinanceConfig => "finance:config",
    // The unified inbox: reading threads, and replying, linking and
    // resolving them. Connecting one's own mailbox needs `inbox:write`.
    InboxRead => "inbox:read",
    InboxWrite => "inbox:write",
    // Seeing every connected account's health and disconnecting any of them.
    IntegrationManage => "integration:manage",
    // The talent portal: a talent member's own dates, availability,
    // documents and money, narrowed to their own talent records.
    PortalUse => "portal:use",
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown permission `{0}`")]
pub struct UnknownPermission(pub String);

/// Who is acting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    /// A signed-in person. `impersonator` is set when a superadmin acts in a
    /// workspace they are not a member of, for support.
    User {
        user_id: UserId,
        impersonator: Option<UserId>,
    },
    /// A workspace API key, acting with its own scoped permissions.
    ApiKey {
        key_id: Uuid,
        created_by: Option<UserId>,
    },
    /// An automation rule acting on its own. `depth` counts how many rule
    /// runs led here, so a rule whose action triggers another rule cannot
    /// loop forever.
    Rule { rule_id: Uuid, depth: u8 },
    /// The operator CLI or another trusted system process.
    System,
}

/// Where a request came from, recorded on every audit row.
#[derive(Debug, Clone, Default)]
pub struct RequestMeta {
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
    pub request_id: Option<String>,
}

/// Everything a service needs to know about the caller of one operation: who,
/// in which workspace, with which permissions.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub workspace_id: WorkspaceId,
    pub actor: Actor,
    /// The membership role, when the actor is a member. Impersonating
    /// superadmins and API keys have none.
    pub role: Option<Role>,
    pub permissions: BTreeSet<Permission>,
    pub meta: RequestMeta,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("missing permission `{0}`")]
pub struct AuthzError(pub Permission);

impl Ctx {
    /// The single authorization check. Every service operation calls this
    /// before touching data.
    pub fn authorize(&self, permission: Permission) -> Result<(), AuthzError> {
        if self.permissions.contains(&permission) {
            Ok(())
        } else {
            Err(AuthzError(permission))
        }
    }

    /// The signed-in user, if the actor is a person.
    pub fn user_id(&self) -> Option<UserId> {
        match self.actor {
            Actor::User { user_id, .. } => Some(user_id),
            Actor::ApiKey { .. } | Actor::Rule { .. } | Actor::System => None,
        }
    }

    pub fn impersonator(&self) -> Option<UserId> {
        match self.actor {
            Actor::User { impersonator, .. } => impersonator,
            Actor::ApiKey { .. } | Actor::Rule { .. } | Actor::System => None,
        }
    }

    pub fn api_key_id(&self) -> Option<Uuid> {
        match self.actor {
            Actor::ApiKey { key_id, .. } => Some(key_id),
            Actor::User { .. } | Actor::Rule { .. } | Actor::System => None,
        }
    }

    pub fn rule_id(&self) -> Option<Uuid> {
        match self.actor {
            Actor::Rule { rule_id, .. } => Some(rule_id),
            Actor::User { .. } | Actor::ApiKey { .. } | Actor::System => None,
        }
    }

    /// How many rule runs led to this operation (0 for people and keys).
    pub fn rule_depth(&self) -> u8 {
        match self.actor {
            Actor::Rule { depth, .. } => depth,
            Actor::User { .. } | Actor::ApiKey { .. } | Actor::System => 0,
        }
    }

    /// A context for trusted system work (operator CLI, seeding) with every permission.
    pub fn system(workspace_id: WorkspaceId) -> Self {
        Self {
            workspace_id,
            actor: Actor::System,
            role: None,
            permissions: Permission::ALL.iter().copied().collect(),
            meta: RequestMeta::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_strings_round_trip() {
        for p in Permission::ALL {
            assert_eq!(p.as_str().parse::<Permission>().expect("parses"), *p);
            let json = serde_json::to_string(p).expect("serialises");
            assert_eq!(json, format!("\"{}\"", p.as_str()));
        }
        assert!("workspace:explode".parse::<Permission>().is_err());
    }

    #[test]
    fn role_defaults_follow_the_spec() {
        let owner = Role::Owner.default_permissions();
        let admin = Role::Admin.default_permissions();
        assert_eq!(owner.len(), Permission::ALL.len());
        assert!(admin.is_subset(&owner));
        for p in [
            Permission::BillingRead,
            Permission::BillingWrite,
            Permission::WorkspaceClose,
        ] {
            assert!(!admin.contains(&p), "admin must not have {p}");
        }
        for role in [Role::Agent, Role::Finance, Role::Viewer, Role::Talent] {
            let perms = role.default_permissions();
            assert!(
                !perms.contains(&Permission::MemberInvite),
                "{role} must not invite"
            );
            assert!(
                !perms.contains(&Permission::AuditRead),
                "{role} must not read the audit log"
            );
        }
        assert!(
            !Role::Talent
                .default_permissions()
                .contains(&Permission::MemberRead)
        );
        let finance = Role::Finance.default_permissions();
        assert!(
            finance.contains(&Permission::BillingRead)
                && !finance.contains(&Permission::BillingWrite)
        );
    }

    #[test]
    fn authorize_checks_the_permission_set() {
        let mut ctx = Ctx::system(WorkspaceId::new());
        ctx.permissions = [Permission::MemberRead].into();
        assert!(ctx.authorize(Permission::MemberRead).is_ok());
        assert_eq!(
            ctx.authorize(Permission::MemberInvite),
            Err(AuthzError(Permission::MemberInvite))
        );
    }
}
