//! Escape sequence parser.
//!
//! A table-free implementation of the DEC VT500 state machine described by
//! Paul Williams (<https://vt100.net/emu/dec_ansi_parser>), extended with:
//!
//! * incremental UTF-8 decoding in the ground state (sequences may be split
//!   across `advance` calls),
//! * colon separated sub-parameters (`CSI 4:3 m`, `CSI 38:2::r:g:b m`),
//! * APC string collection (used by the kitty graphics protocol),
//! * a fast path that hands runs of printable ASCII to the performer in one call.
//!
//! The parser is independent of any terminal state. It only reports what it
//! saw through the [`Perform`] trait.

/// Maximum number of parameters (including sub-parameters) kept for a sequence.
pub const MAX_PARAMS: usize = 32;
const MAX_INTERMEDIATES: usize = 2;
const MAX_OSC_PARAMS: usize = 16;
const MAX_OSC_LEN: usize = 64 * 1024;
const MAX_APC_LEN: usize = 64 * 1024 * 1024;

/// Receives parsed actions.
pub trait Perform {
    /// A single printable character.
    fn print(&mut self, c: char);

    /// A run of printable ASCII bytes (`0x20..=0x7e`). Override for speed.
    fn print_ascii(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.print(b as char);
        }
    }

    /// A run of printable characters, possibly non-ASCII. Override for speed.
    fn print_str(&mut self, text: &str) {
        for c in text.chars() {
            self.print(c);
        }
    }

    /// A C0 control character.
    fn execute(&mut self, byte: u8);

    /// A complete CSI sequence. Private markers (`?`, `>`, `<`, `=`) are part of
    /// `intermediates`. `ignore` is set when limits were exceeded.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: u8);

    /// A complete ESC sequence.
    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8);

    /// A complete OSC string, split on `;`.
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    /// Start of a DCS string.
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: u8) {}

    /// One byte of DCS payload.
    fn put(&mut self, _byte: u8) {}

    /// End of a DCS string.
    fn unhook(&mut self) {}

    /// A complete APC string (without the leading `ESC _`).
    fn apc_dispatch(&mut self, _data: &[u8]) {}
}

/// Numeric parameters of a CSI or DCS sequence.
#[derive(Clone, Debug, Default)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    /// Bit `i` set means `values[i]` is a sub-parameter of the value before it.
    sub: u32,
    len: usize,
}

impl Params {
    /// Total number of stored values, sub-parameters included.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn clear(&mut self) {
        self.len = 0;
        self.sub = 0;
    }

    fn push(&mut self, value: u16, is_sub: bool) -> bool {
        if self.len == MAX_PARAMS {
            return false;
        }
        self.values[self.len] = value;
        if is_sub {
            self.sub |= 1 << self.len;
        }
        self.len += 1;
        true
    }

    /// Iterates over parameter groups. Each group is a parameter followed by
    /// its colon separated sub-parameters.
    pub fn groups(&self) -> Groups<'_> {
        Groups { params: self, pos: 0 }
    }

    /// Returns the first value of group `index`, or `default` when the group is
    /// missing or zero. Zero means "default" for almost every CSI command.
    pub fn get_or(&self, index: usize, default: u16) -> u16 {
        match self.groups().nth(index) {
            Some(g) if g[0] != 0 => g[0],
            _ => default,
        }
    }

    /// Returns the raw first value of group `index`, zero included.
    pub fn raw(&self, index: usize) -> Option<u16> {
        self.groups().nth(index).map(|g| g[0])
    }
}

/// Iterator returned by [`Params::groups`].
#[derive(Clone)]
pub struct Groups<'a> {
    params: &'a Params,
    pos: usize,
}

impl<'a> Iterator for Groups<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        let p = self.params;
        if self.pos >= p.len {
            return None;
        }
        let start = self.pos;
        self.pos += 1;
        while self.pos < p.len && p.sub & (1 << self.pos) != 0 {
            self.pos += 1;
        }
        Some(&p.values[start..self.pos])
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmString,
    ApcString,
}

/// The parser. Feed bytes with [`Parser::advance`].
pub struct Parser {
    state: State,
    params: Params,
    /// Value being accumulated for the next parameter.
    current: u16,
    /// Whether any digit or separator was seen for the current parameter list.
    param_started: bool,
    next_is_sub: bool,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_len: usize,
    ignoring: bool,
    osc: Vec<u8>,
    apc: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: u8,
    utf8_need: u8,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            params: Params::default(),
            current: 0,
            param_started: false,
            next_is_sub: false,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_len: 0,
            ignoring: false,
            osc: Vec::new(),
            apc: Vec::new(),
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
        }
    }

    /// Parses `bytes`, calling into `performer` for every action.
    pub fn advance<P: Perform>(&mut self, performer: &mut P, bytes: &[u8]) {
        let mut i = 0;
        while i < bytes.len() {
            if self.state == State::Ground && self.utf8_need == 0 {
                let rest = &bytes[i..];
                let run = printable_ascii_len(rest);
                if run > 0 {
                    performer.print_ascii(&rest[..run]);
                    i += run;
                    continue;
                }
                if rest[0] >= 0x80 {
                    // Printable UTF-8, up to the next control character.
                    let run = control_position(rest);
                    let valid = match std::str::from_utf8(&rest[..run]) {
                        Ok(text) => {
                            performer.print_str(text);
                            run
                        }
                        Err(error) => {
                            let valid = error.valid_up_to();
                            // SAFETY: `valid_up_to` bytes were just validated as UTF-8.
                            let text = unsafe { std::str::from_utf8_unchecked(&rest[..valid]) };
                            if !text.is_empty() {
                                performer.print_str(text);
                            }
                            valid
                        }
                    };
                    i += valid;
                    if valid == run {
                        continue;
                    }
                    // Invalid or truncated sequence: fall through to the state machine.
                }
                if rest[0] == 0x1b
                    && rest.get(1) == Some(&b'[')
                    && let Some(used) = self.fast_csi(performer, &rest[2..])
                {
                    i += 2 + used;
                    continue;
                }
            }
            if matches!(self.state, State::OscString | State::ApcString) {
                // Collect string payloads (images can be megabytes) up to their terminator in one step.
                let rest = &bytes[i..];
                let end = if self.state == State::OscString {
                    control_position(rest)
                } else {
                    let stop = memchr::memchr3(0x07, 0x18, 0x1b, rest).unwrap_or(rest.len());
                    memchr::memchr(0x1a, &rest[..stop]).unwrap_or(stop)
                };
                if end > 0 {
                    if self.state == State::OscString {
                        self.push_osc(&rest[..end]);
                    } else {
                        let room = MAX_APC_LEN.saturating_sub(self.apc.len());
                        self.apc.extend_from_slice(&rest[..end.min(room)]);
                    }
                    i += end;
                    continue;
                }
            }
            self.byte(performer, bytes[i]);
            i += 1;
        }
    }

    /// Appends to the OSC string. iTerm2 inline images (`OSC 1337`) may be as
    /// long as APC strings; other OSC strings are capped much lower.
    fn push_osc(&mut self, chunk: &[u8]) {
        const IMAGE_PREFIX: &[u8; 5] = b"1337;";
        let mut head = [0u8; 5];
        let have = self.osc.len().min(5);
        head[..have].copy_from_slice(&self.osc[..have]);
        let added = (5 - have).min(chunk.len());
        head[have..have + added].copy_from_slice(&chunk[..added]);
        let limit = if &head == IMAGE_PREFIX { MAX_APC_LEN } else { MAX_OSC_LEN };
        let room = limit.saturating_sub(self.osc.len());
        self.osc.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }

    fn byte<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.state == State::Ground {
            return self.ground(p, b);
        }

        // Transitions valid from any state except ground.
        match b {
            0x18 | 0x1a => {
                self.end_string(p, false);
                p.execute(b);
                self.state = State::Ground;
                return;
            }
            0x1b => {
                self.end_string(p, false);
                self.enter_escape();
                return;
            }
            _ => {}
        }

        match self.state {
            State::Ground => unreachable!(),
            State::Escape => match b {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => p.execute(b),
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::EscapeIntermediate;
                }
                b'[' => self.enter_params(State::CsiEntry),
                b']' => {
                    self.osc.clear();
                    self.state = State::OscString;
                }
                b'P' => self.enter_params(State::DcsEntry),
                b'X' | b'^' => self.state = State::SosPmString,
                b'_' => {
                    self.apc.clear();
                    self.state = State::ApcString;
                }
                0x30..=0x7e => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::EscapeIntermediate => match b {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => p.execute(b),
                0x20..=0x2f => self.collect(b),
                0x30..=0x7e => {
                    p.esc_dispatch(self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiEntry | State::CsiParam => match b {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => p.execute(b),
                b'0'..=b'9' | b';' | b':' => {
                    self.param(b);
                    self.state = State::CsiParam;
                }
                0x3c..=0x3f => {
                    if self.state == State::CsiEntry {
                        self.collect(b);
                        self.state = State::CsiParam;
                    } else {
                        self.state = State::CsiIgnore;
                    }
                }
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::CsiIntermediate;
                }
                0x40..=0x7e => {
                    self.finish_params();
                    p.csi_dispatch(&self.params, self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIntermediate => match b {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => p.execute(b),
                0x20..=0x2f => self.collect(b),
                0x30..=0x3f => self.state = State::CsiIgnore,
                0x40..=0x7e => {
                    self.finish_params();
                    p.csi_dispatch(&self.params, self.intermediates(), self.ignoring, b);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiIgnore => match b {
                0x00..=0x17 | 0x19 | 0x1c..=0x1f => p.execute(b),
                0x40..=0x7e => self.state = State::Ground,
                _ => {}
            },
            State::DcsEntry | State::DcsParam => match b {
                b'0'..=b'9' | b';' | b':' => {
                    self.param(b);
                    self.state = State::DcsParam;
                }
                0x3c..=0x3f => {
                    if self.state == State::DcsEntry {
                        self.collect(b);
                        self.state = State::DcsParam;
                    } else {
                        self.state = State::DcsIgnore;
                    }
                }
                0x20..=0x2f => {
                    self.collect(b);
                    self.state = State::DcsIntermediate;
                }
                0x40..=0x7e => self.hook(p, b),
                _ => {}
            },
            State::DcsIntermediate => match b {
                0x20..=0x2f => self.collect(b),
                0x30..=0x3f => self.state = State::DcsIgnore,
                0x40..=0x7e => self.hook(p, b),
                _ => {}
            },
            State::DcsPassthrough => {
                if b != 0x7f {
                    p.put(b);
                }
            }
            State::DcsIgnore | State::SosPmString => {}
            State::OscString => match b {
                0x07 => {
                    self.dispatch_osc(p, true);
                    self.state = State::Ground;
                }
                0x00..=0x1f => {}
                _ => self.push_osc(&[b]),
            },
            State::ApcString => match b {
                0x07 => {
                    p.apc_dispatch(&self.apc);
                    self.apc.clear();
                    self.state = State::Ground;
                }
                _ => {
                    if self.apc.len() < MAX_APC_LEN {
                        self.apc.push(b);
                    }
                }
            },
        }
    }

    fn ground<P: Perform>(&mut self, p: &mut P, b: u8) {
        if self.utf8_need > 0 {
            if b & 0xc0 == 0x80 {
                self.utf8[self.utf8_len as usize] = b;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_need {
                    let len = self.utf8_len as usize;
                    self.utf8_need = 0;
                    self.utf8_len = 0;
                    let c = std::str::from_utf8(&self.utf8[..len])
                        .ok()
                        .and_then(|s| s.chars().next())
                        .unwrap_or(char::REPLACEMENT_CHARACTER);
                    p.print(c);
                }
                return;
            }
            // Truncated sequence: report it and reprocess this byte.
            self.utf8_need = 0;
            self.utf8_len = 0;
            p.print(char::REPLACEMENT_CHARACTER);
            return self.byte(p, b);
        }

        match b {
            0x1b => self.enter_escape(),
            0x00..=0x1f => p.execute(b),
            0x20..=0x7e => p.print(b as char),
            0x7f => {}
            0xc2..=0xdf => self.start_utf8(b, 2),
            0xe0..=0xef => self.start_utf8(b, 3),
            0xf0..=0xf4 => self.start_utf8(b, 4),
            _ => p.print(char::REPLACEMENT_CHARACTER),
        }
    }

    /// Parses a complete CSI sequence without intermediates in one pass, which
    /// covers nearly all SGR and cursor movement traffic. Returns the bytes
    /// consumed after `ESC [`, or `None` to fall back to the state machine.
    fn fast_csi<P: Perform>(&mut self, p: &mut P, rest: &[u8]) -> Option<usize> {
        self.params.clear();
        self.intermediate_len = 0;
        self.ignoring = false;
        let mut index = 0;
        if let Some(&marker @ 0x3c..=0x3f) = rest.first() {
            self.intermediates[0] = marker;
            self.intermediate_len = 1;
            index = 1;
        }
        let mut current: u32 = 0;
        let mut started = false;
        let mut sub = false;
        while index < rest.len() {
            let b = rest[index];
            match b {
                b'0'..=b'9' => {
                    current = (current * 10 + u32::from(b - b'0')).min(u32::from(u16::MAX));
                    started = true;
                }
                b';' | b':' => {
                    if !self.params.push(current as u16, sub) {
                        self.ignoring = true;
                    }
                    current = 0;
                    sub = b == b':';
                    started = true;
                }
                0x40..=0x7e => {
                    if started && !self.params.push(current as u16, sub) {
                        self.ignoring = true;
                    }
                    let intermediates = &self.intermediates[..self.intermediate_len];
                    p.csi_dispatch(&self.params, intermediates, self.ignoring, b);
                    return Some(index + 1);
                }
                _ => return None,
            }
            index += 1;
        }
        None
    }

    fn start_utf8(&mut self, b: u8, need: u8) {
        self.utf8[0] = b;
        self.utf8_len = 1;
        self.utf8_need = need;
    }

    fn enter_escape(&mut self) {
        self.intermediate_len = 0;
        self.ignoring = false;
        self.state = State::Escape;
    }

    fn enter_params(&mut self, state: State) {
        self.params.clear();
        self.current = 0;
        self.param_started = false;
        self.next_is_sub = false;
        self.intermediate_len = 0;
        self.ignoring = false;
        self.state = state;
    }

    fn collect(&mut self, b: u8) {
        if self.intermediate_len == MAX_INTERMEDIATES {
            self.ignoring = true;
        } else {
            self.intermediates[self.intermediate_len] = b;
            self.intermediate_len += 1;
        }
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediate_len]
    }

    fn param(&mut self, b: u8) {
        self.param_started = true;
        match b {
            b';' | b':' => {
                if !self.params.push(self.current, self.next_is_sub) {
                    self.ignoring = true;
                }
                self.current = 0;
                self.next_is_sub = b == b':';
            }
            _ => {
                self.current = self.current.saturating_mul(10).saturating_add(u16::from(b - b'0'));
            }
        }
    }

    fn finish_params(&mut self) {
        if self.param_started && !self.params.push(self.current, self.next_is_sub) {
            self.ignoring = true;
        }
    }

    fn hook<P: Perform>(&mut self, p: &mut P, b: u8) {
        self.finish_params();
        p.hook(&self.params, self.intermediates(), self.ignoring, b);
        self.state = State::DcsPassthrough;
    }

    /// Terminates a string state when ESC, CAN or SUB interrupts it.
    fn end_string<P: Perform>(&mut self, p: &mut P, bell: bool) {
        match self.state {
            State::OscString => self.dispatch_osc(p, bell),
            State::DcsPassthrough => p.unhook(),
            State::ApcString => {
                p.apc_dispatch(&self.apc);
                self.apc.clear();
            }
            _ => {}
        }
    }

    fn dispatch_osc<P: Perform>(&mut self, p: &mut P, bell: bool) {
        let mut slices: [&[u8]; MAX_OSC_PARAMS] = [&[]; MAX_OSC_PARAMS];
        let mut count = 0;
        let mut rest: &[u8] = &self.osc;
        loop {
            if count == MAX_OSC_PARAMS - 1 {
                slices[count] = rest;
                count += 1;
                break;
            }
            match memchr::memchr(b';', rest) {
                Some(pos) => {
                    slices[count] = &rest[..pos];
                    rest = &rest[pos + 1..];
                    count += 1;
                }
                None => {
                    slices[count] = rest;
                    count += 1;
                    break;
                }
            }
        }
        p.osc_dispatch(&slices[..count], bell);
        if self.osc.capacity() > MAX_OSC_LEN {
            self.osc = Vec::new();
        } else {
            self.osc.clear();
        }
    }
}

/// Length of the leading run of printable ASCII bytes (`0x20..=0x7e`).
#[inline]
fn printable_ascii_len(bytes: &[u8]) -> usize {
    let mut i = 0;
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{
            _mm_and_si128, _mm_cmpgt_epi8, _mm_cmplt_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
        };
        // SAFETY: SSE2 is part of the x86_64 baseline. Loads are unaligned and
        // stay in bounds because `i + 16 <= len`.
        unsafe {
            let low = _mm_set1_epi8(0x1f);
            let high = _mm_set1_epi8(0x7f);
            while i + 16 <= bytes.len() {
                let block = _mm_loadu_si128(bytes.as_ptr().add(i).cast());
                // Signed compares: bytes of 0x80 and above are negative and fail `> 0x1f`.
                let printable = _mm_and_si128(_mm_cmpgt_epi8(block, low), _mm_cmplt_epi8(block, high));
                let mask = _mm_movemask_epi8(printable) as u32;
                if mask != 0xffff {
                    return i + (!mask).trailing_zeros() as usize;
                }
                i += 16;
            }
        }
    }
    i + bytes[i..].iter().position(|&b| !(0x20..0x7f).contains(&b)).unwrap_or(bytes.len() - i)
}

/// Index of the first C0 control or DEL byte, or the length when there is none.
#[inline]
fn control_position(bytes: &[u8]) -> usize {
    let mut i = 0;
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::{
            _mm_cmpeq_epi8, _mm_loadu_si128, _mm_min_epu8, _mm_movemask_epi8, _mm_or_si128, _mm_set1_epi8,
        };
        // SAFETY: SSE2 is part of the x86_64 baseline. Loads are unaligned and
        // stay in bounds because `i + 16 <= len`.
        unsafe {
            let c0_max = _mm_set1_epi8(0x1f);
            let del = _mm_set1_epi8(0x7f);
            while i + 16 <= bytes.len() {
                let block = _mm_loadu_si128(bytes.as_ptr().add(i).cast());
                // Unsigned `byte <= 0x1f` is `min(byte, 0x1f) == byte`.
                let c0 = _mm_cmpeq_epi8(_mm_min_epu8(block, c0_max), block);
                let mask = _mm_movemask_epi8(_mm_or_si128(c0, _mm_cmpeq_epi8(block, del))) as u32;
                if mask != 0 {
                    return i + mask.trailing_zeros() as usize;
                }
                i += 16;
            }
        }
    }
    i + bytes[i..].iter().position(|&b| b < 0x20 || b == 0x7f).unwrap_or(bytes.len() - i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_scans_match_byte_scans() {
        let mut seed: u32 = 0x9e37_79b9;
        for len in 0..80 {
            for _ in 0..200 {
                let bytes: Vec<u8> = (0..len)
                    .map(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        // Mostly printable, sometimes controls, DEL or high bytes.
                        match seed % 16 {
                            0 => (seed >> 8) as u8 % 0x20,
                            1 => 0x7f,
                            2 => 0x80 | (seed >> 8) as u8,
                            _ => 0x20 + (seed >> 8) as u8 % 0x5f,
                        }
                    })
                    .collect();
                let ascii = bytes.iter().position(|&b| !(0x20..0x7f).contains(&b)).unwrap_or(bytes.len());
                let control = bytes.iter().position(|&b| b < 0x20 || b == 0x7f).unwrap_or(bytes.len());
                assert_eq!(printable_ascii_len(&bytes), ascii, "{bytes:?}");
                assert_eq!(control_position(&bytes), control, "{bytes:?}");
            }
        }
    }

    #[derive(Debug, PartialEq)]
    enum Action {
        Print(String),
        Exec(u8),
        Csi(Vec<Vec<u16>>, Vec<u8>, u8),
        Esc(Vec<u8>, u8),
        Osc(Vec<Vec<u8>>, bool),
        Apc(Vec<u8>),
    }

    #[derive(Default)]
    struct Recorder(Vec<Action>);

    impl Perform for Recorder {
        fn print(&mut self, c: char) {
            if let Some(Action::Print(s)) = self.0.last_mut() {
                s.push(c);
            } else {
                self.0.push(Action::Print(c.to_string()));
            }
        }
        fn execute(&mut self, byte: u8) {
            self.0.push(Action::Exec(byte));
        }
        fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: u8) {
            let groups = params.groups().map(<[u16]>::to_vec).collect();
            self.0.push(Action::Csi(groups, inter.to_vec(), action));
        }
        fn esc_dispatch(&mut self, inter: &[u8], _ignore: bool, byte: u8) {
            self.0.push(Action::Esc(inter.to_vec(), byte));
        }
        fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
            self.0.push(Action::Osc(params.iter().map(|p| p.to_vec()).collect(), bell));
        }
        fn apc_dispatch(&mut self, data: &[u8]) {
            self.0.push(Action::Apc(data.to_vec()));
        }
    }

    fn parse(input: &[u8]) -> Vec<Action> {
        let mut r = Recorder::default();
        Parser::new().advance(&mut r, input);
        r.0
    }

    #[test]
    fn plain_text_and_controls() {
        assert_eq!(parse(b"hi\r\n"), vec![Action::Print("hi".into()), Action::Exec(b'\r'), Action::Exec(b'\n')]);
    }

    #[test]
    fn csi_params_and_private_marker() {
        assert_eq!(
            parse(b"\x1b[?1049h\x1b[1;31m\x1b[m"),
            vec![
                Action::Csi(vec![vec![1049]], b"?".to_vec(), b'h'),
                Action::Csi(vec![vec![1], vec![31]], vec![], b'm'),
                Action::Csi(vec![], vec![], b'm'),
            ]
        );
    }

    #[test]
    fn csi_empty_params_are_zero() {
        assert_eq!(parse(b"\x1b[;5H"), vec![Action::Csi(vec![vec![0], vec![5]], vec![], b'H')]);
    }

    #[test]
    fn csi_subparams() {
        assert_eq!(
            parse(b"\x1b[4:3;38:2::10:20:30m"),
            vec![Action::Csi(vec![vec![4, 3], vec![38, 2, 0, 10, 20, 30]], vec![], b'm')]
        );
    }

    #[test]
    fn csi_split_across_chunks_matches_single_chunk() {
        let input = b"\x1b[1;38:2::10:20:30m\x1b[?25l\x1b[2 q\x1b[12;40H";
        let whole = parse(input);
        let mut r = Recorder::default();
        let mut p = Parser::new();
        for chunk in input.chunks(3) {
            p.advance(&mut r, chunk);
        }
        assert_eq!(whole, r.0);
        assert_eq!(whole.len(), 4);
    }

    #[test]
    fn csi_with_intermediate() {
        assert_eq!(parse(b"\x1b[2 q"), vec![Action::Csi(vec![vec![2]], b" ".to_vec(), b'q')]);
    }

    #[test]
    fn osc_bel_and_st() {
        assert_eq!(
            parse(b"\x1b]0;title\x07\x1b]2;a;b\x1b\\"),
            vec![
                Action::Osc(vec![b"0".to_vec(), b"title".to_vec()], true),
                Action::Osc(vec![b"2".to_vec(), b"a".to_vec(), b"b".to_vec()], false),
                Action::Esc(vec![], b'\\'),
            ]
        );
    }

    #[test]
    fn apc_string() {
        assert_eq!(
            parse(b"\x1b_Gf=100;AAAA\x1b\\"),
            vec![Action::Apc(b"Gf=100;AAAA".to_vec()), Action::Esc(vec![], b'\\')]
        );
    }

    #[test]
    fn utf8_split_across_chunks() {
        let mut r = Recorder::default();
        let mut p = Parser::new();
        let bytes = "aé€😀".as_bytes();
        for b in bytes {
            p.advance(&mut r, std::slice::from_ref(b));
        }
        assert_eq!(r.0, vec![Action::Print("aé€😀".into())]);
    }

    #[test]
    fn utf8_run_with_trailing_partial_sequence() {
        let mut r = Recorder::default();
        let mut p = Parser::new();
        let bytes = "λx€".as_bytes();
        p.advance(&mut r, &bytes[..bytes.len() - 1]);
        p.advance(&mut r, &bytes[bytes.len() - 1..]);
        p.advance(&mut r, b"\xffy\r");
        assert_eq!(r.0, vec![Action::Print("λx€\u{fffd}y".into()), Action::Exec(b'\r')]);
    }

    #[test]
    fn invalid_utf8_yields_replacement() {
        assert_eq!(parse(b"\xe2\x82x"), vec![Action::Print("\u{fffd}x".into())]);
        assert_eq!(parse(b"\xff"), vec![Action::Print("\u{fffd}".into())]);
    }

    #[test]
    fn long_strings_arrive_whole_across_chunks() {
        let image = format!("\x1b]1337;File=inline=1:{}\x07", "A".repeat(200_000));
        let kitty = format!("\x1b_Ga=t;{}\x1b\\", "B".repeat(100_000));
        let long_title = format!("\x1b]2;{}\x07", "C".repeat(100_000));
        let input = format!("{image}{kitty}{long_title}");
        let mut r = Recorder::default();
        let mut p = Parser::new();
        for chunk in input.as_bytes().chunks(4096) {
            p.advance(&mut r, chunk);
        }
        let Action::Osc(image, _) = &r.0[0] else { panic!("{:?}", r.0[0]) };
        assert_eq!(image[1].len(), 200_000 + "File=inline=1:".len());
        let Action::Apc(kitty) = &r.0[1] else { panic!() };
        assert_eq!(kitty.len(), 100_000 + "Ga=t;".len());
        let Action::Osc(title, _) = &r.0[3] else { panic!("{:?}", r.0[3]) };
        assert_eq!(title[1].len(), MAX_OSC_LEN - 2);
    }

    #[test]
    fn esc_dispatch_with_intermediate() {
        assert_eq!(parse(b"\x1b(0\x1b7"), vec![Action::Esc(b"(".to_vec(), b'0'), Action::Esc(vec![], b'7')]);
    }

    #[test]
    fn can_aborts_sequence() {
        assert_eq!(parse(b"\x1b[12\x18x"), vec![Action::Exec(0x18), Action::Print("x".into())]);
    }
}
