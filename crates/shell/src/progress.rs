//! The progress bar of a running query on the terminal: when it shows (DuckDB's
//! `ProgressBar::Update` / `ShouldPrint`), the time remaining it estimates
//! (`TerminalProgressBarDisplay::Update` with its `UnscentedKalmanFilter`) and the status
//! line it prints (`ShellProgressBarDisplay` of `tools/shell/shell_progress_bar.cpp`).

use std::time::{Duration, Instant};

use roastedduck::Database;

use crate::printer;
use crate::progress_bar::ProgressStatus;
use crate::shell::ConnectionPromptContext;
use crate::state::ShellState;

/// The unscented Kalman filter that estimates the time remaining from the progress
/// (`UnscentedKalmanFilter`): its state is the progress and its velocity.
#[derive(Debug)]
struct UnscentedKalmanFilter {
    lambda: f64,
    /// The weights of the sigma points for the mean and the covariance.
    wm: [f64; SIGMA_POINTS],
    wc: [f64; SIGMA_POINTS],
    /// The state estimate: progress (0 to the scale factor) and progress per second.
    x: [f64; STATE_DIM],
    /// Its covariance.
    p: [[f64; STATE_DIM]; STATE_DIM],
    /// The process noise.
    q: [[f64; STATE_DIM]; STATE_DIM],
    /// The measurement noise.
    r: f64,
    last_time: f64,
    initialized: bool,
    last_progress: f64,
    scale_factor: f64,
}

const STATE_DIM: usize = 2;
const SIGMA_POINTS: usize = 2 * STATE_DIM + 1;
const ALPHA: f64 = 0.044;
const BETA: f64 = 1.0;
const KAPPA: f64 = 0.0;

impl UnscentedKalmanFilter {
    fn new() -> Self {
        let lambda = ALPHA * ALPHA * (STATE_DIM as f64 + KAPPA) - STATE_DIM as f64;
        let mut wm = [0.5 / (STATE_DIM as f64 + lambda); SIGMA_POINTS];
        let mut wc = wm;
        wm[0] = lambda / (STATE_DIM as f64 + lambda);
        wc[0] = lambda / (STATE_DIM as f64 + lambda) + (1.0 - ALPHA * ALPHA + BETA);
        Self {
            lambda,
            wm,
            wc,
            x: [0.0; STATE_DIM],
            // The variances of the progress and the velocity.
            p: [[0.165, 0.0], [0.0, 0.0098]],
            // The process noise of the progress.
            q: [[0.01, 0.0], [0.0, 0.0]],
            r: 0.000_077,
            last_time: 0.0,
            initialized: false,
            last_progress: -1.0,
            scale_factor: 1.0,
        }
    }

    /// `Update`: a progress measurement at `time` seconds.
    #[allow(
        clippy::float_cmp,
        reason = "a new measurement is a changed one, as in DuckDB"
    )]
    fn update(&mut self, progress: f64, time: f64) {
        let progress = progress * self.scale_factor;
        if !self.initialized {
            self.initialize(progress, time);
            return;
        }
        self.predict(time);
        if self.last_progress != progress {
            self.update_internal(progress);
            self.last_progress = progress;
        }
    }

    /// `Initialize`: once there is progress, scaled up if it is small so that updates
    /// are not taken for noise.
    fn initialize(&mut self, initial_progress: f64, current_time: f64) {
        if initial_progress == 0.0 || current_time == 0.0 {
            return;
        }
        self.scale_factor = (0.1 / initial_progress).max(1.0);
        let initial_progress = initial_progress * self.scale_factor;
        self.x[0] = initial_progress;
        self.x[1] = initial_progress / current_time;
        self.last_time = current_time;
        self.last_progress = initial_progress;
        self.initialized = true;
    }

    /// `MatrixSqrt`: the Cholesky decomposition.
    fn matrix_sqrt(mat: &[[f64; STATE_DIM]; STATE_DIM]) -> [[f64; STATE_DIM]; STATE_DIM] {
        let mut l = [[0.0; STATE_DIM]; STATE_DIM];
        for i in 0..STATE_DIM {
            for j in 0..=i {
                let sum: f64 = (0..j).map(|k| l[i][k] * l[j][k]).sum();
                if i == j {
                    l[j][j] = (mat[j][j] - sum).sqrt();
                } else {
                    l[i][j] = (mat[i][j] - sum) / l[j][j];
                }
            }
        }
        l
    }

    /// `GenerateSigmaPoints`.
    fn generate_sigma_points(&self) -> [[f64; STATE_DIM]; SIGMA_POINTS] {
        let scale = STATE_DIM as f64 + self.lambda;
        let scaled = self.p.map(|row| row.map(|value| value * scale));
        let sqrt_p = Self::matrix_sqrt(&scaled);
        let mut sigma_points = [self.x; SIGMA_POINTS];
        for i in 0..STATE_DIM {
            for j in 0..STATE_DIM {
                sigma_points[i + 1][j] = self.x[j] + sqrt_p[j][i];
                sigma_points[i + 1 + STATE_DIM][j] = self.x[j] - sqrt_p[j][i];
            }
        }
        sigma_points
    }

    /// `StateTransition`: the progress advances by the velocity, within [0, 1].
    fn state_transition(state: [f64; STATE_DIM], dt: f64) -> [f64; STATE_DIM] {
        [(state[0] + state[1] * dt).clamp(0.0, 1.0), state[1]]
    }

    /// `Predict`.
    #[allow(
        clippy::needless_range_loop,
        reason = "matrix arithmetic, indexed as DuckDB's"
    )]
    fn predict(&mut self, current_time: f64) {
        let dt = current_time - self.last_time;
        self.last_time = current_time;
        if dt <= 0.0 {
            return;
        }
        let predicted = self
            .generate_sigma_points()
            .map(|point| Self::state_transition(point, dt));
        self.x = [0.0; STATE_DIM];
        for (point, weight) in predicted.iter().zip(self.wm) {
            for j in 0..STATE_DIM {
                self.x[j] += weight * point[j];
            }
        }
        self.p = [[0.0; STATE_DIM]; STATE_DIM];
        for (point, weight) in predicted.iter().zip(self.wc) {
            for j in 0..STATE_DIM {
                for k in 0..STATE_DIM {
                    self.p[j][k] += weight * (point[j] - self.x[j]) * (point[k] - self.x[k]);
                }
            }
        }
        for i in 0..STATE_DIM {
            for j in 0..STATE_DIM {
                self.p[i][j] += self.q[i][j];
            }
        }
    }

    /// `UpdateInternal`: corrects the state with a measured progress.
    #[allow(
        clippy::needless_range_loop,
        reason = "matrix arithmetic, indexed as DuckDB's"
    )]
    fn update_internal(&mut self, measured_progress: f64) {
        let sigma_points = self.generate_sigma_points();
        // The measurement is the progress.
        let z_sigma = sigma_points.map(|point| point[0]);
        let z_pred: f64 = z_sigma.iter().zip(self.wm).map(|(z, weight)| weight * z).sum();
        let mut s: f64 = z_sigma
            .iter()
            .zip(self.wc)
            .map(|(z, weight)| weight * (z - z_pred) * (z - z_pred))
            .sum();
        s += self.r;
        let mut t = [0.0; STATE_DIM];
        for (i, point) in sigma_points.iter().enumerate() {
            for j in 0..STATE_DIM {
                t[j] += self.wc[i] * (point[j] - self.x[j]) * (z_sigma[i] - z_pred);
            }
        }
        let s_inv = 1.0 / s;
        let k = t.map(|value| value * s_inv);
        let innovation = measured_progress - z_pred;
        for i in 0..STATE_DIM {
            self.x[i] += k[i] * innovation;
        }
        for i in 0..STATE_DIM {
            for j in 0..STATE_DIM {
                self.p[i][j] -= k[i] * s * k[j];
            }
        }
        self.x[0] = self.x[0].clamp(0.0, self.scale_factor);
    }

    /// `GetEstimatedRemainingSeconds`.
    fn estimated_remaining_seconds(&self) -> f64 {
        if !self.initialized {
            return 2_147_483_647.0;
        }
        if self.x[1] <= 0.0 {
            // Without progress the query never finishes.
            return f64::MAX;
        }
        let remaining_progress = self.scale_factor - self.x[0];
        (remaining_progress / self.x[1]).max(0.0)
    }
}

/// The progress bar of one query (DuckDB's `ProgressBar` with a
/// `ShellProgressBarDisplay`).
#[derive(Debug)]
pub struct ProgressDisplay {
    /// The time the query started (`ProgressBar::profiler` and the display's start time).
    start: Instant,
    /// The progress shows once the query ran this long (`show_progress_after`).
    show_progress_after: Duration,
    /// The highest percentage seen, -1 while unknown.
    percentage: f64,
    ukf: UnscentedKalmanFilter,
    /// The percentage and seconds remaining printed last (`displayed_progress_info`).
    displayed: Option<(i32, u64)>,
    /// The terminal width the status line was printed for.
    previous_terminal_width: Option<usize>,
    finished: bool,
}

impl ProgressDisplay {
    pub fn new(show_progress_after: Duration) -> Self {
        Self {
            start: Instant::now(),
            show_progress_after,
            percentage: -1.0,
            ukf: UnscentedKalmanFilter::new(),
            displayed: None,
            previous_terminal_width: None,
            finished: false,
        }
    }

    fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// `ProgressBar::ShouldPrint`.
    fn should_print(&self, finished: bool) -> bool {
        if self.elapsed() <= self.show_progress_after.as_secs_f64() {
            return false;
        }
        finished || self.percentage > -1.0
    }

    /// `ProgressBar::Update(false)` with the query's current progress.
    pub fn update(&mut self, state: &ShellState, db: Option<&Database>, percentage: f64) {
        if percentage > self.percentage {
            self.percentage = percentage;
        }
        if !self.should_print(false) {
            return;
        }
        // `TerminalProgressBarDisplay::Update`.
        let current_time = self.elapsed();
        let filter_percentage = self.percentage / 100.0;
        self.ukf.update(filter_percentage, current_time);
        // Near the end, progress updates are sparse: do not let the estimate oscillate.
        let estimated_seconds_remaining = if filter_percentage > 0.99 {
            0.5
        } else {
            self.ukf.estimated_remaining_seconds().min(2_147_483_647.0)
        };
        let percentage = self.percentage.clamp(0.0, 100.0) as i32;
        let displayed = (percentage, estimated_seconds_remaining as u64);
        if self.displayed != Some(displayed) {
            self.print_progress(state, db, percentage, estimated_seconds_remaining, false);
            printer::flush_stdout();
            self.displayed = Some(displayed);
        }
    }

    /// `ProgressBar::Update(true)`: once the query finished, the status line is cleared
    /// (`ShellProgressBarDisplay::Finish`) if it was shown.
    pub fn finish(&mut self, state: &ShellState, db: Option<&Database>) {
        if self.finished || !self.should_print(true) {
            return;
        }
        let elapsed = self.elapsed();
        self.print_progress(state, db, 100, elapsed, true);
        printer::flush_stdout();
        self.finished = true;
    }

    /// `ShellProgressBarDisplay::PrintProgressInternal`: clears the lines the status line
    /// took and prints it again, its settings read through a connection of its own.
    fn print_progress(
        &mut self,
        state: &ShellState,
        db: Option<&Database>,
        percentage: i32,
        estimated_remaining_seconds: f64,
        is_finished: bool,
    ) {
        let terminal_width = printer::terminal_width();
        let mut result = roastedduck_common::ByteString::new();
        if let Some(previous) = self.previous_terminal_width
            && terminal_width < previous
        {
            // The terminal got narrower: the status line wrapped onto more lines.
            let line_count = previous.div_ceil(terminal_width);
            for _ in 1..line_count {
                result.push_bytes(b"\r\x1b[0K\x1b[1A");
            }
        }
        result.push_bytes(b"\r\x1b[0K");
        self.previous_terminal_width = Some(terminal_width);
        if !is_finished {
            let status = ProgressStatus {
                percentage,
                estimated_remaining_seconds,
            };
            let line = db
                .ok_or_else(|| roastedduck::Error::internal("no database open"))
                .and_then(Database::connect)
                .and_then(|mut connection| {
                    state.progress_bar.generate_progress_bar(
                        status,
                        &mut ConnectionPromptContext(&mut connection),
                        terminal_width,
                    )
                });
            match line {
                Ok(line) => result.push_bytes(&line),
                Err(error) => result.push_bytes(error.to_bytes()),
            }
        }
        printer::write_stdout(result.as_bytes());
    }
}
