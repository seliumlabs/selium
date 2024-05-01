use crate::data::LogIterator;

/// A slice of a log segment, retrieved from reading from the log.
/// The MessageSlice struct contains an `end_offset`, indicating the next offset
/// to read from the log, and may or may not contain an iterator over the log segment.
#[derive(Debug, Default)]
pub struct MessageSlice {
    messages: Option<LogIterator>,
    end_offset: u64,
}

impl MessageSlice {
    /// Constructs a MessageSlice instance.
    pub fn new(messages: LogIterator, end_offset: u64) -> Self {
        Self {
            messages: Some(messages),
            end_offset,
        }
    }

    /// Constructs an empty MessageSlice, with the messages iterator set to [Option::None].
    pub fn empty(end_offset: u64) -> Self {
        Self {
            messages: None,
            end_offset,
        }
    }

    /// An iterator over the log segment.
    pub fn messages(self) -> Option<LogIterator> {
        self.messages
    }

    /// The last segment in this slice.
    ///
    /// Used to determine the next offset to request from the log after processing all messages
    /// in this slice.
    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}
