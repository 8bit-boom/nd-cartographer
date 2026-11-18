//! One ask, judge and repair loop for every AI call (plan 3.3). A reply the judge refuses is
//! answered as a conversation: the next request holds the system prompt, the first request,
//! the model's own refused reply and the complaints (rendered from the pack's `repair.md`),
//! so the model sees what it wrote and what is wrong with it. The loop knows nothing of the
//! network: callers hand in `ask`, which takes the turns so far and returns the reply text.

use crate::aipack;
use std::future::Future;
use std::time::Instant;

/// A refused reply is quoted back to the model at most this long.
const QUOTE_CHARS: usize = 6_000;
/// At most this many complaints are listed in one repair turn.
const MAX_LISTED: usize = 6;

/// One message after the system prompt.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub role: &'static str,
    pub content: String,
}

impl Turn {
    pub fn user(content: impl Into<String>) -> Turn {
        Turn {
            role: "user",
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Turn {
        Turn {
            role: "assistant",
            content: content.into(),
        }
    }
}

/// What to ask and how hard to try.
pub struct Job<'a> {
    pub first_user: String,
    /// Corrections allowed after the first try.
    pub rounds: usize,
    /// What is being made, for the give-up message ("spec", "revision").
    pub what: &'a str,
}

/// Where the loop is, for a progress display.
#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    /// Waiting for the model (1-based `attempt` of `max`).
    Asking { attempt: usize, max: usize },
    /// Judging the reply.
    Checking { attempt: usize },
    /// The reply was refused; asking again with these complaints.
    Repairing {
        attempt: usize,
        problems: Vec<String>,
    },
}

#[derive(Debug)]
pub struct Outcome<T> {
    pub value: T,
    pub attempts: usize,
    pub seconds: f64,
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub attempts: usize,
    /// The judge's last complaints (empty when the backend itself failed).
    pub complaints: Vec<String>,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<Failure> for String {
    fn from(f: Failure) -> String {
        f.message
    }
}

/// The complaints as the text that fills `{{problems}}`.
pub fn problems_text(complaints: &[String]) -> String {
    match complaints {
        [one] => one.clone(),
        many => {
            let mut out: Vec<String> = many
                .iter()
                .take(MAX_LISTED)
                .map(|c| format!("- {c}"))
                .collect();
            if many.len() > MAX_LISTED {
                out.push(format!("- (and {} more)", many.len() - MAX_LISTED));
            }
            out.join("\n")
        }
    }
}

fn quote(reply: &str) -> String {
    reply.chars().take(QUOTE_CHARS).collect()
}

/// Ask until `judge` accepts a reply: the first try plus `job.rounds` corrections. A failure
/// of `ask` itself (the backend) ends the loop at once.
pub async fn run<T, A, Fut>(
    job: &Job<'_>,
    progress: &(dyn Fn(Progress) + Sync),
    mut ask: A,
    judge: impl Fn(&str) -> Result<T, Vec<String>>,
) -> Result<Outcome<T>, Failure>
where
    A: FnMut(Vec<Turn>) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    let started = Instant::now();
    let max = job.rounds + 1;
    let mut turns = vec![Turn::user(job.first_user.clone())];
    let mut last: Vec<String> = Vec::new();
    for attempt in 1..=max {
        progress(Progress::Asking { attempt, max });
        let reply = ask(turns.clone()).await.map_err(|message| Failure {
            message,
            attempts: attempt,
            complaints: Vec::new(),
        })?;
        progress(Progress::Checking { attempt });
        match judge(&reply) {
            Ok(value) => {
                return Ok(Outcome {
                    value,
                    attempts: attempt,
                    seconds: started.elapsed().as_secs_f64(),
                })
            }
            Err(complaints) => {
                if attempt < max {
                    progress(Progress::Repairing {
                        attempt: attempt + 1,
                        problems: complaints.clone(),
                    });
                    turns.push(Turn::assistant(quote(&reply)));
                    turns.push(Turn::user(aipack::repair(&problems_text(&complaints))));
                }
                last = complaints;
            }
        }
    }
    Err(Failure {
        message: format!(
            "the model did not produce a usable {} after {max} tries; last problem: {}",
            job.what,
            problems_text(&last)
        ),
        attempts: max,
        complaints: last,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn job(rounds: usize) -> Job<'static> {
        Job {
            first_user: "Design this map: a crypt".into(),
            rounds,
            what: "spec",
        }
    }

    fn judge(r: &str) -> Result<String, Vec<String>> {
        if r == "good" {
            Ok(r.into())
        } else {
            Err(vec![format!("'{r}' is wrong"), "and so is the rest".into()])
        }
    }

    #[tokio::test]
    async fn a_refused_reply_comes_back_as_a_conversation_with_the_complaints() {
        let seen: Mutex<Vec<Vec<Turn>>> = Mutex::new(Vec::new());
        let replies = ["bad", "good"];
        let mut i = 0;
        let got = run(
            &job(2),
            &|_| {},
            |turns| {
                seen.lock().unwrap().push(turns);
                let r = replies[i].to_string();
                i += 1;
                async move { Ok(r) }
            },
            judge,
        )
        .await
        .unwrap();
        assert_eq!((got.value.as_str(), got.attempts), ("good", 2));
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen[0].len(), 1);
        // system is added by the caller: user, assistant, user.
        assert_eq!(seen[1].len(), 3, "{:?}", seen[1]);
        assert_eq!(seen[1][1], Turn::assistant("bad"));
        assert_eq!(seen[1][2].role, "user");
        assert!(
            seen[1][2].content.contains("'bad' is wrong")
                && seen[1][2].content.contains("and so is the rest"),
            "{}",
            seen[1][2].content
        );
    }

    #[tokio::test]
    async fn it_gives_up_with_the_last_complaints_and_a_backend_error_ends_it_at_once() {
        let e = run(
            &job(1),
            &|_| {},
            |_| async { Ok("nope".to_string()) },
            judge,
        )
        .await
        .unwrap_err();
        assert!(e.message.contains("2 tries") && e.message.contains("'nope' is wrong"));
        assert_eq!((e.attempts, e.complaints.len()), (2, 2));
        let e = run(
            &job(2),
            &|_| {},
            |_| async { Err::<String, _>("nd-world refused it".to_string()) },
            judge,
        )
        .await
        .unwrap_err();
        assert_eq!((e.message.as_str(), e.attempts), ("nd-world refused it", 1));
    }

    #[tokio::test]
    async fn progress_is_reported_and_a_long_reply_is_cut_when_quoted() {
        let events: Mutex<Vec<Progress>> = Mutex::new(Vec::new());
        let long = "x".repeat(10_000);
        let seen: Mutex<Vec<Vec<Turn>>> = Mutex::new(Vec::new());
        let mut n = 0;
        run(
            &job(1),
            &|p| events.lock().unwrap().push(p),
            |turns| {
                seen.lock().unwrap().push(turns);
                n += 1;
                let r = if n == 1 { long.clone() } else { "good".into() };
                async move { Ok(r) }
            },
            judge,
        )
        .await
        .unwrap();
        let ev = events.into_inner().unwrap();
        assert_eq!(ev[0], Progress::Asking { attempt: 1, max: 2 });
        assert!(
            matches!(ev[2], Progress::Repairing { attempt: 2, .. }),
            "{ev:?}"
        );
        assert_eq!(seen.into_inner().unwrap()[1][1].content.len(), QUOTE_CHARS);
    }

    #[test]
    fn many_complaints_become_a_capped_list() {
        let c: Vec<String> = (0..9).map(|i| format!("p{i}")).collect();
        let t = problems_text(&c);
        assert!(t.starts_with("- p0") && t.contains("p5") && !t.contains("p6"));
        assert!(t.ends_with("(and 3 more)"));
        assert_eq!(problems_text(&["only".into()]), "only");
    }
}
