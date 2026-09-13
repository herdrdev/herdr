// Compatibility for legacy raw-C1 XTGETTCAP only. Ordinary ESC P ... ESC \
// requests are answered exclusively by libghostty. Keep bytes untouched: C1
// normalization across the stream could corrupt UTF-8 or binary string payloads.
use bytes::Bytes;

#[derive(Debug, Default)]
pub(super) struct C1XtgettcapQueryTracker {
    state: C1XtgettcapTrackerState,
    raw_c1_intro: bool,
    native_dcs_pending: bool,
    body: Vec<u8>,
    pending: Vec<C1XtgettcapResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct C1XtgettcapResponse {
    pub(super) end_offset: usize,
    pub(super) bytes: Bytes,
    pub(super) suppress_native: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum C1XtgettcapTrackerState {
    #[default]
    Ground,
    Escape,
    DcsIntro,
    DcsIntroPlus,
    DcsBody,
    DcsEscape,
    IgnoreOsc,
    IgnoreOscEscape,
    IgnoreString,
    IgnoreStringEscape,
    OversizedDcs,
    OversizedDcsEscape,
}

impl C1XtgettcapQueryTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) {
        for (index, &byte) in bytes.iter().enumerate() {
            // With a 7-bit intro and raw ST, the native parser still holds the
            // DCS open. At its eventual unhook it can answer earlier keys in a
            // multi-key request again. Discard only that dispatch's XTGETTCAP
            // replies, not subsequent native requests or other response types.
            if self.native_dcs_pending && matches!(byte, 0x1b | 0x18 | 0x1a) {
                self.pending.push(C1XtgettcapResponse {
                    end_offset: index + 1,
                    bytes: Bytes::new(),
                    suppress_native: true,
                });
                self.native_dcs_pending = false;
            }
            match self.state {
                C1XtgettcapTrackerState::Ground => {
                    if byte == 0x1b {
                        self.state = C1XtgettcapTrackerState::Escape;
                    } else if byte == 0x90 {
                        self.body.clear();
                        self.raw_c1_intro = true;
                        self.state = C1XtgettcapTrackerState::DcsIntro;
                    } else if byte == 0x9d {
                        self.state = C1XtgettcapTrackerState::IgnoreOsc;
                    } else if matches!(byte, 0x98 | 0x9e | 0x9f) {
                        self.state = C1XtgettcapTrackerState::IgnoreString;
                    }
                }
                C1XtgettcapTrackerState::Escape => match byte {
                    b'P' => {
                        self.raw_c1_intro = false;
                        self.body.clear();
                        self.state = C1XtgettcapTrackerState::DcsIntro;
                    }
                    b']' => {
                        self.body.clear();
                        self.state = C1XtgettcapTrackerState::IgnoreOsc;
                    }
                    b'_' | b'^' | b'X' => {
                        self.body.clear();
                        self.state = C1XtgettcapTrackerState::IgnoreString;
                    }
                    0x1b => self.state = C1XtgettcapTrackerState::Escape,
                    _ => self.state = C1XtgettcapTrackerState::Ground,
                },
                C1XtgettcapTrackerState::DcsIntro => match byte {
                    b'+' => self.state = C1XtgettcapTrackerState::DcsIntroPlus,
                    0x1b => self.state = C1XtgettcapTrackerState::IgnoreStringEscape,
                    0x9c => self.state = C1XtgettcapTrackerState::Ground,
                    _ => self.state = C1XtgettcapTrackerState::IgnoreString,
                },
                C1XtgettcapTrackerState::DcsIntroPlus => match byte {
                    b'q' => self.state = C1XtgettcapTrackerState::DcsBody,
                    0x1b => self.state = C1XtgettcapTrackerState::IgnoreStringEscape,
                    0x9c => self.state = C1XtgettcapTrackerState::Ground,
                    _ => self.state = C1XtgettcapTrackerState::IgnoreString,
                },
                C1XtgettcapTrackerState::DcsBody => match byte {
                    0x1b => self.state = C1XtgettcapTrackerState::DcsEscape,
                    0x9c => {
                        self.native_dcs_pending |= !self.raw_c1_intro;
                        self.finalize(index + 1);
                        self.state = C1XtgettcapTrackerState::Ground;
                    }
                    _ => self.body.push(byte),
                },
                C1XtgettcapTrackerState::DcsEscape => {
                    if byte == b'\\' {
                        if self.raw_c1_intro {
                            self.finalize(index + 1);
                        } else {
                            self.body.clear();
                        }
                        self.state = C1XtgettcapTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.body.clear();
                        self.state = C1XtgettcapTrackerState::IgnoreString;
                    }
                }
                C1XtgettcapTrackerState::IgnoreOsc => {
                    if byte == 0x1b {
                        self.state = C1XtgettcapTrackerState::IgnoreOscEscape;
                    } else if matches!(byte, 0x07 | 0x9c) {
                        self.state = C1XtgettcapTrackerState::Ground;
                    }
                }
                C1XtgettcapTrackerState::IgnoreOscEscape => {
                    if byte == b'\\' {
                        self.state = C1XtgettcapTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = C1XtgettcapTrackerState::IgnoreOsc;
                    }
                }
                C1XtgettcapTrackerState::IgnoreString => {
                    if byte == 0x1b {
                        self.state = C1XtgettcapTrackerState::IgnoreStringEscape;
                    } else if byte == 0x9c {
                        self.state = C1XtgettcapTrackerState::Ground;
                    }
                }
                C1XtgettcapTrackerState::IgnoreStringEscape => {
                    if byte == b'\\' {
                        self.state = C1XtgettcapTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = C1XtgettcapTrackerState::IgnoreString;
                    }
                }
                C1XtgettcapTrackerState::OversizedDcs => {
                    if byte == 0x1b {
                        self.state = C1XtgettcapTrackerState::OversizedDcsEscape;
                    } else if byte == 0x9c {
                        self.state = C1XtgettcapTrackerState::Ground;
                    }
                }
                C1XtgettcapTrackerState::OversizedDcsEscape => {
                    if byte == b'\\' {
                        self.state = C1XtgettcapTrackerState::Ground;
                    } else if byte != 0x1b {
                        self.state = C1XtgettcapTrackerState::OversizedDcs;
                    }
                }
            }

            if self.body.len() > 1024 {
                self.body.clear();
                self.state = C1XtgettcapTrackerState::OversizedDcs;
            }
        }
    }

    fn finalize(&mut self, end_offset: usize) {
        for cap_hex in self.body.split(|byte| *byte == b';') {
            if let Some(bytes) = xtgettcap_response(cap_hex) {
                self.pending.push(C1XtgettcapResponse {
                    end_offset,
                    bytes,
                    suppress_native: false,
                });
            }
        }
        self.body.clear();
    }

    pub(super) fn drain_pending(&mut self) -> Vec<C1XtgettcapResponse> {
        std::mem::take(&mut self.pending)
    }
}

fn xtgettcap_response(cap_hex: &[u8]) -> Option<Bytes> {
    if cap_hex.is_empty() || !cap_hex.len().is_multiple_of(2) {
        return None;
    }

    let mut normalized_cap_hex = Vec::with_capacity(cap_hex.len());
    for &byte in cap_hex {
        if !byte.is_ascii_hexdigit() {
            return None;
        }
        normalized_cap_hex.push(byte.to_ascii_uppercase());
    }

    let value = xtgettcap_value(&normalized_cap_hex)?;
    Some(build_xtgettcap_response(&normalized_cap_hex, value))
}

fn xtgettcap_value(cap_hex: &[u8]) -> Option<Option<&'static [u8]>> {
    // Mirror only the Ghostty terminfo capabilities that this pane path can stand behind.
    match cap_hex {
        b"5463" => Some(None),
        b"524742" => Some(Some(b"8")),
        b"73657472676266" => Some(Some(b"\\E[38:2:%p1%d:%p2%d:%p3%dm")),
        b"73657472676262" => Some(Some(b"\\E[48:2:%p1%d:%p2%d:%p3%dm")),
        b"4D73" => Some(Some(b"\\E]52;%p1%s;%p2%s\\007")),
        b"5375" => Some(None),
        b"536D756C78" => Some(Some(b"\\E[4:%p1%dm")),
        b"536574756C63" => Some(Some(
            b"\\E[58:2::%p1%{65536}%/%d:%p1%{256}%/%{255}%&%d:%p1%{255}%&%d%;m",
        )),
        _ => None,
    }
}

fn build_xtgettcap_response(cap_hex: &[u8], value: Option<&[u8]>) -> Bytes {
    let mut response =
        Vec::with_capacity(8 + cap_hex.len() + value.map_or(0, |bytes| bytes.len() * 2));
    response.extend_from_slice(b"\x1bP1+r");
    response.extend_from_slice(cap_hex);
    if let Some(value) = value {
        response.push(b'=');
        append_upper_hex(value, &mut response);
    }
    response.extend_from_slice(b"\x1b\\");
    Bytes::from(response)
}

fn append_upper_hex(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        output.push(HEX[usize::from(byte >> 4)]);
        output.push(HEX[usize::from(byte & 0x0f)]);
    }
}
