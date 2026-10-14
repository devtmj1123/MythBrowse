// SPDX-License-Identifier: MPL-2.0
// Copyright (c) 2026 JustMyth / Tan Ming Jing
//! Frame timing for the shell's perf HUD and trace (`MB_PERF=1`, or Ctrl+Shift+F12 at run
//! time): per presented frame, how long the shell spent where, and how long a wheel / scroll
//! input took to reach the screen. Summarised once a second into one line, shown in the HUD
//! and appended to `perf.log` in the app's data folder (`%LOCALAPPDATA%\MythBrowse\perf.log`
//! on Windows), so a report from a real device carries numbers instead of "it is laggy".
//!
//! Pure (times passed in, ms `f64`), unit-tested; the shell owns the clock and the file.

/// One presented frame, ms.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameSample {
    /// Start of `paint()` to the return of `present()`.
    pub total: f64,
    /// `WebView::paint()` of the page (WebRender render into its offscreen target); 0 when
    /// the page had no new frame.
    pub servo_page: f64,
    /// `WebView::paint()` of the chrome; 0 when the chrome had no new frame.
    pub servo_chrome: f64,
    /// Sky + blur + glass + layers (the shell's own GL composition).
    pub compose: f64,
    /// `present()` (swap; blocks on vsync).
    pub present: f64,
    /// The page had a new frame in this paint.
    pub page_frame: bool,
}

/// Which way wheel scrolls went in the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollPath {
    #[default]
    None,
    /// `WebView::notify_scroll_event` (compositor only).
    Fast,
    /// `WheelEvent` through the page's script thread (pages with blocking wheel listeners).
    Dom,
    Both,
}

impl ScrollPath {
    fn add(self, other: ScrollPath) -> ScrollPath {
        match (self, other) {
            (ScrollPath::None, o) | (o, ScrollPath::None) => o,
            (a, b) if a == b => a,
            _ => ScrollPath::Both,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ScrollPath::None => "-",
            ScrollPath::Fast => "fast",
            ScrollPath::Dom => "dom",
            ScrollPath::Both => "fast+dom",
        }
    }
}

/// p50 / p95 / max of a set (ms); zeros when empty.
pub fn percentiles(values: &[f64]) -> (f64, f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize];
    (at(0.5), at(0.95), v[v.len() - 1])
}

/// Collects one window (about a second) of samples.
#[derive(Debug, Clone, Default)]
pub struct PerfWindow {
    start_ms: Option<f64>,
    frames: Vec<FrameSample>,
    /// Frame-to-frame intervals (present to present), ms.
    intervals: Vec<f64>,
    last_present_ms: Option<f64>,
    /// Input to the first present that showed a page frame after it, ms.
    latencies: Vec<f64>,
    /// The oldest input not yet on screen.
    pending_input_ms: Option<f64>,
    /// A scroll for that input has been handed to Servo (the next page frame shows it).
    input_sent: bool,
    wheel_events: u32,
    scroll_sends: u32,
    path: ScrollPath,
}

/// One second, summarised.
#[derive(Debug, Clone, PartialEq)]
pub struct PerfSummary {
    pub seconds: f64,
    pub frames: usize,
    pub fps: f64,
    pub page_frames: usize,
    pub frame_p50: f64,
    pub frame_p95: f64,
    pub frame_max: f64,
    pub interval_p95: f64,
    pub interval_max: f64,
    pub servo_page_avg: f64,
    pub servo_chrome_avg: f64,
    pub compose_avg: f64,
    pub present_avg: f64,
    pub latency_p50: f64,
    pub latency_p95: f64,
    pub latency_n: usize,
    pub wheel_events: u32,
    pub scroll_sends: u32,
    pub path: ScrollPath,
}

impl PerfSummary {
    /// One log / HUD line.
    pub fn line(&self) -> String {
        format!(
            "fps {:.0} (page {}) | frame p50 {:.1} p95 {:.1} max {:.1} | gap p95 {:.1} max {:.1} | \
             servo page {:.1} chrome {:.1} | compose {:.1} | present {:.1} | \
             input->screen p50 {:.0} p95 {:.0} (n {}) | wheel {} sent {} path {}",
            self.fps,
            self.page_frames,
            self.frame_p50,
            self.frame_p95,
            self.frame_max,
            self.interval_p95,
            self.interval_max,
            self.servo_page_avg,
            self.servo_chrome_avg,
            self.compose_avg,
            self.present_avg,
            self.latency_p50,
            self.latency_p95,
            self.latency_n,
            self.wheel_events,
            self.scroll_sends,
            self.path.as_str(),
        )
    }
}

/// Window length before a summary is due.
pub const WINDOW_MS: f64 = 1000.0;

impl PerfWindow {
    /// A wheel / autoscroll input arrived at `now_ms`.
    pub fn on_input(&mut self, now_ms: f64) {
        self.wheel_events += 1;
        if self.pending_input_ms.is_none() {
            self.pending_input_ms = Some(now_ms);
            self.input_sent = false;
        }
    }

    /// A scroll was handed to Servo on `path`.
    pub fn on_scroll_sent(&mut self, path: ScrollPath) {
        self.scroll_sends += 1;
        self.path = self.path.add(path);
        if self.pending_input_ms.is_some() {
            self.input_sent = true;
        }
    }

    /// A frame was presented at `now_ms` (the return of `present()`).
    pub fn on_frame(&mut self, now_ms: f64, s: FrameSample) {
        self.start_ms.get_or_insert(now_ms - s.total);
        if let Some(last) = self.last_present_ms {
            self.intervals.push(now_ms - last);
        }
        self.last_present_ms = Some(now_ms);
        // A page frame painted after the scroll was sent is the first one that can show it.
        if s.page_frame
            && self.input_sent
            && let Some(t) = self.pending_input_ms.take()
        {
            self.latencies.push(now_ms - t);
            self.input_sent = false;
        }
        self.frames.push(s);
    }

    /// The summary once [`WINDOW_MS`] has passed (and the window starts over), else `None`.
    /// An idle window (no frames) yields nothing.
    pub fn take_due(&mut self, now_ms: f64) -> Option<PerfSummary> {
        let start = self.start_ms?;
        if now_ms - start < WINDOW_MS {
            return None;
        }
        let secs = ((now_ms - start) / 1000.0).max(1e-3);
        let n = self.frames.len();
        let totals: Vec<f64> = self.frames.iter().map(|f| f.total).collect();
        let (p50, p95, max) = percentiles(&totals);
        let (_, ip95, imax) = percentiles(&self.intervals);
        let (l50, l95, _) = percentiles(&self.latencies);
        let avg = |f: fn(&FrameSample) -> f64| {
            if n == 0 {
                0.0
            } else {
                self.frames.iter().map(f).sum::<f64>() / n as f64
            }
        };
        let out = PerfSummary {
            seconds: secs,
            frames: n,
            fps: n as f64 / secs,
            page_frames: self.frames.iter().filter(|f| f.page_frame).count(),
            frame_p50: p50,
            frame_p95: p95,
            frame_max: max,
            interval_p95: ip95,
            interval_max: imax,
            servo_page_avg: avg(|f| f.servo_page),
            servo_chrome_avg: avg(|f| f.servo_chrome),
            compose_avg: avg(|f| f.compose),
            present_avg: avg(|f| f.present),
            latency_p50: l50,
            latency_p95: l95,
            latency_n: self.latencies.len(),
            wheel_events: self.wheel_events,
            scroll_sends: self.scroll_sends,
            path: self.path,
        };
        let pending = (self.pending_input_ms, self.input_sent);
        let last = self.last_present_ms;
        *self = PerfWindow::default();
        // An input still in flight is measured in the next window; the next interval is
        // measured from the last present.
        (self.pending_input_ms, self.input_sent) = pending;
        self.last_present_ms = last;
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(total: f64, page: bool) -> FrameSample {
        FrameSample {
            total,
            servo_page: if page { 4.0 } else { 0.0 },
            servo_chrome: 0.0,
            compose: 2.0,
            present: total - 6.0,
            page_frame: page,
        }
    }

    #[test]
    fn percentiles_of_a_set() {
        assert_eq!(percentiles(&[]), (0.0, 0.0, 0.0));
        let v: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let (a, b, c) = percentiles(&v);
        assert_eq!((a.round(), b.round(), c), (51.0, 95.0, 100.0));
    }

    #[test]
    fn a_second_of_frames_is_summarised() {
        let mut w = PerfWindow::default();
        let mut t = 0.0;
        for i in 0..60 {
            t += 16.7;
            if i == 10 {
                w.on_input(t - 3.0);
                w.on_scroll_sent(ScrollPath::Fast);
            }
            w.on_frame(t, frame(16.0, i == 11 || i == 30));
            if i < 59 {
                assert!(w.take_due(t).is_none());
            }
        }
        let s = w.take_due(t + 20.0).expect("due");
        assert_eq!(s.frames, 60);
        assert!(s.fps > 55.0 && s.fps < 62.0, "{}", s.fps);
        assert_eq!(s.page_frames, 2);
        assert_eq!(s.latency_n, 1);
        assert!(
            (s.latency_p50 - (16.7 + 3.0)).abs() < 1e-6,
            "{}",
            s.latency_p50
        );
        assert_eq!(s.path, ScrollPath::Fast);
        assert!(s.line().contains("path fast"));
        assert!(w.take_due(t + 30.0).is_none(), "starts over, idle");
    }

    #[test]
    fn latency_waits_for_a_page_frame_after_the_send() {
        let mut w = PerfWindow::default();
        w.on_input(0.0);
        w.on_frame(10.0, frame(8.0, true)); // a page frame, but nothing was sent yet
        w.on_scroll_sent(ScrollPath::Dom);
        w.on_frame(20.0, frame(8.0, false));
        w.on_frame(45.0, frame(8.0, true));
        let s = w.take_due(2000.0).unwrap();
        assert_eq!(s.latency_n, 1);
        assert_eq!(s.latency_p50, 45.0);
        w.on_scroll_sent(ScrollPath::Fast);
        assert_eq!(w.path, ScrollPath::Fast);
        w.on_scroll_sent(ScrollPath::Dom);
        assert_eq!(w.path, ScrollPath::Both);
    }
}
