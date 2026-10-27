//! Entities and value types shared by every crate: ids, roles and
//! permissions, the request context and its single `authorize` check,
//! validated workspace settings, and the small amount of cryptography the
//! core needs (token generation, hashing, encryption at rest).
//!
//! This crate does no I/O.

/// A Rust enum for a Postgres enum, with `as_str`, `parse`, `ALL` and serde
/// names matching the database's.
macro_rules! db_enum {
    ($(#[$meta:meta])* $name:ident, $pg:literal, [$($variant:ident => $s:literal),+ $(,)?]) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, sqlx::Type, utoipa::ToSchema)]
        #[sqlx(type_name = $pg, rename_all = "snake_case")]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant,)+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant,)+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $s,)+
                }
            }

            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s.trim())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub mod authz;
pub mod bookings;
pub mod crm;
pub mod crypto;
pub mod finance;
pub mod ids;
pub mod money;
pub mod workspace;

pub use authz::{Actor, AuthzError, Ctx, Permission, Role};
pub use bookings::{AvailabilityKind, BookingStatus, QuoteStatus};
pub use crm::TalentKind;
pub use finance::{InvoiceStatus, Rate};
pub use ids::{UserId, WorkspaceId};
pub use money::Money;
