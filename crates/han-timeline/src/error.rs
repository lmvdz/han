//! Reason codes for peer data and local errors (§18).

use thiserror::Error;

use crate::ids::BatchId;

/// Why a receiver rejects an envelope (§18). Never applied to the local writer's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Error)]
pub enum Reason {
    /// Not deterministic CBOR, wrong shape, bad zstd, unknown kind or field.
    #[error("malformed")]
    Malformed,
    /// Over a limit of §17.
    #[error("too_large")]
    TooLarge,
    /// Not a history the receiver keeps.
    #[error("wrong_history")]
    WrongHistory,
    /// The key is not in the actor's profile.
    #[error("unknown_key")]
    UnknownKey,
    /// The signature does not verify.
    #[error("bad_signature")]
    BadSignature,
    /// Another batch with the same op ids and different content.
    #[error("equivocation")]
    Equivocation,
    /// A batch overlapping a held batch of the same replica.
    #[error("overlap")]
    Overlap,
    /// Counters violate TL-04 or TL-76.
    #[error("lamport")]
    Lamport,
    /// TL-10, TL-15.
    #[error("genesis")]
    Genesis,
    /// TL-20 – TL-23, TL-27.
    #[error("timeline")]
    Timeline,
    /// An op is invalid at its parent.
    #[error("invalid_op")]
    InvalidOp,
}

impl Reason {
    /// The code of §18.
    pub fn code(self) -> &'static str {
        match self {
            Reason::Malformed => "malformed",
            Reason::TooLarge => "too_large",
            Reason::WrongHistory => "wrong_history",
            Reason::UnknownKey => "unknown_key",
            Reason::BadSignature => "bad_signature",
            Reason::Equivocation => "equivocation",
            Reason::Overlap => "overlap",
            Reason::Lamport => "lamport",
            Reason::Genesis => "genesis",
            Reason::Timeline => "timeline",
            Reason::InvalidOp => "invalid_op",
        }
    }
}

impl From<crate::codec::DecodeError> for Reason {
    fn from(e: crate::codec::DecodeError) -> Self {
        match e {
            crate::codec::DecodeError::Malformed => Reason::Malformed,
            crate::codec::DecodeError::TooLarge => Reason::TooLarge,
        }
    }
}

/// What integrating an envelope did (TL-100 – TL-104, §18).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// Integrated.
    Integrated,
    /// Already held, identical: ignored.
    Duplicate,
    /// Dependencies missing: held back (TL-102).
    Held,
    /// Not authorised: kept aside until vouched (TL-103).
    Quarantined,
    /// A held-back batch was dropped (TL-102); it comes back in a later sync round.
    Missing,
    /// A quarantined batch was dropped (TL-103).
    Unauthorized,
    /// Rejected.
    Rejected(Reason),
}

impl Verdict {
    /// The text form the vectors use: `integrated`, `held`, …, `rejected:<code>`.
    pub fn code(&self) -> String {
        match self {
            Verdict::Integrated => "integrated".into(),
            Verdict::Duplicate => "duplicate".into(),
            Verdict::Held => "held".into(),
            Verdict::Quarantined => "quarantined".into(),
            Verdict::Missing => "missing".into(),
            Verdict::Unauthorized => "unauthorized".into(),
            Verdict::Rejected(r) => format!("rejected:{}", r.code()),
        }
    }
}

/// The result of handing one envelope to a history: its verdict, and what that delivery did
/// to other batches (held-back or quarantined batches integrated, dropped or rejected).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The delivered envelope's batch id, when it decoded far enough to have one.
    pub batch: Option<BatchId>,
    /// The verdict on the delivered envelope.
    pub verdict: Verdict,
    /// Other batches whose fate this delivery decided, in order.
    pub others: Vec<(BatchId, Verdict)>,
}

/// Local errors (§18), never on the wire, plus storage failures.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum Error {
    /// An edit outside the text.
    #[error("out_of_range")]
    OutOfRange,
    /// An edit into a seal.
    #[error("sealed")]
    Sealed,
    /// No such object, or not of the needed kind.
    #[error("unknown_object")]
    UnknownObject,
    /// No such timeline.
    #[error("unknown_timeline")]
    UnknownTimeline,
    /// More than one timeline has this name.
    #[error("ambiguous_name")]
    AmbiguousName,
    /// The anchor names no character of the text.
    #[error("unknown_anchor")]
    UnknownAnchor,
    /// A timeline merged into itself.
    #[error("merge_self")]
    MergeSelf,
    /// A conflict without a resolution, or a resolution that does not fit its conflict.
    #[error("unresolved")]
    Unresolved,
    /// Nothing to merge.
    #[error("up_to_date")]
    UpToDate,
    /// A version names an op this history does not hold.
    #[error("unknown_op")]
    UnknownOp,
    /// A local write that the history itself rejects.
    #[error("rejected: {0}")]
    Rejected(Reason),
    /// Stored data of a newer format version (TL-125).
    #[error("unsupported format version {0}")]
    Format(u32),
    /// The store failed.
    #[error("store: {0}")]
    Store(String),
}
