//! Benchmark reports: Markdown summary and a self-contained HTML page with an
//! accuracy-vs-latency Pareto chart (WER vs p95 latency), table view and worst clips.

use crate::bench::{Clip, ClipResult, Summary};

fn pct(v: f64) -> String {
    format!("{:.1}%", v * 100.0)
}

/// Configs not dominated on (WER, p95 latency).
fn pareto(summaries: &[&Summary]) -> Vec<bool> {
    summaries
        .iter()
        .map(|a| {
            !summaries.iter().any(|b| {
                b.wer <= a.wer && b.latency_p95 <= a.latency_p95 && (b.wer < a.wer || b.latency_p95 < a.latency_p95)
            })
        })
        .collect()
}

pub fn markdown(all: &[(Summary, Vec<ClipResult>)], clips: &[Clip]) -> String {
    let summaries: Vec<&Summary> = all.iter().map(|(s, _)| s).collect();
    let front = pareto(&summaries);
    let mut tiers: Vec<String> = clips.iter().map(|c| c.tier.clone()).collect();
    tiers.sort();
    tiers.dedup();
    let mut md = String::from("# Wordfall benchmark\n\n");
    md.push_str(&format!("{} clips ({}). ★ = Pareto-optimal on WER vs p95 latency. ΔWER CI is vs the first config (negative = better).\n\n", clips.len(), tiers.join(", ")));
    md.push_str("| | config | WER | ΔWER 95% CI | jargon | format err | halluc. | p50 ms | p95 ms | ≤3s | 3–15s | 15–45s | >45s | RTF | load ms |\n");
    md.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for (s, star) in summaries.iter().zip(&front) {
        let b = |k: &str| s.latency_by_bucket.get(k).map(|v| v.to_string()).unwrap_or_else(|| "–".into());
        let ci = s.wer_delta_ci.map(|(lo, hi)| format!("{:+.1} … {:+.1}", lo * 100.0, hi * 100.0)).unwrap_or_else(|| "baseline".into());
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3} | {} |\n",
            if *star { "★" } else { "" },
            s.config,
            pct(s.wer),
            ci,
            pct(s.jargon_recall),
            pct(s.format_error),
            pct(s.hallucination_rate),
            s.latency_p50,
            s.latency_p95,
            b("≤3s"),
            b("3–15s"),
            b("15–45s"),
            b(">45s"),
            s.rtf,
            s.load_ms
        ));
    }
    if tiers.len() > 1 {
        md.push_str("\n## WER by tier\n\n| config |");
        let speech: Vec<&String> = tiers.iter().filter(|t| *t != "noise").collect();
        for t in &speech {
            md.push_str(&format!(" {t} |"));
        }
        md.push_str("\n|---|");
        md.push_str(&"---|".repeat(speech.len()));
        md.push('\n');
        for s in &summaries {
            md.push_str(&format!("| {} |", s.config));
            for t in &speech {
                md.push_str(&format!(" {} |", s.wer_by_tier.get(*t).map(|v| pct(*v)).unwrap_or_else(|| "–".into())));
            }
            md.push('\n');
        }
    }
    for (s, results) in all {
        md.push_str(&format!("\n## {}: worst clips\n\n", s.config));
        let mut worst: Vec<&ClipResult> = results.iter().filter(|r| r.score.word_errors > 0 || !r.score.missed_terms.is_empty() || (r.tier == "noise" && !r.hypothesis.is_empty())).collect();
        worst.sort_by(|a, b| (b.score.word_errors + b.score.missed_terms.len() * 2).cmp(&(a.score.word_errors + a.score.missed_terms.len() * 2)));
        for r in worst.iter().take(8) {
            md.push_str(&format!(
                "- `{}` ({} errors{}): {}\n  - ref: {}\n",
                r.clip,
                r.score.word_errors,
                if r.score.missed_terms.is_empty() { String::new() } else { format!(", missed {}", r.score.missed_terms.join(", ")) },
                if r.hypothesis.is_empty() { "∅".to_string() } else { r.hypothesis.clone() },
                if r.reference.is_empty() { "∅" } else { &r.reference }
            ));
        }
    }
    md
}

pub fn html(all: &[(Summary, Vec<ClipResult>)]) -> String {
    let summaries: Vec<&Summary> = all.iter().map(|(s, _)| s).collect();
    let front = pareto(&summaries);
    let data: Vec<serde_json::Value> = summaries
        .iter()
        .zip(&front)
        .map(|(s, f)| serde_json::json!({ "summary": s, "pareto": f }))
        .collect();
    // `</` can't appear inside the JSON script block.
    let json = serde_json::to_string(&data).unwrap().replace("</", "<\\/");
    TEMPLATE.replace("__DATA__", &json)
}

const TEMPLATE: &str = r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Wordfall benchmark</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
.viz-root { color-scheme: light; --page:#f9f9f7; --surface-1:#fcfcfb; --text-primary:#0b0b0b; --text-secondary:#52514e;
  --muted:#898781; --grid:#e1e0d9; --axis:#c3c2b7; --border:rgba(11,11,11,.10); --series-1:#2a78d6; --series-2:#eb6834; }
@media (prefers-color-scheme: dark) { :root:where(:not([data-theme="light"])) .viz-root { color-scheme: dark; --page:#0d0d0d;
  --surface-1:#1a1a19; --text-primary:#f0efec; --text-secondary:#c3c2b7; --grid:#2c2c2a; --axis:#383835;
  --border:rgba(255,255,255,.10); --series-1:#3987e5; --series-2:#d95926; } }
:root[data-theme="dark"] .viz-root { color-scheme: dark; --page:#0d0d0d; --surface-1:#1a1a19; --text-primary:#f0efec;
  --text-secondary:#c3c2b7; --grid:#2c2c2a; --axis:#383835; --border:rgba(255,255,255,.10); --series-1:#3987e5; --series-2:#d95926; }
body { margin:0; font-family: system-ui, -apple-system, "Segoe UI", sans-serif; }
.viz-root { background:var(--page); color:var(--text-primary); min-height:100vh; padding:28px; box-sizing:border-box; }
h1 { font-size:20px; margin:0 0 4px; } p.sub { color:var(--text-secondary); margin:0 0 20px; font-size:13px; }
.card { background:var(--surface-1); border:1px solid var(--border); border-radius:12px; padding:18px; margin-bottom:20px; position:relative; }
.legend { display:flex; gap:18px; font-size:12px; color:var(--text-secondary); margin-bottom:8px; }
.legend span { display:inline-flex; align-items:center; gap:6px; }
.legend i { width:10px; height:10px; border-radius:50%; display:inline-block; }
svg text { font-size:11px; fill:var(--muted); font-variant-numeric: tabular-nums; }
svg .label { fill:var(--text-secondary); font-size:11.5px; }
.tip { position:absolute; pointer-events:none; background:var(--surface-1); border:1px solid var(--border); border-radius:8px;
  padding:8px 10px; font-size:12px; box-shadow:0 4px 14px rgba(0,0,0,.18); display:none; min-width:180px; }
.tip b { display:block; margin-bottom:4px; } .tip div { display:flex; justify-content:space-between; gap:14px; color:var(--text-secondary); }
.tip div span:last-child { color:var(--text-primary); font-variant-numeric: tabular-nums; }
table { border-collapse:collapse; width:100%; font-size:12.5px; }
th, td { text-align:right; padding:6px 8px; border-bottom:1px solid var(--grid); font-variant-numeric: tabular-nums; }
th:first-child, td:first-child { text-align:left; } th { color:var(--text-secondary); font-weight:600; }
</style></head>
<body><div class="viz-root">
<h1>Wordfall benchmark</h1>
<p class="sub">Accuracy vs latency per config. Lower-left is better; blue points are Pareto-optimal (nothing is both more accurate and faster).</p>
<div class="card" id="chart-card">
  <div class="legend"><span><i style="background:var(--series-1)"></i>Pareto-optimal</span><span><i style="background:var(--series-2)"></i>Dominated</span></div>
  <svg id="chart" width="100%" height="420" role="img" aria-label="WER versus p95 latency per config"></svg>
  <div class="tip" id="tip"></div>
</div>
<div class="card"><table id="table"><thead><tr><th>Config</th><th>WER</th><th>Jargon recall</th><th>Format err</th><th>Halluc.</th><th>p50 ms</th><th>p95 ms</th><th>RTF</th><th>Load ms</th></tr></thead><tbody></tbody></table></div>
<script id="data" type="application/json">__DATA__</script>
<script>
const data = JSON.parse(document.getElementById('data').textContent);
const pct = v => (v * 100).toFixed(1) + '%';
const svg = document.getElementById('chart'), tip = document.getElementById('tip'), card = document.getElementById('chart-card');
const NS = 'http://www.w3.org/2000/svg';
function el(name, attrs, parent) { const e = document.createElementNS(NS, name); for (const k in attrs) e.setAttribute(k, attrs[k]); parent.appendChild(e); return e; }
function nice(max) { const p = Math.pow(10, Math.floor(Math.log10(max || 1))); for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= max) return m * p; return 10 * p; }
function draw() {
  svg.innerHTML = '';
  const W = svg.clientWidth, H = 420, m = { l: 56, r: 24, t: 12, b: 44 };
  const xs = data.map(d => d.summary.latency_p95), ys = data.map(d => d.summary.wer);
  const xMax = nice(Math.max(...xs) * 1.1), yMax = nice(Math.max(...ys) * 1.15);
  const x = v => m.l + (v / xMax) * (W - m.l - m.r), y = v => H - m.b - (v / yMax) * (H - m.t - m.b);
  for (let i = 0; i <= 5; i++) {
    const yv = (yMax / 5) * i, xv = (xMax / 5) * i;
    el('line', { x1: m.l, x2: W - m.r, y1: y(yv), y2: y(yv), stroke: 'var(--grid)', 'stroke-width': 1 }, svg);
    el('text', { x: m.l - 8, y: y(yv) + 4, 'text-anchor': 'end' }, svg).textContent = (yv * 100).toFixed(yMax < 0.05 ? 1 : 0) + '%';
    el('text', { x: x(xv), y: H - m.b + 18, 'text-anchor': 'middle' }, svg).textContent = Math.round(xv) + ' ms';
  }
  el('line', { x1: m.l, x2: W - m.r, y1: H - m.b, y2: H - m.b, stroke: 'var(--axis)', 'stroke-width': 1 }, svg);
  el('text', { x: (W + m.l) / 2, y: H - 6, 'text-anchor': 'middle' }, svg).textContent = 'p95 latency (release → text)';
  el('text', { x: 14, y: (H - m.b) / 2, transform: `rotate(-90 14 ${(H - m.b) / 2})`, 'text-anchor': 'middle' }, svg).textContent = 'Word error rate';
  data.forEach((d, i) => {
    const cx = x(d.summary.latency_p95), cy = y(d.summary.wer);
    const g = el('g', { tabindex: 0, role: 'img', 'aria-label': `${d.summary.config}: WER ${pct(d.summary.wer)}, p95 ${d.summary.latency_p95} ms` }, svg);
    el('circle', { cx, cy, r: 12, fill: 'transparent' }, g);
    const dot = el('circle', { cx, cy, r: 5, fill: d.pareto ? 'var(--series-1)' : 'var(--series-2)', stroke: 'var(--surface-1)', 'stroke-width': 2 }, g);
    if (d.pareto || i === 0) el('text', { x: cx + 9, y: cy - 8, class: 'label' }, svg).textContent = d.summary.config;
    const show = () => {
      dot.setAttribute('r', 6.5);
      tip.replaceChildren();
      const b = document.createElement('b'); b.textContent = d.summary.config; tip.appendChild(b);
      for (const [k, v] of [['WER', pct(d.summary.wer)], ['Jargon recall', pct(d.summary.jargon_recall)], ['Hallucination', pct(d.summary.hallucination_rate)], ['p50', d.summary.latency_p50 + ' ms'], ['p95', d.summary.latency_p95 + ' ms']]) {
        const row = document.createElement('div'); const a = document.createElement('span'); a.textContent = k; const c = document.createElement('span'); c.textContent = v; row.append(a, c); tip.appendChild(row);
      }
      tip.style.display = 'block';
      const left = Math.min(cx + 16, card.clientWidth - tip.offsetWidth - 8);
      tip.style.left = left + 'px'; tip.style.top = (cy + 30) + 'px';
    };
    const hide = () => { dot.setAttribute('r', 5); tip.style.display = 'none'; };
    g.addEventListener('pointerenter', show); g.addEventListener('pointerleave', hide);
    g.addEventListener('focus', show); g.addEventListener('blur', hide);
  });
}
const tbody = document.querySelector('#table tbody');
for (const d of data) {
  const s = d.summary, tr = document.createElement('tr');
  for (const v of [(d.pareto ? '★ ' : '') + s.config, pct(s.wer), pct(s.jargon_recall), pct(s.format_error), pct(s.hallucination_rate), s.latency_p50, s.latency_p95, s.rtf.toFixed(3), s.load_ms]) {
    const td = document.createElement('td'); td.textContent = v; tr.appendChild(td);
  }
  tbody.appendChild(tr);
}
draw(); addEventListener('resize', draw);
</script></div></body></html>
"##;
