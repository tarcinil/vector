pub mod data_firehose;
pub mod data_streams;

use snafu::Snafu;
use std::error::Error;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Eq, PartialEq, Clone)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Text,
    Json,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct CoreSinkConfig {
    pub encoding: Option<Encoding>,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
    #[serde(default = "default_batch_timeout")]
    pub batch_timeout: u64
}

impl Default for CoreSinkConfig {
    fn default() -> Self {
        CoreSinkConfig {
            encoding: None,
            batch_size: default_batch_size(),
            batch_timeout: default_batch_timeout()
        }
    }
}

pub fn default_batch_size() -> usize { bytesize::mib(1u64) as usize }
pub fn default_batch_timeout() -> u64 { 1 }

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct TowerRequestConfig {
    #[serde(default = "default_in_flight_limit")]
    pub in_flight_limit: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_rate_limit_duration_secs")]
    pub rate_limit_duration_secs: u64,
    #[serde(default = "default_rate_limit_num")]
    pub rate_limit_num: u64,
    #[serde(default = "default_retry_backoff_secs")]
    pub retry_backoff_secs: u64,
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: usize,
}

impl Default for TowerRequestConfig {
    fn default() -> Self {
        TowerRequestConfig {
            in_flight_limit: default_in_flight_limit(),
            timeout_secs: default_timeout_secs(),
            rate_limit_duration_secs: default_rate_limit_duration_secs(),
            rate_limit_num: default_rate_limit_num(),
            retry_backoff_secs: default_retry_backoff_secs(),
            retry_attempts: default_retry_attempts(),
        }
    }
}

pub fn default_in_flight_limit() -> usize { 5 }
pub fn default_timeout_secs() -> u64 { 30 }
pub fn default_rate_limit_duration_secs() -> u64 { 1 }
pub fn default_rate_limit_num() -> u64 { 5 }
pub fn default_retry_backoff_secs() -> u64 { 5 }
pub fn default_retry_attempts() -> usize { usize::max_value() }

#[derive(Debug, Snafu)]
pub enum HealthcheckError<E: Error> where E: 'static {
    #[snafu(display("Retrieval of stream description failed: {}", source))]
    StreamRetrievalFailed { source: E },
    #[snafu(display("The stream {} is not found", stream_name))]
    NoMatchingStreamName { stream_name: String },
    #[snafu(display("The stream {} is not ready to receive input", stream_name))]
    StreamIsNotReady { stream_name: String },
}