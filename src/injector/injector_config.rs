use serde::{Serialize, Deserialize};

use std::time::Duration;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
#[serde(rename_all = "camelCase")]
pub enum InjectorConfig {
    Latency(LatencyConfig),
    Faults(FaultsConfig),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LatencyConfig {
    pub filter: FilterConfig,
    #[serde(with = "humantime_serde")]
    pub latency: Duration,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FaultsConfig {
    pub filter: FilterConfig,
    pub faults: Vec<FaultConfig>
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FilterConfig {
    pub path: String,
    pub methods: Vec<String>,
    pub probability: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FaultConfig {
    pub errno: i32,
    pub weight: i32,
}
