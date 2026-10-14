//! Doubles for tests. They exist only under `cfg(test)` or the `test-doubles` feature, so a release build has
//! no way to allow a prompt without a person.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::{Presence, PresenceError, PresencePrompt, PresenceProof};

/// Confirms every prompt at once.
pub struct AllowAll;

impl Presence for AllowAll {
    fn name(&self) -> &'static str {
        "test-allow"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        Ok(PresenceProof::new("test-allow", &prompt.plan_digest_short))
    }
}

type Hook = Box<dyn Fn() + Send + Sync>;

/// Answers prompts from a script (the front answer, or a refusal once the script is empty), records every
/// prompt, and can run a hook while the "person" is deciding.
#[derive(Default)]
pub struct Scripted {
    answers: Mutex<VecDeque<Result<(), PresenceError>>>,
    seen: Mutex<Vec<PresencePrompt>>,
    during: Mutex<Option<Hook>>,
    wrong_digest: Mutex<bool>,
}

impl Scripted {
    /// A script with no answers: every prompt is refused.
    pub fn new() -> Arc<Scripted> {
        Arc::default()
    }

    /// The next prompt is confirmed.
    pub fn allow(&self) -> &Self {
        self.push(Ok(()))
    }

    /// The next prompt is answered with `err`.
    pub fn fail(&self, err: PresenceError) -> &Self {
        self.push(Err(err))
    }

    /// The next confirmation carries a proof for another digest than the prompt's.
    pub fn confirm_other_digest(&self) -> &Self {
        *self.wrong_digest.lock().unwrap() = true;
        self.push(Ok(()))
    }

    /// Runs `f` while a prompt is open, before it is answered: the plan changes under the person's hand.
    pub fn during(&self, f: impl Fn() + Send + Sync + 'static) -> &Self {
        *self.during.lock().unwrap() = Some(Box::new(f));
        self
    }

    fn push(&self, a: Result<(), PresenceError>) -> &Self {
        self.answers.lock().unwrap().push_back(a);
        self
    }

    /// The prompts shown so far.
    pub fn prompts(&self) -> Vec<PresencePrompt> {
        self.seen.lock().unwrap().clone()
    }
}

impl Presence for Scripted {
    fn name(&self) -> &'static str {
        "test-scripted"
    }

    fn confirm(&self, prompt: &PresencePrompt) -> Result<PresenceProof, PresenceError> {
        self.seen.lock().unwrap().push(prompt.clone());
        if let Some(f) = self.during.lock().unwrap().as_ref() {
            f();
        }
        let answer = self.answers.lock().unwrap().pop_front();
        match answer {
            Some(Ok(())) => {
                let digest = if std::mem::take(&mut *self.wrong_digest.lock().unwrap()) {
                    "ffffffffffff".to_owned()
                } else {
                    prompt.plan_digest_short.clone()
                };
                Ok(PresenceProof::new("test-scripted", digest))
            }
            Some(Err(e)) => Err(e),
            None => Err(PresenceError::Denied("the script has no answer".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn prompt() -> PresencePrompt {
        PresencePrompt {
            action: "a".into(),
            target: "t".into(),
            plan_digest_short: "0123456789ab".into(),
            space: "s".into(),
            requester: "r".into(),
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn a_script_answers_in_order_and_then_refuses() {
        let s = Scripted::new();
        s.allow().fail(PresenceError::Cancelled);
        assert!(s.confirm(&prompt()).is_ok());
        assert_eq!(s.confirm(&prompt()), Err(PresenceError::Cancelled));
        assert!(matches!(
            s.confirm(&prompt()),
            Err(PresenceError::Denied(_))
        ));
        assert_eq!(s.prompts().len(), 3);
    }
}
