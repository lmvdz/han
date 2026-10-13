//! Read rights (P-02) and session rights (P-15).

use serde::Serialize;

use super::doc::{Role, SessionLevel};
use super::guard::{reads, role_of, server_of};
use super::state::{Session, SpaceState};
use crate::types::Seq;

/// P-02: whether `reader` (an actor, or `@:server` for a node) may read the record at `seq`. A
/// member reads the seqs at which it held a readable role; a server reads what its users could.
pub fn can_read(state: &SpaceState, reader: &str, seq: Seq) -> bool {
    if state.policy.visibility == super::doc::Visibility::Public
        || state.space.starts_with("profile:")
    {
        return true;
    }
    let held = |actor: &str| {
        reads(role_of(state, actor))
            || state.role_history.get(actor).is_some_and(|hist| {
                hist.iter()
                    .any(|h| h.from <= seq && h.to.is_none_or(|to| seq < to))
            })
    };
    if let Some(server) = reader.strip_prefix("@:") {
        if server == state.home {
            return true; // the home always reads its own space
        }
        if state.policy.denies(server) {
            return false;
        }
        return state
            .roles
            .keys()
            .chain(state.role_history.keys())
            .any(|a| server_of(a) == server && held(a));
    }
    held(reader)
}

/// What an actor may do with a live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SessionAccess {
    /// Watch its output.
    pub watch: bool,
    /// Read its full context.
    pub read: bool,
    /// Steer it.
    pub steer: bool,
}

/// Live: open (closed sessions are not in the state) and its actor holds a role.
pub(super) fn live(state: &SpaceState, sess: &Session) -> bool {
    reads(role_of(state, &sess.actor))
}

/// P-15: what `actor` may do with the session `session`. The session's own actor has all three; a
/// level of `nobody` grants nothing to anyone else; a session that is not live grants nothing.
pub fn session_access(state: &SpaceState, actor: &str, session: &str) -> SessionAccess {
    let none = SessionAccess {
        watch: false,
        read: false,
        steer: false,
    };
    let Some(s) = state.sessions.get(session).filter(|s| live(state, s)) else {
        return none;
    };
    if s.actor == actor {
        return SessionAccess {
            watch: true,
            read: true,
            steer: true,
        };
    }
    let rank = role_of(state, actor).and_then(Role::rank);
    let allowed = |need: SessionLevel| {
        let need = match need {
            SessionLevel::Owner => 0,
            SessionLevel::Member => 1,
            SessionLevel::Guest => 2,
            SessionLevel::Nobody => return false,
        };
        rank.is_some_and(|r| r <= need)
    };
    let rights = &state.policy.sessions;
    SessionAccess {
        watch: allowed(rights.watch),
        read: allowed(rights.read),
        steer: allowed(rights.steer),
    }
}
