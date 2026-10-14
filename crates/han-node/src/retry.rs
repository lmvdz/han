//! The one table that turns what a home (or the path to it) answered into a [`NodeError`] and a decision:
//! stop, try later, follow the space to another home, or give up on the link until someone fixes the setup.
//! Every path that talks to a home (append, hello/connect, follow, the outbox) asks it, so the same answer
//! never means two things.

use han_net::NetError;
use han_seq::Code;

use crate::api::NodeError;

/// What the node was doing when the answer came.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Via {
    /// An `append` or another call on behalf of a command or the outbox.
    Call,
    /// Dialing the home and saying `hello` as this device's actor.
    Connect,
    /// Subscribing to a followed space.
    Follow,
}

/// What to do about an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// A definite answer: do not ask again.
    Final,
    /// Ask again later, after at least `after_ms` if given. It can resolve on its own.
    Retry { after_ms: Option<u64> },
    /// The space lives at another server now (§4.1: the same request there).
    Moved { to: String },
    /// Waiting will not help: the setup is wrong (our actor is unknown to the home, a key or pin does not
    /// match). The link stops trying quickly and says why.
    Misconfigured,
}

/// The decision for a wire `code` (with the `movedTo` of the error) when the node was doing `via`.
pub(crate) fn decide(
    via: Via,
    code: Code,
    moved_to: Option<&str>,
    after_ms: Option<u64>,
) -> Disposition {
    match (via, code) {
        (_, Code::Limit | Code::Unavailable) => Disposition::Retry { after_ms },
        (_, Code::Moved) => Disposition::Moved {
            to: moved_to.unwrap_or_default().to_owned(),
        },
        (Via::Call, Code::Invalid | Code::Denied | Code::NotFound) => Disposition::Final,
        // not admitted yet, or not created yet: it can change without anyone fixing the setup
        (Via::Follow, Code::Denied | Code::NotFound) => Disposition::Retry { after_ms: None },
        (Via::Follow, Code::Invalid) => Disposition::Final,
        (Via::Connect, Code::Invalid | Code::Denied | Code::NotFound) => Disposition::Misconfigured,
    }
}

/// The decision for a failed call of any kind. An error that is no answer of the home (the network, a closed
/// stream) is a reason to retry; a protocol violation while connecting (a home key or node that does not
/// match the pin) is a setup fault.
pub(crate) fn disposition(via: Via, e: &NetError) -> Disposition {
    match e {
        NetError::Remote(w) => decide(via, w.code, w.moved_to.as_deref(), w.retry_after_ms),
        NetError::Protocol(_) if via == Via::Connect => Disposition::Misconfigured,
        _ => Disposition::Retry { after_ms: None },
    }
}

/// The error a command's caller sees. `Moved` and `Limit` are `unavailable` (the caller may try again).
pub(crate) fn node_error(e: &NetError) -> NodeError {
    match e {
        NetError::Remote(w) => match w.code {
            Code::Denied => NodeError::denied(&w.message),
            Code::Invalid => NodeError::invalid(&w.message),
            Code::NotFound => NodeError::not_found(&w.message),
            Code::Limit | Code::Unavailable => NodeError::Unavailable {
                message: w.message.clone(),
                retry_after_ms: w.retry_after_ms,
                pending: None,
            },
            Code::Moved => NodeError::unavailable(format!(
                "the space moved to {}",
                w.moved_to.as_deref().unwrap_or("another home")
            )),
        },
        other => NodeError::unavailable(other.to_string()),
    }
}

/// The error for a stored rejection: the code the home answered, and its text.
pub(crate) fn stored_error(code: Option<&str>, message: String) -> NodeError {
    match code {
        Some("invalid") => NodeError::invalid(message),
        Some("not_found") => NodeError::not_found(message),
        _ => NodeError::denied(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODES: [Code; 6] = [
        Code::Denied,
        Code::Invalid,
        Code::NotFound,
        Code::Limit,
        Code::Unavailable,
        Code::Moved,
    ];
    const VIAS: [Via; 3] = [Via::Call, Via::Connect, Via::Follow];

    #[test]
    fn every_code_has_a_decision_in_every_context() {
        for via in VIAS {
            for code in CODES {
                let _ = decide(via, code, Some("b.example"), None);
            }
        }
    }

    #[test]
    fn only_answers_that_can_pass_by_themselves_are_retried() {
        for via in VIAS {
            for code in CODES {
                let retried = matches!(decide(via, code, None, None), Disposition::Retry { .. });
                let passes_alone = matches!(code, Code::Limit | Code::Unavailable)
                    || (via == Via::Follow && matches!(code, Code::Denied | Code::NotFound));
                assert_eq!(retried, passes_alone, "{via:?} {code:?}");
            }
        }
    }

    #[test]
    fn a_home_that_does_not_know_us_is_a_setup_fault_not_a_retry() {
        for code in [Code::NotFound, Code::Denied, Code::Invalid] {
            assert_eq!(
                decide(Via::Connect, code, None, None),
                Disposition::Misconfigured
            );
        }
        assert_eq!(
            decide(Via::Call, Code::Invalid, None, None),
            Disposition::Final
        );
    }

    #[test]
    fn moved_names_where_and_retry_after_is_kept() {
        assert_eq!(
            decide(Via::Call, Code::Moved, Some("b.example"), None),
            Disposition::Moved {
                to: "b.example".into()
            }
        );
        assert_eq!(
            decide(Via::Call, Code::Unavailable, None, Some(1000)),
            Disposition::Retry {
                after_ms: Some(1000)
            }
        );
    }

    #[test]
    fn network_errors_retry_and_a_pin_mismatch_is_a_setup_fault() {
        assert_eq!(
            disposition(Via::Connect, &NetError::Dial("x".into())),
            Disposition::Retry { after_ms: None }
        );
        assert_eq!(
            disposition(Via::Connect, &NetError::Protocol("abort: key".into())),
            Disposition::Misconfigured
        );
        assert_eq!(
            disposition(Via::Call, &NetError::Protocol("x".into())),
            Disposition::Retry { after_ms: None }
        );
    }
}
