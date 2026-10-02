//! Progress protocol between CLI child processes and the GUI: the CLI emits
//! prefixed JSON lines on stderr, `gui::task::runner` parses them.
/// Functions that emit progress events when enabled.
mod emitter;
/// Event types, environment flag and line prefix shared by both sides.
mod protocol;

pub(crate) use emitter::{advance, enabled, finish, items, start};
pub(crate) use protocol::{parse_line, ProgressEvent, PROGRESS_ENV};
