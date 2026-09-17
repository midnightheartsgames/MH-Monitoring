//! Протокол между службой и UI (PLAN.md §6/P6, решение D2).
//!
//! Служба работает в сессии 0 от SYSTEM: у неё есть права на ETW и PawnIO, но нет окна в фокусе.
//! Поэтому цель выбирает UI и присылает её готовой, а служба в ответ шлёт снимки.
//!
//! Упаковка: 4 байта длины (little-endian), затем JSON. JSON, а не бинарный формат, потому что
//! снимок — это несколько килобайт несколько раз в секунду, а читаемость дампа при отладке важнее.

use std::io::{self, Read, Write};

use mh_core::{Snapshot, TargetResolution};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Имя канала. Одно на машину: служба одна.
pub const PIPE_NAME: &str = r"\\.\pipe\MHMonitor";

/// Меняется при любом несовместимом изменении сообщений.
pub const PROTOCOL_VERSION: u32 = 1;

/// Больше снимок не бывает даже близко (≈ 5 КБ); защита от мусора в канале.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// UI → служба.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToService {
    /// Первое сообщение соединения.
    Hello { protocol: u32, client_pid: u32 },
    /// Кого мерить. UI присылает при каждой перемене и периодически — на случай перезапуска
    /// службы.
    Target(TargetResolution),
}

/// Служба → UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToClient {
    Hello {
        protocol: u32,
        service_version: String,
    },
    /// Версии протокола не совпали — служба закрывает соединение.
    Refused {
        reason: String,
    },
    Snapshot(Box<Snapshot>),
}

pub fn write_message<T: Serialize>(writer: &mut impl Write, message: &T) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    if body.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "сообщение слишком большое"));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    // Одной записью: в канале побайтового режима половина сообщения не должна уйти отдельно.
    writer.write_all(&frame)?;
    writer.flush()
}

/// Читает одно сообщение. `Ok(None)` — собеседник закрыл соединение между сообщениями.
pub fn read_message<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(header) as usize;
    if length > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "заявлена слишком большая длина"));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mh_core::{FpsAvailability, FpsReason, FpsState, SensorStatus, TargetProcess};
    use std::io::Cursor;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(message: T) {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &message).unwrap();
        let mut cursor = Cursor::new(buffer);
        assert_eq!(read_message::<T>(&mut cursor).unwrap(), Some(message));
        assert_eq!(read_message::<T>(&mut cursor).unwrap(), None, "дальше — конец потока");
    }

    #[test]
    fn messages_survive_the_trip() {
        round_trip(ToService::Hello { protocol: PROTOCOL_VERSION, client_pid: 42 });
        round_trip(ToService::Target(TargetResolution::Resolved(TargetProcess::new(
            7,
            "fury.exe",
            Some(1_000),
        ))));
        round_trip(ToService::Target(TargetResolution::Unresolved {
            reason: FpsReason::NoTarget,
            detail: Some("процесс не выбран".into()),
        }));
        round_trip(ToClient::Refused { reason: "протокол 2".into() });
    }

    /// Снимок — главный груз: всё, что видит HUD, включая график, обязано доехать как есть.
    #[test]
    fn a_full_snapshot_survives_the_trip() {
        let mut snapshot = Snapshot {
            hardware_status: SensorStatus::Partial,
            timestamp_ms: 123_456,
            fps: FpsState {
                availability: FpsAvailability::Available,
                target: Some(TargetProcess::new(7, "fury.exe", None)),
                presentation: Some("Other · Composed: Copy with GPU GDI".into()),
                ..FpsState::INITIAL
            },
            ..Default::default()
        };
        snapshot.gpu.temperature_c = Some(57.0);
        snapshot.frametime_graph.columns[100] = 6.06;
        let mut buffer = Vec::new();
        write_message(&mut buffer, &ToClient::Snapshot(Box::new(snapshot.clone()))).unwrap();
        assert!(buffer.len() < 16_000, "снимок разросся: {} байт", buffer.len());
        let back: ToClient = read_message(&mut Cursor::new(buffer)).unwrap().unwrap();
        assert_eq!(back, ToClient::Snapshot(Box::new(snapshot)));
    }

    #[test]
    fn several_messages_in_a_row_are_split_correctly() {
        let mut buffer = Vec::new();
        for pid in 0..5 {
            write_message(&mut buffer, &ToService::Hello { protocol: 1, client_pid: pid }).unwrap();
        }
        let mut cursor = Cursor::new(buffer);
        for pid in 0..5 {
            let message: ToService = read_message(&mut cursor).unwrap().unwrap();
            assert_eq!(message, ToService::Hello { protocol: 1, client_pid: pid });
        }
    }

    #[test]
    fn a_hostile_length_is_rejected_without_allocating() {
        let mut cursor = Cursor::new(u32::MAX.to_le_bytes().to_vec());
        let error = read_message::<ToService>(&mut cursor).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_cut_message_is_an_error_not_a_clean_end() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &ToService::Hello { protocol: 1, client_pid: 1 }).unwrap();
        buffer.truncate(buffer.len() - 2);
        assert!(read_message::<ToService>(&mut Cursor::new(buffer)).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        let mut buffer = 3u32.to_le_bytes().to_vec();
        buffer.extend_from_slice(b"{{{");
        assert!(read_message::<ToService>(&mut Cursor::new(buffer)).is_err());
    }
}
