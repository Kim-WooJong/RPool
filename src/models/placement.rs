use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Copy, Clone, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Placement {
    RoundRobin,
    FreeRatio,
}

impl Placement {
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            Self::RoundRobin => "round-robin",
            Self::FreeRatio => "free-ratio",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::RoundRobin => "Round robin",
            Self::FreeRatio => "Free-space ratio",
        }
    }
}
