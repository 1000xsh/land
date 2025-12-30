use once_cell::sync::Lazy;
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// cache for HTTP date header (updated every 500ms in background thread).
struct DateCache {
    /// pre-formatted HTTP date bytes (e.g., "Tue, 15 Nov 2025 08:12:31 GMT")
    formatted: UnsafeCell<[u8; 32]>,
    /// actual length of formatted date
    len: UnsafeCell<usize>,
}

// safety: DateCache is written by a single updater thread and read by many request threads.
// the updater only writes every 500ms and reads are atomic at the byte level.
unsafe impl Sync for DateCache {}

/// global cached date (lazy-initialized with background updater)
static CACHED_DATE: Lazy<Arc<DateCache>> = Lazy::new(|| {
    let cache = Arc::new(DateCache {
        formatted: UnsafeCell::new([0u8; 32]),
        len: UnsafeCell::new(0),
    });

    // spawn background thread to update date every 500ms
    let cache_clone = cache.clone();
    std::thread::spawn(move || loop {
        update_date(&cache_clone);
        std::thread::sleep(Duration::from_millis(500));
    });

    // initialize immediately
    update_date(&cache);
    cache
});

/// update the cached date.
fn update_date(cache: &DateCache) {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();

    let formatted = format_http_date(now.as_secs());

    // safety: only called by the updater thread
    unsafe {
        let buf = &mut *cache.formatted.get();
        let len = formatted.len();
        buf[..len].copy_from_slice(formatted.as_bytes());
        *cache.len.get() = len;
    }
}

/// format a unix timestamp as an HTTP date (RFC 7231)
fn format_http_date(secs: u64) -> String {
    const DAYS: &[&str] = &["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: &[&str] = &[
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    // calculate date components
    let days_since_epoch = secs / 86400;
    let secs_today = secs % 86400;

    // calculate day of week (start data of the unix epoch from jan 1, 1970 00:00:00 UTC was thursday = 4)
    let dow = ((days_since_epoch + 4) % 7) as usize;

    // calculate year, month, day using simplified algorithm
    let mut year = 1970;
    let mut days_remaining = days_since_epoch;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if days_remaining < days_in_year {
            break;
        }
        days_remaining -= days_in_year;
        year += 1;
    }

    let mut month = 0;
    for m in 0..12 {
        let days_in_month = get_days_in_month(m, year);
        if days_remaining < days_in_month {
            month = m;
            break;
        }
        days_remaining -= days_in_month;
    }

    let day = days_remaining + 1;
    let hour = secs_today / 3600;
    let minute = (secs_today % 3600) / 60;
    let second = secs_today % 60;

    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        DAYS[dow], day, MONTHS[month], year, hour, minute, second
    )
}

#[inline]
fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0) && (year % 100 != 0 || year % 400 == 0)
}

#[inline]
fn get_days_in_month(month: usize, year: u64) -> u64 {
    match month {
        0 => 31, // january
        1 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        } // february
        2 => 31, // march
        3 => 30, // april
        4 => 31, // may
        5 => 30, // june
        6 => 31, // july
        7 => 31, // august
        8 => 30, // september
        9 => 31, // october
        10 => 30, // november
        11 => 31, // dezember
        _ => 0,
    }
}

/// append the cached HTTP date to a buffer.
/// copies pre-formatted bytes.
#[inline]
pub fn append_date(buf: &mut Vec<u8>) {
    // force lazy initialization
    let _ = &*CACHED_DATE;

    // safety: read-only access to cache: date is updated atomically every 500ms
    unsafe {
        let cache = &*CACHED_DATE;
        let formatted = &*cache.formatted.get();
        let len = *cache.len.get();
        buf.extend_from_slice(&formatted[..len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_http_date() {
        // test known date: Tue, 15 Nov 1994 08:12:31 GMT
        let secs = 784_887_151;
        let formatted = format_http_date(secs);
        assert_eq!(formatted, "Tue, 15 Nov 1994 08:12:31 GMT");

        // test epoch
        let formatted = format_http_date(0);
        assert_eq!(formatted, "Thu, 01 Jan 1970 00:00:00 GMT");
    }

    #[test]
    fn test_append_date() {
        let mut buf = Vec::new();
        append_date(&mut buf);

        // should be valid HTTP date format (29 bytes: "Day, DD Mon YYYY HH:MM:SS GMT")
        assert_eq!(buf.len(), 29);

        // should contain "GMT"
        let s = std::str::from_utf8(&buf).unwrap();
        assert!(s.ends_with(" GMT"));
    }
}
