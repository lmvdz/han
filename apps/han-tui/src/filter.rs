//! The board's filter box: space-separated terms, parsed here and sent to the node as the query's filter. The
//! node does the filtering (board spec BRD-18); this only turns text into a [`Filter`].

use han_node::api::{Filter, SessionState};

/// A parsed filter box.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// What goes to the node.
    pub filter: Filter,
    /// One message per term that could not be used; the rest of the filter still applies.
    pub errors: Vec<String>,
}

fn state(name: &str) -> Option<SessionState> {
    Some(match name {
        "working" => SessionState::Working,
        "waiting-approval" => SessionState::WaitingApproval,
        "blocked" => SessionState::Blocked,
        "idle" => SessionState::Idle,
        "done" => SessionState::Done,
        "stale" => SessionState::Stale,
        "failed" => SessionState::Failed,
        _ => return None,
    })
}

/// Parses the filter box. `me` is the local actor, for `is:mine`.
pub fn parse(text: &str, me: Option<&str>) -> Parsed {
    let mut out = Parsed::default();
    let mut words: Vec<&str> = Vec::new();
    for term in text.split_whitespace() {
        let Some((key, value)) = term.split_once(':') else {
            words.push(term);
            continue;
        };
        if value.is_empty() {
            out.errors.push(format!("Empty value for '{key}'"));
            continue;
        }
        match key {
            "person" | "agent" | "actor" => {
                if out.filter.actor.is_some() {
                    out.errors
                        .push("Only one person or agent term can be used".to_owned());
                } else {
                    out.filter.actor = Some(value.to_owned());
                }
            }
            "issue" => out.filter.issue = Some(value.to_owned()),
            "state" => match state(value) {
                Some(s) => {
                    if !out.filter.states.contains(&s) {
                        out.filter.states.push(s);
                    }
                }
                None => out.errors.push(format!("Unknown state '{value}'")),
            },
            "is" => match (value, me) {
                ("closed", _) => out.filter.include_closed = true,
                ("mine", Some(me)) => out.filter.actor = Some(me.to_owned()),
                ("mine", None) => out.errors.push("is:mine needs the node's actor".to_owned()),
                _ => out.errors.push(format!("Unknown filter 'is:{value}'")),
            },
            // The node searches these among the row's words.
            "harness" | "repo" | "text" => words.push(value),
            _ => out.errors.push(format!("Unknown filter key '{key}'")),
        }
    }
    if !words.is_empty() {
        out.filter.text = Some(words.join(" "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_become_a_filter() {
        let p = parse("person:@ada state:working state:idle auth is:closed", None);
        assert_eq!(p.filter.actor.as_deref(), Some("@ada"));
        assert_eq!(
            p.filter.states,
            vec![SessionState::Working, SessionState::Idle]
        );
        assert_eq!(p.filter.text.as_deref(), Some("auth"));
        assert!(p.filter.include_closed);
        assert!(p.errors.is_empty());
    }

    #[test]
    fn bad_terms_are_reported_and_the_rest_applies() {
        let p = parse("colour:red state:sleepy issue:#4", None);
        assert_eq!(p.errors.len(), 2);
        assert_eq!(p.errors[0], "Unknown filter key 'colour'");
        assert_eq!(p.filter.issue.as_deref(), Some("#4"));
    }
}
