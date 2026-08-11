use nalgebra::Vector3;
use serde::Serialize;
use serde::Serializer;
use serde::ser::{SerializeMap, SerializeSeq};
use std::collections::HashMap;
use std::fmt::Display;

#[derive(Debug, Clone, Serialize)]
pub struct PassEventData {
    pub timestamp: u64,
    pub from_player_id: u32,
    pub to_player_id: u32,
}

impl PassEventData {
    pub fn new(timestamp: u64, from_player_id: u32, to_player_id: u32) -> Self {
        PassEventData {
            timestamp,
            from_player_id,
            to_player_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchEventData {
    pub timestamp: u64,
    pub category: String,
    pub description: String,
}

/// Position data item — stored in memory as full-precision values,
/// serialized as compact JSON arrays: [timestamp, x, y, z] or [timestamp, x, y]
#[derive(Debug, Clone)]
pub struct ResultPositionDataItem {
    pub timestamp: u64,
    pub position: Vector3<f32>,
}

impl ResultPositionDataItem {
    pub fn new(timestamp: u64, position: Vector3<f32>) -> Self {
        ResultPositionDataItem {
            timestamp,
            position,
        }
    }
}

/// Added in this fork: the output of [`ResultMatchPositionData::shape_report`].
///
/// Real-football reference points, for reading the numbers against
/// something other than taste:
///
/// * `mean_within_10m` — a 10 m circle round the ball holds the duel plus
///   the nearest support and cover. Two to four is normal; the whole
///   outfield eleven is a school playground.
/// * `scrum_share` — share of the match with eight or more players inside
///   that circle. Should be a rounding error outside set pieces.
/// * `widest_player_m` — how far the widest player of a side sits from the
///   centre line, averaged. A 68 m pitch gives 34 m to the touchline, and a
///   side holding any width at all keeps somebody past 20 m.
#[derive(Debug, Clone, Default)]
pub struct ShapeReport {
    pub samples: u32,
    pub scrum_samples: u32,
    pub scrum_share: f32,
    pub mean_within_10m: f64,
    /// Indexed [home, away].
    pub lateral_spread_m: [f32; 2],
    pub widest_player_m: [f32; 2],
    pub side_samples: [u32; 2],
    /// Which player states the pile-ups are made of, counted over scrum
    /// frames only. Requires `MatchRuntime::set_events_mode(true)`;
    /// empty otherwise.
    pub scrum_states: HashMap<String, u32>,
}

/// Compact serialization: [timestamp, x, y] or [timestamp, x, y, z]
/// Omits z when it's effectively zero (players on ground), saving ~5 bytes/entry in JSON.
impl Serialize for ResultPositionDataItem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Round to 1 decimal for compact JSON output
        let x = (self.position.x * 10.0).round() / 10.0;
        let y = (self.position.y * 10.0).round() / 10.0;
        let z = (self.position.z * 10.0).round() / 10.0;

        if z.abs() < 0.05 {
            // 2D entry: [timestamp, x, y]
            let mut seq = serializer.serialize_seq(Some(3))?;
            seq.serialize_element(&self.timestamp)?;
            seq.serialize_element(&x)?;
            seq.serialize_element(&y)?;
            seq.end()
        } else {
            // 3D entry: [timestamp, x, y, z]
            let mut seq = serializer.serialize_seq(Some(4))?;
            seq.serialize_element(&self.timestamp)?;
            seq.serialize_element(&x)?;
            seq.serialize_element(&y)?;
            seq.serialize_element(&z)?;
            seq.end()
        }
    }
}

/// Tolerance-based squared distance threshold for deduplication.
/// Positions within 0.3 game units are considered unchanged.
/// 0.3 units on an 840-unit field = 0.036% — completely imperceptible.
const DEDUP_TOLERANCE_SQ: f32 = 0.09; // 0.3 * 0.3

/// Maximum interval between recorded samples for any on-pitch player.
/// A stationary GK or sweeper could otherwise go minutes without a new
/// sample (dedup threshold never tripped). Replay viewers use the gap
/// between samples as a "player left the pitch" signal.
///
/// MUST stay below the viewer's hide-on-gap threshold (1000 ms at the
/// time of writing). At the old 2 s value, any stationary player got a
/// sample at t=0, then none until t=2000 — but the viewer hid them the
/// moment `time > lastTs + 1000`, so the player blinked invisible for
/// half of every 2-second window. Noticeable as "players disappearing
/// a few minutes into the match", especially once the NaN-velocity
/// guard started silencing state bugs by zeroing velocity (which
/// left those players perfectly stationary and fully exposed to the
/// blink). 750 ms keeps them continuously visible with ~1 extra KB
/// of storage per idle player per minute — negligible.
const HEARTBEAT_INTERVAL_MS: u64 = 750;

/// Quantize a coordinate to 0.1 precision.
/// This improves dedup hit rate and produces shorter JSON floats.
#[inline]
fn quantize(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

/// Player state change: recorded only when the state actually changes.
/// Serializes as [timestamp, "StateName"] for compact JSON.
#[derive(Debug, Clone)]
pub struct PlayerStateEntry {
    pub timestamp: u64,
    pub state: String,
}

impl Serialize for PlayerStateEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&self.timestamp)?;
        seq.serialize_element(&self.state)?;
        seq.end()
    }
}

#[derive(Debug, Clone)]
pub struct ResultMatchPositionData {
    ball: Vec<ResultPositionDataItem>,
    players: HashMap<u32, Vec<ResultPositionDataItem>>,
    passes: Vec<PassEventData>,
    events: Vec<MatchEventData>,
    /// Per-player state changes — only populated when track_events is true.
    player_states: HashMap<u32, Vec<PlayerStateEntry>>,
    /// Fast dedup: last recorded state compact ID per player (avoids String allocation)
    last_state_ids: HashMap<u32, u16>,
    track_events: bool,
    track_positions: bool,
}

/// Compact top-level serialization.
/// Uses same key names as before for frontend compatibility.
impl Serialize for ResultMatchPositionData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let has_states = self.track_events && !self.player_states.is_empty();
        let field_count =
            2 + if self.track_events { 2 } else { 0 } + if has_states { 1 } else { 0 };
        let mut map = serializer.serialize_map(Some(field_count))?;

        map.serialize_entry("ball", &self.ball)?;
        map.serialize_entry("players", &self.players)?;

        if self.track_events {
            map.serialize_entry("passes", &self.passes)?;
            map.serialize_entry("events", &self.events)?;
        }

        if has_states {
            map.serialize_entry("states", &self.player_states)?;
        }

        map.end()
    }
}

impl ResultMatchPositionData {
    pub fn new() -> Self {
        ResultMatchPositionData {
            ball: Vec::new(),
            players: HashMap::with_capacity(44),
            passes: Vec::new(),
            events: Vec::new(),
            player_states: HashMap::new(),
            last_state_ids: HashMap::new(),
            track_events: false,
            track_positions: true,
        }
    }

    pub fn new_with_tracking() -> Self {
        ResultMatchPositionData {
            ball: Vec::new(),
            players: HashMap::with_capacity(44),
            passes: Vec::new(),
            events: Vec::new(),
            player_states: HashMap::with_capacity(44),
            last_state_ids: HashMap::with_capacity(44),
            track_events: true,
            track_positions: true,
        }
    }

    pub fn empty() -> Self {
        ResultMatchPositionData {
            ball: Vec::new(),
            players: HashMap::new(),
            passes: Vec::new(),
            events: Vec::new(),
            player_states: HashMap::new(),
            last_state_ids: HashMap::new(),
            track_events: false,
            track_positions: false,
        }
    }

    /// Added in this fork: does this recording actually contain position
    /// samples? The engine hands back `ResultMatchPositionData::new()`-shaped
    /// empties for non-recorded matches; the web storage layer uses this to
    /// persist chunks only for matches that really recorded (per-match
    /// `Match::record` stamping) instead of gating on the global flag.
    pub fn has_data(&self) -> bool {
        !self.ball.is_empty() || !self.players.is_empty()
    }

    /// Added in this fork: how much like a football team did these eleven
    /// look?
    ///
    /// Two numbers, both of them complaints people actually make about the
    /// 2D pitch: "they all run at the ball" and "nobody holds the width".
    /// Written against the recording rather than instrumented into the tick
    /// loop because the recording is already there and costs nothing extra
    /// — and because a metric that only exists inside a debug build is a
    /// metric nobody reruns.
    ///
    /// Distances come back in METRES. The engine's field is 840×545 units
    /// for a 105×68 m pitch, so a unit is 12.5 cm and a raw-unit threshold
    /// reads eight times tighter than whoever wrote it intended.
    pub fn shape_report(
        &self,
        home_outfield: &[u32],
        away_outfield: &[u32],
        field_width: f32,
        field_height: f32,
        sample_every_ms: u64,
    ) -> ShapeReport {
        const UNITS_PER_METRE: f32 = 8.0;

        let mut report = ShapeReport::default();
        if self.ball.is_empty() {
            return report;
        }

        let metres = |units: f32| units / UNITS_PER_METRE;
        let radius_units = 10.0 * UNITS_PER_METRE;
        let centre_y = field_height / 2.0;

        // Walk each series with a cursor instead of searching it per
        // sample: the series are already time-ordered and the sample times
        // only ever move forward, so the whole report is one linear pass.
        let mut ball_cursor = 0usize;
        let mut cursors: HashMap<u32, usize> = HashMap::new();

        let at = |series: &Vec<ResultPositionDataItem>, cursor: &mut usize, t: u64| {
            while *cursor + 1 < series.len() && series[*cursor + 1].timestamp <= t {
                *cursor += 1;
            }
            series.get(*cursor).map(|item| item.position)
        };

        let end = self.max_timestamp();
        let mut t = 0u64;
        while t <= end {
            let Some(ball) = at(&self.ball, &mut ball_cursor, t) else {
                break;
            };

            let mut near_ball = 0u32;
            let mut near_ids: Vec<u32> = Vec::new();

            for (side, roster) in [(0usize, home_outfield), (1usize, away_outfield)] {
                let mut widest: f32 = 0.0;
                let mut ys: Vec<f32> = Vec::with_capacity(roster.len());

                for &id in roster {
                    let Some(series) = self.players.get(&id) else {
                        continue;
                    };
                    let mut cursor = cursors.get(&id).copied().unwrap_or(0);
                    let Some(pos) = at(series, &mut cursor, t) else {
                        continue;
                    };
                    cursors.insert(id, cursor);

                    if (pos - ball).norm() <= radius_units {
                        near_ball += 1;
                        near_ids.push(id);
                    }
                    widest = widest.max((pos.y - centre_y).abs());
                    ys.push(pos.y);
                }

                if ys.is_empty() {
                    continue;
                }

                let mean = ys.iter().sum::<f32>() / ys.len() as f32;
                let variance =
                    ys.iter().map(|y| (y - mean).powi(2)).sum::<f32>() / ys.len() as f32;

                report.lateral_spread_m[side] += metres(variance.sqrt());
                report.widest_player_m[side] += metres(widest);
                report.side_samples[side] += 1;
            }

            report.mean_within_10m += near_ball as f64;
            if near_ball >= 8 {
                report.scrum_samples += 1;

                // Name the states holding the pile-up together. Without
                // this the report can only say "there is a scrum"; with it
                // the fix has an address. Only sampled on scrum frames —
                // the state series is a per-change log, and walking it for
                // every frame would dominate the report's cost.
                for id in &near_ids {
                    if let Some(states) = self.player_states.get(id) {
                        let name = states
                            .iter()
                            .take_while(|entry| entry.timestamp <= t)
                            .last()
                            .map(|entry| entry.state.clone());
                        if let Some(name) = name {
                            *report.scrum_states.entry(name).or_insert(0) += 1;
                        }
                    }
                }
            }
            report.samples += 1;

            t += sample_every_ms;
        }

        if report.samples > 0 {
            report.mean_within_10m /= report.samples as f64;
            report.scrum_share = report.scrum_samples as f32 / report.samples as f32;
        }
        for side in 0..2 {
            if report.side_samples[side] > 0 {
                let n = report.side_samples[side] as f32;
                report.lateral_spread_m[side] /= n;
                report.widest_player_m[side] /= n;
            }
        }

        let _ = field_width;
        report
    }

    /// Build a coarse heatmap (bucket-count grid) for a single player from
    /// their recorded position samples. The output is a `rows x cols` grid,
    /// row-major, where each cell holds the number of position samples that
    /// fell into it. Caller supplies the field dimensions used when the
    /// match was simulated.
    ///
    /// Typical usage: 10×14 or 12×16 buckets is enough to render a readable
    /// FM-style player heatmap in the UI.
    pub fn player_heatmap(
        &self,
        player_id: u32,
        field_width: f32,
        field_height: f32,
        cols: usize,
        rows: usize,
    ) -> Vec<u32> {
        let mut grid = vec![0u32; cols * rows];
        let positions = match self.players.get(&player_id) {
            Some(p) if !p.is_empty() => p,
            _ => return grid,
        };

        let cw = field_width / cols as f32;
        let ch = field_height / rows as f32;
        if cw <= 0.0 || ch <= 0.0 {
            return grid;
        }
        for item in positions {
            let cx = (item.position.x / cw).floor() as isize;
            let cy = (item.position.y / ch).floor() as isize;
            if cx < 0 || cy < 0 {
                continue;
            }
            let cx = (cx as usize).min(cols - 1);
            let cy = (cy as usize).min(rows - 1);
            grid[cy * cols + cx] = grid[cy * cols + cx].saturating_add(1);
        }
        grid
    }

    /// Average position across all samples for a player, or None if no
    /// samples. Useful as the anchor point for an FM-style formation map.
    pub fn player_average_position(&self, player_id: u32) -> Option<(f32, f32)> {
        let positions = self.players.get(&player_id)?;
        if positions.is_empty() {
            return None;
        }
        let (sx, sy) = positions.iter().fold((0.0f32, 0.0f32), |(ax, ay), p| {
            (ax + p.position.x, ay + p.position.y)
        });
        let n = positions.len() as f32;
        Some((sx / n, sy / n))
    }

    /// Split the data into chunks based on time ranges
    /// Returns a vector of chunks, each containing data for a specific time window
    pub fn split_into_chunks(&self, chunk_duration_ms: u64) -> Vec<ResultMatchPositionData> {
        if self.ball.is_empty() {
            return vec![self.clone()];
        }

        let max_timestamp = self.max_timestamp();
        let num_chunks = ((max_timestamp as f64 / chunk_duration_ms as f64).ceil() as usize).max(1);
        let mut chunks = Vec::with_capacity(num_chunks);

        for chunk_idx in 0..num_chunks {
            let start_time = chunk_idx as u64 * chunk_duration_ms;
            let end_time = start_time + chunk_duration_ms;

            let mut chunk = ResultMatchPositionData {
                ball: Vec::new(),
                players: HashMap::new(),
                passes: Vec::new(),
                events: Vec::new(),
                player_states: HashMap::new(),
                last_state_ids: HashMap::new(),
                track_events: self.track_events,
                track_positions: self.track_positions,
            };

            // Filter ball positions for this time window
            chunk.ball = self
                .ball
                .iter()
                .filter(|item| item.timestamp >= start_time && item.timestamp < end_time)
                .cloned()
                .collect();

            // Filter player positions for this time window
            for (player_id, positions) in &self.players {
                let filtered_positions: Vec<ResultPositionDataItem> = positions
                    .iter()
                    .filter(|item| item.timestamp >= start_time && item.timestamp < end_time)
                    .cloned()
                    .collect();

                if !filtered_positions.is_empty() {
                    chunk.players.insert(*player_id, filtered_positions);
                }
            }

            // Filter passes and events for this time window
            if self.track_events {
                chunk.passes = self
                    .passes
                    .iter()
                    .filter(|pass| pass.timestamp >= start_time && pass.timestamp < end_time)
                    .cloned()
                    .collect();

                chunk.events = self
                    .events
                    .iter()
                    .filter(|evt| evt.timestamp >= start_time && evt.timestamp < end_time)
                    .cloned()
                    .collect();

                // Filter player states: include last state before chunk start + states in window
                for (player_id, states) in &self.player_states {
                    let mut chunk_states = Vec::new();

                    // Find the most recent state before this chunk starts (carry-over)
                    if let Some(last_before) =
                        states.iter().rev().find(|s| s.timestamp < start_time)
                    {
                        chunk_states.push(PlayerStateEntry {
                            timestamp: start_time,
                            state: last_before.state.clone(),
                        });
                    }

                    // Add states within this chunk's window
                    for s in states
                        .iter()
                        .filter(|s| s.timestamp >= start_time && s.timestamp < end_time)
                    {
                        chunk_states.push(s.clone());
                    }

                    if !chunk_states.is_empty() {
                        chunk.player_states.insert(*player_id, chunk_states);
                    }
                }
            }

            chunks.push(chunk);
        }

        chunks
    }

    /// Check if event tracking is enabled
    #[inline]
    pub fn is_tracking_events(&self) -> bool {
        self.track_events
    }

    /// Check if position tracking is enabled
    #[inline]
    pub fn is_tracking_positions(&self) -> bool {
        self.track_positions
    }

    /// Add player position with quantization and tolerance-based dedup.
    /// Skips recording if the player hasn't moved more than 0.3 units since last entry.
    pub fn add_player_positions(&mut self, player_id: u32, timestamp: u64, position: Vector3<f32>) {
        if !self.track_positions {
            return;
        }

        // Quantize to 0.1 precision — reduces float noise and produces shorter JSON
        let position = Vector3::new(
            quantize(position.x),
            quantize(position.y),
            quantize(position.z),
        );

        if let Some(player_data) = self.players.get_mut(&player_id) {
            let last = player_data.last().unwrap();
            let dx = position.x - last.position.x;
            let dy = position.y - last.position.y;
            let dz = position.z - last.position.z;

            // Tolerance dedup + heartbeat: skip tiny movements unless we're
            // overdue for a sample. Without the heartbeat, a GK planted in
            // the six-yard box gets no updates until a save, and replay
            // viewers can't distinguish "on-pitch, idle" from "subbed off".
            let distance_sq = dx * dx + dy * dy + dz * dz;
            let since_last = timestamp.saturating_sub(last.timestamp);
            if distance_sq < DEDUP_TOLERANCE_SQ && since_last < HEARTBEAT_INTERVAL_MS {
                return;
            }

            player_data.push(ResultPositionDataItem::new(timestamp, position));
        } else {
            self.players.insert(
                player_id,
                vec![ResultPositionDataItem::new(timestamp, position)],
            );
        }
    }

    /// Add ball position with quantization and tolerance-based dedup.
    /// Previous implementation had a bug: PartialEq compared timestamps too,
    /// so ball positions were NEVER deduplicated (timestamps always differ).
    pub fn add_ball_positions(&mut self, timestamp: u64, position: Vector3<f32>) {
        if !self.track_positions {
            return;
        }

        let position = Vector3::new(
            quantize(position.x),
            quantize(position.y),
            quantize(position.z),
        );

        if let Some(last) = self.ball.last() {
            let dx = position.x - last.position.x;
            let dy = position.y - last.position.y;
            let dz = position.z - last.position.z;

            // Tolerance dedup + heartbeat. Without the heartbeat, an
            // owned-and-stationary ball (stuck with a player who isn't
            // passing) gets no ball samples for the rest of the match
            // — `max_timestamp` freezes at the last movement and the
            // chunk split discards everything after that point, even
            // though the sim is still running. Player positions use
            // the same heartbeat for the same reason.
            let since_last = timestamp.saturating_sub(last.timestamp);
            if dx * dx + dy * dy + dz * dz < DEDUP_TOLERANCE_SQ
                && since_last < HEARTBEAT_INTERVAL_MS
            {
                return;
            }
        }

        self.ball
            .push(ResultPositionDataItem::new(timestamp, position));
    }

    /// Get the maximum timestamp in the recorded data
    pub fn max_timestamp(&self) -> u64 {
        self.ball.last().map(|item| item.timestamp).unwrap_or(0)
    }

    /// Get ball position at a specific timestamp (uses nearest neighbor)
    pub fn get_ball_position_at(&self, timestamp: u64) -> Option<Vector3<f32>> {
        if self.ball.is_empty() {
            return None;
        }

        // Binary search for the closest timestamp
        let idx = self
            .ball
            .binary_search_by_key(&timestamp, |item| item.timestamp)
            .unwrap_or_else(|idx| {
                if idx == 0 {
                    0
                } else if idx >= self.ball.len() {
                    self.ball.len() - 1
                } else {
                    // Choose nearest between idx-1 and idx
                    let before = &self.ball[idx - 1];
                    let after = &self.ball[idx];
                    if timestamp - before.timestamp < after.timestamp - timestamp {
                        idx - 1
                    } else {
                        idx
                    }
                }
            });

        Some(self.ball[idx].position)
    }

    /// Get player position at a specific timestamp (uses nearest neighbor)
    pub fn get_player_position_at(&self, player_id: u32, timestamp: u64) -> Option<Vector3<f32>> {
        let player_data = self.players.get(&player_id)?;

        if player_data.is_empty() {
            return None;
        }

        // Binary search for the closest timestamp
        let idx = player_data
            .binary_search_by_key(&timestamp, |item| item.timestamp)
            .unwrap_or_else(|idx| {
                if idx == 0 {
                    0
                } else if idx >= player_data.len() {
                    player_data.len() - 1
                } else {
                    // Choose nearest between idx-1 and idx
                    let before = &player_data[idx - 1];
                    let after = &player_data[idx];
                    if timestamp - before.timestamp < after.timestamp - timestamp {
                        idx - 1
                    } else {
                        idx
                    }
                }
            });

        Some(player_data[idx].position)
    }

    /// Get all player IDs that have recorded positions
    pub fn get_player_ids(&self) -> Vec<u32> {
        self.players.keys().copied().collect()
    }

    /// Add a match event (only if event tracking is enabled)
    pub fn add_match_event(&mut self, timestamp: u64, category: &str, description: String) {
        if self.track_events {
            self.events.push(MatchEventData {
                timestamp,
                category: category.to_string(),
                description,
            });
        }
    }

    /// Add a pass event (only if event tracking is enabled)
    pub fn add_pass_event(&mut self, timestamp: u64, from_player_id: u32, to_player_id: u32) {
        if self.track_events {
            self.passes
                .push(PassEventData::new(timestamp, from_player_id, to_player_id));
        }
    }

    /// Record a player state change. Uses a cheap integer ID for fast dedup,
    /// only allocating the display String when the state actually changed.
    pub fn add_player_state(
        &mut self,
        player_id: u32,
        timestamp: u64,
        state_id: u16,
        state: &impl Display,
    ) {
        if !self.track_events {
            return;
        }

        // Fast dedup using integer comparison — avoids to_string() ~90% of the time
        if let Some(&last_id) = self.last_state_ids.get(&player_id) {
            if last_id == state_id {
                return;
            }
        }

        self.last_state_ids.insert(player_id, state_id);
        let state_name = state.to_string();

        if let Some(entries) = self.player_states.get_mut(&player_id) {
            entries.push(PlayerStateEntry {
                timestamp,
                state: state_name,
            });
        } else {
            self.player_states.insert(
                player_id,
                vec![PlayerStateEntry {
                    timestamp,
                    state: state_name,
                }],
            );
        }
    }

    /// Get the most recent pass event at or before a timestamp
    pub fn get_recent_pass_at(&self, timestamp: u64) -> Option<&PassEventData> {
        // Find most recent pass that occurred at or before this timestamp
        self.passes
            .iter()
            .rev() // Search from most recent
            .find(|pass| pass.timestamp <= timestamp)
    }

    /// Get all passes that occurred within a time window around the timestamp
    pub fn get_passes_in_window(&self, timestamp: u64, window_ms: u64) -> Vec<&PassEventData> {
        let start = timestamp.saturating_sub(window_ms);
        let end = timestamp + window_ms;

        self.passes
            .iter()
            .filter(|pass| pass.timestamp >= start && pass.timestamp <= end)
            .collect()
    }

    /// Added in this fork: one slice of the recording, for the live 2D view.
    ///
    /// The replay viewer downloads a finished match in five-minute chunks; a
    /// match being *watched* cannot work that way, because the frames the
    /// panel wants do not exist yet when it asks. So the live screen pulls the
    /// same recording the same way it pulls the clock — window by window,
    /// bounded by the cursor it already carries for `advance`.
    ///
    /// `step_ms` is a floor on the gap between kept samples, not a resample
    /// grid: the engine records every 30 ms, which is ~33 positions per player
    /// per second of football. At 12× that is 400 points per player per real
    /// second, and nothing on screen moves smoother for them. Ten frames a
    /// second (`step_ms = 100`) is already past what the eye resolves on a
    /// 90-metre pitch drawn 900 pixels wide.
    ///
    /// The last sample of a series is always kept, whatever the step: it is
    /// the position the viewer holds until the next window arrives, so
    /// dropping it would park every player slightly behind the truth.
    pub fn window(&self, since_ms: u64, until_ms: u64, step_ms: u64) -> PositionWindow {
        let step = step_ms.max(1);

        PositionWindow {
            ball: thin(&self.ball, since_ms, until_ms, step),
            players: self
                .players
                .iter()
                .filter_map(|(id, samples)| {
                    let kept = thin(samples, since_ms, until_ms, step);
                    (!kept.is_empty()).then(|| (*id, kept))
                })
                .collect(),
            passes: self
                .passes
                .iter()
                .filter(|p| p.timestamp >= since_ms && p.timestamp < until_ms)
                .cloned()
                .collect(),
            events: self
                .events
                .iter()
                .filter(|e| e.timestamp >= since_ms && e.timestamp < until_ms)
                .cloned()
                .collect(),
            states: self
                .player_states
                .iter()
                .filter_map(|(id, entries)| {
                    let kept: Vec<PlayerStateEntry> = entries
                        .iter()
                        .filter(|e| e.timestamp >= since_ms && e.timestamp < until_ms)
                        .cloned()
                        .collect();

                    (!kept.is_empty()).then_some((*id, kept))
                })
                .collect(),
        }
    }
}

/// Keep samples inside `[since, until)`, no denser than one per `step`.
fn thin(
    samples: &[ResultPositionDataItem],
    since: u64,
    until: u64,
    step: u64,
) -> Vec<ResultPositionDataItem> {
    let in_window: Vec<&ResultPositionDataItem> = samples
        .iter()
        .filter(|s| s.timestamp >= since && s.timestamp < until)
        .collect();

    let Some(last) = in_window.last() else {
        return Vec::new();
    };

    let mut kept: Vec<ResultPositionDataItem> = Vec::with_capacity(in_window.len());
    let mut previous: Option<u64> = None;

    for sample in &in_window {
        let far_enough = previous.is_none_or(|t| sample.timestamp.saturating_sub(t) >= step);

        if far_enough {
            kept.push((*sample).clone());
            previous = Some(sample.timestamp);
        }
    }

    // The tail is the position the viewer sits on until the next window.
    if kept.last().map(|s| s.timestamp) != Some(last.timestamp) {
        kept.push((*last).clone());
    }

    kept
}

/// A slice of a recording: where everything was, and what happened, between
/// two points on the match clock.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PositionWindow {
    pub ball: Vec<ResultPositionDataItem>,
    pub players: HashMap<u32, Vec<ResultPositionDataItem>>,
    pub passes: Vec<PassEventData>,
    pub events: Vec<MatchEventData>,
    pub states: HashMap<u32, Vec<PlayerStateEntry>>,
}

pub trait VectorExtensions {
    fn length(&self) -> f32;
    fn distance_to(&self, other: &Vector3<f32>) -> f32;
}

impl VectorExtensions for Vector3<f32> {
    #[inline]
    fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    #[inline]
    fn distance_to(&self, other: &Vector3<f32>) -> f32 {
        let diff = self - other;
        diff.dot(&diff).sqrt()
    }
}
