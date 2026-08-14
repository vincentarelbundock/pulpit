//! A textual format for monitor topologies, and a scripted-transition runner.
//!
//! The point is to close the loop between *real* hardware and CI. A live
//! topology can be dumped from any session (`pulpit-topology`), captured
//! on a machine with an awkward dock or projector, committed as a file, and
//! replayed deterministically forever afterwards — with no display server, no
//! GPU and no privileges.
//!
//! ```text
//! # a laptop with a projector attached
//! step plugged-in
//!   monitor eDP-1 builtin 1920x1200+0+0 @1 make=LEN model=Panel id=LEN-0001
//!   monitor HDMI-1 1920x1080+1920+0 @1 make=ACME model=Projector
//! step unplugged
//!   monitor eDP-1 builtin 1920x1200+0+0 @1 make=LEN model=Panel id=LEN-0001
//! ```

use crate::identity::MonitorIdentity;
use crate::snapshot::{is_builtin_connector, DisplaySnapshot, Monitor, Rect};

/// One named topology in a script.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub name: String,
    pub monitors: Vec<Monitor>,
}

impl Step {
    pub fn snapshot(&self, sequence: u64) -> DisplaySnapshot {
        DisplaySnapshot::new(self.monitors.clone(), sequence)
    }
}

/// A sequence of topologies to walk through.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Scenario {
    pub description: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, thiserror::Error)]
#[error("line {line}: {reason}")]
pub struct ParseError {
    pub line: usize,
    pub reason: String,
}

impl Scenario {
    /// Parse the format above. Blank lines and `#` comments are ignored; the
    /// first comment becomes the description.
    pub fn parse(text: &str) -> Result<Scenario, ParseError> {
        let mut scenario = Scenario::default();
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            let number = index + 1;
            if line.is_empty() {
                continue;
            }
            if let Some(comment) = line.strip_prefix('#') {
                if scenario.description.is_empty() {
                    scenario.description = comment.trim().to_string();
                }
                continue;
            }
            let (keyword, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            match keyword {
                "step" => scenario.steps.push(Step {
                    name: rest.trim().to_string(),
                    monitors: Vec::new(),
                }),
                "monitor" => {
                    let monitor = parse_monitor(rest, number)?;
                    let step = scenario.steps.last_mut().ok_or(ParseError {
                        line: number,
                        reason: "a monitor outside any step".into(),
                    })?;
                    step.monitors.push(monitor);
                }
                other => {
                    return Err(ParseError {
                        line: number,
                        reason: format!("unknown keyword {other:?}"),
                    })
                }
            }
        }
        if scenario.steps.is_empty() {
            return Err(ParseError {
                line: 0,
                reason: "a scenario needs at least one step".into(),
            });
        }
        Ok(scenario)
    }

    /// Render a scenario back out, so a captured topology round-trips.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        if !self.description.is_empty() {
            out.push_str(&format!("# {}\n", self.description));
        }
        for step in &self.steps {
            out.push_str(&format!("\nstep {}\n", step.name));
            for monitor in &step.monitors {
                out.push_str(&format!("  {}\n", format_monitor(monitor)));
            }
        }
        out
    }
}

/// Format one monitor as a `monitor …` line.
pub fn format_monitor(monitor: &Monitor) -> String {
    let mut parts = vec!["monitor".to_string()];
    parts.push(
        monitor
            .connector
            .clone()
            .unwrap_or_else(|| "unknown".into()),
    );
    if monitor.builtin {
        parts.push("builtin".into());
    }
    parts.push(format!(
        "{}x{}+{}+{}",
        monitor.geometry.width, monitor.geometry.height, monitor.geometry.x, monitor.geometry.y
    ));
    parts.push(format!("@{}", monitor.scale_factor));
    if let Some(make) = &monitor.make {
        if !make.is_empty() {
            parts.push(format!("make={make}"));
        }
    }
    if let Some(model) = &monitor.model {
        if !model.is_empty() {
            parts.push(format!("model={model}"));
        }
    }
    if let MonitorIdentity::Stable { id } = &monitor.identity {
        parts.push(format!("id={id}"));
    }
    if let Some((width, height)) = monitor.physical_size_mm {
        parts.push(format!("mm={width}x{height}"));
    }
    if monitor.primary {
        parts.push("primary".into());
    }
    parts.join(" ")
}

fn parse_monitor(text: &str, line: usize) -> Result<Monitor, ParseError> {
    let error = |reason: String| ParseError { line, reason };
    let mut tokens = text.split_whitespace();
    let connector = tokens
        .next()
        .ok_or_else(|| error("a monitor needs a connector name".into()))?
        .to_string();

    let mut builtin = is_builtin_connector(&connector);
    let mut primary = false;
    let mut geometry = None;
    let mut scale = 1.0f64;
    let mut make = None;
    let mut model = None;
    let mut stable = None;
    let mut millimetres = None;

    for token in tokens {
        if token == "builtin" {
            builtin = true;
        } else if token == "primary" {
            primary = true;
        } else if let Some(value) = token.strip_prefix('@') {
            scale = value
                .parse()
                .map_err(|_| error(format!("bad scale factor {value:?}")))?;
        } else if let Some(value) = token.strip_prefix("make=") {
            make = Some(value.to_string());
        } else if let Some(value) = token.strip_prefix("model=") {
            model = Some(value.to_string());
        } else if let Some(value) = token.strip_prefix("id=") {
            stable = Some(value.to_string());
        } else if let Some(value) = token.strip_prefix("mm=") {
            let (width, height) = value
                .split_once('x')
                .ok_or_else(|| error(format!("bad physical size {value:?}")))?;
            millimetres = Some((
                width
                    .parse()
                    .map_err(|_| error("bad physical width".into()))?,
                height
                    .parse()
                    .map_err(|_| error("bad physical height".into()))?,
            ));
        } else if token.contains('x') {
            geometry = Some(parse_geometry(token).map_err(error)?);
        } else {
            return Err(error(format!("unknown token {token:?}")));
        }
    }

    let geometry = geometry.ok_or_else(|| error("a monitor needs WxH+X+Y geometry".into()))?;
    let identity = match &stable {
        Some(id) => MonitorIdentity::Stable { id: id.clone() },
        None => MonitorIdentity::Connector {
            connector: connector.clone(),
            make: make.clone().unwrap_or_default(),
            model: model.clone().unwrap_or_default(),
        },
    };
    let fallback = stable.is_some().then(|| MonitorIdentity::Connector {
        connector: connector.clone(),
        make: make.clone().unwrap_or_default(),
        model: model.clone().unwrap_or_default(),
    });

    Ok(Monitor {
        identity,
        fallback_identity: fallback,
        connector: Some(connector),
        make,
        model,
        geometry,
        scale_factor: scale,
        physical_size_mm: millimetres,
        builtin,
        primary,
        handle: 0,
    })
}

/// `1920x1080+1920+0`, allowing negative offsets.
fn parse_geometry(token: &str) -> Result<Rect, String> {
    let bad = || format!("bad geometry {token:?}");
    let (size, offsets) = match token.find(['+', '-'].as_slice()) {
        Some(index) if index > 0 => token.split_at(index),
        _ => (token, "+0+0"),
    };
    let (width, height) = size.split_once('x').ok_or_else(bad)?;
    let width: u32 = width.parse().map_err(|_| bad())?;
    let height: u32 = height.parse().map_err(|_| bad())?;

    // Offsets keep their sign: "+1920-200" is two signed numbers.
    let mut values = Vec::new();
    let mut current = String::new();
    for character in offsets.chars() {
        if (character == '+' || character == '-') && !current.is_empty() {
            values.push(std::mem::take(&mut current));
        }
        if character == '+' && current.is_empty() {
            continue;
        }
        current.push(character);
    }
    if !current.is_empty() {
        values.push(current);
    }
    let x: i32 = values
        .first()
        .map_or(Ok(0), |value| value.parse())
        .map_err(|_| bad())?;
    let y: i32 = values
        .get(1)
        .map_or(Ok(0), |value| value.parse())
        .map_err(|_| bad())?;
    Ok(Rect::new(x, y, width, height))
}

/// Turn a live snapshot into a one-step scenario, for capture tools.
pub fn capture(snapshot: &DisplaySnapshot, name: &str, description: &str) -> Scenario {
    Scenario {
        description: description.to_string(),
        steps: vec![Step {
            name: name.to_string(),
            monitors: snapshot.monitors.clone(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# laptop plus projector
step plugged-in
  monitor eDP-1 builtin 1920x1200+0+0 @1 make=LEN model=Panel id=LEN-0001 mm=344x215
  monitor HDMI-1 1920x1080+1920+0 @1 make=ACME model=Projector

step unplugged
  monitor eDP-1 builtin 1920x1200+0+0 @1 make=LEN model=Panel id=LEN-0001 mm=344x215
";

    #[test]
    fn parses_a_scenario() {
        let scenario = Scenario::parse(SAMPLE).unwrap();
        assert_eq!(scenario.description, "laptop plus projector");
        assert_eq!(scenario.steps.len(), 2);

        let first = &scenario.steps[0];
        assert_eq!(first.name, "plugged-in");
        assert_eq!(first.monitors.len(), 2);
        assert!(first.monitors[0].builtin);
        assert_eq!(
            first.monitors[0].identity,
            MonitorIdentity::Stable {
                id: "LEN-0001".into()
            }
        );
        assert_eq!(first.monitors[0].physical_size_mm, Some((344, 215)));
        assert_eq!(first.monitors[1].geometry, Rect::new(1920, 0, 1920, 1080));
        assert!(!first.monitors[1].builtin);
    }

    #[test]
    fn negative_offsets_and_scales_survive() {
        let scenario = Scenario::parse("step s\n  monitor DP-1 3840x2160-3840-200 @2\n").unwrap();
        let monitor = &scenario.steps[0].monitors[0];
        assert_eq!(monitor.geometry, Rect::new(-3840, -200, 3840, 2160));
        assert_eq!(monitor.scale_factor, 2.0);
    }

    #[test]
    fn round_trips_through_text() {
        let scenario = Scenario::parse(SAMPLE).unwrap();
        let reparsed = Scenario::parse(&scenario.to_text()).unwrap();
        assert_eq!(scenario, reparsed);
    }

    #[test]
    fn malformed_scripts_are_rejected_with_a_line_number() {
        assert!(Scenario::parse("").is_err());
        assert!(Scenario::parse("monitor eDP-1 1920x1080+0+0\n").is_err());
        let error = Scenario::parse("step s\n  monitor eDP-1 nonsense\n").unwrap_err();
        assert_eq!(error.line, 2);
    }
}
