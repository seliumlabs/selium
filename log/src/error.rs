use thiserror::Error;

pub type Result<T, E = LogError> = std::result::Result<T, E>;

#[derive(Error, Debug)]
pub enum LogError {
    #[error("Failed to create log directory.")]
    CreateLogsDirectory(#[source] std::io::Error),

    #[error("Failed to load segment offsets from log directory.")]
    LoadSegments(#[source] std::io::Error),

    #[error("Cannot find a hot segment to write to.")]
    SegmentListEmpty,

    #[error("Failed to map segment index file to memory.")]
    MemoryMapIndex(#[source] std::io::Error),

    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
