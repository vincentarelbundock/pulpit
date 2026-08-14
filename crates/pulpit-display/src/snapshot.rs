//! Immutable monitor snapshots.
//!
//! Every reconciliation builds one of these afresh. No per-window cached copy
//! of monitor geometry, scale or identity may survive a topology change: that
//! is exactly the pdfpc reconnect defect the specification calls out.

use serde::{Deserialize, Serialize};

use crate::identity::{IdentityRecord, MonitorIdentity};

/// Logical desktop rectangle. Coordinates may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn right(&self) -> i64 {
        self.x as i64 + self.width as i64
    }

    pub fn bottom(&self) -> i64 {
        self.y as i64 + self.height as i64
    }

    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }

    pub fn intersection_area(&self, other: &Rect) -> u64 {
        let x0 = self.x.max(other.x) as i64;
        let y0 = self.y.max(other.y) as i64;
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            0
        } else {
            ((x1 - x0) * (y1 - y0)) as u64
        }
    }

    pub fn overlaps(&self, other: &Rect) -> bool {
        self.intersection_area(other) > 0
    }

    pub fn contains_rect(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

/// One monitor as reported by the platform adapter for this snapshot only.
#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    /// Strongest identity the adapter could build.
    pub identity: MonitorIdentity,
    /// Weaker identity retained so persisted choices keep matching.
    pub fallback_identity: Option<MonitorIdentity>,
    pub connector: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    /// Logical geometry, in desktop coordinates.
    pub geometry: Rect,
    pub scale_factor: f64,
    pub physical_size_mm: Option<(u32, u32)>,
    /// True when the connector type says this is an internal panel
    /// (eDP/LVDS/DSI or the platform equivalent).
    pub builtin: bool,
    /// Whether the platform reported this monitor as "primary", if it reports
    /// such a thing at all. Never assumed to exist.
    pub primary: bool,
    /// Session-local handle. Valid only inside this snapshot.
    pub handle: u64,
}

impl Monitor {
    pub fn label(&self) -> String {
        let name = match (&self.make, &self.model) {
            (Some(make), Some(model)) => format!("{make} {model}"),
            (Some(make), None) => make.clone(),
            (None, Some(model)) => model.clone(),
            (None, None) => self
                .connector
                .clone()
                .unwrap_or_else(|| self.identity.label()),
        };
        let connector = self.connector.clone().unwrap_or_default();
        format!(
            // Two decimals: a scale factor is a ratio, not a measurement, and
            // "1.6041666666666667x" is noise in a picker read mid-talk.
            "{connector}{}{name} · {}×{} @ {:.2}x",
            if connector.is_empty() { "" } else { " " },
            self.geometry.width,
            self.geometry.height,
            self.scale_factor
        )
    }

    pub fn record(&self) -> IdentityRecord {
        let record = IdentityRecord::new(self.identity.clone());
        match &self.fallback_identity {
            Some(fallback) => record.with_fallback(fallback.clone()),
            None => record,
        }
    }

    /// Physical pixel size implied by logical size × scale factor. Phase 0
    /// verifies this against the compositor on each Wayland target.
    pub fn physical_pixels(&self) -> (u32, u32) {
        (
            (self.geometry.width as f64 * self.scale_factor).round() as u32,
            (self.geometry.height as f64 * self.scale_factor).round() as u32,
        )
    }

    fn matches(&self, identity: &MonitorIdentity) -> bool {
        let candidates = std::iter::once(&self.identity).chain(self.fallback_identity.iter());
        for mine in candidates {
            if mine == identity {
                return true;
            }
            // A connector identity matches across backends that spell the
            // EDID data differently: X11 reports the three-letter PNP id
            // ("LEN") where a Wayland compositor reports the full vendor
            // string ("Lenovo Group Limited"), and models may carry stray
            // whitespace. The connector name and the model identify the
            // panel; the make is presentation, not identity.
            if let (
                MonitorIdentity::Connector {
                    connector, model, ..
                },
                MonitorIdentity::Connector {
                    connector: oconnector,
                    model: omodel,
                    ..
                },
            ) = (mine, identity)
            {
                if connector == oconnector
                    && !model.trim().is_empty()
                    && model.trim().eq_ignore_ascii_case(omodel.trim())
                {
                    return true;
                }
            }
            // A geometric identity matches even when the monitor has since
            // moved: position is a tie-breaker, not part of the identity.
            if let (
                MonitorIdentity::Geometric {
                    make,
                    model,
                    width_mm,
                    height_mm,
                    ..
                },
                MonitorIdentity::Geometric {
                    make: omake,
                    model: omodel,
                    width_mm: ow,
                    height_mm: oh,
                    ..
                },
            ) = (mine, identity)
            {
                if make == omake && model == omodel && width_mm == ow && height_mm == oh {
                    return true;
                }
            }
        }
        false
    }

    fn matches_exactly(&self, identity: &MonitorIdentity) -> bool {
        std::iter::once(&self.identity)
            .chain(self.fallback_identity.iter())
            .any(|mine| mine == identity)
    }
}

/// Result of resolving a persisted identity against a live snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one monitor matched.
    Unique(usize),
    /// Several monitors matched equally well. Surfaced for confirmation
    /// rather than resolved by enumeration order.
    Ambiguous(Vec<usize>),
    /// Nothing matched: the display is gone, or not connected yet.
    Missing,
}

/// A group of monitors that occupy exactly the same desktop rectangle. They
/// collapse to one logical placement target.
#[derive(Debug, Clone, PartialEq)]
pub struct MirrorGroup {
    pub geometry: Rect,
    pub members: Vec<usize>,
}

/// Two monitors whose geometries overlap without being identical, e.g. a
/// 1080p projector mirroring part of a 4K panel. Kept as distinct targets and
/// reported, because collapsing them is a known competitor failure mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overlap {
    pub a: usize,
    pub b: usize,
    /// True when one rectangle fully contains the other.
    pub nested: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DisplaySnapshot {
    pub monitors: Vec<Monitor>,
    /// Monotonic sequence number. A snapshot with a lower sequence than the
    /// one already applied is a stale delayed notification and is ignored.
    pub sequence: u64,
}

impl DisplaySnapshot {
    pub fn new(monitors: Vec<Monitor>, sequence: u64) -> Self {
        Self { monitors, sequence }
    }

    pub fn is_empty(&self) -> bool {
        self.monitors.is_empty()
    }

    pub fn len(&self) -> usize {
        self.monitors.len()
    }

    pub fn get(&self, index: usize) -> Option<&Monitor> {
        self.monitors.get(index)
    }

    /// True when the two snapshots describe the same topology. Used to make
    /// repeated notifications free.
    pub fn same_topology(&self, other: &DisplaySnapshot) -> bool {
        self.monitors == other.monitors
    }

    pub fn resolve(&self, record: &IdentityRecord) -> Resolution {
        for identity in record.candidates() {
            let exact: Vec<usize> = self
                .monitors
                .iter()
                .enumerate()
                .filter(|(_, m)| m.matches_exactly(identity))
                .map(|(i, _)| i)
                .collect();
            match exact.len() {
                0 => {}
                1 => return Resolution::Unique(exact[0]),
                _ => return self.disambiguate(exact, identity),
            }
            let loose: Vec<usize> = self
                .monitors
                .iter()
                .enumerate()
                .filter(|(_, m)| m.matches(identity))
                .map(|(i, _)| i)
                .collect();
            match loose.len() {
                0 => continue,
                1 => return Resolution::Unique(loose[0]),
                _ => return self.disambiguate(loose, identity),
            }
        }
        Resolution::Missing
    }

    /// Position is the last tie-breaker between otherwise identical monitors;
    /// if it does not separate them the caller must ask the user.
    fn disambiguate(&self, candidates: Vec<usize>, identity: &MonitorIdentity) -> Resolution {
        if let MonitorIdentity::Geometric { x, y, .. } = identity {
            let positioned: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|i| {
                    self.monitors[*i].geometry.x == *x && self.monitors[*i].geometry.y == *y
                })
                .collect();
            if positioned.len() == 1 {
                return Resolution::Unique(positioned[0]);
            }
        }
        Resolution::Ambiguous(candidates)
    }

    /// Mirrored outputs: identical geometry collapses to one target.
    pub fn mirror_groups(&self) -> Vec<MirrorGroup> {
        let mut groups: Vec<MirrorGroup> = Vec::new();
        for (index, monitor) in self.monitors.iter().enumerate() {
            match groups.iter_mut().find(|g| g.geometry == monitor.geometry) {
                Some(group) => group.members.push(index),
                None => groups.push(MirrorGroup {
                    geometry: monitor.geometry,
                    members: vec![index],
                }),
            }
        }
        groups
    }

    /// Distinct logical placement targets: one per mirror group.
    pub fn logical_targets(&self) -> Vec<usize> {
        self.mirror_groups()
            .into_iter()
            .map(|g| g.members[0])
            .collect()
    }

    /// Overlapping-but-unequal geometries, reported rather than collapsed.
    pub fn overlaps(&self) -> Vec<Overlap> {
        let mut out = Vec::new();
        for a in 0..self.monitors.len() {
            for b in (a + 1)..self.monitors.len() {
                let ra = self.monitors[a].geometry;
                let rb = self.monitors[b].geometry;
                if ra == rb || !ra.overlaps(&rb) {
                    continue;
                }
                out.push(Overlap {
                    a,
                    b,
                    nested: ra.contains_rect(&rb) || rb.contains_rect(&ra),
                });
            }
        }
        out
    }

    pub fn builtin(&self) -> Option<usize> {
        self.monitors.iter().position(|m| m.builtin)
    }

    /// External monitors, largest first: the natural audience candidates.
    pub fn external_targets(&self) -> Vec<usize> {
        let mut external: Vec<usize> = self
            .logical_targets()
            .into_iter()
            .filter(|i| !self.monitors[*i].builtin)
            .collect();
        external.sort_by_key(|i| std::cmp::Reverse(self.monitors[*i].geometry.area()));
        external
    }
}

/// True when a connector name denotes an internal panel.
pub fn is_builtin_connector(connector: &str) -> bool {
    let c = connector.to_ascii_uppercase();
    ["EDP", "LVDS", "DSI", "IDP", "DPI"]
        .iter()
        .any(|prefix| c.starts_with(prefix))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn monitor(connector: &str, model: &str, geometry: Rect) -> Monitor {
        Monitor {
            identity: MonitorIdentity::Connector {
                connector: connector.to_string(),
                make: "ACME".to_string(),
                model: model.to_string(),
            },
            fallback_identity: Some(MonitorIdentity::Geometric {
                make: "ACME".to_string(),
                model: model.to_string(),
                width_mm: 600,
                height_mm: 340,
                x: geometry.x,
                y: geometry.y,
            }),
            connector: Some(connector.to_string()),
            make: Some("ACME".to_string()),
            model: Some(model.to_string()),
            geometry,
            scale_factor: 1.0,
            physical_size_mm: Some((600, 340)),
            builtin: is_builtin_connector(connector),
            primary: false,
            handle: 0,
        }
    }

    pub fn laptop() -> Monitor {
        monitor("eDP-1", "InternalPanel", Rect::new(0, 0, 1920, 1200))
    }

    pub fn projector() -> Monitor {
        monitor("HDMI-1", "Projector", Rect::new(1920, 0, 1920, 1080))
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn builtin_connectors_are_recognised() {
        assert!(is_builtin_connector("eDP-1"));
        assert!(is_builtin_connector("LVDS1"));
        assert!(!is_builtin_connector("HDMI-1"));
        assert!(!is_builtin_connector("DP-3"));
    }

    #[test]
    fn identical_geometry_collapses_to_one_target() {
        let mut a = monitor("eDP-1", "Panel", Rect::new(0, 0, 1920, 1080));
        a.builtin = true;
        let b = monitor("HDMI-1", "Projector", Rect::new(0, 0, 1920, 1080));
        let snapshot = DisplaySnapshot::new(vec![a, b], 1);
        assert_eq!(snapshot.mirror_groups().len(), 1);
        assert_eq!(snapshot.logical_targets(), vec![0]);
        assert!(
            snapshot.overlaps().is_empty(),
            "an exact mirror is not an overlap"
        );
    }

    #[test]
    fn unequal_mirror_stays_two_targets_and_is_reported() {
        let panel = monitor("eDP-1", "Panel", Rect::new(0, 0, 3840, 2160));
        let projector = monitor("HDMI-1", "Projector", Rect::new(0, 0, 1920, 1080));
        let snapshot = DisplaySnapshot::new(vec![panel, projector], 1);
        assert_eq!(snapshot.logical_targets().len(), 2, "must not be collapsed");
        let overlaps = snapshot.overlaps();
        assert_eq!(overlaps.len(), 1);
        assert!(overlaps[0].nested);
    }

    #[test]
    fn resolution_survives_a_changed_enumeration_order() {
        let record = projector().record();
        let reordered = DisplaySnapshot::new(vec![projector(), laptop()], 2);
        assert_eq!(reordered.resolve(&record), Resolution::Unique(0));
        let original = DisplaySnapshot::new(vec![laptop(), projector()], 1);
        assert_eq!(original.resolve(&record), Resolution::Unique(1));
    }

    #[test]
    fn resolution_survives_a_changed_resolution_and_scale() {
        let record = projector().record();
        let mut moved = projector();
        moved.geometry = Rect::new(-1280, 200, 1280, 720);
        moved.scale_factor = 2.0;
        let snapshot = DisplaySnapshot::new(vec![laptop(), moved], 3);
        assert_eq!(snapshot.resolve(&record), Resolution::Unique(1));
    }

    #[test]
    fn two_identical_models_are_ambiguous_not_guessed() {
        let a = monitor("DP-1", "Twin", Rect::new(0, 0, 1920, 1080));
        let mut b = monitor("DP-2", "Twin", Rect::new(1920, 0, 1920, 1080));
        // Same make/model/size, different connector and position.
        b.identity = a.identity.clone();
        b.fallback_identity = a.fallback_identity.clone();
        let snapshot = DisplaySnapshot::new(vec![a.clone(), b], 1);
        match snapshot.resolve(&a.record()) {
            Resolution::Ambiguous(candidates) => assert_eq!(candidates, vec![0, 1]),
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn a_record_saved_by_another_backend_still_matches() {
        // X11 spells the make as the PNP id and trims the model; a Wayland
        // compositor reports the full vendor string and a trailing space.
        // Same connector, same panel: the record must keep matching.
        let record = IdentityRecord::new(MonitorIdentity::Connector {
            connector: "eDP-1".into(),
            make: "LEN".into(),
            model: "ATNA40HQ10-0".into(),
        });
        let mut live = laptop();
        live.identity = MonitorIdentity::Connector {
            connector: "eDP-1".into(),
            make: "Lenovo Group Limited".into(),
            model: "ATNA40HQ10-0 ".into(),
        };
        live.fallback_identity = None;
        let snapshot = DisplaySnapshot::new(vec![live], 1);
        assert_eq!(snapshot.resolve(&record), Resolution::Unique(0));
    }

    #[test]
    fn an_empty_model_does_not_match_on_connector_alone() {
        // "HDMI-1 with no EDID" says nothing about which projector is
        // plugged in; the connector name alone is not an identity.
        let record = IdentityRecord::new(MonitorIdentity::Connector {
            connector: "HDMI-1".into(),
            make: String::new(),
            model: String::new(),
        });
        let mut live = projector();
        live.identity = MonitorIdentity::Connector {
            connector: "HDMI-1".into(),
            make: "ACME".into(),
            model: "Projector".into(),
        };
        live.fallback_identity = None;
        let snapshot = DisplaySnapshot::new(vec![live], 1);
        assert_eq!(snapshot.resolve(&record), Resolution::Missing);
    }

    #[test]
    fn a_missing_display_resolves_to_missing() {
        let record = projector().record();
        let snapshot = DisplaySnapshot::new(vec![laptop()], 5);
        assert_eq!(snapshot.resolve(&record), Resolution::Missing);
    }

    #[test]
    fn physical_pixels_follow_the_scale_factor() {
        let mut m = projector();
        m.scale_factor = 2.0;
        assert_eq!(m.physical_pixels(), (3840, 2160));
    }

    #[test]
    fn external_targets_prefer_the_largest_external_display() {
        let small = monitor("HDMI-1", "Small", Rect::new(1920, 0, 1280, 720));
        let big = monitor("DP-1", "Big", Rect::new(3200, 0, 3840, 2160));
        let snapshot = DisplaySnapshot::new(vec![laptop(), small, big], 1);
        assert_eq!(snapshot.external_targets(), vec![2, 1]);
        assert_eq!(snapshot.builtin(), Some(0));
    }
}
