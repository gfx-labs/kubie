#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]

/// Humanize a Kubernetes memory value string like "8129807Ki" into "7.8 GiB".
///
/// Supports suffixes: Ki, Mi, Gi, Ti, Pi, Ei (binary), K, M, G, T, P, E (decimal),
/// and bare bytes.
pub fn humanize_memory(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }

    let (num_str, multiplier) = if let Some(n) = raw.strip_suffix("Ei") {
        (n, 1024_u64.pow(6))
    } else if let Some(n) = raw.strip_suffix("Pi") {
        (n, 1024_u64.pow(5))
    } else if let Some(n) = raw.strip_suffix("Ti") {
        (n, 1024_u64.pow(4))
    } else if let Some(n) = raw.strip_suffix("Gi") {
        (n, 1024_u64.pow(3))
    } else if let Some(n) = raw.strip_suffix("Mi") {
        (n, 1024_u64.pow(2))
    } else if let Some(n) = raw.strip_suffix("Ki") {
        (n, 1024)
    } else if let Some(n) = raw.strip_suffix('E') {
        (n, 1_000_000_000_000_000_000)
    } else if let Some(n) = raw.strip_suffix('P') {
        (n, 1_000_000_000_000_000)
    } else if let Some(n) = raw.strip_suffix('T') {
        (n, 1_000_000_000_000)
    } else if let Some(n) = raw.strip_suffix('G') {
        (n, 1_000_000_000)
    } else if let Some(n) = raw.strip_suffix('M') {
        (n, 1_000_000)
    } else if let Some(n) = raw.strip_suffix('K') {
        (n, 1000)
    } else {
        (raw, 1)
    };

    let Ok(num) = num_str.parse::<u64>() else {
        // Might be a float.
        if let Ok(num_f) = num_str.parse::<f64>() {
            let bytes = num_f * multiplier as f64;
            return format_bytes(bytes as u64);
        }
        return raw.to_string();
    };

    let bytes = num.saturating_mul(multiplier);
    format_bytes(bytes)
}

/// Humanize a Kubernetes CPU value string like "7900m" into "7.9 cores".
///
/// Supports millicores ("m" suffix) and bare core counts.
pub fn humanize_cpu(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }

    if let Some(milli_str) = raw.strip_suffix('m') {
        if let Ok(milli) = milli_str.parse::<u64>() {
            let cores = milli as f64 / 1000.0;
            return if cores == cores.floor() {
                format!("{} cores", cores as u64)
            } else {
                format!("{cores:.1} cores")
            };
        }
    }

    if let Ok(cores) = raw.parse::<u64>() {
        return if cores == 1 {
            "1 core".to_string()
        } else {
            format!("{cores} cores")
        };
    }

    if let Ok(cores) = raw.parse::<f64>() {
        return format!("{cores:.1} cores");
    }

    raw.to_string()
}

fn format_bytes(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const TIB: u64 = 1024 * 1024 * 1024 * 1024;

    if bytes >= TIB {
        let val = bytes as f64 / TIB as f64;
        if val == val.floor() {
            format!("{} TiB", val as u64)
        } else {
            format!("{val:.1} TiB")
        }
    } else if bytes >= GIB {
        let val = bytes as f64 / GIB as f64;
        if val == val.floor() {
            format!("{} GiB", val as u64)
        } else {
            format!("{val:.1} GiB")
        }
    } else if bytes >= MIB {
        let val = bytes as f64 / MIB as f64;
        if val == val.floor() {
            format!("{} MiB", val as u64)
        } else {
            format!("{val:.0} MiB")
        }
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_humanize_memory() {
        assert_eq!(humanize_memory("8129807Ki"), "7.8 GiB");
        assert_eq!(humanize_memory("8388608Ki"), "8 GiB");
        assert_eq!(humanize_memory("2Gi"), "2 GiB");
        assert_eq!(humanize_memory("512Mi"), "512 MiB");
        assert_eq!(humanize_memory("1073741824"), "1 GiB");
        assert_eq!(humanize_memory(""), "");
    }

    #[test]
    fn test_humanize_cpu() {
        assert_eq!(humanize_cpu("7900m"), "7.9 cores");
        assert_eq!(humanize_cpu("4000m"), "4 cores");
        assert_eq!(humanize_cpu("4"), "4 cores");
        assert_eq!(humanize_cpu("1"), "1 core");
        assert_eq!(humanize_cpu(""), "");
    }
}
