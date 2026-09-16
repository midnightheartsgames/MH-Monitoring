//! Кто этот процессор: производитель, семейство, модель, строка названия.
//!
//! Нужно, чтобы выбрать способ чтения датчиков: у AMD Zen и у Intel регистры разные, а у первых
//! Ryzen к температуре ещё и прибавлен сдвиг, зависящий от модели.

/// Производитель по строке из CPUID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Amd,
    Intel,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuIdentity {
    pub vendor: Vendor,
    /// Итоговое семейство — с учётом расширенного поля.
    pub family: u32,
    pub model: u32,
    /// Строка вроде «AMD Ryzen 9 5900X 12-Core Processor», без хвостовых пробелов.
    pub brand: String,
}

impl CpuIdentity {
    /// Семейства, которые понимает модуль PawnIO `AMDFamily17`.
    pub fn is_amd_zen(&self) -> bool {
        self.vendor == Vendor::Amd && (0x17..=0x1A).contains(&self.family)
    }
}

/// Семейство и модель из `eax` листа 1.
///
/// Расширенные поля учитываются так, как предписывают и Intel, и AMD: семейство расширяется
/// только при базовом значении 0xF, модель — при базовом семействе 0x6 или 0xF.
pub fn decode_signature(eax: u32) -> (u32, u32) {
    let base_family = (eax >> 8) & 0xF;
    let base_model = (eax >> 4) & 0xF;
    let family = if base_family == 0xF { base_family + ((eax >> 20) & 0xFF) } else { base_family };
    let model = if base_family == 0x6 || base_family == 0xF {
        base_model | (((eax >> 16) & 0xF) << 4)
    } else {
        base_model
    };
    (family, model)
}

pub fn decode_vendor(ebx: u32, edx: u32, ecx: u32) -> Vendor {
    let mut bytes = [0u8; 12];
    bytes[..4].copy_from_slice(&ebx.to_le_bytes());
    bytes[4..8].copy_from_slice(&edx.to_le_bytes());
    bytes[8..].copy_from_slice(&ecx.to_le_bytes());
    match &bytes {
        b"AuthenticAMD" => Vendor::Amd,
        b"GenuineIntel" => Vendor::Intel,
        _ => Vendor::Other,
    }
}

#[cfg(target_arch = "x86_64")]
pub fn identify() -> CpuIdentity {
    use std::arch::x86_64::__cpuid;

    // CPUID существует на любом x86_64 — проверять его наличие не нужно.
    let leaf0 = __cpuid(0);
    let vendor = decode_vendor(leaf0.ebx, leaf0.edx, leaf0.ecx);
    let (family, model) = decode_signature(__cpuid(1).eax);

    let mut brand = Vec::with_capacity(48);
    if __cpuid(0x8000_0000).eax >= 0x8000_0004 {
        for leaf in 0x8000_0002..=0x8000_0004 {
            let regs = __cpuid(leaf);
            for register in [regs.eax, regs.ebx, regs.ecx, regs.edx] {
                brand.extend_from_slice(&register.to_le_bytes());
            }
        }
    }
    let brand = String::from_utf8_lossy(&brand).trim_matches(['\0', ' ']).to_string();

    CpuIdentity { vendor, family, model, brand }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ryzen 9 5900X: `eax` листа 1 = 0x00A20F10 — семейство 19h, модель 21h.
    #[test]
    fn zen3_signature_decodes_to_family_19h() {
        assert_eq!(decode_signature(0x00A2_0F10), (0x19, 0x21));
    }

    /// Ryzen 7 1700: 0x00800F11 — семейство 17h, модель 1.
    #[test]
    fn zen1_signature_decodes_to_family_17h() {
        assert_eq!(decode_signature(0x0080_0F11), (0x17, 0x01));
    }

    /// Core i9-9900K: 0x000906ED — семейство 6, модель 9Eh. Расширенное семейство при базовом 6
    /// не прибавляется, а расширенная модель — прибавляется.
    #[test]
    fn intel_signature_extends_only_the_model() {
        assert_eq!(decode_signature(0x0009_06ED), (0x6, 0x9E));
    }

    #[test]
    fn vendors_are_recognised() {
        let split = |text: &[u8; 12]| {
            let word =
                |range: std::ops::Range<usize>| u32::from_le_bytes(text[range].try_into().unwrap());
            (word(0..4), word(4..8), word(8..12))
        };
        let (b, d, c) = split(b"AuthenticAMD");
        assert_eq!(decode_vendor(b, d, c), Vendor::Amd);
        let (b, d, c) = split(b"GenuineIntel");
        assert_eq!(decode_vendor(b, d, c), Vendor::Intel);
        let (b, d, c) = split(b"HygonGenuine");
        assert_eq!(decode_vendor(b, d, c), Vendor::Other);
    }

    #[test]
    fn zen_range_is_17h_to_1ah() {
        let cpu = |vendor, family| CpuIdentity { vendor, family, model: 0, brand: String::new() };
        assert!(cpu(Vendor::Amd, 0x17).is_amd_zen());
        assert!(cpu(Vendor::Amd, 0x1A).is_amd_zen());
        assert!(!cpu(Vendor::Amd, 0x16).is_amd_zen());
        assert!(!cpu(Vendor::Intel, 0x19).is_amd_zen());
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn this_machine_identifies_itself() {
        let cpu = identify();
        assert!(cpu.family > 0);
        assert!(!cpu.brand.is_empty(), "у любого x86_64 последних лет есть строка названия");
    }
}
