use super::Message;

#[derive(Debug)]
pub struct MessageSlice {
    messages: Vec<Message>,
    end_offset: u64,
}

impl MessageSlice {
    pub fn new(messages: &[Message], end_offset: u64) -> Self {
        Self {
            messages: messages.to_vec(),
            end_offset,
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn end_offset(&self) -> u64 {
        self.end_offset
    }
}

impl Default for MessageSlice {
    fn default() -> Self {
        Self {
            messages: vec![],
            end_offset: 0,
        }
    }
}
