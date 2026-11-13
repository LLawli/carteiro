//! Data no formato da RFC 5322 (`Fri, 13 Nov 2026 19:07:47 +0000`), sem crate
//! de calendário: o cabeçalho `Received:` só precisa de UTC.

use std::time::{SystemTime, UNIX_EPOCH};

const DIAS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MESES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub fn agora_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Dia civil (ano, mês 1-12, dia 1-31) a partir de dias desde 1970-01-01.
/// Algoritmo de Howard Hinnant (`civil_from_days`).
fn civil(dias: i64) -> (i64, u32, u32) {
    let z = dias + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let a = yoe + era * 400 + i64::from(m <= 2);
    (a, m, d)
}

pub fn rfc5322(segundos: u64) -> String {
    let dias = (segundos / 86_400) as i64;
    let resto = segundos % 86_400;
    let (a, m, d) = civil(dias);
    format!(
        "{}, {} {} {} {:02}:{:02}:{:02} +0000",
        DIAS[dias.rem_euclid(7) as usize],
        d,
        MESES[(m - 1) as usize],
        a,
        resto / 3600,
        (resto % 3600) / 60,
        resto % 60
    )
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn datas_conhecidas() {
        assert_eq!(rfc5322(0), "Thu, 1 Jan 1970 00:00:00 +0000");
        // 2026-11-13 19:07:47 UTC
        assert_eq!(rfc5322(1_794_596_867), "Fri, 13 Nov 2026 19:07:47 +0000");
        // 29 de fevereiro de 2024
        assert_eq!(rfc5322(1_709_208_000), "Thu, 29 Feb 2024 12:00:00 +0000");
    }
}
