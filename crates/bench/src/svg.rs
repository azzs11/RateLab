//! The Pareto chart (store calls against admission error) and time-series line charts
//! as standalone SVGs.
//!
//! Both axes use `log10(1 + value)`, so zero calls and zero error sit on the axes and
//! the interesting region near the origin is not squashed. Each strategy has its own
//! colour **and** marker shape, so the chart does not depend on colour vision. Every
//! point has a tooltip with its exact values.

use std::fmt::Write as _;

/// One point: a strategy on one traffic pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct Point {
    /// Strategy name (picks the colour and marker).
    pub strategy: String,
    /// Traffic pattern, shown in the tooltip.
    pub scenario: String,
    /// Store calls per 1,000 decisions.
    pub calls: f64,
    /// Admission error, %.
    pub error: f64,
}

const W: f64 = 760.0;
const H: f64 = 480.0;
const LEFT: f64 = 70.0;
const RIGHT: f64 = 200.0;
const TOP: f64 = 50.0;
const BOTTOM: f64 = 60.0;

/// Colour-blind-safe colours (Okabe–Ito) and a marker per strategy, in legend order.
const STYLES: [(&str, &str, &str); 5] = [
    ("central_exact", "#0072B2", "circle"),
    ("local_split", "#E69F00", "square"),
    ("async_sync", "#CC79A7", "triangle"),
    ("fixed_leasing", "#009E73", "diamond"),
    ("adaptive_leasing", "#D55E00", "cross"),
];

fn style(strategy: &str) -> (&'static str, &'static str) {
    STYLES
        .iter()
        .find(|(s, _, _)| *s == strategy)
        .map_or(("#555555", "circle"), |(_, c, m)| (c, m))
}

fn symlog(v: f64) -> f64 {
    (1.0 + v.max(0.0)).log10()
}

fn marker(shape: &str, x: f64, y: f64, colour: &str) -> String {
    let r = 6.0;
    match shape {
        "square" => format!(
            r#"<rect x="{:.1}" y="{:.1}" width="{:.1}" height="{:.1}" fill="{colour}"/>"#,
            x - r * 0.85,
            y - r * 0.85,
            r * 1.7,
            r * 1.7
        ),
        "triangle" => format!(
            r#"<polygon points="{:.1},{:.1} {:.1},{:.1} {:.1},{:.1}" fill="{colour}"/>"#,
            x,
            y - r,
            x - r,
            y + r * 0.8,
            x + r,
            y + r * 0.8
        ),
        "diamond" => format!(
            r#"<polygon points="{:.1},{:.1} {:.1},{:.1} {:.1},{:.1} {:.1},{:.1}" fill="{colour}"/>"#,
            x,
            y - r,
            x + r,
            y,
            x,
            y + r,
            x - r,
            y
        ),
        "cross" => format!(
            r#"<path d="M{:.1},{:.1}L{:.1},{:.1}M{:.1},{:.1}L{:.1},{:.1}" stroke="{colour}" stroke-width="3"/>"#,
            x - r,
            y - r,
            x + r,
            y + r,
            x - r,
            y + r,
            x + r,
            y - r
        ),
        _ => format!(r#"<circle cx="{x:.1}" cy="{y:.1}" r="{r:.1}" fill="{colour}"/>"#),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Renders the chart. `title` goes at the top.
pub fn pareto(points: &[Point], title: &str) -> String {
    let max_calls = points.iter().map(|p| p.calls).fold(1_000.0, f64::max);
    let max_error = points.iter().map(|p| p.error).fold(10.0, f64::max);
    let x_ticks: Vec<f64> = [0.0, 1.0, 10.0, 100.0, 1_000.0, 10_000.0]
        .into_iter()
        .filter(|&t| t <= max_calls * 1.01)
        .collect();
    let y_ticks: Vec<f64> = [0.0, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0]
        .into_iter()
        .filter(|&t| t <= max_error * 1.6)
        .collect();
    let x_max = symlog(*x_ticks.last().unwrap_or(&1_000.0)).max(symlog(max_calls));
    let y_max = symlog(*y_ticks.last().unwrap_or(&10.0)).max(symlog(max_error));
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let px = |v: f64| LEFT + symlog(v) / x_max * plot_w;
    let py = |v: f64| TOP + plot_h - symlog(v) / y_max * plot_h;

    let mut svg = String::new();
    let _ = write!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="Inter, Helvetica, Arial, sans-serif" font-size="12" role="img" aria-labelledby="t d">
<title id="t">{}</title>
<desc id="d">Each point is one coordination strategy on one traffic pattern. Lower left is better: fewer store calls and less admission error.</desc>
<rect width="{W}" height="{H}" fill="#ffffff"/>
<text x="{LEFT}" y="28" font-size="15" font-weight="600" fill="#1a1a1a">{}</text>
"##,
        escape(title),
        escape(title)
    );
    for &t in &x_ticks {
        let x = px(t);
        let _ = writeln!(
            svg,
            r##"<line x1="{x:.1}" y1="{TOP}" x2="{x:.1}" y2="{:.1}" stroke="#e5e5e5"/><text x="{x:.1}" y="{:.1}" text-anchor="middle" fill="#444">{t}</text>"##,
            TOP + plot_h,
            TOP + plot_h + 18.0
        );
    }
    for &t in &y_ticks {
        let y = py(t);
        let _ = writeln!(
            svg,
            r##"<line x1="{LEFT}" y1="{y:.1}" x2="{:.1}" y2="{y:.1}" stroke="#e5e5e5"/><text x="{:.1}" y="{:.1}" text-anchor="end" fill="#444">{t}%</text>"##,
            LEFT + plot_w,
            LEFT - 8.0,
            y + 4.0
        );
    }
    let _ = write!(
        svg,
        r##"<rect x="{LEFT}" y="{TOP}" width="{plot_w}" height="{plot_h}" fill="none" stroke="#999"/>
<text x="{:.1}" y="{:.1}" text-anchor="middle" fill="#1a1a1a">Store calls per 1,000 decisions (log scale)</text>
<text transform="translate(18 {:.1}) rotate(-90)" text-anchor="middle" fill="#1a1a1a">Admission error, % (log scale)</text>
"##,
        LEFT + plot_w / 2.0,
        H - 18.0,
        TOP + plot_h / 2.0
    );
    for p in points {
        let (colour, shape) = style(&p.strategy);
        let _ = writeln!(
            svg,
            "<g><title>{} · {}: {:.0} calls per 1k, {:.2}% error</title>{}</g>",
            escape(&p.strategy),
            escape(&p.scenario),
            p.calls,
            p.error,
            marker(shape, px(p.calls), py(p.error), colour)
        );
    }
    let lx = LEFT + plot_w + 20.0;
    for (i, (name, colour, shape)) in STYLES.iter().enumerate() {
        let y = TOP + 10.0 + i as f64 * 24.0;
        let _ = writeln!(
            svg,
            r##"{}<text x="{:.1}" y="{:.1}" fill="#1a1a1a">{name}</text>"##,
            marker(shape, lx + 6.0, y, colour),
            lx + 18.0,
            y + 4.0
        );
    }
    let _ = writeln!(
        svg,
        r##"<text x="{lx:.1}" y="{:.1}" fill="#666" font-size="11">Lower left is better.</text>"##,
        TOP + 10.0 + STYLES.len() as f64 * 24.0 + 6.0
    );
    svg.push_str("</svg>\n");
    svg
}

/// One line of a [`lines`] chart.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Legend label.
    pub name: String,
    /// Stroke colour.
    pub colour: &'static str,
    /// SVG `stroke-dasharray` (empty for solid), so lines differ without colour.
    pub dash: &'static str,
    /// Stroke width.
    pub width: f64,
    /// `(x, y)` points in order.
    pub points: Vec<(f64, f64)>,
}

/// A line chart from zero on the x axis. The y axis starts at zero, or below it when a
/// line goes negative. Each line has a tooltip and a legend entry.
pub fn lines(series: &[Series], title: &str, x_label: &str, y_label: &str) -> String {
    let plot_w = W - LEFT - RIGHT;
    let plot_h = H - TOP - BOTTOM;
    let all = || series.iter().flat_map(|s| s.points.iter());
    let x_max = all().map(|p| p.0).fold(0.0, f64::max).max(1e-9);
    let raw_max = all().map(|p| p.1).fold(0.0, f64::max);
    let raw_min = all().map(|p| p.1).fold(0.0, f64::min);
    // Four equal steps covering both signs.
    let step = nice((raw_max - raw_min) / 4.0);
    let y_min = (raw_min / step).floor() * step;
    let steps = ((raw_max - y_min) / step).ceil().max(1.0) as u32;
    let y_max = y_min + step * f64::from(steps);
    let px = |x: f64| LEFT + x / x_max * plot_w;
    let py = |y: f64| TOP + plot_h - (y - y_min) / (y_max - y_min) * plot_h;
    let mut svg = String::new();
    let _ = write!(
        svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" font-family="Inter, system-ui, sans-serif" font-size="13" role="img" aria-label="{t}">
<rect width="{W}" height="{H}" fill="#ffffff"/>
<text x="{LEFT}" y="28" font-size="15" font-weight="600" fill="#1a1a1a">{t}</text>
"##,
        t = escape(title)
    );
    for i in 0..=steps {
        let v = y_min + step * f64::from(i);
        let y = py(v);
        let _ = writeln!(
            svg,
            r##"<line x1="{LEFT}" x2="{:.1}" y1="{y:.1}" y2="{y:.1}" stroke="#e5e5e5"/><text x="{:.1}" y="{:.1}" text-anchor="end" fill="#555">{v:.0}</text>"##,
            LEFT + plot_w,
            LEFT - 8.0,
            y + 4.0
        );
    }
    let x_step = nice(x_max / 6.0).max(1.0);
    let mut x = 0.0;
    while x <= x_max + 1e-9 {
        let _ = writeln!(
            svg,
            r##"<text x="{:.1}" y="{:.1}" text-anchor="middle" fill="#555">{x:.0}</text>"##,
            px(x),
            TOP + plot_h + 18.0
        );
        x += x_step;
    }
    let _ = write!(
        svg,
        r##"<rect x="{LEFT}" y="{TOP}" width="{plot_w}" height="{plot_h}" fill="none" stroke="#999"/>
<text x="{:.1}" y="{:.1}" text-anchor="middle" fill="#1a1a1a">{}</text>
<text transform="translate(18 {:.1}) rotate(-90)" text-anchor="middle" fill="#1a1a1a">{}</text>
"##,
        LEFT + plot_w / 2.0,
        H - 18.0,
        escape(x_label),
        TOP + plot_h / 2.0,
        escape(y_label)
    );
    for s in series {
        let d: Vec<String> = s
            .points
            .iter()
            .map(|&(x, y)| format!("{:.1},{:.1}", px(x), py(y)))
            .collect();
        let dash = if s.dash.is_empty() {
            String::new()
        } else {
            format!(r#" stroke-dasharray="{}""#, s.dash)
        };
        let _ = writeln!(
            svg,
            r#"<g><title>{}</title><polyline points="{}" fill="none" stroke="{}" stroke-width="{}"{dash} stroke-linejoin="round"/></g>"#,
            escape(&s.name),
            d.join(" "),
            s.colour,
            s.width
        );
    }
    let lx = LEFT + plot_w + 20.0;
    for (i, s) in series.iter().enumerate() {
        let y = TOP + 10.0 + i as f64 * 24.0;
        let dash = if s.dash.is_empty() {
            String::new()
        } else {
            format!(r#" stroke-dasharray="{}""#, s.dash)
        };
        let _ = writeln!(
            svg,
            r##"<line x1="{lx:.1}" x2="{:.1}" y1="{y:.1}" y2="{y:.1}" stroke="{}" stroke-width="{}"{dash}/><text x="{:.1}" y="{:.1}" fill="#1a1a1a">{}</text>"##,
            lx + 28.0,
            s.colour,
            s.width,
            lx + 36.0,
            y + 4.0,
            escape(&s.name)
        );
    }
    svg.push_str("</svg>\n");
    svg
}

/// A round axis maximum at or above `v`: 1, 1.5, 2, 2.5, 3, 4, 5, 6 or 8 times a power of ten.
fn nice(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let p = 10f64.powf(v.log10().floor());
    [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .iter()
        .map(|m| m * p)
        .find(|&c| c >= v)
        .unwrap_or(10.0 * p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(strategy: &str, calls: f64, error: f64) -> Point {
        Point {
            strategy: strategy.to_owned(),
            scenario: "s<1>".to_owned(),
            calls,
            error,
        }
    }

    #[test]
    fn every_point_is_drawn_with_a_tooltip() {
        let svg = pareto(
            &[
                point("central_exact", 1000.0, 0.0),
                point("local_split", 0.0, 30.0),
            ],
            "Test & chart",
        );
        assert!(svg.starts_with("<svg"));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<g><title>").count(), 2);
        assert!(svg.contains("s&lt;1&gt;"));
        assert!(svg.contains("Test &amp; chart"));
    }

    #[test]
    fn line_charts_draw_every_series_with_a_legend() {
        let svg = lines(
            &[
                Series {
                    name: "a & b".into(),
                    colour: "#000",
                    dash: "",
                    width: 2.0,
                    points: vec![(0.0, 0.0), (1.0, 340.0)],
                },
                Series {
                    name: "c".into(),
                    colour: "#111",
                    dash: "4 2",
                    width: 1.0,
                    points: vec![(0.0, 10.0), (1.0, 20.0)],
                },
            ],
            "T",
            "x",
            "y",
        );
        assert_eq!(svg.matches("<polyline").count(), 2);
        assert!(svg.contains("a &amp; b"));
        assert!(svg.contains(r#"stroke-dasharray="4 2""#));
        assert_eq!(nice(340.0), 400.0);
        assert_eq!(nice(2_350.0), 2_500.0);
        assert_eq!(nice(0.0), 1.0);
        assert_eq!(nice(2_000.0), 2_000.0);
    }

    #[test]
    fn zero_sits_on_the_axis() {
        assert_eq!(symlog(0.0), 0.0);
        assert!(symlog(1_000.0) > symlog(100.0));
    }
}
