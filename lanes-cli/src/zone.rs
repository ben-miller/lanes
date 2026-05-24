/// A resolved zone: which monitor handle and the unit rect within it.
#[derive(Debug, PartialEq)]
pub struct ZoneRect {
    pub monitor_handle: String,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Parse a zone string like `lg-left:1-2/3` into a ZoneRect.
/// Returns an error string on invalid input.
pub fn parse(zone: &str) -> Result<ZoneRect, String> {
    let (handle, span_part) = match zone.find(':') {
        None => return Ok(ZoneRect { monitor_handle: zone.to_string(), x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
        Some(i) => (&zone[..i], &zone[i+1..]),
    };

    let (span_str, denom_str) = span_part.split_once('/')
        .ok_or_else(|| format!("zone '{}': expected span/denominator after ':'", zone))?;

    let denom: u32 = denom_str.parse()
        .map_err(|_| format!("zone '{}': denominator must be a number", zone))?;
    if denom == 0 || denom > 4 {
        return Err(format!("zone '{}': denominator must be 1-4", zone));
    }

    let (n, m) = if let Some((n_s, m_s)) = span_str.split_once('-') {
        let n: u32 = n_s.parse().map_err(|_| format!("zone '{}': invalid span", zone))?;
        let m: u32 = m_s.parse().map_err(|_| format!("zone '{}': invalid span", zone))?;
        (n, m)
    } else {
        let n: u32 = span_str.parse().map_err(|_| format!("zone '{}': invalid span", zone))?;
        (n, n)
    };

    if n == 0 || m == 0 || n > m || m > denom {
        return Err(format!("zone '{}': span out of range for denominator {}", zone, denom));
    }

    Ok(ZoneRect {
        monitor_handle: handle.to_string(),
        x: (n - 1) as f64 / denom as f64,
        y: 0.0,
        w: (m - n + 1) as f64 / denom as f64,
        h: 1.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_handle() {
        let z = parse("lg-left").unwrap();
        assert_eq!(z.monitor_handle, "lg-left");
        assert_eq!(z.x, 0.0); assert_eq!(z.w, 1.0);
    }

    #[test]
    fn left_half() {
        let z = parse("lg-left:1/2").unwrap();
        assert_eq!(z.x, 0.0); assert_eq!(z.w, 0.5);
    }

    #[test]
    fn right_half() {
        let z = parse("lg-left:2/2").unwrap();
        assert_eq!(z.x, 0.5); assert_eq!(z.w, 0.5);
    }

    #[test]
    fn left_two_thirds() {
        let z = parse("main:1-2/3").unwrap();
        assert!((z.x - 0.0).abs() < 1e-9);
        assert!((z.w - 2.0/3.0).abs() < 1e-9);
    }

    #[test]
    fn right_third() {
        let z = parse("main:3/3").unwrap();
        assert!((z.x - 2.0/3.0).abs() < 1e-9);
        assert!((z.w - 1.0/3.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_denominator() {
        assert!(parse("lg:1/5").is_err());
    }

    #[test]
    fn span_out_of_range() {
        assert!(parse("lg:3/2").is_err());
    }
}
