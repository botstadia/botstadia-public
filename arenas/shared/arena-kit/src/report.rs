//! What a world reports as it plays: per-turn series values, events and the
//! shaping reward, and at the end the finishing order and stats.
//!
//! Everything is addressed through typed keys the declaration crate defines
//! (`pub const DISTANCE: SeriesKey = SeriesKey("distance");`), so a misspelt
//! name doesn't compile, and the world tests fail on any key the declaration
//! doesn't have. An unknown key is never a trap: it's recorded as a problem.

use botstadia_interface::declaration::{Declaration, SeriesScope};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeriesKey(pub &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventKey(pub &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatKey(pub &'static str);

/// One turn's reports. Series hold their last value until reported again.
#[derive(Debug, Clone)]
pub struct Report {
    keys: Vec<(String, SeriesScope)>,
    event_keys: Vec<String>,
    pub(crate) series: Vec<Vec<f32>>,
    pub(crate) events: Vec<(u32, Option<u32>)>,
    pub(crate) shaping: Vec<f32>,
    pub(crate) problems: Vec<String>,
    seats: u32,
}

impl Report {
    /// A fresh report for `seats`. The session makes its own; an arena's own
    /// unit tests make one to drive their world directly.
    pub fn new(declaration: &Declaration, seats: u32) -> Self {
        Self {
            keys: declaration
                .series
                .iter()
                .map(|series| (series.key.clone(), series.scope))
                .collect(),
            event_keys: declaration
                .events
                .iter()
                .map(|event| event.key.clone())
                .collect(),
            series: declaration
                .series
                .iter()
                .map(|series| match series.scope {
                    SeriesScope::PerSeat => vec![0.0; seats as usize],
                    SeriesScope::Session => vec![0.0],
                })
                .collect(),
            events: Vec::new(),
            shaping: vec![0.0; seats as usize],
            problems: Vec::new(),
            seats,
        }
    }

    /// Clear what belongs to one turn only. Call before each `step`.
    pub fn next_turn(&mut self) {
        self.events.clear();
        self.shaping.iter_mut().for_each(|value| *value = 0.0);
    }

    /// This turn's events so far: `(declared event index, seat)`.
    pub fn events(&self) -> &[(u32, Option<u32>)] {
        &self.events
    }

    /// A per-seat series' value for `seat`.
    pub fn seat_series(&mut self, key: SeriesKey, seat: u32, value: f32) {
        match self.keys.iter().position(|(name, _)| name == key.0) {
            Some(index) if self.keys[index].1 == SeriesScope::PerSeat && seat < self.seats => {
                self.series[index][seat as usize] = value;
            }
            Some(_) => self.problems.push(format!(
                "series `{}` isn't per seat, or seat {seat} doesn't exist",
                key.0
            )),
            None => self
                .problems
                .push(format!("series `{}` isn't declared", key.0)),
        }
    }

    /// A whole-session series' value.
    pub fn session_series(&mut self, key: SeriesKey, value: f32) {
        match self.keys.iter().position(|(name, _)| name == key.0) {
            Some(index) if self.keys[index].1 == SeriesScope::Session => {
                self.series[index][0] = value
            }
            Some(_) => self
                .problems
                .push(format!("series `{}` is per seat", key.0)),
            None => self
                .problems
                .push(format!("series `{}` isn't declared", key.0)),
        }
    }

    pub fn event(&mut self, key: EventKey, seat: Option<u32>) {
        match self.event_keys.iter().position(|name| name == key.0) {
            Some(index) => self.events.push((index as u32, seat)),
            None => self
                .problems
                .push(format!("event `{}` isn't declared", key.0)),
        }
    }

    /// This turn's shaping reward for `seat`, for training only.
    pub fn shaping(&mut self, seat: u32, reward: f32) {
        match self.shaping.get_mut(seat as usize) {
            Some(slot) => *slot += reward,
            None => self
                .problems
                .push(format!("shaping for seat {seat}, which doesn't exist")),
        }
    }
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Finish {
    /// Places from first to last, each listing the seats that share it.
    pub places: Vec<Vec<u32>>,
    /// The line the site shows as given, e.g. "By knockout".
    pub decided_by: String,
    pub(crate) stats: Vec<(&'static str, Vec<f64>)>,
}

impl Finish {
    pub fn new(places: Vec<Vec<u32>>, decided_by: impl Into<String>) -> Self {
        Self {
            places,
            decided_by: decided_by.into(),
            stats: Vec::new(),
        }
    }

    /// Rank seats by a score, highest first, with equal scores sharing a place.
    pub fn by_score(scores: &[f64], decided_by: impl Into<String>) -> Self {
        let mut order: Vec<u32> = (0..scores.len() as u32).collect();
        order.sort_by(|a, b| scores[*b as usize].total_cmp(&scores[*a as usize]));
        let mut places: Vec<Vec<u32>> = Vec::new();
        for seat in order {
            match places.last_mut() {
                Some(place) if scores[place[0] as usize] == scores[seat as usize] => {
                    place.push(seat)
                }
                _ => places.push(vec![seat]),
            }
        }
        Self::new(places, decided_by)
    }

    /// A declared stat's value for every seat, in seat order.
    pub fn stat(mut self, key: StatKey, values: Vec<f64>) -> Self {
        self.stats.push((key.0, values));
        self
    }

    /// Stats in declared order, or why they don't match the declaration.
    pub(crate) fn ordered_stats(
        &self,
        declaration: &Declaration,
        seats: u32,
    ) -> Result<Vec<Vec<f64>>, String> {
        for (key, _) in &self.stats {
            if declaration.stat_index(key).is_none() {
                return Err(format!("stat `{key}` isn't declared"));
            }
        }
        declaration
            .stats
            .iter()
            .map(|decl| {
                let values = self
                    .stats
                    .iter()
                    .find(|(key, _)| *key == decl.key)
                    .map(|(_, values)| values.clone())
                    .ok_or_else(|| format!("stat `{}` wasn't reported", decl.key))?;
                if values.len() != seats as usize {
                    return Err(format!(
                        "stat `{}` has {} values for {seats} seats",
                        decl.key,
                        values.len()
                    ));
                }
                Ok(values)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_rank_with_ties_sharing_a_place() {
        let finish = Finish::by_score(&[3.0, 5.0, 3.0, 1.0], "Score");
        assert_eq!(finish.places, vec![vec![1], vec![0, 2], vec![3]]);
    }
}
