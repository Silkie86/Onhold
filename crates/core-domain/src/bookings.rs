//! Value types of bookings: a booking's status and which changes of status
//! are allowed, the kinds of availability, a quote's status, and quote line
//! totals.

use serde::{Deserialize, Serialize};

macro_rules! db_enum {
    ($(#[$meta:meta])* $name:ident, $pg:literal, [$($variant:ident => $s:literal),+ $(,)?]) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type, utoipa::ToSchema)]
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

db_enum!(
    /// Where a booking stands. A hold pencils the date in; pending means the
    /// buyer has accepted and the paperwork (contract, deposit) is under way;
    /// confirmed is firm and may not clash.
    BookingStatus, "booking_status", [
        Hold => "hold",
        Pending => "pending",
        Confirmed => "confirmed",
        Cancelled => "cancelled",
        Completed => "completed",
        Expired => "expired",
    ]
);

impl BookingStatus {
    /// Whether a person (or rule) may move a booking from `self` to `to`.
    /// Expiry and completion also happen by themselves (the worker), along
    /// the same arrows.
    pub fn can_become(self, to: BookingStatus) -> bool {
        use BookingStatus::*;
        matches!(
            (self, to),
            (Hold, Pending | Confirmed | Cancelled | Expired)
                | (Pending, Confirmed | Cancelled)
                | (Confirmed, Cancelled | Completed)
                // A lapsed hold can be put back while the date is still free.
                | (Expired, Hold)
        )
    }

    /// No further changes of status.
    pub fn is_final(self) -> bool {
        matches!(self, BookingStatus::Cancelled | BookingStatus::Completed)
    }

    /// The event emitted on entering this status: `booking.confirmed`…
    pub fn topic(self) -> &'static str {
        match self {
            BookingStatus::Hold => "booking.held",
            BookingStatus::Pending => "booking.pending",
            BookingStatus::Confirmed => "booking.confirmed",
            BookingStatus::Cancelled => "booking.cancelled",
            BookingStatus::Completed => "booking.completed",
            BookingStatus::Expired => "booking.expired",
        }
    }
}

db_enum!(
    /// A stretch of a talent's time: unavailable (blocks confirming a
    /// booking), tentative (warns), or available only (the talent works
    /// only in these stretches; shown, not enforced).
    AvailabilityKind, "availability_kind", [
        Unavailable => "unavailable",
        Tentative => "tentative",
        AvailableOnly => "available_only",
    ]
);

db_enum!(
    /// Where a quote stands. Withdrawn means a newer revision replaced it.
    QuoteStatus, "quote_status", [
        Draft => "draft",
        Sent => "sent",
        Viewed => "viewed",
        Accepted => "accepted",
        Declined => "declined",
        Expired => "expired",
        Withdrawn => "withdrawn",
    ]
);

impl QuoteStatus {
    /// Out with the buyer, who may still accept or decline it.
    pub fn is_open(self) -> bool {
        matches!(self, QuoteStatus::Sent | QuoteStatus::Viewed)
    }

    pub fn topic(self) -> &'static str {
        match self {
            QuoteStatus::Draft => "quote.created",
            QuoteStatus::Sent => "quote.sent",
            QuoteStatus::Viewed => "quote.viewed",
            QuoteStatus::Accepted => "quote.accepted",
            QuoteStatus::Declined => "quote.declined",
            QuoteStatus::Expired => "quote.expired",
            QuoteStatus::Withdrawn => "quote.withdrawn",
        }
    }
}

/// The most a line or a quote may come to: a trillion in minor units, far
/// beyond any booking and well inside `i64`.
pub const MAX_AMOUNT_MINOR: i64 = 1_000_000_000_000_000;

/// A line's amount: quantity (in thousandths, so 1.5 hours is 1500) times
/// the unit price, rounded half away from zero. `None` if it is too large.
pub fn line_amount(quantity_milli: i64, unit_minor: i64) -> Option<i64> {
    let raw = i128::from(quantity_milli) * i128::from(unit_minor);
    let rounded = if raw >= 0 {
        (raw + 500) / 1000
    } else {
        (raw - 500) / 1000
    };
    i64::try_from(rounded)
        .ok()
        .filter(|v| v.abs() <= MAX_AMOUNT_MINOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_move_only_along_the_arrows() {
        use BookingStatus::*;
        assert!(Hold.can_become(Confirmed));
        assert!(Hold.can_become(Pending));
        assert!(Pending.can_become(Confirmed));
        assert!(Confirmed.can_become(Completed));
        assert!(Expired.can_become(Hold));
        assert!(!Confirmed.can_become(Hold));
        assert!(!Cancelled.can_become(Hold));
        assert!(!Completed.can_become(Cancelled));
        assert!(!Pending.can_become(Expired));
        for s in BookingStatus::ALL {
            assert!(!s.can_become(*s), "{s} to itself is not a change");
            assert_eq!(BookingStatus::parse(s.as_str()), Some(*s));
        }
    }

    #[test]
    fn line_amounts_round_half_away_from_zero() {
        assert_eq!(line_amount(1000, 125_000), Some(125_000));
        assert_eq!(line_amount(1500, 999), Some(1499)); // 1498.5
        assert_eq!(line_amount(2500, 1), Some(3)); // 2.5
        assert_eq!(line_amount(-1000, 500), Some(-500));
        assert_eq!(line_amount(i64::MAX, i64::MAX), None);
    }
}
