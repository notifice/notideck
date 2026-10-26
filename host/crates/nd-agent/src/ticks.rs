// SPDX-License-Identifier: GPL-3.0-or-later
//! Periodic samplers: the 1 s live-data tick and the 80 ms level-meter tick (round 12).
//!
//! The state lock is only held to read the source list and, afterwards, to apply the samples;
//! sampling itself (subprocesses, D-Bus, WASAPI) runs lock-free on the blocking pool.

use crate::AgentState;
use nd_core::live::{sample_levels, sample_plugins, LiveSample};
use nd_core::plugin::{NullPlugins, PluginControl, PluginInstance};
use nd_core::{sample_all, DataSource, LiveProviders};
use nd_proto::v1::UpdateControls;
use nd_transport::ServerHandle;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{info, warn};

/// How often level meters are sampled while a phone shows one.
pub const METER_INTERVAL: Duration = Duration::from_millis(80);

/// Mixer availability observed by one live step (for change-only logging).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MixerHealth {
    pub available: bool,
    pub error: Option<String>,
}

/// One live tick: refresh the mixer once if any visible source reads it, sample every source and
/// apply the samples. Blocking. Returns the patches and, when the mixer was refreshed, its health.
pub fn live_step(state: &AgentState, providers: &LiveProviders) -> (Vec<UpdateControls>, Option<MixerHealth>) {
    live_step_with(state, providers, &NullPlugins)
}

/// Overlay `over` on `base`: every field `over` sets wins (a plugin look beats a data-source reading).
fn overlay(base: &mut LiveSample, over: LiveSample) {
    macro_rules! take {
        ($($f:ident),*) => { $(if over.$f.is_some() { base.$f = over.$f; })* };
    }
    take!(value, title, subtitle, state_index, icon_hash, meter, dimmed, detail);
}

/// Merge plugin samples into the data-source samples: one sample per control id, plugin fields win.
pub fn merge_samples(
    mut samples: Vec<(String, LiveSample)>,
    plugin: Vec<(String, LiveSample)>,
) -> Vec<(String, LiveSample)> {
    for (id, over) in plugin {
        match samples.iter_mut().find(|(i, _)| *i == id) {
            Some((_, base)) => overlay(base, over),
            None => samples.push((id, over)),
        }
    }
    samples
}

/// [`live_step`] with plugin support: reports the on-screen plugin instances to the host
/// (`set_visible`, every tick) and merges the plugins' looks into the samples.
pub fn live_step_with(
    state: &AgentState,
    providers: &LiveProviders,
    plugins: &dyn PluginControl,
) -> (Vec<UpdateControls>, Option<MixerHealth>) {
    let sources = state.live_sources_all();
    let instances = state.plugin_instances_all();
    plugins.set_visible(unique_instances(&instances));
    if sources.is_empty() && instances.is_empty() {
        return (Vec::new(), None);
    }
    let health = sources.iter().any(|(_, s)| matches!(s, DataSource::AppAudio { .. })).then(|| {
        let _ = providers.mixer.refresh();
        MixerHealth { available: providers.mixer.available(), error: providers.mixer.last_error() }
    });
    let samples = merge_samples(sample_all(&sources, providers), sample_plugins(&instances, plugins));
    (state.apply_live_all(&samples), health)
}

/// Instances deduplicated by context (a page and the rail can never share one, but be safe).
fn unique_instances(instances: &[(String, PluginInstance)]) -> Vec<PluginInstance> {
    let mut seen = std::collections::HashSet::new();
    instances.iter().filter(|(_, i)| seen.insert(i.context.clone())).map(|(_, i)| i.clone()).collect()
}

/// One meter tick: levels for the visible meter controls, as change-only `level` patches. Blocking.
pub fn meter_step(state: &AgentState, providers: &LiveProviders) -> Vec<UpdateControls> {
    if !state.has_meter_candidates() {
        return Vec::new();
    }
    let targets = state.meter_targets_all(providers);
    if targets.is_empty() {
        return Vec::new();
    }
    let levels = sample_levels(&targets, providers);
    state.apply_levels_all(&levels)
}

fn any_connected(handle: &ServerHandle) -> bool {
    handle.all_sessions().iter().any(|s| s.is_connected())
}

/// Sample live data sources once a second (or when poked) and push change-only patches.
pub async fn live_ticker(state: AgentState, handle: ServerHandle, providers: LiveProviders, poke: Arc<Notify>) {
    live_ticker_with(state, handle, providers, poke, Arc::new(NullPlugins)).await
}

/// [`live_ticker`] with a plugin host (round 19).
pub async fn live_ticker_with(
    state: AgentState,
    handle: ServerHandle,
    providers: LiveProviders,
    poke: Arc<Notify>,
    plugins: Arc<dyn PluginControl>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_health: Option<MixerHealth> = None;
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = poke.notified() => {}
        }
        if !any_connected(&handle) {
            // Nothing is on screen without a phone: plugins get willDisappear until one reconnects.
            plugins.set_visible(Vec::new());
            continue;
        }
        let (st, pr, pl) = (state.clone(), providers.clone(), plugins.clone());
        match tokio::task::spawn_blocking(move || live_step_with(&st, &pr, pl.as_ref())).await {
            Ok((updates, health)) => {
                if let Some(h) = health {
                    if last_health.as_ref() != Some(&h) {
                        match (&h.error, h.available) {
                            (Some(e), false) => warn!("Per-app mixer unavailable: {e}"),
                            _ => info!(
                                backend = providers.mixer.backend_name(),
                                available = h.available,
                                "Per-app mixer status"
                            ),
                        }
                        last_health = Some(h);
                    }
                }
                for u in updates {
                    tracing::debug!(patches = u.patches.len(), page = %u.page_id, "Pushing live data");
                    if let Err(e) = handle.update_controls(u).await {
                        tracing::debug!("Live update skipped: {e}");
                    }
                }
            }
            Err(e) => warn!("Live sampling task failed: {e}"),
        }
    }
}

/// Sample level meters every [`METER_INTERVAL`] while a phone is connected and a meter is visible.
pub async fn meter_ticker(state: AgentState, handle: ServerHandle, providers: LiveProviders) {
    let mut interval = tokio::time::interval(METER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if !any_connected(&handle) || !state.has_meter_candidates() {
            continue;
        }
        let (st, pr) = (state.clone(), providers.clone());
        match tokio::task::spawn_blocking(move || meter_step(&st, &pr)).await {
            Ok(updates) => {
                for u in updates {
                    if let Err(e) = handle.update_controls(u).await {
                        tracing::debug!("Meter update skipped: {e}");
                    }
                }
            }
            Err(e) => warn!("Meter sampling task failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_fields_win_and_others_are_kept() {
        let base = vec![(
            "a".to_string(),
            LiveSample {
                title: Some("src".into()),
                value: Some(0.5),
                subtitle: Some("s".into()),
                ..Default::default()
            },
        )];
        let plugin = vec![
            ("a".to_string(), LiveSample { title: Some("plug".into()), dimmed: Some(false), ..Default::default() }),
            ("b".to_string(), LiveSample { title: Some("only".into()), ..Default::default() }),
        ];
        let m = merge_samples(base, plugin);
        assert_eq!(m.len(), 2);
        let a = &m[0].1;
        assert_eq!(
            (a.title.as_deref(), a.value, a.subtitle.as_deref(), a.dimmed),
            (Some("plug"), Some(0.5), Some("s"), Some(false))
        );
        assert_eq!(m[1].0, "b");
    }
}
