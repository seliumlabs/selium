//! Type aliases and Enums relating to Selium Log errors.

use thiserror::Error;

/// Result type alias for [LogError].
pub type Result<T, E = LogError> = std::result::Result<T, E>;

/// An enumeration of Selium Log errors.
#[derive(Error, Debug)]
pub enum LogError {
    /// Returned when a [std::io::Error] error occurs while creating a logs directory.
    #[error("Failed to create log directory.")]
    CreateLogsDirectory(#[source] std::io::Error),

    /// Returned when a [std::io::Error] error occurs while provisioning a [SegmentList](crate::segment::SegmentList).
    #[error("Failed to load segment offsets from log directory.")]
    LoadSegments(#[source] std::io::Error),

    /// Returned when attempting to write to an empty [SegmentList](crate::segment::SegmentList).
    #[error("Cannot find a hot segment to write to.")]
    SegmentListEmpty,

    /// Returned when the [Index](crate::index::Index) file fails to map to the memory map buffer.
    #[error("Failed to map segment index file to memory.")]
    MemoryMapIndex(#[source] std::io::Error),

    /// Any generic [std::io::Error] errors that aren't classified.
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
