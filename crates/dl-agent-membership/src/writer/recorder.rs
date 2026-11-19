//! A recording [`Session`] for tests: every statement with its parameters (`DbType` name
//! and invariant text), answered by canned replies matched on the statement text. The
//! golden SQL tests compare [`Recorder::to_json`] with `tests/fixtures/golden/sql/*.json`.

use dl_json::{Map, Value as J};
use dl_tds::Param;

use super::session::{Called, Rec, Session, describe};
use crate::smile::SmileError;

/// One recorded statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stmt {
    /// `text`, `proc` or `batch`.
    pub kind: &'static str,
    /// The SQL text or procedure name.
    pub text: String,
    /// `(name, DbType, value)`.
    pub params: Vec<(String, &'static str, Option<String>)>,
}

type Reply = Box<dyn FnMut(&Stmt) -> Result<Called, SmileError> + Send>;

/// The recording fake.
#[derive(Default)]
pub struct Recorder {
    /// Every statement, in order.
    pub log: Vec<Stmt>,
    replies: Vec<(String, Reply)>,
}

impl Recorder {
    /// Answers statements whose text contains `needle` (the last matching rule wins).
    pub fn on(
        &mut self,
        needle: &str,
        f: impl FnMut(&Stmt) -> Result<Called, SmileError> + Send + 'static,
    ) {
        self.replies.push((needle.to_owned(), Box::new(f)));
    }

    /// Answers with fixed rows.
    pub fn rows(&mut self, needle: &str, rows: Vec<Rec>) {
        self.on(needle, move |_| {
            Ok(Called {
                rows: rows.clone(),
                status: Some(0),
            })
        });
    }

    /// The statement texts.
    pub fn texts(&self) -> Vec<&str> {
        self.log.iter().map(|s| s.text.as_str()).collect()
    }

    /// The log as JSON (`[{kind, text, params: [{name, type, value}]}]`).
    pub fn to_json(&self) -> J {
        J::Array(
            self.log
                .iter()
                .map(|s| {
                    let mut m = Map::new();
                    m.insert("kind", J::from(s.kind));
                    m.insert("text", J::from(s.text.as_str()));
                    let params = s
                        .params
                        .iter()
                        .map(|(n, t, v)| {
                            let mut p = Map::new();
                            p.insert("name", J::from(n.as_str()));
                            p.insert("type", J::from(*t));
                            p.insert("value", v.as_deref().map_or(J::Null, |s: &str| J::from(s)));
                            J::Object(p)
                        })
                        .collect();
                    m.insert("params", J::Array(params));
                    J::Object(m)
                })
                .collect(),
        )
    }

    fn run(
        &mut self,
        kind: &'static str,
        text: &str,
        params: &[Param],
    ) -> Result<Called, SmileError> {
        let stmt = Stmt {
            kind,
            text: text.to_owned(),
            params: params
                .iter()
                .map(|p| {
                    let (t, v) = describe(p.value());
                    (p.name().to_owned(), t, v)
                })
                .collect(),
        };
        self.log.push(stmt.clone());
        let reply = self
            .replies
            .iter_mut()
            .rev()
            .find(|(needle, _)| text.contains(needle.as_str()));
        match reply {
            Some((_, f)) => f(&stmt),
            None => Ok(Called {
                rows: Vec::new(),
                status: Some(0),
            }),
        }
    }
}

impl Session for Recorder {
    fn rows(&mut self, sql: &str, params: &[Param]) -> Result<Vec<Rec>, SmileError> {
        self.run("text", sql, params).map(|c| c.rows)
    }

    fn execute(&mut self, sql: &str, params: &[Param]) -> Result<u64, SmileError> {
        self.run("text", sql, params).map(|_| 1)
    }

    fn call(&mut self, procedure: &str, params: &[Param]) -> Result<Called, SmileError> {
        self.run("proc", procedure, params)
    }

    fn batch(&mut self, sql: &str) -> Result<(), SmileError> {
        self.run("batch", sql, &[]).map(|_| ())
    }
}
