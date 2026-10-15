//! `han-review`: the tier-1 machine reviewer of the landing lane (R33; PLAN.md §12 "Machine review").
//!
//! For one open `vcs.land` proposal of tier 0 or 1 it checks that the reviewer is independent of the author
//! ([`independence`]), recomputes the delta of the bundle and checks it against the plan's `deltaDigest`,
//! asks a model for a typed verdict ([`record::Review`]) and writes a [`ReviewRecord`] over the digest. The
//! model's answer is data: it has no field that names an action, and an answer that does not parse is
//! `needs_human` ([`parse`]).
//!
//! The crate carries **no authority**. Turning an `approve` record into `effect.approve` is
//! [`Reviewer::disposition`], which says yes only when the reviewer's `authority` flag is on and the caller
//! reports that Policy allows this actor to approve; both are off by default. Today's PolicyDoc can not
//! confine an approver to tier 1: an approval rule is `{action, approvers}`, the first rule for the action
//! applies, and one approval from any listed approver satisfies E3 whatever the plan's `tier` says. Listing
//! the reviewer under `vcs.land` would let it approve tiers 2 and 3 too. Nothing in the repository starts
//! this crate.
//!
//! * [`record`]: the record, the verdict and the output schema.
//! * [`independence`]: the vendor and operator-chain rules.
//! * [`backend`]: the model seam, a fake for tests and an OpenAI-compatible client (OmniRoute).
//! * [`review`]: the pipeline.

pub mod backend;
pub mod independence;
pub mod parse;
pub mod record;
pub mod review;

pub use backend::{FakeBackend, KeyStyle, ModelBackend, ModelReply, ModelRequest, OpenAiCompat};
pub use independence::{AuthorInfo, Refusal, ReviewerInfo, independent, vendor_of};
pub use record::{Finding, Review, ReviewRecord, Severity, Usage, Verdict};
pub use review::{
    Disposition, MAX_TIER, ReviewError, ReviewInput, Reviewer, ReviewerConfig, SYSTEM_PROMPT,
    UNRESOLVED, prompt_hash,
};
