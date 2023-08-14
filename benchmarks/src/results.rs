use crate::args::Args;
use num_format::{Locale, ToFormattedString};
use std::{fmt::Display, time::Duration};

#[derive(Debug)]
pub struct BenchmarkResults {
    duration: Duration,
    args: Args,
    total_mb_transferred: f64,
    avg_throughput: f64,
    avg_latency: f64,
}

impl BenchmarkResults {
    pub fn calculate(duration: Duration, message_size: usize, args: Args) -> Self {
        let total_bytes_transferred = args.num_of_messages * message_size as u64;
        let total_mb_transferred = total_bytes_transferred as f64 / 1024.0 / 1024.0;
        let avg_throughput = total_mb_transferred / duration.as_secs_f64();
        let avg_latency = duration.as_nanos() as f64 / args.num_of_messages as f64;

        Self {
            duration,
            args,
            total_mb_transferred,
            avg_throughput,
            avg_latency,
        }
    }
}

impl Display for BenchmarkResults {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let duration = format!("{:.4} Secs", self.duration.as_secs_f64());
        let total_transferred = format!("{:.2} MB", self.total_mb_transferred);
        let avg_throughput = format!("{:.2} MB/s", self.avg_throughput);
        let avg_latency = format!("{:.2} ns", self.avg_latency);

        let summary = format!(
            "
Benchmark Results
---------------------
Number of Messages: {}
Number of Streams: {}",
            self.args.num_of_messages.to_formatted_string(&Locale::en),
            self.args.num_of_streams.to_formatted_string(&Locale::en),
        );

        let header = format!(
            "| {: <20} | {: <20} | {: <20} | {: <20} |",
            "Duration", "Total Transferred", "Avg. Throughput", "Avg. Latency"
        );

        let body = format!(
            "| {: <20} | {: <20} | {: <20} | {: <20} |",
            duration, total_transferred, avg_throughput, avg_latency
        );

        write!(f, "{summary}\n\n{header}\n{body}\n")
    }
}
