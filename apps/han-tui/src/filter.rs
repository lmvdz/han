//! The board's filter box. The grammar is the node's (`Filter::parse`): the node says which keys exist, so the
//! box and every other face agree, and an unknown key is an error rather than a word to search for. The node
//! does the filtering (board spec BRD-18); this only reports what is wrong with the text.

use han_node::api::{Filter, NodeError};

/// A parsed filter box.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    /// What goes to the node (the default filter when the text is wrong).
    pub filter: Filter,
    /// What was wrong with the text, in one short line each. When there is one, the filter is not applied.
    pub errors: Vec<String>,
}

/// The first part of a node error, without the list of keys it ends with.
fn short(e: &NodeError) -> String {
    let text = match e {
        NodeError::Invalid { message } => message.as_str(),
        other => return other.to_string(),
    };
    let head = text.split(" (").next().unwrap_or(text);
    let mut chars = head.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Parses the filter box.
pub fn parse(text: &str) -> Parsed {
    match Filter::parse(text) {
        Ok(filter) => Parsed {
            filter,
            errors: Vec::new(),
        },
        Err(e) => Parsed {
            filter: Filter::default(),
            errors: vec![short(&e)],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use han_node::api::SessionState;

    #[test]
    fn terms_become_the_nodes_filter() {
        let p = parse(
            "person:@ada state:working state:idle path:src/** repo:r mine:true auth is:closed",
        );
        assert_eq!(p.filter.person, vec!["@ada".to_owned()]);
        assert_eq!(
            p.filter.states,
            vec![SessionState::Working, SessionState::Idle]
        );
        assert_eq!(p.filter.path, vec!["src/**".to_owned()]);
        assert_eq!(p.filter.repo, vec!["r".to_owned()]);
        assert!(p.filter.mine);
        assert_eq!(p.filter.text.as_deref(), Some("auth"));
        assert!(p.filter.include_closed);
        assert!(p.errors.is_empty());
    }

    #[test]
    fn nothing_is_folded_into_free_text() {
        let p = parse("harness:pi repo:o/r label:bug");
        assert_eq!(p.filter.text, None);
        assert_eq!(p.filter.harness, vec!["pi".to_owned()]);
    }

    #[test]
    fn a_bad_term_is_an_error_and_the_filter_is_not_applied() {
        let p = parse("colour:red state:idle");
        assert_eq!(p.errors, vec!["Unknown filter key 'colour'".to_owned()]);
        assert_eq!(p.filter, Filter::default());
        assert_eq!(
            parse("state:sleepy").errors,
            vec!["Unknown state 'sleepy' in the filter".to_owned()]
        );
    }
}
