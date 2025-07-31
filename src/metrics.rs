use std::{
    collections::VecDeque,
    fs,
    io::Write,
    path::Path,
    process::exit,
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::mpsc::Receiver;

use anyhow::Result;

use crate::{
    data::{Block, BlockType},
    node_config::NodeConfig,
};

pub(crate) struct Metrics {
    config: NodeConfig,
    finalized_block_rx: Receiver<(Block, BlockType, u64, u64)>,
    total_e2e_delay: u64,
    total_delay: u64,
    finalized_transactions: u64,
    finalized_blocks: u64,
    start_time: Instant,
    type_counts: [u64; 2],
    key_delay: u64,
    key_totals: u64,
    sample_window: VecDeque<MetricsSample>,
    next_sample_index: usize,
    // 新增：跟踪上一次采样的状态
    last_sample_time: Instant,
    last_total_e2e_delay: u64,
    last_total_delay: u64,
    last_finalized_transactions: u64,
    last_finalized_blocks: u64,
    last_key_delay: u64,
    last_key_totals: u64,
    
    // 新增：当前视图
    current_view: u64,
}

impl Metrics {
    pub fn new(config: NodeConfig, finalized_block_rx: Receiver<(Block, BlockType, u64, u64)>) -> Self {
        let next_sample_index = 1;

        Self {
            config,
            finalized_block_rx,
            total_e2e_delay: 0,
            total_delay: 0,
            key_delay: 0,
            finalized_transactions: 0,
            start_time: Instant::now(),
            type_counts: [0, 0],
            key_totals: 0,
            finalized_blocks: 0,
            sample_window: VecDeque::new(),
            next_sample_index,
            // 初始化上一次采样的状态
            last_sample_time: Instant::now(),
            last_total_e2e_delay: 0,
            last_total_delay: 0,
            last_finalized_transactions: 0,
            last_finalized_blocks: 0,
            last_key_delay: 0,
            last_key_totals: 0,

            current_view: 1, // 新增：初始化视图
        }
    }

    pub async fn dispatch(&mut self) {
        while let Some((block, block_type, finalized_time, view)) = self.finalized_block_rx.recv().await {

            // 更新当前视图
            self.current_view = view;
            // blocks statistics
            self.finalized_blocks += 1;
            self.finalized_transactions += block.payloads.len() as u64;

            if block_type == BlockType::Key {
                self.type_counts[0] += 1;
                self.key_totals += block.payloads.len() as u64;
                self.key_delay += (finalized_time - block.timestamp) * block.payloads.len() as u64;
            } else {
                self.type_counts[1] += 1;
            }

            // transaction statistics

            self.total_delay += (finalized_time - block.timestamp) * block.payloads.len() as u64;
            if !block.payloads.is_empty() {
                self.total_e2e_delay += (finalized_time
                    - block
                        .payloads
                        .get(0)
                        .unwrap()
                        .clone()
                        .into_command()
                        .created_time)
                    * block.payloads.len() as u64;
            }

            if self.config.get_metrics().trace_finalization {
                tracing::info!(
                    "Finalized block {} with {} transactions in {} ms, start: {}, view: {}",
                    block.height,
                    block.payloads.len(),
                    finalized_time - block.timestamp,
                    block.timestamp,
                    view // 添加视图信息
                );
            }

            // TODO: 计算 critcal path 的吞吐率
            if self.start_time.elapsed()
                > (Duration::from_millis(self.config.get_metrics().sampling_interval)
                    * self.next_sample_index as u32)
            {
                self.sample();

                self.next_sample_index += 1;
            }

            // self.try_exit().unwrap();
        }
    }

    fn sample(&mut self) {
        let current_time = Instant::now();
        let sample = MetricsSample::from_metrics(self, current_time);
        
        if let Some(report_interval) = self.config.get_metrics().report_every_n_samples {
            if self.next_sample_index % report_interval == 0 {
                tracing::info!("{}", sample);
                // 更新上一次采样的状态
                self.last_sample_time = current_time;
                self.last_total_e2e_delay = self.total_e2e_delay;
                self.last_total_delay = self.total_delay;
                self.last_finalized_transactions = self.finalized_transactions;
                self.last_finalized_blocks = self.finalized_blocks;
                self.last_key_delay = self.key_delay;
                self.last_key_totals = self.key_totals;
            }
        }
        
        self.sample_window.push_front(sample);
        if self.sample_window.len() > self.config.get_metrics().sampling_window {
            self.sample_window.pop_back();
        }
        
        // // 更新上一次采样的状态
        // self.last_sample_time = current_time;
        // self.last_total_e2e_delay = self.total_e2e_delay;
        // self.last_total_delay = self.total_delay;
        // self.last_finalized_transactions = self.finalized_transactions;
        // self.last_finalized_blocks = self.finalized_blocks;
        // self.last_key_delay = self.key_delay;
        // self.last_key_totals = self.key_totals;
    }

    /// Sign of exits
    fn high_enough(&self) -> bool {
        if let Some(height) = self.config.get_metrics().stop_after {
            self.finalized_blocks > height
        } else {
            false
        }
    }

    /// Sign of exits
    fn stable(&self) -> bool {
        if self.sample_window.len() < self.config.get_metrics().sampling_window {
            return false;
        }

        std_deviation(
            &self
                .sample_window
                .iter()
                .map(|d| d.average_delay)
                .collect::<Vec<_>>(),
        )
        .unwrap()
            < self.config.get_metrics().stable_threshold
            && std_deviation(
                &self
                    .sample_window
                    .iter()
                    .map(|d| d.consensus_throughput)
                    .collect::<Vec<_>>(),
            )
            .unwrap()
                < self.config.get_metrics().stable_threshold
    }

    fn stop_after_n_samples(&self) -> bool {
        if let Some(n) = self.config.get_metrics().stop_after_n_samples {
            self.next_sample_index > n
        } else {
            false
        }
    }

    fn try_exit(&mut self) -> Result<()> {
        if self.high_enough() || self.stable() || self.stop_after_n_samples() {
            if let Some(path) = self.config.get_metrics().export_path.clone() {
                self.export(&path)?;
            }
            tracing::info!("Node exits");
            exit(0);
        }
        Ok(())
    }

    fn export(&mut self, path: &Path) -> Result<()> {
        let mut file = fs::File::create(path)?;

        let metrics_result = MetricsResult {
            input: self.config.clone(),
            output: self.sample_window.pop_front().unwrap(),
        };

        file.write_all(serde_json::to_string_pretty(&metrics_result)?.as_bytes())?;

        tracing::info!("Metrics exported to {}", path.display());

        Ok(())
    }
}

#[allow(dead_code)]
#[derive(Serialize)]
struct MetricsSample {
    e2e_delay: f64,
    // ms
    average_delay: f64,
    finalized_transactions: u64,
    consensus_throughput: f64,
    key_block_ratio: f64,
    finalized_blocks: u64,
    // ms
    key_block_delay: f64,
    average_batch_size: f64,
    // ms
    block_delay: f64,


    // 新增：瞬时/区间指标
    instant_e2e_delay: f64,
    instant_average_delay: f64,
    instant_throughput: f64,
    instant_key_delay: f64,
    interval_duration_ms: f64,
    interval_transactions: u64,
    interval_blocks: u64,

    // 新增：当前视图
    current_view: u64,
}

impl MetricsSample {
    fn from_metrics(m: &Metrics, current_time: Instant) -> Self {
        let e2e_delay = m.total_e2e_delay as f64 / m.finalized_transactions as f64;
        let average_delay = m.total_delay as f64 / m.finalized_transactions as f64;
        let finalized_transactions = m.finalized_transactions;
        let consensus_throughput =
            m.finalized_transactions as f64 / m.start_time.elapsed().as_millis() as f64;

        let key_block_ratio = m.type_counts[0] as f64 / m.finalized_blocks as f64;
        let finalized_blocks = m.finalized_blocks;
        let key_block_delay = m.key_delay as f64 / m.key_totals as f64;

        let average_batch_size = m.finalized_transactions as f64 / m.finalized_blocks as f64;
        let block_delay = m.start_time.elapsed().as_millis() as f64 / m.finalized_blocks as f64;


        // 计算区间指标
        let interval_duration_ms = current_time.duration_since(m.last_sample_time).as_millis() as f64;
        let interval_transactions = m.finalized_transactions.saturating_sub(m.last_finalized_transactions);
        let interval_blocks = m.finalized_blocks.saturating_sub(m.last_finalized_blocks);
        
        let interval_e2e_delay = m.total_e2e_delay.saturating_sub(m.last_total_e2e_delay);
        let interval_total_delay = m.total_delay.saturating_sub(m.last_total_delay);
        let interval_key_delay = m.key_delay.saturating_sub(m.last_key_delay);
        let interval_key_totals = m.key_totals.saturating_sub(m.last_key_totals);

        // 瞬时指标计算
        let instant_e2e_delay = if interval_transactions > 0 {
            interval_e2e_delay as f64 / interval_transactions as f64
        } else {
            0.0
        };
        
        let instant_average_delay = if interval_transactions > 0 {
            interval_total_delay as f64 / interval_transactions as f64
        } else {
            0.0
        };
        
        let instant_throughput = if interval_duration_ms > 0.0 {
            interval_transactions as f64 / interval_duration_ms
        } else {
            0.0
        };
        
        let instant_key_delay = if interval_key_totals > 0 {
            interval_key_delay as f64 / interval_key_totals as f64
        } else {
            0.0
        };
        Self {
            block_delay,
            e2e_delay,
            average_delay,
            finalized_transactions,
            consensus_throughput,
            key_block_ratio,
            finalized_blocks,
            key_block_delay,
            average_batch_size,

            // 瞬时指标
            instant_e2e_delay,
            instant_average_delay,
            instant_throughput,
            instant_key_delay,
            interval_duration_ms,
            interval_transactions,
            interval_blocks,
            // 当前视图
            current_view: m.current_view,
        }
    }
}

impl std::fmt::Display for MetricsSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "View: {}, E2E delay: {:.2} ms, Average delay: {:.2} ms, Throughput: {:.3} Kops/sec, key:in-between: {:.2}:{:.2}, finalized_blocks: {}, average_batch_size: {:.2}, key_delay: {:.2}, block_delay: {:.2} | Instant - E2E: {:.2}ms, Delay: {:.2}ms, Throughput: {:.3}Kops/s | Interval: {}ms, {}tx, {}blocks",
            self.current_view, // 添加视图信息到输出
            self.e2e_delay,
            self.average_delay,
            self.consensus_throughput,
            self.key_block_ratio,
            1.0 - self.key_block_ratio,
            self.finalized_blocks,
            self.average_batch_size,
            self.key_block_delay,
            self.block_delay,
            
            // 瞬时指标
            self.instant_e2e_delay,
            self.instant_average_delay,
            self.instant_throughput,
            self.interval_duration_ms,
            self.interval_transactions,
            self.interval_blocks,
        )
    }
}

#[derive(Serialize)]
struct MetricsResult {
    input: NodeConfig,
    output: MetricsSample,
}

fn mean(data: &[f64]) -> Option<f64> {
    let sum: f64 = data.iter().sum();
    let count = data.len();

    match count {
        positive if positive > 0 => Some(sum / count as f64),
        _ => None,
    }
}

fn std_deviation(data: &[f64]) -> Option<f64> {
    match (mean(data), data.len()) {
        (Some(data_mean), count) if count > 0 => {
            let variance = data
                .iter()
                .map(|value| {
                    let diff = data_mean - *value;

                    diff * diff
                })
                .sum::<f64>()
                / count as f64;

            Some(variance.sqrt())
        }
        _ => None,
    }
}
