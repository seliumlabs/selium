use regex::Regex;
use selium_std::errors::SeliumError;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

const TOPIC_REGEX: &str = r"^\/([\w-]+)\/([\w-]+)$";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicName {
    namespace: String,
    topic: String,
}

impl TopicName {
    pub fn new(namespace: &str, topic: &str) -> Self {
        Self {
            namespace: namespace.to_owned(),
            topic: topic.to_owned(),
        }
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
        let regex = Regex::new(TOPIC_REGEX).unwrap();

        let matches = regex
            .captures(value)
            .ok_or(SeliumError::ParseTopicNameError)?;

        let namespace = matches.get(1).unwrap().as_str().to_owned();
        let topic = matches.get(2).unwrap().as_str().to_owned();

        Ok(Self { namespace, topic })
    }
}

impl Display for TopicName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.namespace, self.topic)
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
            "namespace/",
            "name-space/topic-name",
            "_name_space/topic_name",
            "name_space/topic_name_",
            "namespace/topic/other",
        ];

        for topic_name in topic_names {
            let result = TopicName::try_from(topic_name);
            assert!(result.is_err());
        }
    }

    #[test]
    fn successfully_parses_topic_name() {
        let topic_names = [
            "namespace/topic",
            "name_space/topic",
            "namespace/to_pic",
            "name_space/to_pic",
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
        let topic_name = TopicName::new(namespace, topic);
        let expected = format!("{namespace}/{topic}");

        assert_eq!(topic_name.to_string(), expected);
    }
}
