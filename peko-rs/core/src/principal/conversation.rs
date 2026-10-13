//! Conversation sessions bind only to channels. Actors are channel members.
use peko_session::metadata::SessionMetadata;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conversation {
    Channel(String),
}
pub(crate) fn binding(meta: &SessionMetadata) -> Option<Conversation> {
    meta.channel_id.clone().map(Conversation::Channel)
}
