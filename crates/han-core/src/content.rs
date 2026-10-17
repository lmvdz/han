//! An entry's content, typed per kind (R-09): the one schema of every core kind's content.
//! [`Content::parse`] decodes a content strictly into its kind's type and checks what the types
//! cannot say. A record keeps its content as the Json it was sent as; the content hash is over that.

use serde::{Deserialize, Deserializer, Serialize};

use crate::error::Error;
use crate::grammar as g;
use crate::json::{self, Value};
use crate::limits::MAX_LIST;
use crate::types::{BlobRef, Link};

/// Content of `space.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceCreateContent {
    /// The space's name, at most 256 bytes.
    pub name: String,
}

/// Content of `message`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageContent {
    /// The text, at most 32,768 bytes.
    pub text: String,
    /// Attached blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<BlobRef>>,
    /// Pointers into other spaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Link>>,
}

/// Content of `task`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskContent {
    /// What is asked, at most 32,768 bytes.
    pub ask: String,
    /// Attached blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<BlobRef>>,
    /// Pointers into other spaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Link>>,
}

/// Content of `task.update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUpdateContent {
    /// A note, at most 32,768 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Attached blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<BlobRef>>,
}

/// What an effect would do: the plan of `effect.propose`, digested into its `planDigest`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Plan {
    /// The action name; equals the body's `action`.
    pub action: String,
    /// What it acts on, at most 1,024 bytes.
    pub target: String,
    /// Arguments: plain Json, `null` included.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub args: Option<Value>,
    /// Arguments too large to carry inline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_blob: Option<BlobRef>,
}

/// A field whose every value, `null` included, counts as present.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

/// Content of `effect.propose`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposeContent {
    /// The plan.
    pub plan: Plan,
}

/// Content of `effect.approve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApproveContent {
    /// A note, at most 4,096 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The record the body's `evidence` names (P-17).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BlobRef>,
}

/// Content of `effect.cancel`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelContent {
    /// Why, at most 1,024 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the executor saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BlobRef>,
}

/// Content of `effect.outcome`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutcomeContent {
    /// What the executor saw.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<BlobRef>,
}

/// Content of `effect.resolve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveContent {
    /// Why the outcome is settled so.
    pub evidence: BlobRef,
}

/// Content of `checkpoint`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointContent {
    /// The snapshot.
    pub blobs: Vec<BlobRef>,
}

/// Content of `redact`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactContent {
    /// Why, at most 1,024 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Content of `session.open`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionOpenContent {
    /// A title, at most 256 bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Content of `session.input`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionInputContent {
    /// The input, at most 32,768 bytes.
    pub text: String,
    /// Attached blobs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blobs: Option<Vec<BlobRef>>,
}

/// An entry's content, typed by kind. Only the kinds below have content; an extension kind's
/// content is plain Json and stays a [`Value`].
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// `space.create`
    SpaceCreate(SpaceCreateContent),
    /// `message`
    Message(MessageContent),
    /// `task`
    Task(TaskContent),
    /// `task.update`
    TaskUpdate(TaskUpdateContent),
    /// `effect.propose`
    Propose(ProposeContent),
    /// `effect.approve`
    Approve(ApproveContent),
    /// `effect.cancel`
    Cancel(CancelContent),
    /// `effect.outcome`
    Outcome(OutcomeContent),
    /// `effect.resolve`
    Resolve(ResolveContent),
    /// `checkpoint`
    Checkpoint(CheckpointContent),
    /// `redact`
    Redact(RedactContent),
    /// `session.open`
    SessionOpen(SessionOpenContent),
    /// `session.input`
    SessionInput(SessionInputContent),
}

fn typed<T: serde::de::DeserializeOwned>(content: &Value) -> Result<T, Error> {
    json::from_value_strict(content).map_err(|e| Error::invalid(format!("content: {e}")))
}

/// `Ok` if `ok`, else `invalid` naming the content field.
fn field(ok: bool, name: &str) -> Result<(), Error> {
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!("content: bad field {name}")))
    }
}

fn text<const N: usize>(s: &str) -> bool {
    s.len() <= N
}

fn opt_text<const N: usize>(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(text::<N>)
}

/// At most 64 blob refs, each valid.
pub(crate) fn blobs(b: &[BlobRef]) -> bool {
    b.len() <= MAX_LIST && b.iter().all(BlobRef::is_valid)
}

fn opt_blob(b: &Option<BlobRef>) -> bool {
    b.as_ref().is_none_or(BlobRef::is_valid)
}

fn opt_blobs(b: &Option<Vec<BlobRef>>) -> bool {
    b.as_deref().is_none_or(blobs)
}

fn opt_links(l: &Option<Vec<Link>>) -> bool {
    l.as_deref()
        .is_none_or(|l| l.len() <= MAX_LIST && l.iter().all(Link::is_valid))
}

impl Content {
    /// Whether a core kind must carry content: `Some(true)` if it must, `Some(false)` if it may,
    /// `None` if it has none (or `kind` is not a core kind).
    pub fn required(kind: &str) -> Option<bool> {
        match kind {
            "space.create" | "message" | "task" | "effect.propose" | "effect.resolve"
            | "checkpoint" | "session.input" => Some(true),
            "task.update" | "effect.approve" | "effect.cancel" | "effect.outcome" | "redact"
            | "session.open" => Some(false),
            _ => None,
        }
    }

    /// Decodes `content` strictly into the type of the core kind `kind` and checks it ([`Content::check`]).
    pub fn parse(kind: &str, content: &Value) -> Result<Content, Error> {
        if !matches!(content, Value::Object(_)) {
            return Err(Error::invalid("content: not an object"));
        }
        let typed = match kind {
            "space.create" => Content::SpaceCreate(typed(content)?),
            "message" => Content::Message(typed(content)?),
            "task" => Content::Task(typed(content)?),
            "task.update" => Content::TaskUpdate(typed(content)?),
            "effect.propose" => Content::Propose(typed(content)?),
            "effect.approve" => Content::Approve(typed(content)?),
            "effect.cancel" => Content::Cancel(typed(content)?),
            "effect.outcome" => Content::Outcome(typed(content)?),
            "effect.resolve" => Content::Resolve(typed(content)?),
            "checkpoint" => Content::Checkpoint(typed(content)?),
            "redact" => Content::Redact(typed(content)?),
            "session.open" => Content::SessionOpen(typed(content)?),
            "session.input" => Content::SessionInput(typed(content)?),
            _ => return Err(Error::invalid("this kind has no content")),
        };
        typed.check()?;
        Ok(typed)
    }

    /// Checks what the types cannot say: text sizes, list sizes, and every blob ref and link.
    pub fn check(&self) -> Result<(), Error> {
        match self {
            Content::SpaceCreate(c) => field(text::<256>(&c.name), "name"),
            Content::Message(c) => {
                field(text::<32768>(&c.text), "text")?;
                field(opt_blobs(&c.blobs), "blobs")?;
                field(opt_links(&c.links), "links")
            }
            Content::Task(c) => {
                field(text::<32768>(&c.ask), "ask")?;
                field(opt_blobs(&c.blobs), "blobs")?;
                field(opt_links(&c.links), "links")
            }
            Content::TaskUpdate(c) => {
                field(opt_text::<32768>(&c.text), "text")?;
                field(opt_blobs(&c.blobs), "blobs")
            }
            Content::Propose(c) => {
                let p = &c.plan;
                field(
                    g::is_action(&p.action)
                        && text::<1024>(&p.target)
                        && p.args_blob.as_ref().is_none_or(BlobRef::is_valid)
                        && p.args.as_ref().is_none_or(g::is_opaque_json),
                    "plan",
                )
            }
            Content::Approve(c) => {
                field(opt_text::<4096>(&c.note), "note")?;
                field(opt_blob(&c.evidence), "evidence")
            }
            Content::Cancel(c) => {
                field(opt_text::<1024>(&c.reason), "reason")?;
                field(opt_blob(&c.evidence), "evidence")
            }
            Content::Outcome(c) => field(opt_blob(&c.evidence), "evidence"),
            Content::Resolve(c) => field(c.evidence.is_valid(), "evidence"),
            Content::Checkpoint(c) => field(blobs(&c.blobs), "blobs"),
            Content::Redact(c) => field(opt_text::<1024>(&c.reason), "reason"),
            Content::SessionOpen(c) => field(opt_text::<256>(&c.title), "title"),
            Content::SessionInput(c) => {
                field(text::<32768>(&c.text), "text")?;
                field(opt_blobs(&c.blobs), "blobs")
            }
        }
    }
}
