//! В каких единицах HUD показывает значения. Датчики и пороги всегда в °C, МГц и байтах —
//! единицы касаются только вывода.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temperature {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    /// До 1000 МГц — в МГц, дальше — в ГГц.
    #[default]
    Auto,
    Mhz,
    Ghz,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Memory {
    /// Гигабайты; память приложения меньше гигабайта — в мегабайтах.
    #[default]
    Gb,
    Mb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Units {
    #[serde(deserialize_with = "crate::settings::lenient")]
    pub temperature: Temperature,
    #[serde(deserialize_with = "crate::settings::lenient")]
    pub frequency: Frequency,
    #[serde(deserialize_with = "crate::settings::lenient")]
    pub memory: Memory,
}

impl Temperature {
    pub const ALL: [Temperature; 2] = [Temperature::Celsius, Temperature::Fahrenheit];

    pub fn title(self) -> &'static str {
        match self {
            Temperature::Celsius => "°C (Цельсий)",
            Temperature::Fahrenheit => "°F (Фаренгейт)",
        }
    }
}

impl Frequency {
    pub const ALL: [Frequency; 3] = [Frequency::Auto, Frequency::Mhz, Frequency::Ghz];

    pub fn title(self) -> &'static str {
        match self {
            Frequency::Auto => "Авто (МГц / ГГц)",
            Frequency::Mhz => "МГц",
            Frequency::Ghz => "ГГц",
        }
    }
}

impl Memory {
    pub const ALL: [Memory; 2] = [Memory::Gb, Memory::Mb];

    pub fn title(self) -> &'static str {
        match self {
            Memory::Gb => "ГБ",
            Memory::Mb => "МБ",
        }
    }
}
