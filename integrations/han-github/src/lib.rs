//! `han-github`: HAN's GitHub integration, a separate process and an actor over the node API.
//!
//! It reads a team's repositories with its own read-only GitHub App (installation tokens, `issues: read`, conditional
//! requests), surfaces issues in a space as `github.issue/1` stream items, and suggests a task
//! (`TaskCreate{external: "github:owner/repo#N"}`) for an open issue that gains the `agent-ready` label. See
//! the README for the design and the item's shape.

pub mod config;
pub mod github;
pub mod mint;
pub mod sync;

pub use config::{Config, ConfigError};
pub use github::{Fetched, GhError, Issue, Reader};
pub use mint::{JwtSource, ReadMinter, RsaJwt};
pub use sync::{PROFILE, Source, SyncError, Syncer, Tick};
