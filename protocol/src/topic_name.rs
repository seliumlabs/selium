use lazy_regex::{lazy_regex, Lazy};
use regex::Regex;
use selium_std::errors::{Result, SeliumError};
use serde::{Deserialize, Serialize};
use std::fmt::Display;

const RESERVED_NAMESPACE: &str = "selium";
// Any [a-zA-Z0-9-_] with a length between 3 and 64 chars
const COMPONENT_REGEX: Lazy<Regex> = lazy_regex!(r"^[\w-]{3,64}$");
const TOPIC_REGEX: Lazy<Regex> = lazy_regex!(r"^\/([\w-]{3,64})\/([\w-]{3,64})$");

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct TopicName {
    namespace: String,
    topic: String,
}

impl TopicName {
    pub fn create(namespace: &str, topic: &str) -> Result<Self> {
        let s = Self {
            namespace: namespace.to_owned(),
            topic: topic.to_owned(),
        };

        if s.is_valid() {
            Ok(s)
        } else {
            Err(SeliumError::ParseTopicNameError)
        }
    }

    pub fn is_valid(&self) -> bool {
        !(self.namespace.starts_with(RESERVED_NAMESPACE)
            || !COMPONENT_REGEX.is_match(&self.namespace)
            || !COMPONENT_REGEX.is_match(&self.topic))
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn topic(&self) -> &str {
        &self.topic
    }
}

impl TryFrom<&str> for TopicName {
    type Error = SeliumError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value == "" {
            return Err(SeliumError::ParseTopicNameError);
        }

        if value[1..].starts_with(RESERVED_NAMESPACE) {
            return Err(SeliumError::ReservedNamespaceError);
        }

        let matches = TOPIC_REGEX
            .captures(value)
            .ok_or(SeliumError::ParseTopicNameError)?;

        let namespace = matches.get(1).unwrap().as_str().to_owned();
        let topic = matches.get(2).unwrap().as_str().to_owned();

        Ok(Self { namespace, topic })
    }
}

impl Display for TopicName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/{}", self.namespace, self.topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fails_to_parse_poorly_formatted_topic_names() {
        let topic_names = [
            "",
            "namespace",
            "/namespace/",
            "/namespace/topic/other",
            "/namespace/topic!",
            "/selium/topic",
        ];

        for topic_name in topic_names {
            let result = TopicName::try_from(topic_name);
            assert!(result.is_err());
        }
    }

    #[test]
    fn successfully_parses_topic_name() {
        let topic_names = [
            "/namespace/topic",
            "/name_space/topic",
            "/namespace/to_pic",
            "/name_space/to_pic",
        ];

        for topic_name in topic_names {
            let result = TopicName::try_from(topic_name);
            assert!(result.is_ok());
        }
    }

    #[test]
    fn outputs_formatted_topic_name() {
        let namespace = "namespace";
        let topic = "topic";
        let topic_name = TopicName::create(namespace, topic).unwrap();
        let expected = format!("/{namespace}/{topic}");

        assert_eq!(topic_name.to_string(), expected);
    }
}
