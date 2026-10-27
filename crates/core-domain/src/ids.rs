//! Typed ids. Workspace and user ids are the two that decide what a request
//! may see, so they get their own types and cannot be swapped by accident.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema)]
        #[serde(transparent)]
        #[sqlx(transparent)]
        #[schema(value_type = String, format = Uuid)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A new time-ordered (UUIDv7) id.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }
    };
}

id_type!(
    /// A workspace: one tenant's data boundary.
    WorkspaceId
);
id_type!(
    /// A person who can sign in.
    UserId
);

/// A new UUIDv7 for rows without a dedicated id type.
pub fn new_id() -> Uuid {
    Uuid::now_v7()
}
