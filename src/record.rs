//! The records stored in the tree, and the reports read out of it.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::id::{FlagId, SessionId};

/// The contents of a session's `meta.json`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SessionMeta {
    pub(crate) desc: String,
    pub(crate) created: Timestamp,
}

/// The contents of a ctf flag's `meta.json`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FlagMeta {
    pub(crate) desc: String,
    pub(crate) created: Timestamp,
    /// One past the flags the session held when this one was added. It orders
    /// flags whose timestamps are equal.
    pub(crate) counter: u64,
}

/// One line of a `hits.log`.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Hit {
    pub(crate) ts: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) note: Option<String>,
}

/// A session and everything recorded under it.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub session: SessionId,
    pub desc: String,
    pub created: Timestamp,
    /// Which id the status was looked up by.
    pub matched: Target,
    pub flags: Vec<FlagStatus>,
}

/// The id a [`Status`] was asked for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    Session,
    Flag { id: FlagId },
}

/// One ctf flag and its verification count.
#[derive(Clone, Debug, Serialize)]
pub struct FlagStatus {
    pub id: FlagId,
    pub desc: String,
    pub created: Timestamp,
    /// One past the flags the session held when this one was added. Together
    /// with `created` it orders the flags of a session.
    pub counter: u64,
    /// How many verifications have been recorded.
    pub hits: u64,
    /// When the most recent verification was recorded: the last hit appended,
    /// which is the log's own order. A caller that supplies out-of-order
    /// instants still sees the one recorded most recently.
    pub last_hit: Option<Timestamp>,
}

/// One row of the record book's session list.
#[derive(Clone, Debug, Serialize)]
pub struct SessionSummary {
    pub session: SessionId,
    pub desc: String,
    pub created: Timestamp,
    pub flags: u64,
    pub hits: u64,
}
