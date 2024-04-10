use crate::data::LogIterator;

#[derive(Debug, Default)]
pub struct MessageSlice {
    messages: Option<LogIterator>,
    end_offset: u64,
}

impl MessageSlice {
    pub fn new(messages: LogIterator, end_offset: u64) -> Self {
        Self {
            messages: Some(messages),
            end_offset,
        }
    }

    pub fn empty(end_offset: u64) -> Self {
        Self {
            messages: None,
            end_offset,
        }
    }

    pub fn messages(self) -> Option<LogIterator> {
        self.messages
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}
