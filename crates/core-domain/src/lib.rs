//! Entities and value types shared by every crate: ids, roles and
//! permissions, the request context and its single `authorize` check,
//! validated workspace settings, and the small amount of cryptography the
//! core needs (token generation, hashing, encryption at rest).
//!
//! This crate does no I/O.

pub mod authz;
pub mod bookings;
pub mod crm;
pub mod crypto;
pub mod ids;
pub mod money;
pub mod workspace;

pub use authz::{Actor, AuthzError, Ctx, Permission, Role};
pub use bookings::{AvailabilityKind, BookingStatus, QuoteStatus};
pub use crm::TalentKind;
pub use ids::{UserId, WorkspaceId};
pub use money::Money;
