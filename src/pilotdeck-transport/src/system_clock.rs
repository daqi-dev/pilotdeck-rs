//! Production UTC wall clock and a Rust port of Node v22.22.0's V8 date parser.
//! Parsing consumes UTF16 directly; it never runs JavaScript or drops an
//! engine-valid local date because the timezone resolver cannot represent it.
// Parser control flow is derived from deps/v8/src/date/dateparser{,-inl}.h
// and dateparser.cc in https://github.com/nodejs/node/tree/v22.22.0.
// Copyright 2011, the V8 project authors. All rights reserved.
// Copyright 2014, the V8 project authors. All rights reserved.
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are met:
// * Redistributions of source code must retain the above copyright notice,
//   this list of conditions and the following disclaimer.
// * Redistributions in binary form must reproduce the above copyright notice,
//   this list of conditions and the following disclaimer in the documentation
//   and/or other materials provided with the distribution.
// * Neither the name of Google Inc. nor the names of its contributors may be
//   used to endorse or promote products derived from this software without
//   specific prior written permission.
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
// AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
// IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
// ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE
// LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
// CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
// SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
// INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
// CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
// ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
// POSSIBILITY OF SUCH DAMAGE.
use crate::server::ExecutionClock;
use pilotdeck_protocol::JsString;
use std::time::{SystemTime, UNIX_EPOCH};

const DAY_MS: i64 = 86_400_000;
const TIME_CLIP: i64 = 8_640_000_000_000_000;
const BEFORE_UTC_CLIP: i64 = TIME_CLIP + 30 * DAY_MS;
// Official Node22 arm64 build defaults disable pointer compression/31-bit Smis.
// Independent frozen-engine configuration verification remains part of adoption.
const SMI_MAX: u32 = 2_147_483_647;

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemExecutionClock;
impl ExecutionClock for SystemExecutionClock {
    fn now_epoch_ms(&self) -> f64 {
        system_epoch_ms(SystemTime::now()) as f64
    }
    fn now_iso(&self) -> JsString {
        format_epoch_iso(system_epoch_ms(SystemTime::now())).into()
    }
    fn parse_date(&self, input: &JsString) -> Result<Option<f64>, JsString> {
        parse_node22_date(input)
    }
}
fn system_epoch_ms(now: SystemTime) -> i64 {
    match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}

/// Same UTC ISO year/sign/millisecond form as Date.prototype.toISOString.
/// The production wall clock is sampled separately for each trait invocation.
pub fn format_epoch_iso(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(DAY_MS);
    let (year, month, day) = civil_from_days(days);
    let time = epoch_ms.rem_euclid(DAY_MS);
    let y = if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else if year < 0 {
        format!("-{:06}", -year)
    } else {
        format!("+{year:06}")
    };
    format!(
        "{y}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        time / 3_600_000,
        time / 60_000 % 60,
        time / 1000 % 60,
        time % 1000
    )
}

/// Uses only the checked system TZif resolver for local dates, including normal,
/// repeated and skipped times. UTC/explicit offsets bypass it. The offset policy
/// is ICU FORMER/FORMER; system TZ data equivalence to pinned ICU remains a
/// separate independently exercised profile, not a universal data-version claim.
pub fn parse_node22_date(input: &JsString) -> Result<Option<f64>, JsString> {
    parse_node22_date_with_local(input, &resolve_local)
}
/// Injectable local-wall-time→UTC conversion makes parser policy deterministic
/// without changing process TZ or consulting ambient clocks in unit fixtures.
pub fn parse_node22_date_with_local(
    input: &JsString,
    local: &impl Fn(i64) -> Result<i64, JsString>,
) -> Result<Option<f64>, JsString> {
    let Some(parts) = parse_parts(input.units()) else {
        return Ok(None);
    };
    // V8 MakeDay rejects years outside +/-1,000,000 before arithmetic.
    if !(-1_000_000..=1_000_000).contains(&parts.year) {
        return Ok(None);
    }
    let day = days_from_civil(parts.year, parts.month, 1) + i64::from(parts.day - 1);
    let wall = day * DAY_MS
        + i64::from(parts.hour) * 3_600_000
        + i64::from(parts.minute) * 60_000
        + i64::from(parts.second) * 1000
        + i64::from(parts.millisecond);
    let utc = if let Some(offset) = parts.utc_offset {
        wall - i64::from(offset) * 1000
    } else {
        if !(-BEFORE_UTC_CLIP..=BEFORE_UTC_CLIP).contains(&wall) {
            return Ok(None);
        }
        local(wall)?
    };
    Ok((-TIME_CLIP..=TIME_CLIP)
        .contains(&utc)
        .then_some(utc as f64))
}
fn resolve_local(wall: i64) -> Result<i64, JsString> {
    let bytes = system_tzif().ok_or_else(|| JsString::from(
        "NODE22_DATE_LOCAL_SYSTEM_TZIF_UNSUPPORTED: no readable safe system TZif establishes the offset for this valid local date."
    ))?;
    resolve_tzif_local(wall, &bytes)
}

// Read only the explicitly admitted system timezone sources on this Unix
// target. Custom TZ paths/POSIX environment rules require a separate profile;
// don't read arbitrary host/user files or silently guess UTC on lookup failure.
fn system_tzif() -> Option<Vec<u8>> {
    let path = system_tzif_path(std::env::var_os("TZ").as_deref())?;
    std::fs::read(path).ok()
}
/// Safe process-TZ path selection shared with conservative stats retention.
pub fn system_tzif_path(tz: Option<&std::ffi::OsStr>) -> Option<std::path::PathBuf> {
    match tz {
        None => Some(std::path::PathBuf::from("/etc/localtime")),
        Some(tz) => {
            let tz = tz.to_str()?;
            let tz = tz.strip_prefix(':').unwrap_or(tz);
            if matches!(tz, "localtime" | "/etc/localtime") {
                Some(std::path::PathBuf::from("/etc/localtime"))
            } else if !tz.is_empty()
                && tz
                    .split('/')
                    .all(|part| !part.is_empty() && part != "." && part != "..")
                && tz
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'/' | b'_' | b'-' | b'+'))
            {
                Some(std::path::Path::new("/usr/share/zoneinfo").join(tz))
            } else {
                None
            }
        }
    }
}

// Pure checked adapter: the registry-pinned parser owns TZif/POSIX decoding.
// ICU's FORMER policy is applied to each checked normal/skipped result, not to
// chrono's narrower calendar and not by mapping dates to a convenient year.
pub fn resolve_tzif_local(wall: i64, bytes: &[u8]) -> Result<i64, JsString> {
    let zone = tz::TimeZone::from_tz_data(bytes).map_err(|_| JsString::from(
        "NODE22_DATE_LOCAL_TZIF_UNSUPPORTED: system timezone bytes are not an admitted TZif profile."
    ))?;
    resolve_zone_local(wall, zone.as_ref())
}
fn resolve_zone_local(wall: i64, zone: tz::TimeZoneRef<'_>) -> Result<i64, JsString> {
    use tz::datetime::FoundDateTimeKind;
    if !zone.leap_seconds().is_empty() {
        return Err("NODE22_DATE_LOCAL_LEAP_PROFILE_UNSUPPORTED: leap-corrected TZif is not the original ICU Unix-millisecond profile.".into());
    }
    // All possible actual offsets include historical types AND recurring rule
    // types. If every possible conversion is outside TimeClip, its exact rule
    // is irrelevant: return an out-of-range UTC bound for the parser's NaN.
    // This is a proof over supplied data, not a UTC/+8/current-offset guess.
    let mut offsets: Vec<i32> = zone
        .local_time_types()
        .iter()
        .map(|t| t.ut_offset())
        .collect();
    match zone.extra_rule() {
        Some(tz::timezone::TransitionRule::Fixed(rule)) => offsets.push(rule.ut_offset()),
        Some(tz::timezone::TransitionRule::Alternate(rule)) => {
            offsets.push(rule.std().ut_offset());
            offsets.push(rule.dst().ut_offset());
        }
        None => (),
    }
    let earliest = wall.checked_sub(
        i64::from(*offsets.iter().max().expect("checked TZif has local types")) * 1000,
    );
    let latest = wall.checked_sub(
        i64::from(*offsets.iter().min().expect("checked TZif has local types")) * 1000,
    );
    if let Some(earliest) = earliest.filter(|value| *value > TIME_CLIP) {
        return Ok(earliest);
    }
    if let Some(latest) = latest.filter(|value| *value < -TIME_CLIP) {
        return Ok(latest);
    }
    let (year, month, day) = civil_from_days(wall.div_euclid(DAY_MS));
    let time = wall.rem_euclid(DAY_MS);
    let found = tz::DateTime::find(
        i32::try_from(year).map_err(|_| JsString::from("NODE22_DATE_LOCAL_RANGE_UNSUPPORTED: local year exceeds the checked timezone calendar."))?,
        month as u8, day as u8, (time / 3_600_000) as u8,
        (time / 60_000 % 60) as u8, (time / 1000 % 60) as u8,
        (time % 1000) as u32 * 1_000_000, zone,
    ).map_err(|_| JsString::from("NODE22_DATE_LOCAL_TZIF_UNSUPPORTED: system timezone cannot represent this checked local date."))?.into_inner();
    let subtract_offset = |offset: i32| {
        wall.checked_sub(i64::from(offset) * 1000)
        .ok_or_else(|| JsString::from("NODE22_DATE_LOCAL_RANGE_UNSUPPORTED: checked local offset arithmetic exceeded the epoch range."))
    };
    match found.as_slice() {
        // tz-rs uses before <= candidateUTC < transition; exactly at the end
        // of a fold only the latter offset remains valid. No Chrono cached
        // Single result bypasses this admission.
        [FoundDateTimeKind::Normal(value)] => subtract_offset(value.local_time_type().ut_offset()),
        [FoundDateTimeKind::Normal(first), FoundDateTimeKind::Normal(second)] => {
            // FORMER for a backward transition means the earlier UTC instant,
            // including the first millisecond of the duplicated interval.
            Ok(subtract_offset(first.local_time_type().ut_offset())?.min(subtract_offset(second.local_time_type().ut_offset())?))
        }
        [FoundDateTimeKind::Skipped { before_transition, .. }] => {
            // earliest() returns the transition instant, which loses the original
            // minutes/milliseconds inside the gap. Source UTC(t) uses the former
            // offset applied to the entire requested wall time instead.
            subtract_offset(before_transition.local_time_type().ut_offset())
        }
        // Missing footer after historical data or exotic multi-overlap native
        // policies are explicit gaps, not Source NaN or a guessed final offset.
        _ => Err("NODE22_DATE_LOCAL_TRANSITION_PROFILE_UNSUPPORTED: this wall time has no checked ordinary single/gap/overlap interpretation.".into()),
    }
}

/// One local wall-clock reading in an admitted IANA zone or fixed offset.
/// The same checked TZif profile that backs local date parsing is used, so the
/// gateway never re-parses timezone files or guesses an offset. `weekday` is
/// 0=Sunday..=6=Saturday (Intl `weekday: "short"` order); month/day are
/// 1-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // constructed in main crate; flagged dead in test-only #[path] include
pub struct ZonedDateTimeParts {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub weekday: u32,
    pub offset_seconds: i32,
}

/// A timezone admitted once and reused across many instants: the same
/// admission rules and data source as [`zoned_datetime_parts`] (an ICU
/// UTC-offset identifier, or an IANA name matched case-insensitively against
/// the system zoneinfo tree), with the TZif bytes parsed a single time.
/// #1324's minute-granular cron scan resolves a zone once and then walks up
/// to 366*24*60 candidates over it, so the per-instant entry point's
/// read-and-parse-per-call cost is not acceptable there.
#[derive(Clone)]
#[allow(dead_code)] // called from main crate; flagged dead in test-only #[path] include
pub enum ZonedPartsResolver {
    FixedOffset(i32),
    SystemZone(tz::TimeZone),
}

#[allow(dead_code)] // called from main crate; flagged dead in test-only #[path] include
impl ZonedPartsResolver {
    pub fn new(timezone: &str) -> Result<Self, JsString> {
        match parse_offset_timezone(timezone) {
            Some(offset) => Ok(Self::FixedOffset(offset)),
            None => {
                let bytes = timezone_tzif(timezone)
                    .ok_or_else(|| JsString::from("Invalid time zone specified"))?;
                Ok(Self::SystemZone(
                    tz::TimeZone::from_tz_data(&bytes)
                        .map_err(|_| JsString::from("Invalid time zone specified"))?,
                ))
            }
        }
    }

    fn offset_seconds(&self, epoch_ms: i64) -> Result<i32, JsString> {
        match self {
            Self::FixedOffset(offset) => Ok(*offset),
            Self::SystemZone(zone) => zone
                .find_local_time_type(epoch_ms.div_euclid(1000))
                .map_err(|_| JsString::from("Invalid time zone specified"))
                .map(|time_type| time_type.ut_offset()),
        }
    }

    pub fn parts_at(&self, epoch_ms: i64) -> Result<ZonedDateTimeParts, JsString> {
        let offset_seconds = self.offset_seconds(epoch_ms)?;
        let local_seconds = epoch_ms
            .div_euclid(1000)
            .checked_add(i64::from(offset_seconds))
            .ok_or_else(|| JsString::from("Invalid time value"))?;
        let days = local_seconds.div_euclid(86_400);
        let time = local_seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        // 1970-01-01 was a Thursday; weekday 0 is Sunday.
        let weekday = (days + 4).rem_euclid(7);
        Ok(ZonedDateTimeParts {
            year,
            month: u32::try_from(month).map_err(|_| JsString::from("Invalid time value"))?,
            day: u32::try_from(day).map_err(|_| JsString::from("Invalid time value"))?,
            hour: u32::try_from(time / 3_600).map_err(|_| JsString::from("Invalid time value"))?,
            minute: u32::try_from(time / 60 % 60)
                .map_err(|_| JsString::from("Invalid time value"))?,
            second: u32::try_from(time % 60).map_err(|_| JsString::from("Invalid time value"))?,
            weekday: u32::try_from(weekday).map_err(|_| JsString::from("Invalid time value"))?,
            offset_seconds,
        })
    }
}

/// Local fields for one UTC instant. `timezone` is an IANA name (matched
/// case-insensitively against the system zoneinfo tree) or an ICU UTC-offset
/// identifier such as `+05:30` / `-0800`. Invalid names are the only error;
/// a valid name whose system TZif is unreadable is reported as invalid rather
/// than silently formatted as UTC.
#[allow(dead_code)] // called from main crate; flagged dead in test-only #[path] include
pub fn zoned_datetime_parts(epoch_ms: i64, timezone: &str) -> Result<ZonedDateTimeParts, JsString> {
    ZonedPartsResolver::new(timezone)?.parts_at(epoch_ms)
}

#[allow(dead_code)] // called from main crate; flagged dead in test-only #[path] include
fn parse_offset_timezone(value: &str) -> Option<i32> {
    let bytes = value.as_bytes();
    let sign = match bytes.first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let body = &value[1..];
    let (hours, minutes) = if let Some((hours, minutes)) = body.split_once(':') {
        if hours.len() != 2 || minutes.len() != 2 {
            return None;
        }
        (hours, minutes)
    } else if body.len() == 2 {
        (body, "0")
    } else if body.len() == 4 {
        (&body[..2], &body[2..])
    } else {
        return None;
    };
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours >= 24 || minutes >= 60 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Resolve an IANA-like name under the admitted system zoneinfo root. Every
/// component is matched exactly first and then case-insensitively (ICU names
/// are case-insensitive), and the final real path must remain under the root.
#[allow(dead_code)] // called from main crate; flagged dead in test-only #[path] include
fn timezone_tzif(name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.starts_with('/') || name.contains('\0') {
        return None;
    }
    let root = std::path::Path::new("/usr/share/zoneinfo");
    let mut path = root.to_path_buf();
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return None;
        }
        let exact = path.join(component);
        if exact.exists() {
            path = exact;
            continue;
        }
        let entries = std::fs::read_dir(&path).ok()?;
        let mut found = None;
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if let Some(candidate) = file_name.to_str()
                && candidate.eq_ignore_ascii_case(component)
            {
                found = Some(entry.path());
                break;
            }
        }
        path = found?;
    }
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let canonical = std::fs::canonicalize(&path).ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    std::fs::read(canonical).ok()
}

fn days_from_civil(year: i32, month: i32, day: i32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = i64::from(month) + if month > 2 { -3 } else { 9 };
    let doy = (153 * m + 2) / 5 + i64::from(day) - 1;
    era * 146097 + y * 365 + y / 4 - y / 100 + doy - 719468
}
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    y += i64::from(m <= 2);
    (y, m, d)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keyword {
    Invalid,
    Month,
    Zone,
    Separator,
    AmPm,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token {
    Invalid,
    End,
    Unknown,
    Space,
    Number(i32, usize),
    Symbol(u16),
    Word(Keyword, i32, usize),
}
impl Token {
    fn number(self) -> Option<i32> {
        if let Self::Number(n, _) = self {
            Some(n)
        } else {
            None
        }
    }
    fn fixed(self, n: usize) -> Option<i32> {
        if let Self::Number(value, length) = self
            && length == n
        {
            Some(value)
        } else {
            None
        }
    }
    fn sign(self) -> Option<i32> {
        match self {
            Self::Symbol(43) => Some(1),
            Self::Symbol(45) => Some(-1),
            _ => None,
        }
    }
    fn z(self) -> bool {
        matches!(self, Self::Word(Keyword::Zone, 0, 1))
    }
    fn ms(self) -> i32 {
        let Self::Number(mut n, length) = self else {
            return -1;
        };
        if length < 3 {
            n *= if length == 1 { 100 } else { 10 };
        } else if length > 3 {
            n /= 10_i32.pow((length.min(9) - 3) as u32);
        }
        n
    }
}
fn white_space(c: u16) -> bool {
    matches!(
        c,
        9 | 11 | 12 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}
fn white_or_line(c: u16) -> bool {
    white_space(c) || matches!(c, 10 | 13 | 0x2028 | 0x2029)
}
fn keyword(prefix: [u16; 3], length: usize) -> Token {
    const MONTHS: [&[u8; 3]; 12] = [
        b"jan", b"feb", b"mar", b"apr", b"may", b"jun", b"jul", b"aug", b"sep", b"oct", b"nov",
        b"dec",
    ];
    for (index, name) in MONTHS.iter().enumerate() {
        if prefix == name.map(u16::from) {
            return Token::Word(Keyword::Month, index as i32 + 1, length);
        }
    }
    if length <= 3 {
        let key = prefix.map(|c| u8::try_from(c).unwrap_or(255));
        let (kind, value) = match &key {
            b"am\0" => (Keyword::AmPm, 0),
            b"pm\0" => (Keyword::AmPm, 12),
            b"ut\0" | b"utc" | b"z\0\0" | b"gmt" => (Keyword::Zone, 0),
            b"cdt" => (Keyword::Zone, -5),
            b"cst" => (Keyword::Zone, -6),
            b"edt" => (Keyword::Zone, -4),
            b"est" => (Keyword::Zone, -5),
            b"mdt" => (Keyword::Zone, -6),
            b"mst" | b"pdt" => (Keyword::Zone, -7),
            b"pst" => (Keyword::Zone, -8),
            b"t\0\0" => (Keyword::Separator, 0),
            _ => (Keyword::Invalid, 0),
        };
        return Token::Word(kind, value, length);
    }
    Token::Word(Keyword::Invalid, 0, length)
}
struct Scanner<'a> {
    units: &'a [u16],
    position: usize,
    next: Token,
}
impl<'a> Scanner<'a> {
    fn new(units: &'a [u16]) -> Self {
        let mut s = Self {
            units,
            position: 0,
            next: Token::End,
        };
        s.next = s.scan();
        s
    }
    fn ch(&self) -> u16 {
        self.units.get(self.position).copied().unwrap_or(0)
    }
    fn take(&mut self) -> Token {
        let old = self.next;
        self.next = self.scan();
        old
    }
    fn symbol(&mut self, c: u16) -> bool {
        if self.next == Token::Symbol(c) {
            self.take();
            true
        } else {
            false
        }
    }
    fn scan(&mut self) -> Token {
        let c = self.ch();
        if c == 0 {
            return Token::End;
        }
        let start = self.position;
        if (48..=57).contains(&c) {
            while self.ch() == 48 {
                self.position += 1;
            }
            let mut n = 0;
            let mut significant = 0;
            while (48..=57).contains(&self.ch()) {
                if significant < 9 {
                    n = n * 10 + i32::from(self.ch() - 48);
                }
                significant += 1;
                self.position += 1;
            }
            return Token::Number(n, self.position - start);
        }
        if matches!(c, 43 | 45 | 46 | 58 | 41) {
            self.position += 1;
            return Token::Symbol(c);
        }
        if c >= 65 && !white_space(c) {
            let mut prefix = [0; 3];
            let mut length = 0;
            while self.ch() >= 65 && !white_space(self.ch()) {
                if length < 3 {
                    let c = self.ch();
                    prefix[length] = if (65..=90).contains(&c) { c + 32 } else { c };
                }
                length += 1;
                self.position += 1;
            }
            return keyword(prefix, length);
        }
        if white_or_line(c) {
            self.position += 1;
            return Token::Space;
        }
        if c == 40 {
            let mut balance = 0;
            loop {
                match self.ch() {
                    40 => balance += 1,
                    41 => balance -= 1,
                    _ => {}
                }
                self.position += 1;
                if balance == 0 || self.ch() == 0 {
                    break;
                }
            }
            return Token::Unknown;
        }
        self.position += 1;
        Token::Unknown
    }
}
#[derive(Default)]
struct Day {
    values: Vec<i32>,
    month: Option<i32>,
    iso: bool,
}
impl Day {
    fn add(&mut self, n: i32) -> bool {
        if self.values.len() < 3 {
            self.values.push(n);
            true
        } else {
            false
        }
    }
    fn write(&mut self) -> Option<(i32, i32, i32)> {
        if self.values.is_empty() {
            return None;
        }
        while self.values.len() < 3 {
            self.values.push(1);
        }
        let a = self.values[0];
        let b = self.values[1];
        let c = self.values[2];
        let (mut year, month, day) = if let Some(month) = self.month {
            if !(1..=31).contains(&a) {
                (a, month, b)
            } else {
                (b, month, a)
            }
        } else if self.iso || !(1..=31).contains(&a) {
            (a, b, c)
        } else {
            (c, a, b)
        };
        if !self.iso {
            if (0..=49).contains(&year) {
                year += 2000;
            } else if (50..=99).contains(&year) {
                year += 1900;
            }
        }
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return None;
        }
        Some((year, month, day))
    }
}
#[derive(Default)]
struct Time {
    values: Vec<i32>,
    hour_offset: Option<i32>,
}
impl Time {
    fn expecting(&self, n: i32) -> bool {
        match self.values.len() {
            1 | 2 => (0..=59).contains(&n),
            3 => (0..=999).contains(&n),
            _ => false,
        }
    }
    fn add(&mut self, n: i32) -> bool {
        if self.values.len() < 4 {
            self.values.push(n);
            true
        } else {
            false
        }
    }
    fn final_value(&mut self, n: i32) -> bool {
        if !self.add(n) {
            return false;
        }
        while self.values.len() < 4 {
            self.values.push(0);
        }
        true
    }
    fn write(&mut self) -> Option<(i32, i32, i32, i32)> {
        while self.values.len() < 4 {
            self.values.push(0);
        }
        let mut h = self.values[0];
        let m = self.values[1];
        let s = self.values[2];
        let ms = self.values[3];
        if let Some(offset) = self.hour_offset {
            if !(0..=12).contains(&h) {
                return None;
            }
            h = h % 12 + offset;
        }
        if !((0..=23).contains(&h)
            && (0..=59).contains(&m)
            && (0..=59).contains(&s)
            && (0..=999).contains(&ms))
            && !(h == 24 && m == 0 && s == 0 && ms == 0)
        {
            return None;
        }
        Some((h, m, s, ms))
    }
}
#[derive(Default)]
struct Zone {
    sign: Option<i32>,
    hour: Option<i32>,
    minute: Option<i32>,
}
impl Zone {
    fn set(&mut self, h: i32) {
        self.sign = Some(if h < 0 { -1 } else { 1 });
        self.hour = Some(h.abs());
        self.minute = Some(0);
    }
    fn utc(&self) -> bool {
        self.hour == Some(0) && self.minute == Some(0)
    }
    fn expecting(&self, n: i32) -> bool {
        self.hour.is_some() && self.minute.is_none() && (0..=59).contains(&n)
    }
    fn write(&self) -> Option<Option<i32>> {
        let Some(sign) = self.sign else {
            return Some(None);
        };
        let seconds = (self.hour.unwrap_or(0) as u32)
            .wrapping_mul(3600)
            .wrapping_add((self.minute.unwrap_or(0) as u32).wrapping_mul(60));
        if seconds > SMI_MAX {
            return None;
        }
        Some(Some(seconds as i32 * sign))
    }
}
struct Parts {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    millisecond: i32,
    utc_offset: Option<i32>,
}
fn parse_iso(scanner: &mut Scanner<'_>, day: &mut Day, time: &mut Time, zone: &mut Zone) -> Token {
    if let Some(sign) = scanner.next.sign() {
        let token = scanner.take();
        let Some(year) = scanner.next.fixed(6) else {
            return token;
        };
        scanner.take();
        if sign < 0 && year == 0 {
            return token;
        }
        day.add(sign * year);
    } else if let Some(year) = scanner.next.fixed(4) {
        scanner.take();
        day.add(year);
    } else {
        return scanner.take();
    }
    if scanner.symbol(45) {
        let Some(month) = scanner.next.fixed(2).filter(|n| (1..=12).contains(n)) else {
            return scanner.take();
        };
        scanner.take();
        day.add(month);
        if scanner.symbol(45) {
            let Some(date) = scanner.next.fixed(2).filter(|n| (1..=31).contains(n)) else {
                return scanner.take();
            };
            scanner.take();
            day.add(date);
        }
    }
    if !matches!(scanner.next, Token::Word(Keyword::Separator, _, _)) {
        if scanner.next != Token::End {
            return scanner.take();
        }
    } else {
        scanner.take();
        let Some(hour) = scanner.next.fixed(2).filter(|n| (0..=24).contains(n)) else {
            return Token::Invalid;
        };
        scanner.take();
        time.add(hour);
        if !scanner.symbol(58) {
            return Token::Invalid;
        }
        let Some(minute) = scanner
            .next
            .fixed(2)
            .filter(|n| (0..=59).contains(n) && (hour != 24 || *n == 0))
        else {
            return Token::Invalid;
        };
        scanner.take();
        time.add(minute);
        if scanner.symbol(58) {
            let Some(second) = scanner
                .next
                .fixed(2)
                .filter(|n| (0..=59).contains(n) && (hour != 24 || *n == 0))
            else {
                return Token::Invalid;
            };
            scanner.take();
            time.add(second);
            if scanner.symbol(46) {
                let Some(n) = scanner.next.number() else {
                    return Token::Invalid;
                };
                if hour == 24 && n > 0 {
                    return Token::Invalid;
                }
                time.add(scanner.take().ms());
            }
        }
        if scanner.next.z() {
            scanner.take();
            zone.set(0);
        } else if let Some(sign) = scanner.next.sign() {
            zone.sign = Some(sign);
            scanner.take();
            if let Some(hm) = scanner.next.fixed(4) {
                scanner.take();
                let h = hm / 100;
                let m = hm % 100;
                if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
                    return Token::Invalid;
                }
                zone.hour = Some(h);
                zone.minute = Some(m);
            } else {
                let Some(h) = scanner.next.fixed(2).filter(|n| (0..=23).contains(n)) else {
                    return Token::Invalid;
                };
                scanner.take();
                zone.hour = Some(h);
                if !scanner.symbol(58) {
                    return Token::Invalid;
                }
                let Some(m) = scanner.next.fixed(2).filter(|n| (0..=59).contains(n)) else {
                    return Token::Invalid;
                };
                scanner.take();
                zone.minute = Some(m);
            }
        }
        if scanner.next != Token::End {
            return Token::Invalid;
        }
    }
    if zone.hour.is_none() && time.values.is_empty() {
        zone.set(0);
    }
    day.iso = true;
    Token::End
}
fn parse_parts(input: &[u16]) -> Option<Parts> {
    let mut scanner = Scanner::new(input);
    let mut day = Day::default();
    let mut time = Time::default();
    let mut zone = Zone::default();
    let mut token = parse_iso(&mut scanner, &mut day, &mut time, &mut zone);
    if token == Token::Invalid {
        return None;
    }
    let mut has_number = !day.values.is_empty();
    while token != Token::End {
        match token {
            Token::Number(n, _) => {
                has_number = true;
                if scanner.symbol(58) {
                    if scanner.symbol(58) {
                        if !time.values.is_empty() {
                            return None;
                        }
                        time.add(n);
                        time.add(0);
                    } else {
                        if !time.add(n) {
                            return None;
                        }
                        if scanner.next == Token::Symbol(46) {
                            scanner.take();
                        }
                    }
                } else if scanner.symbol(46) && time.expecting(n) {
                    time.add(n);
                    scanner.next.number()?;
                    let ms = scanner.take().ms();
                    if ms < 0 {
                        return None;
                    }
                    time.final_value(ms);
                } else if zone.expecting(n) {
                    zone.minute = Some(n);
                } else if time.expecting(n) {
                    time.final_value(n);
                    let next = scanner.next;
                    if next != Token::End
                        && next != Token::Space
                        && !next.z()
                        && next.sign().is_none()
                    {
                        return None;
                    }
                } else {
                    if !day.add(n) {
                        return None;
                    }
                    scanner.symbol(45);
                }
            }
            Token::Word(kind, value, _) => {
                if kind == Keyword::AmPm && !time.values.is_empty() {
                    time.hour_offset = Some(value);
                } else if kind == Keyword::Month {
                    day.month = Some(value);
                    scanner.symbol(45);
                } else if kind == Keyword::Zone && has_number {
                    zone.set(value);
                } else if has_number || scanner.next.number().is_some() {
                    return None;
                }
            }
            token if token.sign().is_some() && (zone.utc() || !time.values.is_empty()) => {
                zone.sign = token.sign();
                let (mut n, mut length) = (0, 0);
                if let Token::Number(value, len) = scanner.next {
                    scanner.take();
                    n = value;
                    length = len;
                }
                has_number = true;
                if scanner.next == Token::Symbol(58) {
                    zone.hour = Some(n);
                    zone.minute = None;
                } else if length == 1 || length == 2 {
                    zone.hour = Some(n);
                    zone.minute = Some(0);
                } else if length == 3 || length == 4 {
                    zone.hour = Some(n / 100);
                    zone.minute = Some(n % 100);
                } else {
                    return None;
                }
            }
            token if (token.sign().is_some() || token == Token::Symbol(41)) && has_number => {
                return None;
            }
            _ => {}
        }
        token = scanner.take();
    }
    let (year, month, date) = day.write()?;
    let (hour, minute, second, millisecond) = time.write()?;
    let utc_offset = zone.write()?;
    Some(Parts {
        year,
        month,
        day: date,
        hour,
        minute,
        second,
        millisecond,
        utc_offset,
    })
}
