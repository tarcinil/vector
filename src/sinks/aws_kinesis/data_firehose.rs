use crate::{
    buffers::Acker,
    event::{self, Event},
    region::RegionOrEndpoint,
    sinks::{
        util::{
            retries::{FixedRetryPolicy, RetryLogic},
            BatchServiceSink, SinkExt,
        },
        Healthcheck, RouterSink,
    },
    topology::config::{DataType, SinkConfig},
};
use futures::{stream::iter_ok, Future, Poll, Sink};
use rusoto_core::RusotoFuture;
use rusoto_firehose::{
    DescribeDeliveryStreamError::{self, ResourceNotFound},
    DescribeDeliveryStreamInput, KinesisFirehose, KinesisFirehoseClient, PutRecordBatchError,
    PutRecordBatchInput, PutRecordBatchOutput, Record,
};
use serde::{Deserialize, Serialize};
use std::{
    convert::TryInto,
    fmt, sync::Arc,
    time::Duration,
};
use tower::{Service, ServiceBuilder};
use tracing_futures::{Instrument, Instrumented};

use super::{Encoding, CoreSinkConfig, TowerRequestConfig};

#[derive(Clone)]
pub struct FirehoseService {
    delivery_stream_name: String,
    client: Arc<KinesisFirehoseClient>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct FirehoseConfig {
    pub delivery_stream_name: String,
    #[serde(flatten)]
    pub region: RegionOrEndpoint,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
pub struct FirehoseSinkConfig {
    #[serde(flatten)]
    pub firehose_config: FirehoseConfig,
    #[serde(default, flatten, rename = "core")]
    pub core_config: CoreSinkConfig,
    #[serde(default, rename = "request")]
    pub request_config: TowerRequestConfig
}

#[typetag::serde(name = "aws_kinesis_firehose")]
impl SinkConfig for FirehoseSinkConfig {
    fn build(&self, acker: Acker) -> crate::Result<(RouterSink, Healthcheck)> {
        let config = self.clone();
        let sink = FirehoseService::new(config, acker)?;
        let healthcheck = healthcheck(self.firehose_config.clone())?;
        Ok((Box::new(sink), healthcheck))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }
}

impl FirehoseService {

    fn construct<L,T,S,B>(svc: S,
                          policy: FixedRetryPolicy<L>,
                          acker: Acker,
                          ccfg: CoreSinkConfig,
                          rcfg: TowerRequestConfig) -> crate::Result<impl Sink<SinkItem = Event, SinkError = ()>>
    where L: RetryLogic,
          S: Service<T> + Clone + Sync,
          <S as tower_service::Service<T>>::Error: std::marker::Sync,
          B: crate::sinks::util::Batch<Output = T> {
        let svc = ServiceBuilder::new()
            .concurrency_limit(rcfg.in_flight_limit)
            .rate_limit(rcfg.rate_limit_num, Duration::from_secs(rcfg.rate_limit_duration_secs))
            .retry(policy)
            .timeout(Duration::from_secs(rcfg.timeout_secs))
            .service(svc);

        let encoding = ccfg.encoding.clone();
        let sink = BatchServiceSink::new(svc, acker)
            .batched_with_min(Vec::new(), ccfg.batch_size, Duration::from_secs(ccfg.batch_timeout))
            .with_flat_map(move |e| iter_ok(encode_event(e, &encoding)));

        Ok(sink)
    }

    pub fn new(
        config: FirehoseSinkConfig,
        acker: Acker,
    ) -> crate::Result<impl Sink<SinkItem = Event, SinkError = ()>> {
        let firehose_config = config.firehose_config;
        let request_config = config.request_config;
        let core_config = config.core_config;

        let client = Arc::new(KinesisFirehoseClient::new(
            firehose_config.region.clone().try_into()?,
        ));

        let policy = FixedRetryPolicy::new(
            request_config.retry_attempts,
            Duration::from_secs(request_config.retry_backoff_secs),
            FirehoseRetryLogic,
        );

        let firehose = FirehoseService {
            delivery_stream_name: firehose_config.delivery_stream_name,
            client
        };

        Self::construct(firehose, policy, acker, core_config, request_config)
    }
}

impl Service<Vec<Record>> for FirehoseService {
    type Response = PutRecordBatchOutput;
    type Error = PutRecordBatchError;
    type Future = Instrumented<RusotoFuture<PutRecordBatchOutput, PutRecordBatchError>>;

    fn poll_ready(&mut self) -> Poll<(), Self::Error> {
        Ok(().into())
    }

    fn call(&mut self, records: Vec<Record>) -> Self::Future {
        debug!(
            message = "sending records.",
            events = %records.len(),
        );

        let request = PutRecordBatchInput {
            records,
            delivery_stream_name: self.delivery_stream_name.clone(),
        };

        self.client
            .put_record_batch(request)
            .instrument(info_span!("request"))
    }
}

impl fmt::Debug for FirehoseService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FirehoseService")
            .field("delivery_stream_name", &self.delivery_stream_name)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct FirehoseRetryLogic;

impl RetryLogic for FirehoseRetryLogic {
    type Error = PutRecordBatchError;
    type Response = PutRecordBatchOutput;

    fn is_retriable_error(&self, error: &Self::Error) -> bool {
        match error {
            PutRecordBatchError::HttpDispatch(_) => true,
            PutRecordBatchError::ServiceUnavailable(_) => true,
            PutRecordBatchError::Unknown(res) if res.status.is_server_error() => true,
            _ => false,
        }
    }
}

type HealthcheckError = super::HealthcheckError<DescribeDeliveryStreamError>;

//todo: de-copypaste
fn healthcheck(config: FirehoseConfig) -> crate::Result<crate::sinks::Healthcheck> {
    let client = KinesisFirehoseClient::new(config.region.try_into()?);
    let stream_name = config.delivery_stream_name;

    //todo: result lacks delivery_stream_type field, need workaround probably

    //todo: and better to check against real AWS because there was similar problem with Kinesis

    let fut = client
        .describe_delivery_stream(DescribeDeliveryStreamInput {
            delivery_stream_name: stream_name,
            exclusive_start_destination_id: None,
            limit: Some(0),
        })
        //todo: what to do if the stream's type is not specified?
        .map_err(|source| match source {
            ResourceNotFound(resource) => HealthcheckError::NoMatchingStreamName {
                stream_name: resource,
            }
            .into(),
            other => HealthcheckError::StreamRetrievalFailed { source: other }.into(),
        })
        .and_then(move |res| {
            let description = res.delivery_stream_description;
            let status = &description.delivery_stream_status[..];

            match status {
                "CREATING" | "DELETING" => Err(HealthcheckError::StreamIsNotReady {
                    stream_name: description.delivery_stream_name,
                }
                .into()),
                _ => Ok(()),
            }
        });

    Ok(Box::new(fut))
}

fn encode_event(event: Event, encoding: &Option<Encoding>) -> Option<Record> {
    let log = event.into_log();
    let data = match (encoding, log.is_structured()) {
        (&Some(Encoding::Json), _) | (_, true) => {
            serde_json::to_vec(&log.unflatten()).expect("Error encoding event as json.")
        }

        (&Some(Encoding::Text), _) | (_, false) => log
            .get(&event::MESSAGE)
            .map(|v| v.as_bytes().to_vec())
            .unwrap_or(Vec::new()),
    };

    Some(Record { data })
}
