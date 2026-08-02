const MAX_RESPONSE_BYTES: usize = 4096;
const KITTY_PREFIX: &[u8] = b"\x1b_G";
const KITTY_SUFFIX: &[u8] = b"\x1b\\";
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const LATE_RESPONSE_DRAIN: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Response {
    pub(super) transfer_id: u64,
    pub(super) image_id: u32,
    pub(super) success: bool,
}

#[derive(Debug, Default)]
pub(super) struct ResponseMatcher {
    expected: Option<(u64, u32, Option<std::time::Instant>)>,
    retired: Option<(u32, std::time::Instant)>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl ResponseMatcher {
    pub(super) fn active_handle(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.active.clone()
    }

    fn refresh_active(&self) {
        self.active.store(
            self.expected.is_some() || self.retired.is_some(),
            std::sync::atomic::Ordering::Release,
        );
    }
    pub(super) fn interested(&mut self) -> bool {
        self.expire();
        self.expected.is_some() || self.retired.is_some()
    }
    pub(super) fn arm(&mut self, transfer_id: u64, image_id: u32) -> bool {
        self.expire();
        if self.expected.is_some() {
            return false;
        }
        self.expected = Some((transfer_id, image_id, None));
        self.refresh_active();
        true
    }

    pub(super) fn start(&mut self, transfer_id: u64) {
        self.start_at(transfer_id, std::time::Instant::now());
    }

    fn start_at(&mut self, transfer_id: u64, now: std::time::Instant) {
        if let Some((id, _, deadline)) = &mut self.expected {
            if *id == transfer_id {
                *deadline = Some(now + RESPONSE_TIMEOUT);
            }
        }
    }

    pub(super) fn cancel(&mut self, transfer_id: u64) {
        if self.expected.is_some_and(|(id, _, _)| id == transfer_id) {
            self.expected = None;
            self.refresh_active();
        }
    }

    pub(super) fn retire(&mut self, transfer_id: u64) {
        if self.expected.is_some_and(|(id, _, _)| id == transfer_id) {
            if let Some((_, image_id, _)) = self.expected.take() {
                self.retired = Some((image_id, std::time::Instant::now() + LATE_RESPONSE_DRAIN));
                self.refresh_active();
            }
        }
    }

    pub(super) fn expire(&mut self) {
        self.expire_at(std::time::Instant::now());
    }

    fn expire_at(&mut self, now: std::time::Instant) {
        if self
            .expected
            .is_some_and(|(_, _, deadline)| deadline.is_some_and(|deadline| deadline <= now))
        {
            if let Some((_, image_id, _)) = self.expected.take() {
                self.retired = Some((image_id, now + LATE_RESPONSE_DRAIN));
            }
        }
        if self.retired.is_some_and(|(_, deadline)| deadline <= now) {
            self.retired = None;
        }
        self.refresh_active();
    }

    pub(super) fn consume(&mut self, bytes: &[u8]) -> Option<Option<Response>> {
        self.expire();
        if !bytes.starts_with(b"\x1b_G")
            || bytes.len() > MAX_RESPONSE_BYTES
            || !bytes.ends_with(b"\x1b\\")
        {
            return None;
        }
        let payload = &bytes[3..bytes.len() - 2];
        let separator = payload.iter().position(|byte| *byte == b';')?;
        if let Some((retired_id, _)) = self.retired {
            if matching_response_controls(&payload[..separator], retired_id) {
                self.retired = None;
                self.refresh_active();
                return Some(None);
            }
        }
        let (transfer_id, image_id, _) = self.expected?;
        if !matching_response_controls(&payload[..separator], image_id) {
            return None;
        }
        self.expected = None;
        self.refresh_active();
        Some(Some(Response {
            transfer_id,
            image_id,
            success: &payload[separator + 1..] == b"OK",
        }))
    }
}

#[derive(Default)]
pub(super) struct InputFilter {
    pending: Vec<u8>,
}

impl InputFilter {
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn flush_if_inactive(&mut self, matcher: &mut ResponseMatcher) -> Option<Vec<u8>> {
        (self.has_pending() && !matcher.interested()).then(|| std::mem::take(&mut self.pending))
    }

    pub(super) fn push(
        &mut self,
        bytes: &[u8],
        matcher: &mut ResponseMatcher,
    ) -> (Vec<Vec<u8>>, Vec<Response>) {
        self.pending.extend_from_slice(bytes);
        let mut output = Vec::new();
        let mut responses = Vec::new();
        if !matcher.interested() {
            output.push(std::mem::take(&mut self.pending));
            return (output, responses);
        }
        loop {
            let Some(start) = self
                .pending
                .windows(3)
                .position(|window| window == KITTY_PREFIX)
            else {
                let keep = (1..KITTY_PREFIX.len())
                    .rev()
                    .find(|len| self.pending.ends_with(&KITTY_PREFIX[..*len]))
                    .unwrap_or(0);
                let emit = self.pending.len().saturating_sub(keep);
                if emit > 0 {
                    output.push(self.pending.drain(..emit).collect());
                }
                break;
            };
            if start > 0 {
                output.push(self.pending.drain(..start).collect());
            }
            let end = self
                .pending
                .windows(2)
                .position(|window| window == KITTY_SUFFIX);
            let Some(end) = end else {
                if self.pending.len() > MAX_RESPONSE_BYTES {
                    output.push(std::mem::take(&mut self.pending));
                }
                break;
            };
            let command: Vec<u8> = self.pending.drain(..end + 2).collect();
            match matcher.consume(&command) {
                Some(Some(response)) => responses.push(response),
                Some(None) => {}
                None => output.push(command),
            }
        }
        (output, responses)
    }
}

fn matching_response_controls(bytes: &[u8], expected: u32) -> bool {
    let mut matched = false;
    for field in bytes.split(|byte| *byte == b',') {
        let Some(separator) = field.iter().position(|byte| *byte == b'=') else {
            return false;
        };
        let (key, value) = field.split_at(separator);
        let value = &value[1..];
        if value.is_empty() || !value.iter().all(u8::is_ascii_digit) {
            return false;
        }
        match key {
            b"i" if !matched
                && std::str::from_utf8(value).ok().and_then(|v| v.parse().ok())
                    == Some(expected) =>
            {
                matched = true
            }
            b"I" | b"p" => {}
            _ => return false,
        }
    }
    matched
}

pub(super) fn valid_control(control: &str, image_id: u32) -> bool {
    if control.len() > 1024 || control.contains([';', '\x1b']) {
        return false;
    }
    let mut action = false;
    let mut format = false;
    let mut image = false;
    let mut quiet = false;
    let mut cursor = false;
    for field in control.split(',') {
        let Some((key, value)) = field.split_once('=') else {
            return false;
        };
        if key == "t"
            || !matches!(
                key,
                "a" | "f"
                    | "s"
                    | "v"
                    | "i"
                    | "p"
                    | "c"
                    | "r"
                    | "z"
                    | "C"
                    | "q"
                    | "x"
                    | "y"
                    | "w"
                    | "h"
                    | "X"
                    | "Y"
            )
        {
            return false;
        }
        let numeric = value
            .strip_prefix('-')
            .unwrap_or(value)
            .bytes()
            .all(|byte| byte.is_ascii_digit())
            && !value.is_empty();
        if key != "a" && !numeric {
            return false;
        }
        match key {
            "a" => action = value == "T",
            "f" => format = value == "32",
            "i" => image = value.parse() == Ok(image_id),
            "q" => quiet = value == "0",
            "C" => cursor = value == "1",
            _ => {}
        }
    }
    action && format && image && quiet && cursor
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_the_armed_image_and_preserves_unrelated_input() {
        let mut matcher = ResponseMatcher::default();
        let active = matcher.active_handle();
        assert!(matcher.arm(7, 42));
        assert!(active.load(std::sync::atomic::Ordering::Acquire));
        assert!(!matcher.arm(8, 43));
        assert_eq!(matcher.consume(b"typed"), None);
        assert_eq!(matcher.consume(b"\x1b_Gi=41;OK\x1b\\"), None);
        assert_eq!(
            matcher.consume(b"\x1b_Gi=42;OK\x1b\\"),
            Some(Some(Response {
                transfer_id: 7,
                image_id: 42,
                success: true,
            }))
        );
        assert!(!active.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn explicit_error_is_reported_and_malformed_responses_are_ignored() {
        let mut matcher = ResponseMatcher::default();
        matcher.arm(9, 44);
        assert_eq!(
            matcher.consume(b"\x1b_Gi=44;ENOENT\x1b\\"),
            Some(Some(Response {
                transfer_id: 9,
                image_id: 44,
                success: false,
            }))
        );
        matcher.arm(10, 45);
        assert_eq!(matcher.consume(b"\x1b_Gi=45;broken"), None);
        matcher.cancel(10);
        matcher.arm(11, 46);
        assert_eq!(matcher.consume(b"\x1b_Gi=46oops;OK\x1b\\"), None);
    }

    #[test]
    fn validated_control_is_one_owned_rgba_transmit_and_display() {
        assert!(valid_control(
            "a=T,f=32,s=10,v=20,i=42,p=7,c=5,r=6,z=-1,C=1,q=0,x=2",
            42
        ));
        for invalid in [
            "a=T,f=24,i=42,C=1,q=0",
            "a=T,f=32,i=41,C=1,q=0",
            "a=T,t=f,f=32,i=42,C=1,q=0",
            "a=p,f=32,i=42,C=1,q=0",
        ] {
            assert!(!valid_control(invalid, 42), "{invalid}");
        }
    }

    #[test]
    fn fragmented_response_is_held_before_generic_input_timeout() {
        let mut matcher = ResponseMatcher::default();
        let mut filter = InputFilter::default();
        matcher.arm(19, 49);
        for foreign in [
            b"\x1b_Ga=p,i=49;OK\x1b\\".as_slice(),
            b"\x1b_Gi=49oops;OK\x1b\\",
        ] {
            let (output, responses) = filter.push(foreign, &mut matcher);
            assert_eq!(output, [foreign.to_vec()]);
            assert!(responses.is_empty());
        }
        let (output, responses) = filter.push(b"typed\x1b_Gi=49;", &mut matcher);
        assert_eq!(output, [b"typed".to_vec()]);
        assert!(responses.is_empty());
        let (output, responses) = filter.push(b"OK\x1b\\tail", &mut matcher);
        assert_eq!(output, [b"tail".to_vec()]);
        assert_eq!(responses[0].transfer_id, 19);
        assert!(responses[0].success);
    }

    #[test]
    fn attempted_output_failure_retires_response_without_forwarding_it() {
        let mut matcher = ResponseMatcher::default();
        let mut filter = InputFilter::default();
        matcher.arm(20, 50);
        matcher.retire(20);
        for foreign in [
            b"\x1b_Ga=p,i=50;OK\x1b\\".as_slice(),
            b"\x1b_Gi=50oops;OK\x1b\\",
        ] {
            let (output, responses) = filter.push(foreign, &mut matcher);
            assert_eq!(output, [foreign.to_vec()]);
            assert!(responses.is_empty());
        }
        assert!(filter
            .push(b"\x1b_Gi=50;EINVAL\x1b\\", &mut matcher)
            .0
            .is_empty());
    }

    #[test]
    fn timed_out_expectation_requires_a_new_transfer_before_matching() {
        let mut matcher = ResponseMatcher::default();
        let started = std::time::Instant::now();
        assert!(matcher.arm(12, 47));
        matcher.start_at(12, started);
        assert!(!matcher.arm(13, 48));
        matcher.expire_at(started + RESPONSE_TIMEOUT);
        assert!(matcher.arm(13, 48));
        assert_eq!(matcher.consume(b"typed"), None);
        assert_eq!(matcher.consume(b"\x1b_Gi=47;OK\x1b\\"), Some(None));
        assert_eq!(
            matcher.consume(b"\x1b_Gi=48;OK\x1b\\"),
            Some(Some(Response {
                transfer_id: 13,
                image_id: 48,
                success: true,
            }))
        );
    }
}
