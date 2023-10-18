use time::{Duration, OffsetDateTime};

const SECONDS_IN_DAY: i64 = 86_400;

pub struct ValidityRange {
    pub start: OffsetDateTime,
    pub end: OffsetDateTime,
}

impl ValidityRange {
    pub fn new(days: i64) -> Self {
        let offset = Duration::new(SECONDS_IN_DAY * days, 0);
        let start = OffsetDateTime::now_utc().checked_sub(offset).unwrap();
        let end = OffsetDateTime::now_utc().checked_add(offset).unwrap();

        Self { start, end }
    }
}
