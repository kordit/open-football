use crate::r#match::engine::FootballEngine;
use crate::r#match::{
    Match, MatchDispatcherRegistry, MatchInterceptor, MatchInterceptorRegistry, MatchResult,
    MatchResultRaw, MatchSquad,
};
use rayon::ThreadPool;
use rayon::ThreadPoolBuilder;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

pub struct MatchPlayEnginePool {
    pool: ThreadPool,
    num_threads: usize,
}

impl MatchPlayEnginePool {
    pub fn new(num_threads: usize) -> Self {
        let pool = ThreadPoolBuilder::new()
            .num_threads(num_threads)
            .thread_name(|idx| format!("match-worker-{}", idx))
            .build()
            .expect("failed to create match engine thread pool");

        MatchPlayEnginePool { pool, num_threads }
    }

    /// Worker-thread count this pool was built with. Reported by the
    /// distributed worker handshake so the coordinator can weight the
    /// per-worker share by CPU.
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    /// Run a batch of league matches strictly on the local rayon pool,
    /// bypassing any installed `MatchDispatcher`. Used by the
    /// distributed dispatcher itself for its "local share" of a batch
    /// so the coordinator's own CPU isn't idle while remote workers
    /// process the rest. Functionally identical to the local branch of
    /// `play()`; existing call sites should keep calling `play()`.
    pub fn play_local(&self, matches: Vec<Match>) -> Vec<MatchResult> {
        self.pool
            .install(|| matches.into_par_iter().map(|m| m.play()).collect())
    }

    /// Squad-only counterpart to [`play_local`]. Same semantics —
    /// bypasses the dispatcher; runs on the local rayon pool only.
    pub fn play_squads_local(
        &self,
        matches: Vec<(usize, MatchSquad, MatchSquad, bool)>,
    ) -> Vec<(usize, MatchResultRaw)> {
        self.pool.install(|| {
            matches
                .into_par_iter()
                .map(|(idx, home, away, is_knockout)| {
                    let result =
                        FootballEngine::<840, 545>::play(home, away, false, false, is_knockout);
                    (idx, result)
                })
                .collect()
        })
    }

    /// Play league/cup matches through the pool (produces MatchResult with league metadata).
    ///
    /// When a [`MatchDispatcher`](crate::r#match::MatchDispatcher) is
    /// installed via `MatchDispatcherRegistry::set`, the pool first
    /// offers the work to the dispatcher. On `Ok` the dispatcher fully
    /// claims the batch (no local execution); on `Err` it hands the
    /// input back and the pool runs the local rayon path.
    pub fn play(&self, matches: Vec<Match>) -> Vec<MatchResult> {
        // Modified from upstream: one fixture in the batch may belong to a
        // human. Checked before the dispatcher, because the dispatcher claims
        // batches whole and would ship the manager's own match to a worker.
        if let Some(interceptor) = MatchInterceptorRegistry::try_get() {
            if let Some(index) = matches.iter().position(|m| interceptor.claims(m.id())) {
                return self.play_intercepted(matches, index, interceptor.as_ref());
            }
        }

        self.play_dispatched(matches)
    }

    /// The rest of the day, while somebody plays theirs.
    ///
    /// The two run **at the same time**, and that is the entire point: a
    /// manager watching ninety minutes of their own match is ninety minutes of
    /// wall clock the other four hundred fixtures were going to cost anyway.
    /// Running them first and the human's second makes the manager wait for
    /// both; running them together makes a matchday cost the longer of the
    /// two. It also means the tables are live while the match is on.
    ///
    /// `std::thread::scope` because neither side is `'static` — the batch
    /// borrows this pool, and the interceptor is borrowed from the registry.
    fn play_intercepted(
        &self,
        mut matches: Vec<Match>,
        index: usize,
        interceptor: &dyn MatchInterceptor,
    ) -> Vec<MatchResult> {
        let claimed = matches.remove(index);

        let (mut results, mine) = std::thread::scope(|scope| {
            let rest = scope.spawn(|| self.play_dispatched(matches));
            // The human's match blocks this thread until the final whistle.
            // The interceptor owns the watchdog that stops it being forever.
            let mine = interceptor.play(claimed);
            (rest.join().expect("matchday batch thread panicked"), mine)
        });

        // Back on its own index: `WorldMatchdayResult::process` slices these
        // by `continent_ranges`, so a result in the wrong slot is applied to
        // the wrong league.
        results.insert(index, mine);
        results
    }

    fn play_dispatched(&self, matches: Vec<Match>) -> Vec<MatchResult> {
        let matches = match MatchDispatcherRegistry::try_get() {
            Some(dispatcher) => match dispatcher.dispatch_league(matches) {
                Ok(results) => return results,
                Err(returned) => returned,
            },
            None => matches,
        };
        self.pool
            .install(|| matches.into_par_iter().map(|m| m.play()).collect())
    }

    /// Play raw squad-vs-squad matches through the pool (for national team / international matches).
    /// Each input is (index, home_squad, away_squad). Returns (index, MatchResultRaw).
    /// Convenience wrapper that defaults `is_knockout = false`.
    pub fn play_squads(
        &self,
        matches: Vec<(usize, MatchSquad, MatchSquad)>,
    ) -> Vec<(usize, MatchResultRaw)> {
        let with_flag: Vec<(usize, MatchSquad, MatchSquad, bool)> = matches
            .into_iter()
            .map(|(i, h, a)| (i, h, a, false))
            .collect();
        self.play_squads_with_knockout(with_flag)
    }

    /// Play raw squad-vs-squad matches with explicit knockout flagging.
    /// Knockout fixtures route through the engine's full penalty
    /// shootout when the score is level after extra time — callers can
    /// then read the winner straight from `Score::outcome()` instead of
    /// guessing based on reputation.
    pub fn play_squads_with_knockout(
        &self,
        matches: Vec<(usize, MatchSquad, MatchSquad, bool)>,
    ) -> Vec<(usize, MatchResultRaw)> {
        let matches = match MatchDispatcherRegistry::try_get() {
            Some(dispatcher) => match dispatcher.dispatch_squads(matches) {
                Ok(results) => return results,
                Err(returned) => returned,
            },
            None => matches,
        };
        self.pool.install(|| {
            matches
                .into_par_iter()
                .map(|(idx, home, away, is_knockout)| {
                    let result =
                        FootballEngine::<840, 545>::play(home, away, false, false, is_knockout);
                    (idx, result)
                })
                .collect()
        })
    }
}
